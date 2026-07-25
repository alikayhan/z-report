use crate::models::*;
use anyhow::Result;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

const MAX_PROMPTS: usize = 25;
const PROMPT_CHARS: usize = 400;
const RESPONSE_CHARS: usize = 1500;

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

/// Transcripts of work the session delegated, written beside the parent.
fn sidechain_files(transcript: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(transcript.with_extension("").join("subagents")) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
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
            if path.extension().map_or(true, |e| e != "jsonl") {
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

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    }
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

/// Widens the session span. Sidechain files are read after the parent, so
/// records do not arrive in chronological order.
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
        "cargo test", "npm test", "npm run test", "pytest", "jest", "vitest",
        "go test", "xcodebuild test", "swift test", "mvn test", "gradle test",
        "rspec", "phpunit", "bundle exec rspec",
    ];
    let build_markers = [
        "cargo build", "npm run build", "tsc", "xcodebuild build", "swift build",
        "make", "go build", "gradle build", "mvn package", "vite build", "webpack",
        "cargo check",
    ];
    let check_markers = [
        "clippy", "eslint", "lint", "fmt --check", "prettier --check", "typecheck",
        "mypy", "ruff",
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

#[derive(Default)]
struct Parse {
    facts: SessionFacts,
    files: HashMap<String, FileChange>,
    pending_cmds: HashMap<String, CommandFact>,
    cmd_order: Vec<String>,
    pr_urls: HashSet<String>,
}

/// Parse a full session transcript into normalized facts.
/// The JSONL schema is internal to Claude Code, so parsing is defensive:
/// unknown record types are skipped, missing fields degrade to None.
pub fn parse_transcript(path: &PathBuf, session_id: &str, retain_prompts: bool) -> Result<SessionFacts> {
    let mut p = Parse {
        facts: SessionFacts {
            session_id: session_id.to_string(),
            file_path: path.to_string_lossy().to_string(),
            ..Default::default()
        },
        ..Default::default()
    };
    absorb(&mut p, &std::fs::read_to_string(path)?, session_id);
    for side in sidechain_files(path) {
        if let Ok(content) = std::fs::read_to_string(&side) {
            absorb(&mut p, &content, session_id);
        }
    }

    let Parse {
        mut facts,
        files,
        mut pending_cmds,
        cmd_order,
        ..
    } = p;
    facts.commands = cmd_order
        .iter()
        .filter_map(|id| pending_cmds.remove(id))
        .collect();
    facts.files_changed = files.into_values().collect();
    facts.files_changed.sort_by(|a, b| a.path.cmp(&b.path));
    if !retain_prompts {
        facts.prompts.clear();
        facts.final_response = None;
    }
    Ok(facts)
}

fn absorb(p: &mut Parse, content: &str, session_id: &str) {
    for line in content.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let rec_type = v["type"].as_str().unwrap_or("");
        if rec_type == "pr-link" {
            let Some(number) = v["prNumber"].as_u64() else {
                continue;
            };
            let (Some(url), Some(repository)) = (v["prUrl"].as_str(), v["prRepository"].as_str())
            else {
                continue;
            };
            let pr = PrLink {
                number,
                url: url.to_string(),
                repository: repository.to_string(),
                ts: v["timestamp"].as_str().map(String::from),
            };
            if !pr.has_canonical_url() {
                continue;
            }
            // A PR opened after midnight files the session under the day the work
            // finished, and widens the Git window to catch the commit just before it.
            if let Some(ts) = pr.ts.as_deref() {
                update_timestamps(&mut p.facts, ts);
            }
            if p.pr_urls.insert(pr.url.clone()) {
                p.facts.pr_links.push(pr);
            }
            continue;
        }
        if rec_type != "user" && rec_type != "assistant" {
            continue;
        }
        let delegated = v["isSidechain"].as_bool() == Some(true);
        if let Some(ts) = v["timestamp"].as_str() {
            update_timestamps(&mut p.facts, ts);
        }
        // A delegate can run in its own worktree, so its cwd and branch are not
        // the session's; only the parent defines where the work happened.
        if !delegated {
            if let Some(cwd) = v["cwd"].as_str() {
                p.facts.cwd = Some(cwd.to_string());
            }
            if let Some(branch) = v["gitBranch"].as_str() {
                if !branch.is_empty() && branch != "HEAD" {
                    p.facts.git_branch = Some(branch.to_string());
                }
            }
            if let Some(ver) = v["version"].as_str() {
                p.facts.cli_version = Some(ver.to_string());
            }
        }

        if rec_type == "user" {
            let msg = &v["message"];
            if let Some(text) = msg["content"].as_str() {
                // A sidechain user record carries the delegating instruction, not
                // anything the developer typed.
                let is_human =
                    !delegated && v["origin"]["kind"].as_str().map_or(true, |k| k == "human");
                let is_meta = text.starts_with('<') || text.starts_with("[Request interrupted");
                if is_human && !is_meta && p.facts.prompts.len() < MAX_PROMPTS {
                    p.facts.prompts.push(truncate(text.trim(), PROMPT_CHARS));
                }
            } else if let Some(blocks) = msg["content"].as_array() {
                for b in blocks {
                    if b["type"].as_str() == Some("tool_result") {
                        if let Some(id) = b["tool_use_id"].as_str() {
                            if let Some(cmd) = p.pending_cmds.get_mut(id) {
                                cmd.ok = b["is_error"].as_bool() != Some(true);
                            }
                        }
                    }
                }
            }
        } else {
            let Some(blocks) = v["message"]["content"].as_array() else {
                continue;
            };
            for b in blocks {
                match b["type"].as_str() {
                    Some("text") => {
                        if let Some(t) = b["text"].as_str() {
                            if !delegated && !t.trim().is_empty() {
                                p.facts.final_response = Some(truncate(t.trim(), RESPONSE_CHARS));
                            }
                        }
                    }
                    Some("tool_use") => {
                        let name = b["name"].as_str().unwrap_or("");
                        let input = &b["input"];
                        match name {
                            "Bash" => {
                                if let (Some(id), Some(cmd)) =
                                    (b["id"].as_str(), input["command"].as_str())
                                {
                                    let fact = CommandFact {
                                        id: format!("cmd:{}:{}", session_id, p.cmd_order.len()),
                                        command: truncate(cmd, 300),
                                        ok: true,
                                        kind: classify_command(cmd).to_string(),
                                        ts: v["timestamp"].as_str().map(String::from),
                                        via_delegate: delegated,
                                    };
                                    p.cmd_order.push(id.to_string());
                                    p.pending_cmds.insert(id.to_string(), fact);
                                }
                            }
                            "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => {
                                if let Some(fp) = input["file_path"].as_str() {
                                    let entry =
                                        p.files.entry(fp.to_string()).or_insert(FileChange {
                                            path: fp.to_string(),
                                            tool: name.to_string(),
                                            count: 0,
                                            via_delegate: delegated,
                                        });
                                    entry.count += 1;
                                    entry.via_delegate &= delegated;
                                }
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Local calendar day a session belongs to (day of last activity).
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
            r#"{"type":"user","message":{"role":"user","content":"Fix the login bug"},"timestamp":"2026-07-20T10:00:00.000Z","cwd":"/tmp/repo","gitBranch":"main","version":"2.1.215","origin":{"kind":"human"}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"cargo test"}}]},"timestamp":"2026-07-20T10:01:00.000Z"}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","is_error":false}]},"timestamp":"2026-07-20T10:02:00.000Z"}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t2","name":"Edit","input":{"file_path":"/tmp/repo/src/auth.rs"}}]},"timestamp":"2026-07-20T10:03:00.000Z"}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Fixed the login bug and tests pass."}]},"timestamp":"2026-07-20T10:04:00.000Z"}"#,
        ];
        std::fs::write(&p, lines.join("\n")).unwrap();
        let facts = parse_transcript(&p, "s1", true).unwrap();
        assert_eq!(facts.prompts, vec!["Fix the login bug"]);
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
            dt.with_timezone(&chrono::Local).format("%Y-%m-%d").to_string()
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
        assert_eq!(
            facts.last_ts.as_deref(),
            Some("2026-07-21T00:06:00.000Z")
        );
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

