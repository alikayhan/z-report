use super::{
    clean_title, command_fact, describe, each_record, external_action, home, is_transcript,
    named_branch, push_prompt, record_file_change, set_final_response, update_timestamps,
    DiscoveredFile,
};
use crate::models::*;
use anyhow::Result;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

fn sidechain_files(transcript: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(transcript.with_extension("").join("subagents")) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| is_transcript(p))
        .collect();
    out.sort();
    out
}

pub(super) fn discover() -> Vec<DiscoveredFile> {
    let mut out = Vec::new();
    let Ok(projects) = std::fs::read_dir(home().join(".claude").join("projects")) else {
        return out;
    };
    for project in projects.flatten() {
        let Ok(files) = std::fs::read_dir(project.path()) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if !is_transcript(&path) {
                continue;
            }
            let Some(session_id) = path.file_stem().and_then(|s| s.to_str()).map(str::to_owned)
            else {
                continue;
            };
            let delegates = sidechain_files(&path);
            out.extend(describe(Agent::Claude, path, session_id, None, delegates));
        }
    }
    out
}

#[derive(Default)]
struct Parse {
    facts: SessionFacts,
    commands: Vec<(String, CommandFact)>,
    actions: Vec<(String, ExternalAction)>,
    results: HashMap<String, bool>,
    pr_urls: HashSet<String>,
}

// Transcript JSONL is undocumented; unknown records and missing fields are ignored.
pub(super) fn parse(file: &DiscoveredFile, facts: SessionFacts) -> Result<SessionFacts> {
    let mut p = Parse {
        facts,
        ..Default::default()
    };
    each_record(file, |record, _| absorb_record(&mut p, record))?;

    let Parse {
        mut facts,
        commands,
        actions,
        results,
        ..
    } = p;
    // Missing tool results carry no evidence of failure.
    let succeeded = |id: &str| results.get(id).copied().unwrap_or(true);
    facts.commands = commands
        .into_iter()
        .map(|(id, mut c)| {
            c.ok = succeeded(&id);
            c
        })
        .collect();
    facts.external_actions = actions
        .into_iter()
        .map(|(id, mut a)| {
            a.ok = succeeded(&id);
            a
        })
        .collect();
    Ok(facts)
}

fn absorb_record(p: &mut Parse, record: &Value) {
    match record["type"].as_str() {
        Some("ai-title") => {
            if let Some(title) = record["aiTitle"].as_str().and_then(clean_title) {
                p.facts.title = Some(title);
            }
        }
        Some("pr-link") => absorb_pr_link(p, record),
        Some(kind @ ("user" | "assistant")) => {
            let delegated = record["isSidechain"].as_bool() == Some(true);
            if let Some(timestamp) = record["timestamp"].as_str() {
                update_timestamps(&mut p.facts, timestamp);
            }
            if !delegated {
                absorb_session_metadata(&mut p.facts, record);
            }
            if kind == "user" {
                absorb_user(p, record, delegated);
            } else {
                absorb_assistant(p, record, delegated);
            }
        }
        _ => {}
    }
}

fn absorb_pr_link(p: &mut Parse, record: &Value) {
    let Some(number) = record["prNumber"].as_u64() else {
        return;
    };
    let (Some(url), Some(repository)) = (record["prUrl"].as_str(), record["prRepository"].as_str())
    else {
        return;
    };
    let pr = PrLink {
        number,
        url: url.to_owned(),
        repository: repository.to_owned(),
        ts: record["timestamp"].as_str().map(String::from),
    };
    if !pr.has_canonical_url() {
        return;
    }
    // PR timestamps extend the session span across midnight and its Git correlation window.
    if let Some(timestamp) = pr.ts.as_deref() {
        update_timestamps(&mut p.facts, timestamp);
    }
    if p.pr_urls.insert(pr.url.clone()) {
        p.facts.pr_links.push(pr);
    }
}

fn absorb_session_metadata(facts: &mut SessionFacts, record: &Value) {
    if let Some(cwd) = record["cwd"].as_str() {
        facts.cwd = Some(cwd.to_owned());
    }
    if let Some(branch) = named_branch(&record["gitBranch"]) {
        facts.git_branch = Some(branch);
    }
    if let Some(version) = record["version"].as_str() {
        facts.cli_version = Some(version.to_owned());
    }
}

fn absorb_user(p: &mut Parse, record: &Value, delegated: bool) {
    let message = &record["message"];
    if let Some(prompt) = message["content"].as_str() {
        let is_human = !delegated
            && record["origin"]["kind"]
                .as_str()
                .is_none_or(|kind| kind == "human");
        let is_meta = prompt.starts_with('<') || prompt.starts_with("[Request interrupted");
        if is_human && !is_meta {
            push_prompt(&mut p.facts, prompt);
        }
        return;
    }

    let Some(blocks) = message["content"].as_array() else {
        return;
    };
    for block in blocks {
        if block["type"].as_str() == Some("tool_result") {
            if let Some(id) = block["tool_use_id"].as_str() {
                p.results
                    .insert(id.to_owned(), block["is_error"].as_bool() != Some(true));
            }
        }
    }
}

fn absorb_assistant(p: &mut Parse, record: &Value, delegated: bool) {
    let Some(blocks) = record["message"]["content"].as_array() else {
        return;
    };
    for block in blocks {
        match block["type"].as_str() {
            Some("text") => absorb_response(p, block, delegated),
            Some("tool_use") => absorb_tool_use(p, block, record["timestamp"].as_str(), delegated),
            _ => {}
        }
    }
}

fn absorb_response(p: &mut Parse, block: &Value, delegated: bool) {
    if let Some(response) = block["text"].as_str().filter(|_| !delegated) {
        set_final_response(&mut p.facts, response);
    }
}

fn absorb_tool_use(p: &mut Parse, block: &Value, timestamp: Option<&str>, delegated: bool) {
    let name = block["name"].as_str().unwrap_or("");
    let input = &block["input"];
    match name {
        "Bash" => {
            let (Some(id), Some(command)) = (block["id"].as_str(), input["command"].as_str())
            else {
                return;
            };
            let fact = command_fact(
                &p.facts.session_id,
                p.commands.len(),
                command,
                timestamp,
                delegated,
            );
            p.commands.push((id.to_owned(), fact));
        }
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => {
            let Some(path) = input["file_path"].as_str() else {
                return;
            };
            record_file_change(&mut p.facts, path, name, delegated);
        }
        _ => {
            let Some((server, tool)) = name
                .strip_prefix("mcp__")
                .and_then(|rest| rest.split_once("__"))
            else {
                return;
            };
            let Some(id) = block["id"].as_str() else {
                return;
            };
            let action = external_action(
                &p.facts.session_id,
                p.actions.len(),
                server,
                tool,
                timestamp,
                delegated,
            );
            p.actions.push((id.to_owned(), action));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::session_day;

    fn parse_transcript(
        path: &Path,
        session_id: &str,
        retain_prompts: bool,
    ) -> Result<SessionFacts> {
        let file = describe(
            Agent::Claude,
            path.to_path_buf(),
            session_id.to_owned(),
            None,
            sidechain_files(path),
        )
        .expect("fixture exists");
        crate::ingest::parse_transcript(&file, retain_prompts)
    }

    #[test]
    fn parses_minimal_transcript() {
        let dir = std::env::temp_dir().join("zreport-test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("s1.jsonl");
        let lines = [
            r#"{"type":"mode","mode":"normal","sessionId":"s1"}"#,
            r#"{"type":"ai-title","aiTitle":"Fix the failing login flow","sessionId":"s1"}"#,
            r#"{"type":"user","message":{"role":"user","content":"Fix the login bug"},"timestamp":"2026-07-20T10:00:00.000Z","cwd":"/tmp/repo","gitBranch":"main","version":"2.1.215","origin":{"kind":"human"}}"#,
            r#"{"type":"ai-title","aiTitle":"Fix the failing login flow","sessionId":"s1"}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"cargo test"}}]},"timestamp":"2026-07-20T10:01:00.000Z"}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","is_error":false}]},"timestamp":"2026-07-20T10:02:00.000Z"}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t2","name":"Edit","input":{"file_path":"/tmp/repo/src/auth.rs"}}]},"timestamp":"2026-07-20T10:03:00.000Z"}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Fixed the login bug and tests pass."}]},"timestamp":"2026-07-20T10:04:00.000Z"}"#,
        ];
        std::fs::write(&p, lines.join("\n")).unwrap();
        let facts = parse_transcript(&p, "s1", true).unwrap();
        assert_eq!(facts.agent, Agent::Claude);
        assert_eq!(facts.prompts, vec!["Fix the login bug"]);
        assert_eq!(facts.title.as_deref(), Some("Fix the failing login flow"));
        assert_eq!(facts.cwd.as_deref(), Some("/tmp/repo"));
        assert_eq!(facts.git_branch.as_deref(), Some("main"));
        assert_eq!(facts.commands.len(), 1);
        assert!(facts.commands[0].ok);
        assert_eq!(facts.commands[0].kind, "test");
        assert_eq!(facts.files_changed.len(), 1);
        assert_eq!(
            facts.final_response.as_deref(),
            Some("Fixed the login bug and tests pass.")
        );
        assert_eq!(session_day(&facts).unwrap(), {
            let dt = chrono::DateTime::parse_from_rfc3339("2026-07-20T10:04:00.000Z").unwrap();
            dt.with_timezone(&chrono::Local)
                .format("%Y-%m-%d")
                .to_string()
        });
    }

    #[test]
    fn folds_delegated_work_into_the_session() {
        let dir = std::env::temp_dir().join("zreport-test/delegated");
        let subagents = dir.join("s2/subagents");
        std::fs::create_dir_all(&subagents).unwrap();
        let parent = dir.join("s2.jsonl");
        std::fs::write(
            &parent,
            [
                r#"{"type":"user","message":{"role":"user","content":"Review the diff"},"timestamp":"2026-07-20T10:00:00.000Z","cwd":"/tmp/repo","gitBranch":"main","origin":{"kind":"human"}}"#,
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Edit","input":{"file_path":"/tmp/repo/src/a.rs"}}]},"timestamp":"2026-07-20T10:01:00.000Z"}"#,
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Review complete."}]},"timestamp":"2026-07-20T10:30:00.000Z"}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        std::fs::write(
            subagents.join("agent-a1.jsonl"),
            [
                r#"{"type":"user","isSidechain":true,"message":{"role":"user","content":"You are reviewing a diff for efficiency"},"timestamp":"2026-07-20T10:02:00.000Z","cwd":"/tmp/worktree","gitBranch":"detached-copy"}"#,
                r#"{"type":"assistant","isSidechain":true,"message":{"role":"assistant","content":[{"type":"tool_use","id":"t2","name":"Bash","input":{"command":"cargo test --all"}}]},"timestamp":"2026-07-20T10:03:00.000Z"}"#,
                r#"{"type":"user","isSidechain":true,"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t2","is_error":false}]},"timestamp":"2026-07-20T10:04:00.000Z"}"#,
                r#"{"type":"assistant","isSidechain":true,"message":{"role":"assistant","content":[{"type":"tool_use","id":"t3","name":"Edit","input":{"file_path":"/tmp/repo/src/a.rs"}},{"type":"tool_use","id":"t4","name":"Write","input":{"file_path":"/tmp/repo/src/b.rs"}}]},"timestamp":"2026-07-20T10:05:00.000Z"}"#,
                r#"{"type":"assistant","isSidechain":true,"message":{"role":"assistant","content":[{"type":"text","text":"Delegate summary."}]},"timestamp":"2026-07-20T10:06:00.000Z"}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let facts = parse_transcript(&parent, "s2", true).unwrap();

        assert_eq!(facts.prompts, vec!["Review the diff"]);
        assert_eq!(facts.final_response.as_deref(), Some("Review complete."));
        assert_eq!(facts.cwd.as_deref(), Some("/tmp/repo"));
        assert_eq!(facts.git_branch.as_deref(), Some("main"));
        assert_eq!(facts.commands.len(), 1);
        assert_eq!(facts.commands[0].id, "cmd:s2:0");
        assert!(facts.commands[0].ok && facts.commands[0].via_delegate);
        assert_eq!(facts.commands[0].kind, "test");
        let changed: Vec<(&str, bool)> = facts
            .files_changed
            .iter()
            .map(|f| (f.path.as_str(), f.via_delegate))
            .collect();
        assert_eq!(
            changed,
            vec![("/tmp/repo/src/a.rs", false), ("/tmp/repo/src/b.rs", true)]
        );
        assert_eq!(facts.files_changed[0].count, 2);
        assert_eq!(facts.first_ts.as_deref(), Some("2026-07-20T10:00:00.000Z"));
        assert_eq!(facts.last_ts.as_deref(), Some("2026-07-20T10:30:00.000Z"));
    }

    #[test]
    fn delegated_work_alone_is_substantial() {
        let dir = std::env::temp_dir().join("zreport-test/delegated-only");
        let subagents = dir.join("s3/subagents");
        std::fs::create_dir_all(&subagents).unwrap();
        let parent = dir.join("s3.jsonl");
        std::fs::write(&parent, "").unwrap();
        std::fs::write(
            subagents.join("agent-a1.jsonl"),
            r#"{"type":"assistant","isSidechain":true,"message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Write","input":{"file_path":"/tmp/repo/src/c.rs"}}]},"timestamp":"2026-07-20T10:05:00.000Z"}"#,
        )
        .unwrap();

        let facts = parse_transcript(&parent, "s3", true).unwrap();

        assert!(facts.has_substance());
        assert!(facts.prompts.is_empty());
        assert_eq!(facts.files_changed.len(), 1);
    }

    #[test]
    fn cites_only_external_actions_that_change_something() {
        let dir = std::env::temp_dir().join("zreport-test/external-actions");
        let subagents = dir.join("s4/subagents");
        std::fs::create_dir_all(&subagents).unwrap();
        let parent = dir.join("s4.jsonl");
        let mcp = |id: &str, name: &str, ts: &str| {
            format!(
                r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"{id}","name":"{name}","input":{{}}}}]}},"timestamp":"{ts}"}}"#
            )
        };
        let result = |id: &str, is_error: bool| {
            format!(
                r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"{id}","is_error":{is_error}}}]}},"timestamp":"2026-07-20T10:10:00.000Z"}}"#
            )
        };
        std::fs::write(
            &parent,
            [
                mcp(
                    "t1",
                    "mcp__claude_ai_Atlassian__searchJiraIssuesUsingJql",
                    "2026-07-20T10:00:00.000Z",
                ),
                mcp(
                    "t2",
                    "mcp__claude_ai_Atlassian__addCommentToJiraIssue",
                    "2026-07-20T10:01:00.000Z",
                ),
                result("t2", false),
                mcp(
                    "t3",
                    "mcp__Claude_Preview__preview_screenshot",
                    "2026-07-20T10:03:00.000Z",
                ),
                mcp(
                    "t4",
                    "mcp__claude_ai_Notion__notion-update-page",
                    "2026-07-20T10:04:00.000Z",
                ),
                result("t4", true),
            ]
            .join("\n"),
        )
        .unwrap();
        std::fs::write(
            subagents.join("agent-a1.jsonl"),
            r#"{"type":"assistant","isSidechain":true,"message":{"role":"assistant","content":[{"type":"tool_use","id":"t5","name":"mcp__claude_ai_Atlassian__editJiraIssue","input":{}}]},"timestamp":"2026-07-20T10:06:00.000Z"}"#,
        )
        .unwrap();

        let facts = parse_transcript(&parent, "s4", true).unwrap();

        let cited: Vec<(&str, &str, bool, bool)> = facts
            .external_changes()
            .map(|a| (a.id.as_str(), a.tool.as_str(), a.ok, a.via_delegate))
            .collect();
        assert_eq!(
            cited,
            vec![
                ("action:s4:1", "addCommentToJiraIssue", true, false),
                ("action:s4:3", "notion-update-page", false, false),
                ("action:s4:4", "editJiraIssue", true, true),
            ]
        );
        assert_eq!(facts.external_actions.len(), 5);
        assert_eq!(
            facts.external_changes().next().unwrap().server,
            "claude_ai_Atlassian"
        );
        assert!(facts.has_substance());
    }

    #[test]
    fn read_only_external_calls_are_not_substance() {
        let dir = std::env::temp_dir().join("zreport-test/external-reads");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("s5.jsonl");
        std::fs::write(
            &p,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"mcp__claude_ai_Notion__notion-fetch","input":{}}]},"timestamp":"2026-07-20T10:00:00.000Z"}"#,
        )
        .unwrap();

        let facts = parse_transcript(&p, "s5", true).unwrap();

        assert_eq!(facts.external_actions.len(), 1);
        assert!(!facts.has_substance());
    }

    #[test]
    fn parses_and_deduplicates_pr_links() {
        let dir = std::env::temp_dir().join("zreport-test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("pr-links.jsonl");
        let lines = [
            r#"{"type":"user","message":{"role":"user","content":"Ship the fix"},"timestamp":"2026-07-20T10:00:00.000Z"}"#,
            r#"{"type":"pr-link","sessionId":"s1","prNumber":5159,"prUrl":"https://github.com/acme/widgets/pull/5159","prRepository":"acme/widgets","timestamp":"2026-07-20T10:04:00.000Z"}"#,
            r#"{"type":"pr-link","sessionId":"s1","prNumber":5159,"prUrl":"https://github.com/acme/widgets/pull/5159","prRepository":"acme/widgets","timestamp":"2026-07-20T10:05:00.000Z"}"#,
            r#"{"type":"pr-link","sessionId":"s1","prNumber":9999,"prUrl":"javascript:alert(1)","prRepository":"acme/widgets","timestamp":"2026-07-20T10:06:00.000Z"}"#,
            r#"{"type":"pr-link","sessionId":"s1","prNumber":9998,"prUrl":"https://github.com/acme/widgets/pull/9999","prRepository":"acme/widgets","timestamp":"2026-07-20T10:07:00.000Z"}"#,
            r#"{"type":"pr-link","sessionId":"s1","prNumber":5160,"prUrl":"https://github.com/acme/widgets/pull/5160","prRepository":"acme/widgets","timestamp":"2026-07-21T00:06:00.000Z"}"#,
        ];
        std::fs::write(&p, lines.join("\n")).unwrap();

        let facts = parse_transcript(&p, "s1", true).unwrap();

        assert_eq!(facts.pr_links.len(), 2);
        assert_eq!(
            facts.pr_links[0],
            PrLink {
                number: 5159,
                url: "https://github.com/acme/widgets/pull/5159".into(),
                repository: "acme/widgets".into(),
                ts: Some("2026-07-20T10:04:00.000Z".into()),
            }
        );
        assert_eq!(facts.pr_links[1].number, 5160);
        assert_eq!(facts.last_ts.as_deref(), Some("2026-07-21T00:06:00.000Z"));
        assert_eq!(
            session_day(&facts),
            Some(
                chrono::DateTime::parse_from_rfc3339("2026-07-21T00:06:00.000Z")
                    .unwrap()
                    .with_timezone(&chrono::Local)
                    .format("%Y-%m-%d")
                    .to_string()
            )
        );
    }
}
