use super::{
    clean_title, command_fact, describe, each_record, external_action, home, is_transcript,
    named_branch, push_prompt, record_file_change, set_final_response, update_timestamps,
    DiscoveredFile,
};
use crate::models::*;
use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

const MAX_DEPTH: usize = 6;
// Earlier releases wrote several other record shapes; item_completed events start here.
const FIRST_SUPPORTED: (u64, u64) = (0, 147);

fn supported_version(version: &str) -> bool {
    let mut parts = version.split(['.', '-']).map(|part| part.parse::<u64>());
    match (parts.next(), parts.next()) {
        (Some(Ok(major)), Some(Ok(minor))) => (major, minor) >= FIRST_SUPPORTED,
        _ => false,
    }
}

pub(super) fn discover() -> Vec<DiscoveredFile> {
    let root = home().join(".codex");
    discover_in(
        &[root.join("sessions"), root.join("archived_sessions")],
        &thread_names(&root.join("session_index.jsonl")),
    )
}

// Only the desktop app names threads; CLI threads are absent from the index.
fn thread_names(index: &Path) -> HashMap<String, String> {
    let Ok(content) = std::fs::read_to_string(index) else {
        return HashMap::new();
    };
    super::records(&content)
        .filter_map(|entry| {
            let id = entry["id"].as_str()?.to_owned();
            Some((id, clean_title(entry["thread_name"].as_str()?)?))
        })
        .collect()
}

enum Thread {
    Session { id: String },
    Delegate { id: String, parent: String },
    Skip,
}

fn classify(path: &Path) -> Thread {
    let Ok(file) = std::fs::File::open(path) else {
        return Thread::Skip;
    };
    let mut first = String::new();
    if BufReader::new(file).read_line(&mut first).is_err() {
        return Thread::Skip;
    }
    let Ok(record) = serde_json::from_str::<Value>(&first) else {
        return Thread::Skip;
    };
    if record["type"].as_str() != Some("session_meta") {
        return Thread::Skip;
    }
    let meta = &record["payload"];
    if !meta["cli_version"].as_str().is_some_and(supported_version) {
        return Thread::Skip;
    }
    let Some(id) = meta["id"].as_str().map(str::to_owned) else {
        return Thread::Skip;
    };
    let subagent = &meta["source"]["subagent"];
    // Approval-reviewer threads quote the parent's transcript as their own prompt.
    if subagent.get("other").is_some() {
        return Thread::Skip;
    }
    match meta["parent_thread_id"].as_str() {
        Some(parent) => Thread::Delegate {
            id,
            parent: parent.to_owned(),
        },
        None if subagent.is_object() => Thread::Skip,
        None => Thread::Session { id },
    }
}

fn transcripts_under(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            if depth < MAX_DEPTH {
                transcripts_under(&path, depth + 1, out);
            }
        } else if is_transcript(&path) {
            out.push(path);
        }
    }
}

fn descendants(
    children: &HashMap<String, Vec<(String, PathBuf)>>,
    id: &str,
    depth: usize,
    out: &mut Vec<PathBuf>,
) {
    if depth >= MAX_DEPTH {
        return;
    }
    for (child_id, path) in children.get(id).into_iter().flatten() {
        out.push(path.clone());
        descendants(children, child_id, depth + 1, out);
    }
}

fn discover_in(roots: &[PathBuf], names: &HashMap<String, String>) -> Vec<DiscoveredFile> {
    let mut paths = Vec::new();
    for root in roots {
        transcripts_under(root, 0, &mut paths);
    }
    let mut sessions: Vec<(String, PathBuf)> = Vec::new();
    let mut children: HashMap<String, Vec<(String, PathBuf)>> = HashMap::new();
    for path in paths {
        match classify(&path) {
            Thread::Session { id } => sessions.push((id, path)),
            Thread::Delegate { id, parent } => children.entry(parent).or_default().push((id, path)),
            Thread::Skip => {}
        }
    }
    sessions
        .into_iter()
        .filter_map(|(id, path)| {
            let mut delegates = Vec::new();
            descendants(&children, &id, 0, &mut delegates);
            delegates.sort();
            describe(
                Agent::Codex,
                path,
                id.clone(),
                names.get(&id).cloned(),
                delegates,
            )
        })
        .collect()
}

pub(super) fn parse(file: &DiscoveredFile, mut facts: SessionFacts) -> Result<SessionFacts> {
    each_record(file, |record, delegated| {
        absorb_record(&mut facts, record, delegated)
    })?;
    Ok(facts)
}

fn absorb_record(facts: &mut SessionFacts, record: &Value, delegated: bool) {
    let payload = &record["payload"];
    let kind = record["type"].as_str();
    let timestamp = record["timestamp"].as_str();
    if matches!(kind, Some("turn_context" | "event_msg" | "response_item")) {
        if let Some(ts) = timestamp {
            update_timestamps(facts, ts);
        }
    }
    match kind {
        Some("session_meta") if !delegated => absorb_meta(facts, payload),
        Some("turn_context") if !delegated => {
            if let Some(cwd) = payload["cwd"].as_str() {
                facts.cwd = Some(cwd.to_owned());
            }
        }
        Some("event_msg") if payload["type"].as_str() == Some("item_completed") => {
            absorb_item(facts, &payload["item"], timestamp, delegated)
        }
        _ => {}
    }
}

fn absorb_meta(facts: &mut SessionFacts, meta: &Value) {
    // A fork repeats the parent's metadata under the parent's id.
    if meta["id"].as_str().is_some_and(|id| id != facts.session_id) {
        return;
    }
    if let Some(cwd) = meta["cwd"].as_str() {
        facts.cwd = Some(cwd.to_owned());
    }
    if let Some(version) = meta["cli_version"].as_str() {
        facts.cli_version = Some(version.to_owned());
    }
    if let Some(branch) = named_branch(&meta["git"]["branch"]) {
        facts.git_branch = Some(branch);
    }
}

fn absorb_item(facts: &mut SessionFacts, item: &Value, timestamp: Option<&str>, delegated: bool) {
    match item["type"].as_str() {
        Some("UserMessage") if !delegated => absorb_prompt(facts, item),
        Some("AgentMessage") if !delegated => absorb_response(facts, item),
        Some("CommandExecution") => absorb_command(facts, item, timestamp, delegated),
        Some("FileChange") => absorb_file_change(facts, item, delegated),
        Some("McpToolCall") => absorb_mcp_call(facts, item, timestamp, delegated),
        _ => {}
    }
}

fn message_text(item: &Value) -> String {
    item["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn absorb_prompt(facts: &mut SessionFacts, item: &Value) {
    let prompt = message_text(item);
    let prompt = prompt.trim();
    if !prompt.is_empty() && !prompt.starts_with('<') {
        push_prompt(facts, prompt);
    }
}

fn absorb_response(facts: &mut SessionFacts, item: &Value) {
    if item["phase"]
        .as_str()
        .is_none_or(|phase| phase == "final_answer")
    {
        set_final_response(facts, &message_text(item));
    }
}

fn absorb_command(
    facts: &mut SessionFacts,
    item: &Value,
    timestamp: Option<&str>,
    delegated: bool,
) {
    let Some(command) = item["command"]
        .as_array()
        .and_then(|parts| parts.last())
        .and_then(Value::as_str)
    else {
        return;
    };
    let ok = match item["exit_code"].as_i64() {
        Some(code) => code == 0,
        None => item["status"].as_str() != Some("failed"),
    };
    let fact = CommandFact {
        ok,
        ..command_fact(
            &facts.session_id,
            facts.commands.len(),
            command,
            timestamp,
            delegated,
        )
    };
    facts.commands.push(fact);
}

fn absorb_file_change(facts: &mut SessionFacts, item: &Value, delegated: bool) {
    if item["status"].as_str() == Some("failed") {
        return;
    }
    let Some(changes) = item["changes"].as_object() else {
        return;
    };
    for path in changes.keys() {
        let path = path.strip_prefix("file://").unwrap_or(path);
        record_file_change(facts, path, "apply_patch", delegated);
    }
}

fn absorb_mcp_call(
    facts: &mut SessionFacts,
    item: &Value,
    timestamp: Option<&str>,
    delegated: bool,
) {
    let (Some(server), Some(tool)) = (item["server"].as_str(), item["tool"].as_str()) else {
        return;
    };
    let ok = item["status"].as_str() != Some("failed")
        && item["result"]["isError"].as_bool() != Some(true);
    let mut action = external_action(
        &facts.session_id,
        facts.external_actions.len(),
        server,
        tool,
        timestamp,
        delegated,
    );
    action.ok = ok;
    if let Some(read_only) = item["readOnlyHint"].as_bool() {
        action.mutating = !read_only;
    }
    facts.external_actions.push(action);
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = "01a02e39-3fb0-7010-bd83-4beea0e74c78";

    fn meta(id: &str, cwd: &str, branch: &str, extra: &str) -> String {
        format!(
            r#"{{"timestamp":"2026-08-23T10:45:23.629Z","type":"session_meta","payload":{{"id":"{id}","timestamp":"2026-08-23T10:45:23.000Z","cwd":"{cwd}","originator":"codex-tui","cli_version":"0.149.0","source":"cli","thread_source":"user","git":{{"commit_hash":"abc","branch":"{branch}","repository_url":"git@github.com:acme/z-report.git"}}{extra}}}}}"#
        )
    }

    fn item(ts: &str, body: &str) -> String {
        format!(
            r#"{{"timestamp":"{ts}","type":"event_msg","payload":{{"type":"item_completed","thread_id":"{SESSION}","turn_id":"t1","item":{body}}}}}"#
        )
    }

    fn write(dir: &str, name: &str, lines: &[String]) -> PathBuf {
        let dir = std::env::temp_dir().join("zreport-test/codex").join(dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, lines.join("\n")).unwrap();
        path
    }

    fn parse_file(path: &Path, title: Option<&str>, retain_prompts: bool) -> SessionFacts {
        let file = describe(
            Agent::Codex,
            path.to_path_buf(),
            SESSION.to_owned(),
            title.map(String::from),
            vec![],
        )
        .unwrap();
        crate::ingest::parse_transcript(&file, retain_prompts).unwrap()
    }

    fn thread_lines() -> Vec<String> {
        vec![
            meta(SESSION, "/tmp/repo", "homebrew-distribution", ""),
            r#"{"timestamp":"2026-08-23T10:45:24.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>\n  <cwd>/tmp/repo</cwd>\n</environment_context>"}]}}"#.into(),
            r#"{"timestamp":"2026-08-23T10:45:25.000Z","type":"turn_context","payload":{"turn_id":"t1","cwd":"/tmp/repo","model":"gpt-5.6-sol"}}"#.into(),
            item("2026-08-23T10:45:26.000Z", r#"{"type":"UserMessage","id":"u1","content":[{"type":"text","text":"Check this branch against the Homebrew issue","text_elements":[]}]}"#),
            item("2026-08-23T10:45:27.000Z", r#"{"type":"Reasoning","id":"r1","summary_text":[],"raw_content":[]}"#),
            item("2026-08-23T10:46:00.000Z", r#"{"type":"CommandExecution","id":"exec-1","command":["/bin/zsh","-lc","cargo test --locked"],"cwd":"file:///tmp/repo","parsed_cmd":[{"type":"unknown","cmd":"cargo test --locked"}],"status":"completed","exit_code":0,"stdout":"","stderr":"","aggregated_output":""}"#),
            item("2026-08-23T10:46:30.000Z", r#"{"type":"CommandExecution","id":"exec-2","command":["/bin/zsh","-lc","npm test"],"cwd":"file:///tmp/repo","status":"failed","exit_code":1}"#),
            item("2026-08-23T10:47:00.000Z", r#"{"type":"FileChange","id":"exec-3","status":"completed","changes":{"/tmp/repo/README.md":{"type":"update","unified_diff":"@@","move_path":null},"/tmp/repo/packaging/README.md":{"type":"add","content":"x"}}}"#),
            item("2026-08-23T10:47:30.000Z", r#"{"type":"FileChange","id":"exec-4","status":"completed","changes":{"/tmp/repo/README.md":{"type":"update","unified_diff":"@@"}}}"#),
            item("2026-08-23T10:48:00.000Z", r#"{"type":"McpToolCall","id":"exec-5","server":"pencil","tool":"get_screenshot","arguments":{},"readOnlyHint":true,"status":"completed","result":{"content":[],"isError":false}}"#),
            item("2026-08-23T10:48:10.000Z", r#"{"type":"McpToolCall","id":"exec-6","server":"notion","tool":"notion-update-page","arguments":{},"readOnlyHint":null,"status":"completed","result":{"content":[]}}"#),
            item("2026-08-23T10:48:20.000Z", r#"{"type":"McpToolCall","id":"exec-7","server":"node_repl","tool":"js","arguments":{},"readOnlyHint":false,"status":"failed","result":{"content":[],"isError":true}}"#),
            item("2026-08-23T10:49:00.000Z", r#"{"type":"AgentMessage","id":"m1","content":[{"type":"Text","text":"Working through the release workflow."}],"phase":"commentary"}"#),
            item("2026-08-23T10:50:00.000Z", r#"{"type":"AgentMessage","id":"m2","content":[{"type":"Text","text":"Updated the release workflow and the Cask."}],"phase":"final_answer"}"#),
            r#"{"timestamp":"2026-08-23T10:51:00.000Z","type":"compacted","payload":{"message":"","replacement_history":[{"type":"message","role":"user","content":[{"type":"input_text","text":"Check this branch against the Homebrew issue"}]}]}}"#.into(),
        ]
    }

    #[test]
    fn parses_a_codex_thread() {
        let path = write("thread", "rollout.jsonl", &thread_lines());

        let facts = parse_file(&path, None, true);

        assert_eq!(facts.agent, Agent::Codex);
        assert_eq!(
            facts.prompts,
            vec!["Check this branch against the Homebrew issue"]
        );
        assert_eq!(
            facts.title.as_deref(),
            Some("Check this branch against the Homebrew issue")
        );
        assert_eq!(facts.cwd.as_deref(), Some("/tmp/repo"));
        assert_eq!(facts.git_branch.as_deref(), Some("homebrew-distribution"));
        assert_eq!(facts.cli_version.as_deref(), Some("0.149.0"));
        assert_eq!(facts.first_ts.as_deref(), Some("2026-08-23T10:45:24.000Z"));
        assert_eq!(facts.last_ts.as_deref(), Some("2026-08-23T10:50:00.000Z"));

        let commands: Vec<(&str, &str, bool)> = facts
            .commands
            .iter()
            .map(|c| (c.id.as_str(), c.kind.as_str(), c.ok))
            .collect();
        assert_eq!(
            commands,
            vec![
                (&*format!("cmd:{SESSION}:0"), "test", true),
                (&*format!("cmd:{SESSION}:1"), "test", false),
            ]
        );
        assert_eq!(facts.commands[0].command, "cargo test --locked");

        let files: Vec<(&str, u32)> = facts
            .files_changed
            .iter()
            .map(|f| (f.path.as_str(), f.count))
            .collect();
        assert_eq!(
            files,
            vec![
                ("/tmp/repo/README.md", 2),
                ("/tmp/repo/packaging/README.md", 1)
            ]
        );
        assert_eq!(facts.files_changed[0].tool, "apply_patch");

        assert_eq!(facts.external_actions.len(), 3);
        let changes: Vec<(&str, bool)> = facts
            .external_changes()
            .map(|a| (a.tool.as_str(), a.ok))
            .collect();
        assert_eq!(changes, vec![("notion-update-page", true), ("js", false)]);
        assert_eq!(
            facts.final_response.as_deref(),
            Some("Updated the release workflow and the Cask.")
        );
        assert!(facts.has_substance());
    }

    #[test]
    fn title_prefers_the_generated_thread_name() {
        let path = write("title", "rollout.jsonl", &thread_lines());

        let named = parse_file(&path, Some("Ship the Homebrew cask"), true);
        assert_eq!(named.title.as_deref(), Some("Ship the Homebrew cask"));

        let private = parse_file(&path, None, false);
        assert_eq!(
            private.title, None,
            "a withheld prompt never leaks as a title"
        );
        assert!(private.prompts.is_empty());
        assert_eq!(private.final_response, None);
        assert_eq!(private.commands.len(), 2, "metadata is kept");
    }

    #[test]
    fn fork_copies_of_parent_metadata_are_ignored() {
        let path = write(
            "fork",
            "rollout.jsonl",
            &[
                meta(
                    SESSION,
                    "/tmp/fork",
                    "feature",
                    r#","forked_from_id":"019e516a-af62-7ee2-ae68-b2eb3b7d17aa""#,
                ),
                meta(
                    "019e516a-af62-7ee2-ae68-b2eb3b7d17aa",
                    "/tmp/parent",
                    "main",
                    "",
                )
                .replace("2026-08-23T10:45:23.629Z", "2026-08-01T08:00:00.000Z"),
                item(
                    "2026-08-23T11:00:00.000Z",
                    r#"{"type":"UserMessage","id":"u1","content":[{"type":"text","text":"Continue from here"}]}"#,
                ),
            ],
        );

        let facts = parse_file(&path, None, true);

        assert_eq!(facts.cwd.as_deref(), Some("/tmp/fork"));
        assert_eq!(facts.git_branch.as_deref(), Some("feature"));
        assert_eq!(facts.first_ts.as_deref(), Some("2026-08-23T11:00:00.000Z"));
    }

    #[test]
    fn discovery_skips_reviewer_threads_and_folds_spawned_agents() {
        let root = std::env::temp_dir().join("zreport-test/codex/discover");
        let _ = std::fs::remove_dir_all(&root);
        let day = "sessions/2026/08/23";
        let child = "01a02e39-aaaa-7010-bd83-000000000001";
        let grandchild = "01a02e39-aaaa-7010-bd83-000000000002";
        let guardian = "01a02e39-aaaa-7010-bd83-000000000003";
        let archived = "01a02e39-aaaa-7010-bd83-000000000004";
        let spawned = |id: &str, parent: &str| {
            format!(
                r#"{{"timestamp":"2026-08-23T10:50:00.000Z","type":"session_meta","payload":{{"id":"{id}","cwd":"/tmp/worktree","cli_version":"0.149.0","originator":"codex-tui","source":{{"subagent":{{"thread_spawn":{{"parent_thread_id":"{parent}","depth":1}}}}}},"thread_source":"subagent","parent_thread_id":"{parent}"}}}}"#
            )
        };
        let sub = |name: &str| format!("discover/{day}/{name}");
        let parent_path = write(
            &sub(""),
            &format!("rollout-2026-08-23T12-45-23-{SESSION}.jsonl"),
            &thread_lines(),
        );
        write(
            &sub(""),
            &format!("rollout-2026-08-23T12-50-00-{child}.jsonl"),
            &[
                spawned(child, SESSION),
                item(
                    "2026-08-23T10:51:00.000Z",
                    r#"{"type":"UserMessage","id":"u9","content":[{"type":"text","text":"You are reviewing the diff for reuse"}]}"#,
                ),
                item(
                    "2026-08-23T10:52:00.000Z",
                    r#"{"type":"CommandExecution","id":"exec-9","command":["/bin/zsh","-lc","cargo clippy"],"status":"completed","exit_code":0}"#,
                ),
                item(
                    "2026-08-23T10:53:00.000Z",
                    r#"{"type":"FileChange","id":"exec-10","status":"completed","changes":{"/tmp/repo/src/lib.rs":{"type":"update"}}}"#,
                ),
            ],
        );
        write(
            &sub(""),
            &format!("rollout-2026-08-23T12-55-00-{grandchild}.jsonl"),
            &[spawned(grandchild, child)],
        );
        write(
            &sub(""),
            &format!("rollout-2026-08-23T12-56-00-{guardian}.jsonl"),
            &[format!(
                r#"{{"timestamp":"2026-08-23T10:56:00.000Z","type":"session_meta","payload":{{"id":"{guardian}","cwd":"/tmp/repo","cli_version":"0.149.0","originator":"codex-tui","source":{{"subagent":{{"other":"guardian"}}}},"parent_thread_id":"{SESSION}"}}}}"#
            )],
        );
        write(
            "discover/archived_sessions",
            &format!("rollout-2026-05-10T16-22-00-{archived}.jsonl"),
            &[meta(archived, "/tmp/other", "main", "")],
        );
        let names = HashMap::from([(SESSION.to_owned(), "Ship the Homebrew cask".to_owned())]);

        let mut found = discover_in(
            &[root.join("sessions"), root.join("archived_sessions")],
            &names,
        );
        found.sort_by(|a, b| a.session_id.cmp(&b.session_id));

        let ids: Vec<&str> = found.iter().map(|f| f.session_id.as_str()).collect();
        assert_eq!(ids, vec![SESSION, archived]);
        let parent = &found[0];
        assert_eq!(parent.path, parent_path);
        assert_eq!(parent.title.as_deref(), Some("Ship the Homebrew cask"));
        assert_eq!(
            parent.delegates.len(),
            2,
            "child and grandchild fold into the parent"
        );
        assert!(found[1].delegates.is_empty());

        let facts = crate::ingest::parse_transcript(parent, true).unwrap();
        assert_eq!(facts.prompts.len(), 1, "only the parent's prompts count");
        assert_eq!(facts.cwd.as_deref(), Some("/tmp/repo"));
        let delegated: Vec<(&str, bool)> = facts
            .commands
            .iter()
            .map(|c| (c.command.as_str(), c.via_delegate))
            .collect();
        assert_eq!(
            delegated,
            vec![
                ("cargo test --locked", false),
                ("npm test", false),
                ("cargo clippy", true),
            ]
        );
        assert!(facts
            .files_changed
            .iter()
            .any(|f| f.path == "/tmp/repo/src/lib.rs" && f.via_delegate));
    }

    #[test]
    fn rollouts_from_earlier_releases_are_not_read() {
        assert!(supported_version("0.147.0"));
        assert!(supported_version("0.153.0-alpha.5"));
        assert!(supported_version("1.0.0"));
        assert!(!supported_version("0.146.1"));
        assert!(!supported_version("0.99.0-alpha.5"));
        assert!(!supported_version("garbage"));

        let old = write(
            "old",
            &format!("rollout-2026-05-22T23-27-13-{SESSION}.jsonl"),
            &[meta(SESSION, "/tmp/repo", "main", "").replace("0.149.0", "0.146.0")],
        );
        assert!(matches!(classify(&old), Thread::Skip));
        let current = write(
            "current",
            &format!("rollout-2026-08-23T12-45-23-{SESSION}.jsonl"),
            &thread_lines(),
        );
        assert!(matches!(classify(&current), Thread::Session { id } if id == SESSION));
    }

    #[test]
    fn reads_thread_names_from_the_desktop_index() {
        let path = write(
            "index",
            "session_index.jsonl",
            &[
                r#"{"id":"01a04a84-13fc-70d3-93fc-879cd7f49825","thread_name":"Research Microduck","updated_at":"2026-08-28T22:36:08Z"}"#.into(),
                r#"{"id":"01a0351e-26e6-7132-b6ba-17259d898f81","thread_name":"   ","updated_at":"2026-08-24T18:52:48Z"}"#.into(),
                "not json".into(),
            ],
        );
        let names = thread_names(&path);
        assert_eq!(
            names
                .get("01a04a84-13fc-70d3-93fc-879cd7f49825")
                .map(String::as_str),
            Some("Research Microduck")
        );
        assert_eq!(names.len(), 1);
        assert!(thread_names(Path::new("/nonexistent/index.jsonl")).is_empty());
    }
}
