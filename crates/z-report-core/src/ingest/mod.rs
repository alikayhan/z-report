mod claude;
mod codex;

use crate::models::*;
use crate::text;
use anyhow::{Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const MAX_PROMPTS: usize = 25;
const PROMPT_CHARS: usize = 400;
const RESPONSE_CHARS: usize = 1500;
const TITLE_CHARS: usize = 120;
const COMMAND_CHARS: usize = 300;

pub struct DiscoveredFile {
    pub agent: Agent,
    pub path: PathBuf,
    pub session_id: String,
    pub title: Option<String>,
    pub delegates: Vec<PathBuf>,
    pub content_hash: String,
    pub mtime: u64,
}

pub fn discover() -> Vec<DiscoveredFile> {
    discover_checked().0
}

pub fn discover_checked() -> (Vec<DiscoveredFile>, Vec<String>) {
    let mut discovery = Discovery::default();
    let mut files = claude::discover(&mut discovery);
    files.extend(codex::discover(&mut discovery));
    (files, discovery.diagnostics)
}

#[derive(Default)]
struct Discovery {
    diagnostics: Vec<String>,
}

pub fn parse_transcript(file: &DiscoveredFile, retain_prompts: bool) -> Result<SessionFacts> {
    let facts = SessionFacts {
        session_id: file.session_id.clone(),
        agent: file.agent,
        file_path: file.path.to_string_lossy().to_string(),
        title: file.title.clone(),
        ..Default::default()
    };
    let mut facts = match file.agent {
        Agent::Claude => claude::parse(file, facts)?,
        Agent::Codex => codex::parse(file, facts)?,
    };
    facts.files_changed.sort_by(|a, b| a.path.cmp(&b.path));
    if !retain_prompts {
        facts.prompts.clear();
        facts.final_response = None;
        facts.title = None;
    } else if facts.title.is_none() {
        facts.title = facts.prompts.first().and_then(|prompt| clean_title(prompt));
    }
    Ok(facts)
}

fn home() -> PathBuf {
    if let Some(dir) = std::env::var_os("Z_REPORT_TRANSCRIPT_HOME") {
        return PathBuf::from(dir);
    }
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
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

fn records(content: &str) -> impl Iterator<Item = Value> + '_ {
    content
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
}

fn read_hashed(path: &Path, hash: &mut Sha256) -> Result<Vec<u8>> {
    let bytes = std::fs::read(path).with_context(|| format!("Cannot read {}", path.display()))?;
    hash.update((bytes.len() as u64).to_le_bytes());
    hash.update(&bytes);
    Ok(bytes)
}

fn file_hash(path: &Path, delegates: &[PathBuf]) -> Result<String> {
    let mut hash = Sha256::new();
    for path in std::iter::once(path).chain(delegates.iter().map(PathBuf::as_path)) {
        read_hashed(path, &mut hash)?;
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn each_record(file: &DiscoveredFile, mut visit: impl FnMut(&Value, bool)) -> Result<()> {
    let mut hash = Sha256::new();
    for (index, path) in std::iter::once(&file.path)
        .chain(file.delegates.iter())
        .enumerate()
    {
        let content = String::from_utf8(read_hashed(path, &mut hash)?)?;
        for (line_no, line) in content.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let record: Value = serde_json::from_str(line)
                .with_context(|| format!("Malformed transcript at line {}", line_no + 1))?;
            visit(&record, index > 0);
        }
    }
    anyhow::ensure!(
        format!("{:x}", hash.finalize()) == file.content_hash,
        "Transcript changed during scan; retry the read"
    );
    Ok(())
}

impl Discovery {
    fn describe(
        &mut self,
        agent: Agent,
        path: PathBuf,
        session_id: String,
        title: Option<String>,
        delegates: Vec<PathBuf>,
    ) -> Option<DiscoveredFile> {
        let meta = match path.metadata() {
            Ok(m) => m,
            Err(e) => {
                self.diagnostic(&path, &e.to_string());
                return None;
            }
        };
        let mtime = mtime_secs(&meta);
        let content_hash = match file_hash(&path, &delegates) {
            Ok(h) => h,
            Err(e) => {
                self.diagnostic(&path, &e.to_string());
                return None;
            }
        };
        Some(DiscoveredFile {
            agent,
            path,
            session_id,
            title,
            delegates,
            content_hash,
            mtime,
        })
    }

    fn diagnostic(&mut self, path: &Path, message: &str) {
        self.diagnostics
            .push(format!("{}: {message}", path.display()));
    }

    fn entries(&mut self, path: &Path) -> Vec<std::fs::DirEntry> {
        match std::fs::read_dir(path) {
            Ok(entries) => entries
                .filter_map(|entry| match entry {
                    Ok(entry) => Some(entry),
                    Err(error) => {
                        self.diagnostic(path, &error.to_string());
                        None
                    }
                })
                .collect(),
            Err(error) => {
                if error.kind() != std::io::ErrorKind::NotFound {
                    self.diagnostic(path, &error.to_string());
                }
                Vec::new()
            }
        }
    }
}

fn clean_title(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text::truncate(text, TITLE_CHARS))
}

fn named_branch(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|branch| !branch.is_empty() && *branch != "HEAD")
        .map(str::to_owned)
}

fn push_prompt(facts: &mut SessionFacts, prompt: &str) {
    if facts.prompts.len() < MAX_PROMPTS {
        facts
            .prompts
            .push(text::truncate(prompt.trim(), PROMPT_CHARS));
    }
}

fn set_final_response(facts: &mut SessionFacts, response: &str) {
    let response = response.trim();
    if !response.is_empty() {
        facts.final_response = Some(text::truncate(response, RESPONSE_CHARS));
    }
}

fn record_file_change(facts: &mut SessionFacts, path: &str, tool: &str, delegated: bool) {
    match facts.files_changed.iter_mut().find(|f| f.path == path) {
        Some(change) => {
            change.count += 1;
            // Any direct edit makes the combined file change direct.
            change.via_delegate &= delegated;
        }
        None => facts.files_changed.push(FileChange {
            path: path.to_owned(),
            tool: tool.to_owned(),
            count: 1,
            via_delegate: delegated,
        }),
    }
}

fn command_fact(
    session_id: &str,
    index: usize,
    command: &str,
    ts: Option<&str>,
    delegated: bool,
) -> CommandFact {
    CommandFact {
        id: format!("cmd:{session_id}:{index}"),
        command: text::truncate(command, COMMAND_CHARS),
        ok: true,
        kind: classify_command(command).to_owned(),
        ts: ts.map(String::from),
        via_delegate: delegated,
    }
}

fn external_action(
    session_id: &str,
    index: usize,
    server: &str,
    tool: &str,
    ts: Option<&str>,
    delegated: bool,
) -> ExternalAction {
    ExternalAction {
        id: format!("action:{session_id}:{index}"),
        server: server.to_owned(),
        tool: tool.to_owned(),
        ok: true,
        mutating: mutates_external_state(tool),
        ts: ts.map(String::from),
        via_delegate: delegated,
    }
}

fn earlier(a: &str, b: &str) -> bool {
    // Same-width UTC stamps order lexically; parsing only pays for mixed formats.
    if a.len() == b.len() && a.ends_with('Z') && b.ends_with('Z') {
        return a < b;
    }
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
    fn delegate_files_extend_the_content_hash() {
        let dir = std::env::temp_dir().join("zreport-test/describe");
        std::fs::create_dir_all(&dir).unwrap();
        let parent = dir.join("p.jsonl");
        let child = dir.join("c.jsonl");
        std::fs::write(&parent, "{}").unwrap();
        std::fs::write(&child, "{}\n{}").unwrap();

        let alone = Discovery::default()
            .describe(Agent::Codex, parent.clone(), "p".into(), None, vec![])
            .unwrap();
        let with_child = Discovery::default()
            .describe(Agent::Codex, parent, "p".into(), None, vec![child])
            .unwrap();

        assert_eq!(alone.content_hash.len(), 64);
        assert_ne!(alone.content_hash, with_child.content_hash);
    }
}
