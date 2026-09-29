use crate::lifecycle::EvaluatorGuard;
use crate::models::*;
use crate::store;
use anyhow::{anyhow, bail, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

const EVAL_TIMEOUT: Duration = Duration::from_secs(900);
/// Hard per-run safety stop for a runaway evaluation, not a money budget: on a
/// subscription `--max-budget-usd` caps estimated work, not dollars spent.
const MAX_BUDGET_USD: f64 = 5.0;

pub fn model_for(agent: Agent) -> &'static str {
    match agent {
        Agent::Claude => "claude-opus-5-5",
        Agent::Codex => "gpt-6.1-sol",
    }
}

pub fn effort_for(agent: Agent) -> &'static str {
    match agent {
        Agent::Claude => "high",
        Agent::Codex => "high",
    }
}

fn cli_name(agent: Agent) -> &'static str {
    match agent {
        Agent::Claude => "claude",
        Agent::Codex => "codex",
    }
}

pub struct EvalResult {
    pub achievements: Vec<Achievement>,
    pub model: Option<String>,
    pub cost_usd: Option<f64>,
    pub num_turns: Option<i64>,
    pub duration_ms: Option<i64>,
    pub note: Option<String>,
}

#[derive(Clone, Serialize)]
pub struct EvaluatorInfo {
    pub agent: Agent,
    pub model: &'static str,
    pub effort: &'static str,
    pub found: bool,
}

#[derive(Clone, Serialize)]
pub struct Availability {
    pub evaluators: Vec<EvaluatorInfo>,
    pub metered: bool,
}

impl Default for Availability {
    fn default() -> Self {
        let evaluators = PRIORITY
            .iter()
            .map(|&agent| EvaluatorInfo {
                agent,
                model: model_for(agent),
                effort: effort_for(agent),
                found: false,
            })
            .collect();
        Self {
            evaluators,
            metered: false,
        }
    }
}

// Claude Code has priority; Codex takes over when it is missing or its run fails.
const PRIORITY: [Agent; 2] = [Agent::Claude, Agent::Codex];

struct Completed {
    output: Value,
    model: Option<String>,
    cost_usd: Option<f64>,
    num_turns: Option<i64>,
    duration_ms: Option<i64>,
}

#[derive(PartialEq)]
enum JobKind {
    Evaluation,
    Rewrite,
}

struct Job<'a> {
    kind: JobKind,
    prompt: &'a str,
    schema: Value,
}

pub fn find_cli(agent: Agent, settings: &Settings) -> Result<PathBuf> {
    if let Some(p) = settings
        .claude_path
        .as_ref()
        .filter(|_| agent == Agent::Claude)
    {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return Ok(pb);
        }
    }
    let name = cli_name(agent);
    if let Ok(out) = Command::new("/usr/bin/which").arg(name).output() {
        if out.status.success() {
            let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !p.is_empty() {
                return Ok(PathBuf::from(p));
            }
        }
    }
    let home = dirs::home_dir().unwrap_or_default();
    let bundled = match agent {
        Agent::Claude => home.join(".claude/local/claude"),
        Agent::Codex => PathBuf::from("/Applications/ChatGPT.app/Contents/Resources/codex"),
    };
    for cand in [
        PathBuf::from("/opt/homebrew/bin").join(name),
        PathBuf::from("/usr/local/bin").join(name),
        home.join(".local/bin").join(name),
        bundled,
    ] {
        if cand.exists() {
            return Ok(cand);
        }
    }
    bail!("{} CLI not found", agent.label())
}

/// An evaluator invocation with the vendor API key stripped, so every run uses the
/// developer's subscription login rather than silently billing an API key.
fn cli_command(agent: Agent, settings: &Settings) -> Result<Command> {
    let mut cmd = Command::new(find_cli(agent, settings)?);
    cmd.env_remove(match agent {
        Agent::Claude => "ANTHROPIC_API_KEY",
        Agent::Codex => "OPENAI_API_KEY",
    });
    cmd.env("Z_REPORT_EVALUATOR", "1");
    cmd.env_remove("CLAUDE_CODE_ENABLE_FUNCTION_HOOKS");
    cmd.stdin(Stdio::null());
    Ok(cmd)
}

pub fn discover(settings: &Settings) -> Vec<EvaluatorInfo> {
    let mut evaluators = Availability::default().evaluators;
    for info in &mut evaluators {
        info.found = find_cli(info.agent, settings).is_ok();
    }
    evaluators
}

pub fn probe(settings: &Settings, _busy: &EvaluatorGuard) -> Availability {
    let evaluators = discover(settings);
    let metered = evaluators
        .iter()
        .find(|info| info.found)
        .is_some_and(|info| metered_login(info.agent, settings));
    Availability {
        evaluators,
        metered,
    }
}

// Only confirmed API-key auth is metered; subscription cost values are estimates.
fn metered_login(agent: Agent, settings: &Settings) -> bool {
    let Ok(mut cmd) = cli_command(agent, settings) else {
        return false;
    };
    let args: &[&str] = match agent {
        Agent::Claude => &["auth", "status", "--json"],
        Agent::Codex => &["login", "status"],
    };
    let Ok(out) = cmd.args(args).output() else {
        return false;
    };
    let status = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out.status.success() && auth_is_metered(agent, &status)
}

fn with_fallback<T>(
    settings: &Settings,
    mut attempt: impl FnMut(Agent) -> Result<T>,
) -> Result<(T, Option<String>)> {
    let installed: Vec<Agent> = PRIORITY
        .into_iter()
        .filter(|&agent| find_cli(agent, settings).is_ok())
        .collect();
    if installed.is_empty() {
        bail!("No evaluator found. Install the Claude Code or Codex CLI.");
    }
    let mut failures: Vec<String> = Vec::new();
    for agent in installed {
        match attempt(agent) {
            Ok(value) => {
                let note = (!failures.is_empty()).then(|| failures.join(" · "));
                return Ok((value, note));
            }
            Err(e) => failures.push(format!("{}: {e}", agent.label())),
        }
    }
    bail!("{}", failures.join(" · "))
}

fn auth_is_metered(agent: Agent, status: &str) -> bool {
    match agent {
        Agent::Claude => serde_json::from_str::<Value>(status.trim())
            .ok()
            .is_some_and(|value| value["authMethod"].as_str() == Some("api-key")),
        Agent::Codex => status.to_lowercase().contains("api key"),
    }
}

fn structured_command(agent: Agent, settings: &Settings, job: &Job, dir: &Path) -> Result<Command> {
    let mut cmd = cli_command(agent, settings)?;
    match agent {
        Agent::Claude => {
            let (tools, disallowed) = match job.kind {
                JobKind::Evaluation => (
                    "Read,Grep,Glob",
                    "Bash,Edit,Write,NotebookEdit,WebFetch,WebSearch,Task,Agent",
                ),
                JobKind::Rewrite => (
                    "",
                    "Bash,Edit,Write,Read,Grep,Glob,NotebookEdit,WebFetch,WebSearch,Task,Agent",
                ),
            };
            cmd.args([
                "-p",
                job.prompt,
                "--model",
                model_for(agent),
                "--output-format",
                "json",
                "--json-schema",
                &serde_json::to_string(&job.schema)?,
                "--tools",
                tools,
                "--disallowedTools",
                disallowed,
                "--safe-mode",
                "--strict-mcp-config",
                "--mcp-config",
                "{\"mcpServers\":{}}",
                "--no-session-persistence",
                "--setting-sources",
                "",
            ]);
            if job.kind == JobKind::Evaluation {
                cmd.args(["--effort", effort_for(agent)]);
                if settings.cost_limit_enabled {
                    cmd.args(["--max-budget-usd", &format!("{MAX_BUDGET_USD:.2}")]);
                }
            }
        }
        Agent::Codex => {
            let schema_path = dir.join("schema.json");
            std::fs::write(&schema_path, serde_json::to_string(&job.schema)?)?;
            cmd.arg("exec")
                .args(["--model", model_for(agent)])
                .args(["-c", "approval_policy=\"never\""])
                .args(["--sandbox", "read-only"])
                .args([
                    "--skip-git-repo-check",
                    "--ephemeral",
                    "--ignore-user-config",
                ])
                .args(["--color", "never", "--json"])
                .arg("--output-schema")
                .arg(&schema_path)
                .arg("-o")
                .arg(dir.join("output.json"))
                .arg("-C")
                .arg(dir);
            if job.kind == JobKind::Evaluation {
                cmd.args([
                    "-c",
                    &format!("model_reasoning_effort=\"{}\"", effort_for(agent)),
                ]);
            }
            cmd.arg(job.prompt);
        }
    }
    Ok(cmd)
}

fn run_dir(name: &str) -> Result<PathBuf> {
    let dir = store::data_dir()
        .join("eval")
        .join(format!("{}-{}", name, crate::engine::new_id()));
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(dir)
}

fn run(
    agent: Agent,
    settings: &Settings,
    job: &Job,
    dir: &Path,
    busy: &EvaluatorGuard,
) -> Result<Completed> {
    let cmd = structured_command(agent, settings, job, dir)?;
    let (out, elapsed) = crate::process::run(cmd, dir, EVAL_TIMEOUT, busy)?;
    decode(agent, &out, dir, elapsed)
}

fn decode(agent: Agent, out: &Output, dir: &Path, elapsed: Duration) -> Result<Completed> {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        bail!("{}", classify_failure(agent, &stdout, &stderr));
    }
    match agent {
        Agent::Claude => decode_claude(&stdout, &stderr),
        Agent::Codex => decode_codex(&stdout, &stderr, dir, elapsed),
    }
}

// Claude can report in-band failures with a successful process exit.
fn decode_claude(stdout: &str, stderr: &str) -> Result<Completed> {
    let v: Value = serde_json::from_str(stdout.trim())
        .map_err(|_| anyhow!("unexpected output (not JSON): {}", excerpt(stdout)))?;
    if v["is_error"].as_bool() == Some(true) {
        bail!("{}", classify_failure(Agent::Claude, stdout, stderr));
    }
    let output = v
        .get("structured_output")
        .cloned()
        .ok_or_else(|| anyhow!("returned no structured output"))?;
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
    Ok(Completed {
        output,
        model,
        cost_usd: v["total_cost_usd"].as_f64(),
        num_turns: v["num_turns"].as_i64(),
        duration_ms: v["duration_ms"].as_i64(),
    })
}

// `codex exec --json` streams one event per line; the schema-checked answer lands
// in the -o file, and a failed turn surfaces as an error event with exit code 0.
fn decode_codex(stdout: &str, stderr: &str, dir: &Path, elapsed: Duration) -> Result<Completed> {
    let events: Vec<Value> = stdout
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    if let Some(message) = events.iter().find_map(codex_error) {
        bail!("{}", classify_failure(Agent::Codex, message, stderr));
    }
    let output = std::fs::read_to_string(dir.join("output.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(text.trim()).ok())
        .ok_or_else(|| anyhow!("returned no structured output"))?;
    let actions = events
        .iter()
        .filter(|e| e["type"].as_str() == Some("item.completed"))
        .count();
    Ok(Completed {
        output,
        model: Some(model_for(Agent::Codex).to_owned()),
        cost_usd: None,
        num_turns: Some(actions as i64),
        duration_ms: Some(elapsed.as_millis() as i64),
    })
}

fn codex_error(event: &Value) -> Option<&str> {
    match event["type"].as_str()? {
        "error" => event["message"].as_str(),
        "turn.failed" => event["error"]["message"].as_str(),
        _ => None,
    }
}

fn output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "achievements": {
                "type": "array",
                "items": {
                    "type": "object",
        "additionalProperties": false,
                    "properties": {
                        "title": { "type": "string", "description": "Short, specific, outcome-first statement; max ~70 characters." },
                        "contribution": { "type": "string", "description": "At most 2-3 plain sentences a teammate who wasn't there could understand: what was done and why it mattered. No jargon or filler; let the outcome bullets carry the specifics." },
                        "outcomes": {
                            "type": "array",
                            "items": {
                                "type": "object",
        "additionalProperties": false,
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
                    "required": ["title", "contribution", "outcomes", "uncertainties", "confidence", "session_ids"]
                }
            }
        },
        "required": ["achievements"]
    })
}

pub fn build_evidence_package(day: &str, sessions: &[SessionFacts]) -> Value {
    json!({
        "date": day,
        "instructions": "Evidence collected locally from coding-agent sessions (Claude Code, Codex) and Git. Reference facts by their ref ids.",
        "sessions": sessions.iter().map(|s| json!({
            "id": s.session_id,
            "ref": format!("session:{}", s.session_id),
            "agent": s.agent.label(),
            "session_title": s.title,
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
                "edits": f.count,
                "delegated": f.via_delegate
            })).collect::<Vec<_>>(),
            "commands": s.commands.iter().map(|c| json!({
                "ref": c.id,
                "command": c.command,
                "succeeded": c.ok,
                "kind": c.kind,
                "delegated": c.via_delegate
            })).collect::<Vec<_>>(),
            "external_actions": s.external_changes().map(|a| json!({
                "ref": a.id,
                "server": a.server,
                "tool": a.tool,
                "succeeded": a.ok,
                "delegated": a.via_delegate,
                "performed_at": a.ts
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

const EVALUATOR_PROMPT: &str = r#"You are the evaluator for Z Report, a private local accomplishment journal. Read ./evidence.json — it contains today's coding-agent session evidence (Claude Code and Codex, see each session's "agent") and Git facts for one developer.

Reconstruct the day's accomplishments as achievements a developer would be proud to put in a standup or performance review. Follow these rules strictly:

1. Celebrate outcomes, not activity. "Fixed flaky auth test that blocked CI" is an achievement; "ran 14 commands" is not.
2. Cluster related sessions into a single achievement when they share a repository, branch, files, or a clear narrative thread, regardless of which agent ran them — the developer often carries one piece of work across both tools. Use each session at most once. In session_ids, list the bare session id values, not "session:" refs.
3. Every outcome claim must cite evidence_refs that literally exist in evidence.json ("session:…", "file:…", "cmd:…", "action:…", "commit:…", "pr:…"). Never invent refs.
4. Assign each claim the highest evidence level the cited refs support:
   1 = work observed in a session, 2 = a concrete change was produced, 3 = a relevant test/build/check passed, 4 = the change exists in a local commit or has a recorded PR link alongside a file change.
   Never claim level 3 without a succeeded test/build/check ref; never claim level 4 without a commit ref or a PR ref from a session with a file change. A PR ref proves the change was proposed, not merged. An "action:" ref never supports more than level 2.
5. State uncertainties honestly (e.g. "tests were not run", "change not committed"). Do not speculate about production impact.
6. Keep each achievement brief and legible to someone who wasn't there — a teammate or manager skimming a standup. Title: a short, specific, outcome-first statement (max ~70 chars). Contribution: at most 2-3 plain sentences saying what the developer did and why it mattered — no jargon or filler. Keep each outcome claim to a single scannable bullet line; let the bullets, not the prose, carry the specifics.
7. Confidence is your honest probability that the developer would recognize this as a real, correctly described accomplishment.
8. Skip noise: exploratory sessions with no output can be omitted or grouped into one low-confidence "investigation" achievement if the investigation itself was substantial.
9. Facts marked "delegated": true come from a sub-session the developer directed rather than steered step by step. They still count as the developer's contribution and carry the same evidence weight, but never split them into a separate achievement, and do not describe them as hands-on work.
10. "session_title" is generated from how a session opened, so it states what the developer set out to do, not what they achieved. Use it to understand intent and to link sessions working the same thread; never reuse it as the achievement title.
11. "external_actions" are calls to services outside the repository — an issue commented on, a document updated. Only the call is recorded; nothing local proves what it did. Describe them as performed ("posted the migration notes to the tracker"), never as confirmed impact ("unblocked the team"), and do not build an achievement out of external actions alone unless the action itself was the point of the work.

Return only the structured output."#;

pub fn evaluate_day(
    settings: &Settings,
    busy: &EvaluatorGuard,
    day: &str,
    sessions: &[SessionFacts],
) -> Result<EvalResult> {
    let dir = run_dir(day)?;
    let package = build_evidence_package(day, sessions);
    std::fs::write(
        dir.join("evidence.json"),
        serde_json::to_string_pretty(&package)?,
    )?;
    let job = Job {
        kind: JobKind::Evaluation,
        prompt: EVALUATOR_PROMPT,
        schema: output_schema(),
    };
    let result = with_fallback(settings, |agent| {
        anyhow::ensure!(!busy.cancelled(), "Read cancelled");
        let done = run(agent, settings, &job, &dir, busy)?;
        let parsed: EvaluatorOutput = serde_json::from_value(done.output)
            .map_err(|e| anyhow!("output did not match contract: {e}"))?;
        Ok(EvalResult {
            achievements: parsed.achievements,
            model: done.model,
            cost_usd: done.cost_usd,
            num_turns: done.num_turns,
            duration_ms: done.duration_ms,
            note: None,
        })
    });
    let _ = std::fs::remove_dir_all(&dir);
    let (mut result, note) = result?;
    result.note = note;
    Ok(result)
}

const MERGE_PROMPT: &str = r#"Several achievement cards below describe one piece of work the developer carried across more than one day. They were written separately and read as stitched fragments.

Rewrite them as a single achievement. Title: short, specific, outcome-first, max ~70 characters. Contribution: at most 2-3 plain sentences a teammate who wasn't there could follow, covering the whole arc of the work rather than summarizing each fragment in turn.

Use only what the cards state. Do not invent outcomes, do not add detail that is not present, and do not describe anything as verified or shipped unless a card already does. Return only the structured output."#;

pub fn rewrite_merged(
    settings: &Settings,
    busy: &EvaluatorGuard,
    parts: &[(String, String)],
) -> Result<(String, String)> {
    let cards: Vec<Value> = parts
        .iter()
        .map(|(title, contribution)| json!({ "title": title, "contribution": contribution }))
        .collect();
    let prompt = format!(
        "{MERGE_PROMPT}\n\nCards:\n{}",
        serde_json::to_string_pretty(&json!({ "cards": cards }))?
    );
    let job = Job {
        kind: JobKind::Rewrite,
        prompt: &prompt,
        schema: json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "title": { "type": "string" },
                "contribution": { "type": "string" }
            },
            "required": ["title", "contribution"]
        }),
    };
    let dir = run_dir("merge")?;
    let result = with_fallback(settings, |agent| {
        anyhow::ensure!(!busy.cancelled(), "Read cancelled");
        let s = run(agent, settings, &job, &dir, busy)?.output;
        let (Some(title), Some(contribution)) = (s["title"].as_str(), s["contribution"].as_str())
        else {
            bail!("merge rewrite output did not match contract");
        };
        if title.trim().is_empty() || contribution.trim().is_empty() {
            bail!("merge rewrite returned empty prose");
        }
        Ok((title.trim().to_string(), contribution.trim().to_string()))
    });
    let _ = std::fs::remove_dir_all(&dir);
    Ok(result?.0)
}

fn excerpt(s: &str) -> String {
    s.chars().take(300).collect()
}

fn classify_failure(agent: Agent, stdout: &str, stderr: &str) -> String {
    // Codex notes on stderr that stdin is not a terminal; that line is never the failure.
    let stderr: String = stderr
        .lines()
        .filter(|line| !line.contains("Reading additional input from stdin"))
        .collect::<Vec<_>>()
        .join("\n");
    let all = format!("{stdout}\n{stderr}").to_lowercase();
    if all.contains("not logged in")
        || all.contains("please run /login")
        || all.contains("codex login")
        || all.contains("authentication")
        || all.contains("unauthorized")
        || all.contains("api key")
        || all.contains("oauth")
    {
        let fix = match agent {
            Agent::Claude => "Open Claude Code and log in, then try again.",
            Agent::Codex => "Run `codex login`, then try again.",
        };
        format!("not authenticated. {fix}")
    } else if all.contains("network")
        || all.contains("fetch failed")
        || all.contains("econnrefused")
        || all.contains("enotfound")
        || all.contains("timeout")
        || all.contains("offline")
    {
        format!(
            "could not reach {}. Z-read will retry when you are back online.",
            agent.vendor()
        )
    } else if all.contains("budget") {
        "stopped at its per-run safety limit — an unusually large day. It will retry on the next read.".into()
    } else if stderr.trim().is_empty() {
        "exited without a result".into()
    } else {
        excerpt(stderr.trim())
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
                via_delegate: true,
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
            external_actions: vec![ExternalAction {
                id: "action:abc:0".into(),
                server: "claude_ai_Atlassian".into(),
                tool: "addCommentToJiraIssue".into(),
                ok: true,
                mutating: true,
                ts: Some("2026-07-20T10:06:00+02:00".into()),
                via_delegate: false,
            }],
            ..Default::default()
        };
        let pkg = build_evidence_package("2026-07-20", &[facts]);
        let s = pkg["sessions"][0].clone();
        assert_eq!(s["ref"], "session:abc");
        assert_eq!(s["agent"], "Claude Code");
        assert_eq!(s["commands"][0]["ref"], "cmd:abc:0");
        assert_eq!(s["commands"][0]["delegated"], true);
        assert_eq!(s["external_actions"][0]["ref"], "action:abc:0");
        assert_eq!(s["external_actions"][0]["tool"], "addCommentToJiraIssue");
        assert_eq!(s["external_actions"][0]["succeeded"], true);
        assert_eq!(s["commits"][0]["ref"], "commit:deadbeef");
        assert_eq!(s["pull_requests"][0]["ref"], "pr:acme/widgets#5159");
        assert_eq!(
            s["pull_requests"][0]["url"],
            "https://github.com/acme/widgets/pull/5159"
        );
    }

    #[test]
    fn classifies_auth_failure() {
        let msg = classify_failure(
            Agent::Claude,
            "",
            "Error: not logged in — please run /login",
        );
        assert!(msg.starts_with("not authenticated. Open Claude Code"));
        let msg = classify_failure(Agent::Codex, "", "Not logged in. Run `codex login`.");
        assert!(msg.starts_with("not authenticated. Run `codex login`"));
    }

    #[test]
    fn codex_stdin_notice_is_not_the_failure() {
        let msg = classify_failure(
            Agent::Codex,
            "",
            "Reading additional input from stdin...\nError: model gpt-6-sol is unavailable",
        );
        assert_eq!(msg, "Error: model gpt-6-sol is unavailable");
    }

    #[test]
    fn codex_run_is_decoded_from_events_and_output_file() {
        let dir = std::env::temp_dir().join("zreport-test/codex-eval");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("output.json"), r#"{"achievements":[]}"#).unwrap();
        let events = concat!(
            r#"{"type":"thread.started","thread_id":"t"}"#,
            "\n",
            r#"{"type":"item.completed","item":{"type":"command_execution","command":"cat evidence.json"}}"#,
            "\n",
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"{}"}}"#,
            "\n",
            r#"{"type":"turn.completed","usage":{"input_tokens":10,"output_tokens":2}}"#,
            "\n",
        );

        let done = decode_codex(events, "", &dir, Duration::from_millis(1500)).unwrap();
        assert_eq!(done.output, json!({"achievements": []}));
        assert_eq!(done.model.as_deref(), Some("gpt-6.1-sol"));
        assert_eq!(done.num_turns, Some(2));
        assert_eq!(done.duration_ms, Some(1500));
        assert_eq!(done.cost_usd, None);

        let failed = concat!(
            r#"{"type":"thread.started","thread_id":"t"}"#,
            "\n",
            r#"{"type":"turn.failed","error":{"message":"stream disconnected: network error"}}"#,
            "\n",
        );
        let err = decode_codex(failed, "", &dir, Duration::ZERO)
            .err()
            .expect("a failed turn is an error");
        assert!(err.to_string().contains("could not reach OpenAI"));
    }

    #[test]
    fn output_schema_is_strict_for_every_backend() {
        fn closed(v: &Value) -> bool {
            match v {
                Value::Object(map) => {
                    let ok = map.get("type") != Some(&json!("object"))
                        || (map.get("additionalProperties") == Some(&json!(false))
                            && map["properties"].as_object().is_some_and(|props| {
                                map["required"]
                                    .as_array()
                                    .is_some_and(|req| req.len() == props.len())
                            }));
                    ok && map.values().all(closed)
                }
                Value::Array(items) => items.iter().all(closed),
                _ => true,
            }
        }
        assert!(closed(&output_schema()));
    }

    #[test]
    fn only_api_key_auth_is_metered() {
        assert!(auth_is_metered(
            Agent::Claude,
            r#"{"loggedIn":true,"authMethod":"api-key"}"#
        ));
        assert!(!auth_is_metered(
            Agent::Claude,
            r#"{"loggedIn":true,"authMethod":"claude.ai","subscriptionType":"team"}"#
        ));
        assert!(!auth_is_metered(Agent::Claude, r#"{"loggedIn":false}"#));
        assert!(!auth_is_metered(Agent::Claude, "not json"));
        assert!(auth_is_metered(Agent::Codex, "Logged in using an API key"));
        assert!(!auth_is_metered(Agent::Codex, "Logged in using ChatGPT"));
    }
}
