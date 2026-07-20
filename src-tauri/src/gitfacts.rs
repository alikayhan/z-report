use crate::models::*;
use std::path::Path;
use std::process::Command;

fn git(dir: &str, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub fn repo_root(cwd: &str) -> Option<String> {
    if !Path::new(cwd).exists() {
        return None;
    }
    git(cwd, &["rev-parse", "--show-toplevel"])
}

pub fn commit_exists(repo: &str, sha: &str) -> bool {
    if sha.len() < 7 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return false;
    }
    let spec = format!("{sha}^{{commit}}");
    Command::new("git")
        .args(["-C", repo, "cat-file", "-e", &spec])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Commits authored in the window [since, until] (RFC3339), by the repo's
/// configured user if set — someone else's commits are not the user's work.
pub fn commits_in_window(repo: &str, since: &str, until: &str) -> Vec<CommitFact> {
    let mut args = vec![
        "log".to_string(),
        format!("--since={since}"),
        format!("--until={until}"),
        "--pretty=format:%H\t%s\t%cI".to_string(),
        "--shortstat".to_string(),
        "--no-merges".to_string(),
    ];
    if let Some(email) = git(repo, &["config", "user.email"]) {
        if !email.is_empty() {
            args.push(format!("--author={email}"));
        }
    }
    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let Some(out) = git(repo, &arg_refs) else {
        return Vec::new();
    };
    let mut commits = Vec::new();
    let mut current: Option<CommitFact> = None;
    for line in out.lines() {
        if line.contains('\t') {
            if let Some(c) = current.take() {
                commits.push(c);
            }
            let mut parts = line.splitn(3, '\t');
            let sha = parts.next().unwrap_or("").to_string();
            let subject = parts.next().unwrap_or("").to_string();
            let ts = parts.next().unwrap_or("").to_string();
            current = Some(CommitFact {
                sha,
                subject,
                ts,
                files: 0,
                insertions: 0,
                deletions: 0,
            });
        } else if let Some(c) = current.as_mut() {
            for piece in line.split(',') {
                let piece = piece.trim();
                let num: u32 = piece
                    .split_whitespace()
                    .next()
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0);
                if piece.contains("file") {
                    c.files = num;
                } else if piece.contains("insertion") {
                    c.insertions = num;
                } else if piece.contains("deletion") {
                    c.deletions = num;
                }
            }
        }
    }
    if let Some(c) = current.take() {
        commits.push(c);
    }
    commits
}

/// Attach repo root and same-day commits to session facts.
pub fn correlate(facts: &mut SessionFacts) {
    let Some(cwd) = facts.cwd.clone() else { return };
    facts.repo_root = repo_root(&cwd);
    let (Some(repo), Some(first), Some(last)) = (
        facts.repo_root.clone(),
        facts.first_ts.clone(),
        facts.last_ts.clone(),
    ) else {
        return;
    };
    // Widen window slightly: commits usually land minutes after the session's last message.
    let until = chrono::DateTime::parse_from_rfc3339(&last)
        .map(|t| (t + chrono::Duration::hours(2)).to_rfc3339())
        .unwrap_or(last);
    facts.commits = commits_in_window(&repo, &first, &until);
}

/// Highest evidence level a set of refs deterministically supports.
/// Refs: "commit:<sha>", "cmd:<session>:<n>", "file:<path>", "session:<id>".
pub fn verify_outcome(
    outcome: &mut Outcome,
    sessions: &[&SessionFacts],
    uncertainties: &mut Vec<String>,
) {
    let claimed = outcome.evidence_level.clamp(1, 4);
    let mut supported: u8 = 0;
    for r in &outcome.evidence_refs {
        let level = if let Some(sha) = r.strip_prefix("commit:") {
            let ok = sessions
                .iter()
                .filter_map(|s| s.repo_root.as_deref())
                .any(|repo| commit_exists(repo, sha));
            if ok {
                4
            } else {
                0
            }
        } else if r.starts_with("cmd:") {
            let found = sessions
                .iter()
                .flat_map(|s| &s.commands)
                .find(|c| &c.id == r);
            match found {
                Some(c) if c.ok && matches!(c.kind.as_str(), "test" | "build" | "check") => 3,
                Some(c) if c.ok => 1,
                _ => 0,
            }
        } else if let Some(path) = r.strip_prefix("file:") {
            let ok = sessions
                .iter()
                .flat_map(|s| &s.files_changed)
                .any(|f| f.path == path || f.path.ends_with(path));
            if ok {
                2
            } else {
                0
            }
        } else if r.starts_with("session:") {
            let id = r.trim_start_matches("session:");
            if sessions.iter().any(|s| s.session_id == id) {
                1
            } else {
                0
            }
        } else {
            0
        };
        supported = supported.max(level);
    }
    outcome.verified = claimed <= supported;
    if !outcome.verified {
        let final_level = supported.max(1);
        if claimed > final_level {
            uncertainties.push(format!(
                "Claim \"{}\" was stated at evidence level {} but local facts only support level {}.",
                truncate_claim(&outcome.claim),
                claimed,
                final_level
            ));
        }
        outcome.evidence_level = final_level;
    } else {
        outcome.evidence_level = claimed;
    }
}

fn truncate_claim(s: &str) -> String {
    if s.len() > 80 {
        format!("{}…", &s[..s.char_indices().take(80).last().map(|(i, c)| i + c.len_utf8()).unwrap_or(80)])
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_with_cmd(ok: bool, kind: &str) -> SessionFacts {
        SessionFacts {
            session_id: "s1".into(),
            commands: vec![CommandFact {
                id: "cmd:s1:0".into(),
                command: "cargo test".into(),
                ok,
                kind: kind.into(),
                ts: None,
            }],
            files_changed: vec![FileChange {
                path: "/repo/src/lib.rs".into(),
                tool: "Edit".into(),
                count: 2,
            }],
            ..Default::default()
        }
    }

    #[test]
    fn verifies_passing_test_ref() {
        let s = session_with_cmd(true, "test");
        let mut o = Outcome {
            claim: "Tests pass".into(),
            evidence_level: 3,
            evidence_refs: vec!["cmd:s1:0".into()],
            verified: false,
        };
        let mut u = Vec::new();
        verify_outcome(&mut o, &[&s], &mut u);
        assert!(o.verified);
        assert_eq!(o.evidence_level, 3);
        assert!(u.is_empty());
    }

    #[test]
    fn downgrades_unsupported_commit_claim() {
        let s = session_with_cmd(true, "test");
        let mut o = Outcome {
            claim: "Landed as commit".into(),
            evidence_level: 4,
            evidence_refs: vec!["commit:deadbeef1234".into(), "file:src/lib.rs".into()],
            verified: false,
        };
        let mut u = Vec::new();
        verify_outcome(&mut o, &[&s], &mut u);
        assert!(!o.verified);
        assert_eq!(o.evidence_level, 2);
        assert_eq!(u.len(), 1);
    }

    #[test]
    fn failed_command_supports_nothing() {
        let s = session_with_cmd(false, "test");
        let mut o = Outcome {
            claim: "Tests pass".into(),
            evidence_level: 3,
            evidence_refs: vec!["cmd:s1:0".into()],
            verified: false,
        };
        let mut u = Vec::new();
        verify_outcome(&mut o, &[&s], &mut u);
        assert!(!o.verified);
        assert_eq!(o.evidence_level, 1);
    }
}
