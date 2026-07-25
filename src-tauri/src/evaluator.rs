use crate::models::*;
use crate::store;
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const EVAL_MODEL: &str = "claude-opus-5";
pub const EVAL_EFFORT: &str = "xhigh";
const EVAL_TIMEOUT: Duration = Duration::from_secs(900);
/// Hard per-run safety stop for a runaway evaluation, not a money budget: on a
/// subscription `--max-budget-usd` caps estimated work, not dollars spent.
const MAX_BUDGET_USD: f64 = 5.0;

pub struct EvalResult {
    pub achievements: Vec<Achievement>,
    pub model: Option<String>,
    pub cost_usd: Option<f64>,
    pub num_turns: Option<i64>,
    pub duration_ms: Option<i64>,
}

pub fn find_claude(settings: &Settings) -> Result<PathBuf> {
    if let Some(p) = &settings.claude_path {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return Ok(pb);
        }
    }
    if let Ok(out) = Command::new("/usr/bin/which").arg("claude").output() {
        if out.status.success() {
            let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !p.is_empty() {
                return Ok(PathBuf::from(p));
            }
        }
    }
    let home = dirs::home_dir().unwrap_or_default();
    for cand in [
        PathBuf::from("/opt/homebrew/bin/claude"),
        PathBuf::from("/usr/local/bin/claude"),
        home.join(".local/bin/claude"),
        home.join(".claude/local/claude"),
    ] {
        if cand.exists() {
            return Ok(cand);
        }
    }
    bail!("Claude Code CLI not found. Install it or set its path in Settings.")
}

/// A `claude` invocation with ANTHROPIC_API_KEY stripped, so every run uses the
/// developer's subscription login rather than silently billing an API key.
fn claude_command(settings: &Settings) -> Result<Command> {
    let mut cmd = Command::new(find_claude(settings)?);
    cmd.env_remove("ANTHROPIC_API_KEY");
    Ok(cmd)
}

/// True only when runs are billed per-token (an API key); a subscription's cost
/// is an estimate, not money. Auth we cannot confirm as an API key is not metered.
pub fn is_metered(settings: &Settings) -> bool {
    let Ok(mut cmd) = claude_command(settings) else {
        return false;
    };
    let Ok(out) = cmd.args(["auth", "status", "--json"]).output() else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    auth_is_metered(&String::from_utf8_lossy(&out.stdout))
}

fn auth_is_metered(stdout: &str) -> bool {
    serde_json::from_str::<Value>(stdout.trim())
        .ok()
        .and_then(|v| v["authMethod"].as_str().map(|m| m == "api-key"))
        .unwrap_or(false)
}

fn output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "achievements": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "title": { "type": "string", "description": "Short, specific, outcome-first statement; max ~70 characters." },
                        "contribution": { "type": "string", "description": "At most 2-3 plain sentences a teammate who wasn't there could understand: what was done and why it mattered. No jargon or filler; let the outcome bullets carry the specifics." },
                        "outcomes": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "claim": { "type": "string", "description": "One concrete outcome as a single scannable bullet line, understandable on its own." },
                                    "evidence_level": { "type": "integer", "minimum": 1, "maximum": 4 },
                                    "evidence_refs": { "type": "array", "items": { "type": "string" } }
                                },
                                "required": ["claim", "evidence_level", "evidence_refs"]
                            }
                        },
                        "uncertainties": { "type": "array", "items": { "type": "string" } },
                        "confidence": { "type": "number", "minimum": 0, "maximum": 1 },
                        "session_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Bare id values of the sessions this achievement draws from (the sessions[].id field, not the session: ref)"
                        }
                    },
                    "required": ["title", "contribution", "outcomes", "confidence", "session_ids"]
                }
            }
        },
        "required": ["achievements"]
    })
}

pub fn build_evidence_package(day: &str, sessions: &[SessionFacts]) -> Value {
    json!({
        "date": day,
        "instructions": "Evidence collected locally from Claude Code sessions and Git. Reference facts by their ref ids.",
        "sessions": sessions.iter().map(|s| json!({
            "id": s.session_id,
            "ref": format!("session:{}", s.session_id),
            "cwd": s.cwd,
            "repo": s.repo_root,
            "branch": s.git_branch,
            "started": s.first_ts,
            "ended": s.last_ts,
            "user_prompts": s.prompts,
            "final_response_excerpt": s.final_response,
            "files_changed": s.files_changed.iter().map(|f| json!({
                "ref": format!("file:{}", f.path),
                "path": f.path,
                "tool": f.tool,
                "edits": f.count
            })).collect::<Vec<_>>(),
            "commands": s.commands.iter().map(|c| json!({
                "ref": c.id,
                "command": c.command,
                "succeeded": c.ok,
                "kind": c.kind
            })).collect::<Vec<_>>(),
            "commits": s.commits.iter().map(|c| json!({
                "ref": format!("commit:{}", c.sha),
                "sha": c.sha,
                "subject": c.subject,
                "committed_at": c.ts,
                "files": c.files,
                "insertions": c.insertions,
                "deletions": c.deletions
            })).collect::<Vec<_>>(),
            "pull_requests": s.pr_links.iter().map(|pr| json!({
                "ref": pr.evidence_ref(),
                "number": pr.number,
                "url": pr.url,
                "repository": pr.repository,
                "recorded_at": pr.ts
            })).collect::<Vec<_>>()
        })).collect::<Vec<_>>()
    })
}

const EVALUATOR_PROMPT: &str = r#"You are the evaluator for Z Report, a private local accomplishment journal. Read ./evidence.json — it contains today's Claude Code session evidence and Git facts for one developer.

Reconstruct the day's accomplishments as achievements a developer would be proud to put in a standup or performance review. Follow these rules strictly:

1. Celebrate outcomes, not activity. "Fixed flaky auth test that blocked CI" is an achievement; "ran 14 commands" is not.
2. Cluster related sessions into a single achievement when they share a repository, branch, files, or a clear narrative thread. Use each session at most once. In session_ids, list the bare session id values, not "session:" refs.
3. Every outcome claim must cite evidence_refs that literally exist in evidence.json ("session:…", "file:…", "cmd:…", "commit:…", "pr:…"). Never invent refs.
4. Assign each claim the highest evidence level the cited refs support:
   1 = work observed in a session, 2 = a concrete change was produced, 3 = a relevant test/build/check passed, 4 = the change exists in a local commit or has a recorded PR link alongside a file change.
   Never claim level 3 without a succeeded test/build/check ref; never claim level 4 without a commit ref or a PR ref from a session with a file change. A PR ref proves the change was proposed, not merged.
5. State uncertainties honestly (e.g. "tests were not run", "change not committed"). Do not speculate about production impact.
6. Keep each achievement brief and legible to someone who wasn't there — a teammate or manager skimming a standup. Title: a short, specific, outcome-first statement (max ~70 chars). Contribution: at most 2-3 plain sentences saying what the developer did and why it mattered — no jargon or filler. Keep each outcome claim to a single scannable bullet line; let the bullets, not the prose, carry the specifics.
7. Confidence is your honest probability that the developer would recognize this as a real, correctly described accomplishment.
8. Skip noise: exploratory sessions with no output can be omitted or grouped into one low-confidence "investigation" achievement if the investigation itself was substantial.

Return only the structured output."#;

pub fn evaluate_day(settings: &Settings, day: &str, sessions: &[SessionFacts]) -> Result<EvalResult> {
    let mut cmd = claude_command(settings)?;
    let run_dir = store::data_dir().join("eval").join(format!(
        "{}-{}",
        day,
        chrono::Local::now().format("%H%M%S")
    ));
    std::fs::create_dir_all(&run_dir)?;
    let package = build_evidence_package(day, sessions);
    std::fs::write(
        run_dir.join("evidence.json"),
        serde_json::to_string_pretty(&package)?,
    )?;

    let schema = serde_json::to_string(&output_schema())?;
    let budget = format!("{:.2}", MAX_BUDGET_USD);
    let mut args: Vec<&str> = vec![
        "-p",
        EVALUATOR_PROMPT,
        "--model",
        EVAL_MODEL,
        "--effort",
        EVAL_EFFORT,
        "--output-format",
        "json",
        "--json-schema",
        &schema,
        "--tools",
        "Read,Grep,Glob",
        "--disallowedTools",
        "Bash,Edit,Write,NotebookEdit,WebFetch,WebSearch,Task",
        "--no-session-persistence",
        "--setting-sources",
        "",
    ];
    if settings.cost_limit_enabled {
        args.push("--max-budget-usd");
        args.push(&budget);
    }
    let mut child = cmd
        .current_dir(&run_dir)
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| anyhow!("failed to launch Claude Code: {e}"))?;

    let started = Instant::now();
    let status = loop {
        match child.try_wait()? {
            Some(status) => break status,
            None => {
                if started.elapsed() > EVAL_TIMEOUT {
                    let _ = child.kill();
                    let _ = std::fs::remove_dir_all(&run_dir);
                    bail!("evaluation timed out after {}s", EVAL_TIMEOUT.as_secs());
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    };
    let out = child.wait_with_output()?;
    let _ = std::fs::remove_dir_all(&run_dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    if !status.success() {
        bail!("{}", classify_failure(&stdout, &stderr));
    }
    let v: Value = serde_json::from_str(stdout.trim())
        .map_err(|_| anyhow!("unexpected evaluator output (not JSON): {}", excerpt(&stdout)))?;
    if v["is_error"].as_bool() == Some(true) {
        bail!("{}", classify_failure(&stdout, &stderr));
    }
    let structured = v
        .get("structured_output")
        .cloned()
        .ok_or_else(|| anyhow!("evaluator returned no structured output"))?;
    let parsed: EvaluatorOutput = serde_json::from_value(structured)
        .map_err(|e| anyhow!("evaluator output did not match contract: {e}"))?;

    // modelUsage can include utility turns from small models; record the
    // model that did the actual work (largest share of run cost).
    let model = v["modelUsage"].as_object().and_then(|m| {
        m.iter()
            .max_by(|a, b| {
                let ca = a.1["costUSD"].as_f64().unwrap_or(0.0);
                let cb = b.1["costUSD"].as_f64().unwrap_or(0.0);
                ca.partial_cmp(&cb).unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(k, _)| k.clone())
    });
    Ok(EvalResult {
        achievements: parsed.achievements,
        model,
        cost_usd: v["total_cost_usd"].as_f64(),
        num_turns: v["num_turns"].as_i64(),
        duration_ms: v["duration_ms"].as_i64(),
    })
}

fn excerpt(s: &str) -> String {
    let t: String = s.chars().take(300).collect();
    t
}

fn classify_failure(stdout: &str, stderr: &str) -> String {
    let all = format!("{stdout}\n{stderr}").to_lowercase();
    if all.contains("not logged in")
        || all.contains("please run /login")
        || all.contains("authentication")
        || all.contains("api key")
        || all.contains("oauth")
    {
        "Claude Code is not authenticated. Open Claude Code and log in, then try again.".into()
    } else if all.contains("network")
        || all.contains("fetch failed")
        || all.contains("econnrefused")
        || all.contains("enotfound")
        || all.contains("timeout")
        || all.contains("offline")
    {
        "Could not reach Anthropic. Z-read will retry when you are back online.".into()
    } else if all.contains("budget") {
        "Evaluation stopped at its per-run safety limit — an unusually large day. It will retry on the next read.".into()
    } else {
        format!("Evaluation failed: {}", excerpt(stderr.trim()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_package_uses_stable_refs() {
        let facts = SessionFacts {
            session_id: "abc".into(),
            commands: vec![CommandFact {
                id: "cmd:abc:0".into(),
                command: "cargo test".into(),
                ok: true,
                kind: "test".into(),
                ts: None,
            }],
            commits: vec![CommitFact {
                sha: "deadbeef".into(),
                subject: "fix".into(),
                ts: "2026-07-20T10:00:00+02:00".into(),
                files: 1,
                insertions: 2,
                deletions: 3,
            }],
            pr_links: vec![PrLink {
                number: 5159,
                url: "https://github.com/acme/widgets/pull/5159".into(),
                repository: "acme/widgets".into(),
                ts: Some("2026-07-20T10:05:00+02:00".into()),
            }],
            ..Default::default()
        };
        let pkg = build_evidence_package("2026-07-20", &[facts]);
        let s = pkg["sessions"][0].clone();
        assert_eq!(s["ref"], "session:abc");
        assert_eq!(s["commands"][0]["ref"], "cmd:abc:0");
        assert_eq!(s["commits"][0]["ref"], "commit:deadbeef");
        assert_eq!(s["pull_requests"][0]["ref"], "pr:acme/widgets#5159");
        assert_eq!(
            s["pull_requests"][0]["url"],
            "https://github.com/acme/widgets/pull/5159"
        );
    }

    #[test]
    fn classifies_auth_failure() {
        let msg = classify_failure("", "Error: not logged in — please run /login");
        assert!(msg.contains("not authenticated"));
    }

    #[test]
    fn only_api_key_auth_is_metered() {
        assert!(auth_is_metered(r#"{"loggedIn":true,"authMethod":"api-key"}"#));
        assert!(!auth_is_metered(
            r#"{"loggedIn":true,"authMethod":"claude.ai","subscriptionType":"team"}"#
        ));
        assert!(!auth_is_metered(r#"{"loggedIn":false}"#));
        assert!(!auth_is_metered("not json"));
    }
}
