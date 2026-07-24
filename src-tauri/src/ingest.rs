use crate::models::*;
use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

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
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            out.push(DiscoveredFile {
                session_id: stem.to_string(),
                content_hash: format!("{}:{}", meta.len(), mtime),
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

/// Parse a full session transcript into normalized facts.
/// The JSONL schema is internal to Claude Code, so parsing is defensive:
/// unknown record types are skipped, missing fields degrade to None.
pub fn parse_transcript(path: &PathBuf, session_id: &str, retain_prompts: bool) -> Result<SessionFacts> {
    let content = std::fs::read_to_string(path)?;
    let mut facts = SessionFacts {
        session_id: session_id.to_string(),
        file_path: path.to_string_lossy().to_string(),
        ..Default::default()
    };
    let mut files: HashMap<String, (String, u32)> = HashMap::new();
    let mut pending_cmds: HashMap<String, CommandFact> = HashMap::new();
    let mut cmd_order: Vec<String> = Vec::new();

    for line in content.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let rec_type = v["type"].as_str().unwrap_or("");
        if rec_type != "user" && rec_type != "assistant" {
            continue;
        }
        if v["isSidechain"].as_bool() == Some(true) {
            continue;
        }
        if let Some(ts) = v["timestamp"].as_str() {
            if facts.first_ts.is_none() {
                facts.first_ts = Some(ts.to_string());
            }
            facts.last_ts = Some(ts.to_string());
        }
        if let Some(cwd) = v["cwd"].as_str() {
            facts.cwd = Some(cwd.to_string());
        }
        if let Some(branch) = v["gitBranch"].as_str() {
            if !branch.is_empty() && branch != "HEAD" {
                facts.git_branch = Some(branch.to_string());
            }
        }
        if let Some(ver) = v["version"].as_str() {
            facts.cli_version = Some(ver.to_string());
        }

        if rec_type == "user" {
            let msg = &v["message"];
            if let Some(text) = msg["content"].as_str() {
                let is_human = v["origin"]["kind"].as_str().map_or(true, |k| k == "human");
                let is_meta = text.starts_with('<') || text.starts_with("[Request interrupted");
                if is_human && !is_meta && facts.prompts.len() < MAX_PROMPTS {
                    facts.prompts.push(truncate(text.trim(), PROMPT_CHARS));
                }
            } else if let Some(blocks) = msg["content"].as_array() {
                for b in blocks {
                    if b["type"].as_str() == Some("tool_result") {
                        if let Some(id) = b["tool_use_id"].as_str() {
                            if let Some(cmd) = pending_cmds.get_mut(id) {
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
                            if !t.trim().is_empty() {
                                facts.final_response = Some(truncate(t.trim(), RESPONSE_CHARS));
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
                                        id: format!("cmd:{}:{}", session_id, cmd_order.len()),
                                        command: truncate(cmd, 300),
                                        ok: true,
                                        kind: classify_command(cmd).to_string(),
                                        ts: v["timestamp"].as_str().map(String::from),
                                    };
                                    cmd_order.push(id.to_string());
                                    pending_cmds.insert(id.to_string(), fact);
                                }
                            }
                            "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => {
                                if let Some(fp) = input["file_path"].as_str() {
                                    let entry = files
                                        .entry(fp.to_string())
                                        .or_insert((name.to_string(), 0));
                                    entry.1 += 1;
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

    facts.commands = cmd_order
        .iter()
        .filter_map(|id| pending_cmds.remove(id))
        .collect();
    facts.files_changed = files
        .into_iter()
        .map(|(path, (tool, count))| FileChange { path, tool, count })
        .collect();
    facts.files_changed.sort_by(|a, b| a.path.cmp(&b.path));
    if !retain_prompts {
        facts.prompts.clear();
        facts.final_response = None;
    }
    Ok(facts)
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
}
