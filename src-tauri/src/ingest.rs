use crate::models::*;
use crate::text;
use anyhow::Result;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

const MAX_PROMPTS: usize = 25;
const PROMPT_CHARS: usize = 400;
const RESPONSE_CHARS: usize = 1500;
const TITLE_CHARS: usize = 120;

pub fn transcripts_root() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
        .join("projects")
}

pub struct DiscoveredFile {
    pub path: PathBuf,
    pub session_id: String,
    pub content_hash: String,
    pub mtime: u64,
}

fn mtime_secs(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn is_transcript(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "jsonl")
}

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

pub fn discover() -> Vec<DiscoveredFile> {
    let mut out = Vec::new();
    let root = transcripts_root();
    let Ok(projects) = std::fs::read_dir(&root) else {
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
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let Ok(meta) = file.metadata() else { continue };
            let mtime = mtime_secs(&meta);
            let mut content_hash = format!("{}:{}", meta.len(), mtime);
            // Delegated work can change without the parent growing, and the suffix
            // only appears when there is any, so existing hashes stay valid.
            let sidechains: Vec<std::fs::Metadata> = sidechain_files(&path)
                .iter()
                .filter_map(|p| p.metadata().ok())
                .collect();
            if !sidechains.is_empty() {
                let len: u64 = sidechains.iter().map(|m| m.len()).sum();
                let newest = sidechains.iter().map(mtime_secs).max().unwrap_or(0);
                content_hash.push_str(&format!(":{}:{len}:{newest}", sidechains.len()));
            }
            out.push(DiscoveredFile {
                session_id: stem.to_string(),
                content_hash,
                path,
                mtime,
            });
        }
    }
    out
}

fn earlier(a: &str, b: &str) -> bool {
    match (
        chrono::DateTime::parse_from_rfc3339(a),
        chrono::DateTime::parse_from_rfc3339(b),
    ) {
        (Ok(a), Ok(b)) => a < b,
        _ => a < b,
    }
}

fn update_timestamps(facts: &mut SessionFacts, ts: &str) {
    if facts.first_ts.as_deref().is_none_or(|cur| earlier(ts, cur)) {
        facts.first_ts = Some(ts.to_string());
    }
    if facts.last_ts.as_deref().is_none_or(|cur| earlier(cur, ts)) {
        facts.last_ts = Some(ts.to_string());
    }
}

fn classify_command(cmd: &str) -> &'static str {
    let c = cmd.to_lowercase();
    let test_markers = [
        "cargo test",
        "npm test",
        "npm run test",
        "pytest",
        "jest",
        "vitest",
        "go test",
        "xcodebuild test",
        "swift test",
        "mvn test",
        "gradle test",
        "rspec",
        "phpunit",
        "bundle exec rspec",
    ];
    let build_markers = [
        "cargo build",
        "npm run build",
        "tsc",
        "xcodebuild build",
        "swift build",
        "make",
        "go build",
        "gradle build",
        "mvn package",
        "vite build",
        "webpack",
        "cargo check",
    ];
    let check_markers = [
        "clippy",
        "eslint",
        "lint",
        "fmt --check",
        "prettier --check",
        "typecheck",
        "mypy",
        "ruff",
    ];
    if test_markers.iter().any(|m| c.contains(m)) {
        "test"
    } else if check_markers.iter().any(|m| c.contains(m)) {
        "check"
    } else if build_markers.iter().any(|m| c.contains(m)) {
        "build"
    } else if c.starts_with("git ") || c.contains(" git ") {
        "git"
    } else {
        "other"
    }
}

const MUTATING_VERBS: &[&str] = &[
    "add",
    "append",
    "archive",
    "assign",
    "close",
    "copy",
    "create",
    "delete",
    "duplicate",
    "edit",
    "insert",
    "merge",
    "move",
    "post",
    "publish",
    "remove",
    "rename",
    "reply",
    "schedule",
    "send",
    "set",
    "submit",
    "transition",
    "update",
    "upload",
    "write",
];

// Tool metadata has no mutability flag, so only whole action words count as writes.
fn mutates_external_state(tool: &str) -> bool {
    let mut words = String::new();
    for c in tool.chars() {
        if c == '_' || c == '-' {
            words.push(' ');
        } else {
            if c.is_uppercase() {
                words.push(' ');
            }
            words.extend(c.to_lowercase());
        }
    }
    words
        .split_whitespace()
        .any(|w| MUTATING_VERBS.contains(&w))
}

#[derive(Default)]
struct Parse {
    facts: SessionFacts,
    files: HashMap<String, FileChange>,
    commands: Vec<(String, CommandFact)>,
    actions: Vec<(String, ExternalAction)>,
    results: HashMap<String, bool>,
    pr_urls: HashSet<String>,
}

// Transcript JSONL is undocumented; unknown records and missing fields are ignored.
pub fn parse_transcript(
    path: &Path,
    session_id: &str,
    retain_prompts: bool,
) -> Result<SessionFacts> {
    let mut p = Parse {
        facts: SessionFacts {
            session_id: session_id.to_string(),
            file_path: path.to_string_lossy().to_string(),
            ..Default::default()
        },
        ..Default::default()
    };
    absorb(&mut p, &std::fs::read_to_string(path)?);
    for side in sidechain_files(path) {
        if let Ok(content) = std::fs::read_to_string(&side) {
            absorb(&mut p, &content);
        }
    }

    let Parse {
        mut facts,
        files,
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
    facts.files_changed = files.into_values().collect();
    facts.files_changed.sort_by(|a, b| a.path.cmp(&b.path));
    if !retain_prompts {
        facts.prompts.clear();
        facts.final_response = None;
    }
    Ok(facts)
}

fn absorb(p: &mut Parse, content: &str) {
    for line in content.lines() {
        if let Ok(record) = serde_json::from_str::<Value>(line) {
            absorb_record(p, &record);
        }
    }
}

fn absorb_record(p: &mut Parse, record: &Value) {
    match record["type"].as_str() {
        Some("ai-title") => {
            if let Some(title) = record["aiTitle"]
                .as_str()
                .map(str::trim)
                .filter(|title| !title.is_empty())
            {
                p.facts.title = Some(text::truncate(title, TITLE_CHARS));
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
    if let Some(branch) = record["gitBranch"].as_str() {
        if !branch.is_empty() && branch != "HEAD" {
            facts.git_branch = Some(branch.to_owned());
        }
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
        if is_human && !is_meta && p.facts.prompts.len() < MAX_PROMPTS {
            p.facts
                .prompts
                .push(text::truncate(prompt.trim(), PROMPT_CHARS));
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
    let Some(response) = block["text"].as_str().map(str::trim) else {
        return;
    };
    if !delegated && !response.is_empty() {
        p.facts.final_response = Some(text::truncate(response, RESPONSE_CHARS));
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
            let fact = CommandFact {
                id: format!("cmd:{}:{}", p.facts.session_id, p.commands.len()),
                command: text::truncate(command, 300),
                ok: true,
                kind: classify_command(command).to_owned(),
                ts: timestamp.map(String::from),
                via_delegate: delegated,
            };
            p.commands.push((id.to_owned(), fact));
        }
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => {
            let Some(path) = input["file_path"].as_str() else {
                return;
            };
            let entry = p.files.entry(path.to_owned()).or_insert(FileChange {
                path: path.to_owned(),
                tool: name.to_owned(),
                count: 0,
                via_delegate: delegated,
            });
            entry.count += 1;
            // Any direct edit makes the combined file change direct.
            entry.via_delegate &= delegated;
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
            let action = ExternalAction {
                id: format!("action:{}:{}", p.facts.session_id, p.actions.len()),
                server: server.to_owned(),
                tool: tool.to_owned(),
                ok: true,
                mutating: mutates_external_state(tool),
                ts: timestamp.map(String::from),
                via_delegate: delegated,
            };
            p.actions.push((id.to_owned(), action));
        }
    }
}

pub fn session_day(facts: &SessionFacts) -> Option<String> {
    let ts = facts.last_ts.as_deref()?;
    let dt = chrono::DateTime::parse_from_rfc3339(ts).ok()?;
    Some(
        dt.with_timezone(&chrono::Local)
            .format("%Y-%m-%d")
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_commands() {
        assert_eq!(classify_command("cargo test --all"), "test");
        assert_eq!(classify_command("npx tsc --noEmit"), "build");
        assert_eq!(classify_command("cargo clippy"), "check");
        assert_eq!(classify_command("git commit -m x"), "git");
        assert_eq!(classify_command("ls -la"), "other");
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
    fn classifies_mutating_mcp_tools() {
        for tool in [
            "addCommentToJiraIssue",
            "createJiraIssue",
            "transitionJiraIssue",
            "notion-update-page",
            "slack_send_message",
            "upload_assets",
        ] {
            assert!(mutates_external_state(tool), "{tool} should be a mutation");
        }
        for tool in [
            "getTransitionsForJiraIssue",
            "download_assets",
            "searchJiraIssuesUsingJql",
            "notion-fetch",
            "preview_screenshot",
        ] {
            assert!(!mutates_external_state(tool), "{tool} should be a read");
        }
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
