use crate::lifecycle::{ClaudeGuard, Lifecycle};
use crate::models::*;
use crate::store::Store;
use crate::{calendar, evaluator, gitfacts, ingest, related};
use anyhow::Result;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;

pub(crate) const EVAL_WINDOW_DAYS: i64 = 15;
const EVIDENCE_HORIZON_DAYS: i64 = 90;
const EVIDENCE_PRUNE_SLACK_DAYS: i64 = 7;

pub struct AppState {
    pub store: Mutex<Store>,
    pub lifecycle: Lifecycle,
    pub claude_found: AtomicBool,
    pub metered: Mutex<Option<bool>>,
}

pub fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

pub fn scan(app: &AppHandle) -> Result<u32> {
    let state = app.state::<AppState>();
    let files = ingest::discover();
    let live_ids: Vec<String> = files.iter().map(|f| f.session_id.clone()).collect();
    let (settings, known) = {
        let store = state.store.lock().unwrap();
        (store.settings(), store.session_content_hashes()?)
    };

    let stale_before = chrono::Local::now().timestamp() - EVIDENCE_HORIZON_DAYS * 86_400;
    let mut updated = 0u32;
    let removed;
    for file in &files {
        if (file.mtime as i64) < stale_before {
            continue;
        }
        if known.get(&file.session_id).map(String::as_str) == Some(file.content_hash.as_str()) {
            continue;
        }
        let Ok(mut facts) = ingest::parse_transcript(file, settings.retain_prompts) else {
            continue;
        };
        gitfacts::correlate(&mut facts);
        if let Some(repo) = &facts.repo_root {
            if settings.excluded_repos.iter().any(|r| repo.starts_with(r)) {
                continue;
            }
        }
        let Some(day) = ingest::session_day(&facts) else {
            continue;
        };
        let store = state.store.lock().unwrap();
        if store.upsert_session_if_changed(&facts, &day, &file.content_hash)? {
            updated += 1;
        }
    }

    {
        let store = state.store.lock().unwrap();
        if settings.retention_days > 0 {
            let min_day = calendar::offset(&today(), -(settings.retention_days as i64));
            store.prune_candidates_older_than(&min_day)?;
        }
        let evidence_min_day = calendar::offset(
            &today(),
            -(EVIDENCE_HORIZON_DAYS + EVIDENCE_PRUNE_SLACK_DAYS),
        );
        removed = store.prune_sessions_older_than(&evidence_min_day)?
            + store.delete_sessions_missing_from(&live_ids)?;
        store.kv_set("last_scan_at", &chrono::Local::now().to_rfc3339())?;
    }
    if updated > 0 || removed > 0 {
        let _ = app.emit("zr:refresh", ());
    }
    Ok(updated)
}

pub fn evaluate_pending(app: &AppHandle, kind: &str) -> Result<usize> {
    let state = app.state::<AppState>();
    let Some(busy) = state.lifecycle.begin_evaluation() else {
        return Ok(0);
    };
    let _ = app.emit("zr:evaluating", true);
    let result = evaluate_pending_inner(app, kind, &busy);
    drop(busy);
    let _ = app.emit("zr:evaluating", false);
    let _ = app.emit("zr:refresh", ());

    match &result {
        Ok(count) if *count > 0 => {
            let noun = if *count == 1 {
                "achievement"
            } else {
                "achievements"
            };
            let title = if kind == "zread" {
                "Today's Z-read is ready"
            } else {
                "X-read complete"
            };
            let _ = app
                .notification()
                .builder()
                .title(title)
                .body(format!("{count} {noun} ready for review"))
                .show();
        }
        Ok(_) => {}
        Err(e) => {
            let _ = app
                .notification()
                .builder()
                .title("Z Report")
                .body(e.to_string())
                .show();
        }
    }
    result
}

pub fn spawn_evaluation(app: AppHandle, kind: &'static str) {
    std::thread::spawn(move || {
        let _ = evaluate_pending(&app, kind);
    });
}

fn evaluate_pending_inner(app: &AppHandle, kind: &str, claude: &ClaudeGuard) -> Result<usize> {
    scan(app)?;
    let state = app.state::<AppState>();
    let (settings, pending) = {
        let store = state.store.lock().unwrap();
        let today = today();
        let min_day = calendar::offset(&today, -(EVAL_WINDOW_DAYS - 1));
        (store.settings(), store.pending_sessions(&today, &min_day)?)
    };

    let mut by_day: BTreeMap<String, Vec<SessionFacts>> = BTreeMap::new();
    for (day, facts) in pending {
        by_day.entry(day).or_default().push(facts);
    }

    let mut total_candidates = 0usize;
    for (day, sessions) in by_day {
        let all_ids: Vec<String> = sessions.iter().map(|s| s.session_id.clone()).collect();
        let substantial: Vec<SessionFacts> = sessions
            .into_iter()
            .filter(SessionFacts::has_substance)
            .collect();
        if substantial.is_empty() {
            let store = state.store.lock().unwrap();
            store.mark_sessions_evaluated(&all_ids)?;
            continue;
        }

        let run_id = format!("run-{}-{}", day, chrono::Local::now().format("%H%M%S%3f"));
        let started_at = chrono::Local::now().to_rfc3339();
        let mut run = EvalRun {
            id: run_id,
            day: day.clone(),
            kind: kind.to_string(),
            started_at,
            finished_at: None,
            status: "running".into(),
            model: None,
            cost_usd: None,
            num_turns: None,
            duration_ms: None,
            session_count: substantial.len() as i64,
            candidate_count: 0,
            error: None,
        };
        {
            let store = state.store.lock().unwrap();
            store.insert_eval_run(&run)?;
        }

        match evaluator::evaluate_day(&settings, claude, &day, &substantial) {
            Ok(result) => {
                let candidates = build_candidates(
                    &day,
                    &substantial,
                    result.achievements,
                    result.model.clone(),
                );
                let store = state.store.lock().unwrap();
                store.delete_pending_for_sessions(&day, &all_ids)?;
                for c in &candidates {
                    store.insert_candidate(c)?;
                }
                store.mark_sessions_evaluated(&all_ids)?;
                run.status = "ok".into();
                run.model = result.model;
                run.cost_usd = result.cost_usd;
                run.num_turns = result.num_turns;
                run.duration_ms = result.duration_ms;
                run.candidate_count = candidates.len() as i64;
                run.finished_at = Some(chrono::Local::now().to_rfc3339());
                store.insert_eval_run(&run)?;
                total_candidates += candidates.len();
            }
            Err(e) => {
                let store = state.store.lock().unwrap();
                run.status = "error".into();
                run.error = Some(e.to_string());
                run.finished_at = Some(chrono::Local::now().to_rfc3339());
                store.insert_eval_run(&run)?;
                return Err(e);
            }
        }
    }
    {
        let store = state.store.lock().unwrap();
        link_related(&store)?;
    }
    Ok(total_candidates)
}

pub fn merge_into_one(parts: &[Candidate]) -> Candidate {
    let lead = parts
        .iter()
        .min_by(|a, b| {
            b.evidence_level
                .cmp(&a.evidence_level)
                .then(a.day.cmp(&b.day))
        })
        .expect("merge needs at least one candidate");
    let ordered = {
        let mut v: Vec<&Candidate> = parts.iter().collect();
        v.sort_by(|a, b| a.day.cmp(&b.day).then(a.created_at.cmp(&b.created_at)));
        v
    };
    let outcomes = unique_by(
        ordered.iter().flat_map(|c| c.outcomes.iter().cloned()),
        |outcome| outcome.claim.trim().to_lowercase(),
    );
    let uncertainties = unique_by(
        ordered.iter().flat_map(|c| c.uncertainties.iter().cloned()),
        |uncertainty| uncertainty.trim().to_owned(),
    );
    let session_ids = unique_by(
        ordered.iter().flat_map(|c| c.session_ids.iter().cloned()),
        Clone::clone,
    );
    let agents = unique_agents(ordered.iter().flat_map(|c| c.agents.iter().copied()));
    let day = ordered[0].day.clone();
    let day_end = ordered
        .iter()
        .map(|c| c.day_end().to_string())
        .max()
        .filter(|d| *d != day);
    Candidate {
        id: format!("m-{}", lead.id),
        day,
        day_end,
        title: lead.title.clone(),
        contribution: ordered
            .iter()
            .map(|c| c.contribution.trim())
            .filter(|c| !c.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
        outcomes,
        uncertainties,
        confidence: parts.iter().map(|c| c.confidence).fold(1.0, f64::min),
        evidence_level: parts.iter().map(|c| c.evidence_level).max().unwrap_or(1),
        session_ids,
        agents,
        pr_links: unique_pr_links(ordered.iter().flat_map(|c| &c.pr_links)),
        repo: ordered.iter().find_map(|c| c.repo.clone()),
        model: lead.model.clone(),
        status: "pending".into(),
        related: None,
        created_at: chrono::Local::now().to_rfc3339(),
    }
}

pub fn link_related(store: &Store) -> Result<()> {
    let dismissed = store.dismissed_links()?;
    let pending = store.candidates_by_status("pending")?;
    let journal = store.journal_since(&calendar::offset(&today(), -EVAL_WINDOW_DAYS))?;
    let cited = unique_by(
        pending
            .iter()
            .flat_map(|candidate| candidate.session_ids.iter().cloned()),
        Clone::clone,
    );
    let sessions = store.sessions_by_ids(&cited)?;
    let sessions_by_id = session_index(&sessions);
    let sides: Vec<related::MatchFacts> = pending
        .iter()
        .map(|candidate| related::MatchFacts::build_indexed(candidate, &sessions_by_id))
        .collect();

    for (i, c) in pending.iter().enumerate() {
        let journaled = journal
            .iter()
            .find(|e| e.session_ids.iter().any(|s| c.session_ids.contains(s)))
            .map(|e| RelatedLink {
                kind: RelatedKind::Journaled,
                target_id: e.id.clone(),
                target_title: e.title.clone(),
                target_day: e.day.clone(),
                pair_key: related::pair_key(&c.session_ids, &e.session_ids),
            });
        let link = journaled
            .or_else(|| {
                related::best_match(&sides[i], &sides).map(|(m, _)| RelatedLink {
                    kind: RelatedKind::Continuation,
                    target_id: m.id.clone(),
                    target_title: m.title.clone(),
                    target_day: m.day.clone(),
                    pair_key: related::pair_key(&c.session_ids, &m.session_ids),
                })
            })
            .filter(|link| !dismissed.contains(&link.pair_key));

        if link.as_ref() != c.related.as_ref() {
            store.set_candidate_related(&c.id, link.as_ref())?;
        }
    }
    Ok(())
}

// Prevent a slow rewrite from clobbering a user edit or status change.
pub fn rewrite_merged_in_background(
    app: AppHandle,
    stitched: Candidate,
    parts: Vec<(String, String)>,
) {
    std::thread::spawn(move || {
        let state = app.state::<AppState>();
        let Some(busy) = state.lifecycle.begin_rewrite() else {
            return;
        };
        let settings = state.store.lock().unwrap().settings();
        let Ok((title, contribution)) = evaluator::rewrite_merged(&settings, &busy, &parts) else {
            return;
        };
        {
            let store = state.store.lock().unwrap();
            let Ok(current) = store.candidate(&stitched.id) else {
                return;
            };
            if current.status != "pending"
                || current.title != stitched.title
                || current.contribution != stitched.contribution
            {
                return;
            }
            let _ = store.update_candidate_fields(
                &stitched.id,
                &title,
                &contribution,
                &current.outcomes,
            );
            let _ = link_related(&store);
        }
        let _ = app.emit("zr:refresh", ());
    });
}

fn ref_session_id(r: &str) -> Option<&str> {
    if let Some(rest) = r.strip_prefix("session:") {
        Some(rest)
    } else if let Some(rest) = r.strip_prefix("cmd:").or_else(|| r.strip_prefix("action:")) {
        rest.rsplit_once(':').map(|(sid, _)| sid)
    } else {
        None
    }
}

fn session_index(sessions: &[SessionFacts]) -> HashMap<&str, &SessionFacts> {
    sessions
        .iter()
        .map(|session| (session.session_id.as_str(), session))
        .collect()
}

fn prepare_achievement(
    mut achievement: Achievement,
    sessions: &HashMap<&str, &SessionFacts>,
) -> Option<Achievement> {
    let mut ids: Vec<String> = achievement
        .session_ids
        .iter()
        .map(|id| id.strip_prefix("session:").unwrap_or(id))
        .filter(|id| sessions.contains_key(*id))
        .map(String::from)
        .collect();
    if ids.is_empty() {
        ids = achievement
            .outcomes
            .iter()
            .flat_map(|outcome| &outcome.evidence_refs)
            .filter_map(|reference| ref_session_id(reference))
            .filter(|id| sessions.contains_key(*id))
            .map(String::from)
            .collect();
    }
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        return None;
    }

    let cited: Vec<&SessionFacts> = ids
        .iter()
        .filter_map(|id| sessions.get(id.as_str()).copied())
        .collect();
    achievement.session_ids = ids;
    for outcome in &mut achievement.outcomes {
        gitfacts::verify_outcome(outcome, &cited, &mut achievement.uncertainties);
    }
    Some(achievement)
}

pub fn prepare_achievements(
    achievements: Vec<Achievement>,
    sessions: &[SessionFacts],
) -> Vec<Achievement> {
    let sessions = session_index(sessions);
    achievements
        .into_iter()
        .filter_map(|achievement| prepare_achievement(achievement, &sessions))
        .collect()
}

fn build_candidates(
    day: &str,
    sessions: &[SessionFacts],
    achievements: Vec<Achievement>,
    model: Option<String>,
) -> Vec<Candidate> {
    let sessions_by_id = session_index(sessions);
    let mut out = Vec::new();
    for (i, achievement) in achievements.into_iter().enumerate() {
        let Some(a) = prepare_achievement(achievement, &sessions_by_id) else {
            continue;
        };
        let cited: Vec<&SessionFacts> = a
            .session_ids
            .iter()
            .filter_map(|id| sessions_by_id.get(id.as_str()).copied())
            .collect();
        let level = a
            .outcomes
            .iter()
            .map(|o| o.evidence_level)
            .max()
            .unwrap_or(1);
        let repo = cited.iter().find_map(|s| s.repo_root.clone());
        let pr_links = unique_pr_links(cited.iter().flat_map(|s| &s.pr_links));
        out.push(Candidate {
            id: format!(
                "c-{}-{}-{}",
                day,
                chrono::Local::now().format("%H%M%S%3f"),
                i
            ),
            day: day.to_string(),
            day_end: None,
            title: a.title,
            contribution: a.contribution,
            outcomes: a.outcomes,
            uncertainties: a.uncertainties,
            confidence: a.confidence.clamp(0.0, 1.0),
            evidence_level: level,
            session_ids: a.session_ids,
            agents: unique_agents(cited.iter().map(|s| s.agent)),
            pr_links,
            repo,
            model: model.clone(),
            status: "pending".into(),
            related: None,
            created_at: chrono::Local::now().to_rfc3339(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eval_window_covers_fifteen_calendar_days() {
        let min_day = calendar::offset("2026-07-24", -(EVAL_WINDOW_DAYS - 1));
        assert_eq!(min_day, "2026-07-10");
    }

    #[test]
    fn candidates_drop_unknown_sessions_and_verify_levels() {
        let sessions = vec![SessionFacts {
            session_id: "s1".into(),
            repo_root: None,
            commands: vec![CommandFact {
                id: "cmd:s1:0".into(),
                command: "npm test".into(),
                ok: true,
                kind: "test".into(),
                ts: None,
                via_delegate: false,
            }],
            files_changed: vec![FileChange {
                path: "/r/a.ts".into(),
                tool: "Edit".into(),
                count: 1,
                via_delegate: false,
            }],
            external_actions: vec![ExternalAction {
                id: "action:s1:0".into(),
                server: "claude_ai_Atlassian".into(),
                tool: "transitionJiraIssue".into(),
                ok: true,
                mutating: true,
                ts: None,
                via_delegate: false,
            }],
            prompts: vec!["p".into()],
            ..Default::default()
        }];
        let achievements = vec![
            Achievement {
                title: "Fixed a bug".into(),
                contribution: "…".into(),
                outcomes: vec![Outcome {
                    claim: "Tests pass".into(),
                    evidence_level: 4,
                    evidence_refs: vec!["cmd:s1:0".into()],
                    verified: false,
                }],
                uncertainties: vec![],
                confidence: 0.9,
                session_ids: vec!["s1".into()],
            },
            Achievement {
                title: "Ghost work".into(),
                contribution: "…".into(),
                outcomes: vec![],
                uncertainties: vec![],
                confidence: 0.9,
                session_ids: vec!["unknown".into()],
            },
            Achievement {
                title: "Ref-form ids".into(),
                contribution: "…".into(),
                outcomes: vec![],
                uncertainties: vec![],
                confidence: 0.8,
                session_ids: vec!["session:s1".into()],
            },
            Achievement {
                title: "Ids only in refs".into(),
                contribution: "…".into(),
                outcomes: vec![Outcome {
                    claim: "Tests pass".into(),
                    evidence_level: 3,
                    evidence_refs: vec!["cmd:s1:0".into()],
                    verified: false,
                }],
                uncertainties: vec![],
                confidence: 0.8,
                session_ids: vec!["unknown".into()],
            },
            Achievement {
                title: "Ids only in action refs".into(),
                contribution: "…".into(),
                outcomes: vec![Outcome {
                    claim: "Moved the ticket to done".into(),
                    evidence_level: 2,
                    evidence_refs: vec!["action:s1:0".into()],
                    verified: false,
                }],
                uncertainties: vec![],
                confidence: 0.8,
                session_ids: vec!["unknown".into()],
            },
        ];
        let cands = build_candidates(
            "2026-07-20",
            &sessions,
            achievements,
            Some("claude-opus-5".into()),
        );
        assert_eq!(cands.len(), 4);
        assert_eq!(cands[0].evidence_level, 3);
        assert!(!cands[0].outcomes[0].verified);
        assert!(!cands[0].uncertainties.is_empty());
        assert_eq!(cands[1].session_ids, vec!["s1".to_string()]);
        assert_eq!(cands[2].session_ids, vec!["s1".to_string()]);
        assert_eq!(cands[3].session_ids, vec!["s1".to_string()]);
        assert_eq!(cands[3].evidence_level, 2);
    }

    fn part(id: &str, day: &str, level: u8, title: &str, claim: &str) -> Candidate {
        Candidate {
            id: id.into(),
            day: day.into(),
            day_end: None,
            title: title.into(),
            contribution: format!("Did {title}."),
            outcomes: vec![Outcome {
                claim: claim.into(),
                evidence_level: level,
                evidence_refs: vec![format!("cmd:{id}:0")],
                verified: true,
            }],
            uncertainties: vec!["tests were not run".into()],
            confidence: 0.8,
            evidence_level: level,
            session_ids: vec![format!("s-{id}")],
            agents: vec![Agent::Claude],
            pr_links: vec![],
            repo: Some("/r/synapse".into()),
            model: None,
            status: "pending".into(),
            related: None,
            created_at: "2026-07-20T09:00:00+02:00".into(),
        }
    }

    #[test]
    fn merging_spans_days_and_keeps_every_verified_outcome() {
        let scoping = part("a", "2026-07-19", 1, "Scoped the checks", "Approach agreed");
        let building = part(
            "b",
            "2026-07-20",
            4,
            "Added the checks",
            "Committed the scanner",
        );

        let merged = merge_into_one(&[building.clone(), scoping.clone()]);

        assert_eq!(
            merged.day, "2026-07-19",
            "dated from where the work started"
        );
        assert_eq!(merged.day_end.as_deref(), Some("2026-07-20"));
        assert_eq!(
            merged.title, "Added the checks",
            "title from the best-evidenced part"
        );
        assert_eq!(merged.evidence_level, 4);
        assert_eq!(merged.outcomes.len(), 2);
        assert!(merged.outcomes.iter().all(|o| o.verified));
        assert_eq!(
            merged.uncertainties.len(),
            1,
            "identical uncertainties collapse"
        );
        assert_eq!(merged.session_ids, vec!["s-a".to_string(), "s-b".into()]);
        assert!(
            merged.contribution.starts_with("Did Scoped"),
            "read in day order"
        );
    }

    #[test]
    fn merging_within_one_day_leaves_day_end_unset() {
        let merged = merge_into_one(&[
            part("a", "2026-07-19", 2, "First", "One"),
            part("b", "2026-07-19", 3, "Second", "Two"),
        ]);
        assert_eq!(merged.day, "2026-07-19");
        assert_eq!(merged.day_end, None);
    }

    #[test]
    fn merging_drops_a_repeated_claim() {
        let merged = merge_into_one(&[
            part("a", "2026-07-19", 2, "First", "Scanner committed"),
            part("b", "2026-07-20", 2, "Second", "  scanner committed  "),
        ]);
        assert_eq!(merged.outcomes.len(), 1);
    }

    #[test]
    fn candidates_carry_deduplicated_pr_links_from_cited_sessions() {
        let pr = PrLink {
            number: 5159,
            url: "https://github.com/acme/widgets/pull/5159".into(),
            repository: "acme/widgets".into(),
            ts: None,
        };
        let sessions = vec![
            SessionFacts {
                session_id: "s1".into(),
                pr_links: vec![pr.clone()],
                ..Default::default()
            },
            SessionFacts {
                session_id: "s2".into(),
                pr_links: vec![pr.clone()],
                ..Default::default()
            },
        ];
        let achievements = vec![Achievement {
            title: "Opened the fix for review".into(),
            contribution: "Prepared the change for review.".into(),
            outcomes: vec![],
            uncertainties: vec![],
            confidence: 0.9,
            session_ids: vec!["s1".into(), "s2".into()],
        }];

        let candidates = build_candidates("2026-07-20", &sessions, achievements, None);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].pr_links, vec![pr]);
    }

    #[test]
    fn candidates_and_merges_record_which_agents_did_the_work() {
        let sessions = vec![
            SessionFacts {
                session_id: "s1".into(),
                agent: Agent::Codex,
                ..Default::default()
            },
            SessionFacts {
                session_id: "s2".into(),
                agent: Agent::Claude,
                ..Default::default()
            },
        ];
        let achievements = vec![Achievement {
            title: "Wired the release pipeline".into(),
            contribution: "…".into(),
            outcomes: vec![],
            uncertainties: vec![],
            confidence: 0.9,
            session_ids: vec!["s1".into(), "s2".into()],
        }];
        let candidates = build_candidates("2026-07-20", &sessions, achievements, None);
        assert_eq!(candidates[0].agents, vec![Agent::Claude, Agent::Codex]);

        let mut codex_only = part("a", "2026-07-19", 2, "Scoped", "One");
        codex_only.agents = vec![Agent::Codex];
        let merged = merge_into_one(&[part("b", "2026-07-20", 3, "Built", "Two"), codex_only]);
        assert_eq!(merged.agents, vec![Agent::Claude, Agent::Codex]);
    }
}
