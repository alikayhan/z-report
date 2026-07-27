use crate::models::*;
use crate::store::Store;
use crate::{evaluator, gitfacts, ingest, related};
use anyhow::Result;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;

const EVAL_WINDOW_DAYS: i64 = 15;
const EVIDENCE_HORIZON_DAYS: i64 = 90;
const EVIDENCE_PRUNE_SLACK_DAYS: i64 = 7;

pub struct AppState {
    pub store: Mutex<Store>,
    pub evaluating: AtomicBool,
    pub pinned: AtomicBool,
    /// None until the startup billing-mode probe finishes; see evaluator::is_metered.
    pub metered: Mutex<Option<bool>>,
}

pub fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

fn day_offset(day: &str, days: i64) -> String {
    chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .map(|d| (d + chrono::Duration::days(days)).format("%Y-%m-%d").to_string())
        .unwrap_or_else(|_| day.to_string())
}

/// Discover transcripts, parse changed ones, correlate with Git, upsert facts.
pub fn scan(app: &AppHandle) -> Result<u32> {
    let state = app.state::<AppState>();
    let files = ingest::discover();
    let live_ids: Vec<String> = files.iter().map(|f| f.session_id.clone()).collect();
    let (settings, known): (Settings, Vec<(String, Option<(String, String)>)>) = {
        let store = state.store.lock().unwrap();
        let settings = store.settings();
        let known = files
            .iter()
            .map(|f| (f.session_id.clone(), store.session_hash(&f.session_id)))
            .collect();
        (settings, known)
    };

    let stale_before = chrono::Local::now().timestamp() - EVIDENCE_HORIZON_DAYS * 86_400;
    let mut updated = 0u32;
    let removed;
    for file in &files {
        if (file.mtime as i64) < stale_before {
            continue;
        }
        let prior = known
            .iter()
            .find(|(id, _)| *id == file.session_id)
            .and_then(|(_, h)| h.clone());
        if prior.as_ref().map(|(content, _)| content.as_str()) == Some(file.content_hash.as_str()) {
            continue;
        }
        let Ok(mut facts) =
            ingest::parse_transcript(&file.path, &file.session_id, settings.retain_prompts)
        else {
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
            let min_day = day_offset(&today(), -(settings.retention_days as i64));
            store.prune_candidates_older_than(&min_day)?;
        }
        let evidence_min_day =
            day_offset(&today(), -(EVIDENCE_HORIZON_DAYS + EVIDENCE_PRUNE_SLACK_DAYS));
        removed = store.prune_sessions_older_than(&evidence_min_day)?
            + store.delete_sessions_missing_from(&live_ids)?;
        store.kv_set("last_scan_at", &chrono::Local::now().to_rfc3339())?;
    }
    if updated > 0 || removed > 0 {
        let _ = app.emit("zr:refresh", ());
    }
    Ok(updated)
}

/// Run the Z-read/X-read: evaluate all sessions whose content changed since
/// their last evaluation, one evaluator run per day, then queue candidates.
pub fn evaluate_pending(app: &AppHandle, kind: &str) -> Result<usize> {
    let state = app.state::<AppState>();
    if state
        .evaluating
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Ok(0);
    }
    let _ = app.emit("zr:evaluating", true);
    let result = evaluate_pending_inner(app, kind);
    state.evaluating.store(false, Ordering::SeqCst);
    let _ = app.emit("zr:evaluating", false);
    let _ = app.emit("zr:refresh", ());

    match &result {
        Ok(count) if *count > 0 => {
            let noun = if *count == 1 { "achievement" } else { "achievements" };
            let title = if kind == "zread" { "Today's Z-read is ready" } else { "X-read complete" };
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

fn evaluate_pending_inner(app: &AppHandle, kind: &str) -> Result<usize> {
    scan(app)?;
    let state = app.state::<AppState>();
    let (settings, pending) = {
        let store = state.store.lock().unwrap();
        let today = today();
        let min_day = day_offset(&today, -(EVAL_WINDOW_DAYS - 1));
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

        match evaluator::evaluate_day(&settings, &day, &substantial) {
            Ok(result) => {
                let candidates = build_candidates(&day, &substantial, result.achievements, result.model.clone());
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

/// Folds several candidates into one achievement spanning their days. Outcomes
/// carry across untouched so verified evidence survives the merge; only the
/// prose is re-derived, and the title comes from the best-evidenced part.
pub fn merge_into_one(parts: &[Candidate]) -> Candidate {
    let lead = parts
        .iter()
        .min_by(|a, b| b.evidence_level.cmp(&a.evidence_level).then(a.day.cmp(&b.day)))
        .expect("merge needs at least one candidate");
    // Everything the card carries reads in the order the work happened, not
    // the order the developer happened to tick the boxes.
    let ordered = {
        let mut v: Vec<&Candidate> = parts.iter().collect();
        v.sort_by(|a, b| a.day.cmp(&b.day).then(a.created_at.cmp(&b.created_at)));
        v
    };
    let mut outcomes: Vec<Outcome> = Vec::new();
    for o in ordered.iter().flat_map(|c| &c.outcomes) {
        let key = o.claim.trim().to_lowercase();
        if !outcomes.iter().any(|k| k.claim.trim().to_lowercase() == key) {
            outcomes.push(o.clone());
        }
    }
    let mut uncertainties: Vec<String> = Vec::new();
    for u in ordered.iter().flat_map(|c| &c.uncertainties) {
        if !uncertainties.iter().any(|k| k.trim() == u.trim()) {
            uncertainties.push(u.clone());
        }
    }
    let mut session_ids: Vec<String> = Vec::new();
    for s in ordered.iter().flat_map(|c| &c.session_ids) {
        if !session_ids.contains(s) {
            session_ids.push(s.clone());
        }
    }
    let day = ordered.first().map(|c| c.day.clone()).unwrap_or_default();
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
        pr_links: unique_pr_links(ordered.iter().flat_map(|c| &c.pr_links)),
        repo: ordered.iter().find_map(|c| c.repo.clone()),
        model: lead.model.clone(),
        status: "pending".into(),
        related: None,
        created_at: chrono::Local::now().to_rfc3339(),
    }
}

/// Attaches at most one suggestion to every pending candidate: work already
/// approved into the journal, or an earlier card on the same thread. Scoring
/// only — it writes the suggestion, never the achievement it points at.
pub fn link_related(store: &Store) -> Result<()> {
    let dismissed = store.dismissed_links()?;
    let pending = store.candidates_by_status("pending")?;
    let journal = store.journal_since(&day_offset(&today(), -EVAL_WINDOW_DAYS))?;
    let cited: Vec<String> = pending
        .iter()
        .flat_map(|c| c.session_ids.iter().cloned())
        .collect();
    let sessions = store.sessions_by_ids(&cited)?;
    let sides: Vec<related::MatchFacts> = pending
        .iter()
        .map(|c| related::MatchFacts::build(c, &sessions))
        .collect();

    for (i, c) in pending.iter().enumerate() {
        let journaled = journal
            .iter()
            .find(|e| e.session_ids.iter().any(|s| c.session_ids.contains(s)))
            .map(|e| {
                (
                    RelatedLink {
                        kind: "journaled".into(),
                        target_id: e.id.clone(),
                        target_title: e.title.clone(),
                        target_day: e.day.clone(),
                        score: 1.0,
                    },
                    e.session_ids.clone(),
                )
            });
        let link = journaled
            .or_else(|| {
                related::best_match(&sides[i], &sides).map(|(m, score)| {
                    (
                        RelatedLink {
                            kind: "continuation".into(),
                            target_id: m.id.clone(),
                            target_title: m.title.clone(),
                            target_day: m.day.clone(),
                            score: (score * 100.0).round() / 100.0,
                        },
                        m.session_ids.clone(),
                    )
                })
            })
            .filter(|(_, target)| !dismissed.contains(&related::pair_key(&c.session_ids, target)))
            .map(|(link, _)| link);

        if link.as_ref() != c.related.as_ref() {
            store.set_candidate_related(&c.id, link.as_ref())?;
        }
    }
    Ok(())
}

fn ref_session_id(r: &str) -> Option<&str> {
    if let Some(rest) = r.strip_prefix("session:") {
        Some(rest)
    } else if let Some(rest) = r
        .strip_prefix("cmd:")
        .or_else(|| r.strip_prefix("action:"))
    {
        rest.rsplit_once(':').map(|(sid, _)| sid)
    } else {
        None
    }
}

fn build_candidates(
    day: &str,
    sessions: &[SessionFacts],
    achievements: Vec<Achievement>,
    model: Option<String>,
) -> Vec<Candidate> {
    let mut out = Vec::new();
    for (i, mut a) in achievements.into_iter().enumerate() {
        // The model sometimes fills session_ids with "session:<id>" refs, or
        // cites sessions only via outcome evidence_refs; an exact-id match
        // here silently discarded whole achievements.
        let mut ids: Vec<String> = a
            .session_ids
            .iter()
            .map(|id| id.strip_prefix("session:").unwrap_or(id))
            .filter(|id| sessions.iter().any(|s| s.session_id == *id))
            .map(String::from)
            .collect();
        if ids.is_empty() {
            ids = a
                .outcomes
                .iter()
                .flat_map(|o| o.evidence_refs.iter())
                .filter_map(|r| ref_session_id(r))
                .filter(|id| sessions.iter().any(|s| s.session_id == *id))
                .map(String::from)
                .collect();
        }
        ids.sort();
        ids.dedup();
        a.session_ids = ids;
        if a.session_ids.is_empty() {
            continue;
        }
        let cited: Vec<&SessionFacts> = sessions
            .iter()
            .filter(|s| a.session_ids.contains(&s.session_id))
            .collect();
        let mut uncertainties = a.uncertainties.clone();
        for outcome in a.outcomes.iter_mut() {
            gitfacts::verify_outcome(outcome, &cited, &mut uncertainties);
        }
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
            uncertainties,
            confidence: a.confidence.clamp(0.0, 1.0),
            evidence_level: level,
            session_ids: a.session_ids,
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
        let min_day = day_offset("2026-07-24", -(EVAL_WINDOW_DAYS - 1));
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
        let cands = build_candidates("2026-07-20", &sessions, achievements, Some("claude-opus-5".into()));
        assert_eq!(cands.len(), 4);
        // Claimed commit-level (4) but only a passing test ref: downgraded to 3, flagged.
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
        let building = part("b", "2026-07-20", 4, "Added the checks", "Committed the scanner");

        let merged = merge_into_one(&[building.clone(), scoping.clone()]);

        assert_eq!(merged.day, "2026-07-19", "dated from where the work started");
        assert_eq!(merged.day_end.as_deref(), Some("2026-07-20"));
        assert_eq!(merged.title, "Added the checks", "title from the best-evidenced part");
        assert_eq!(merged.evidence_level, 4);
        assert_eq!(merged.outcomes.len(), 2);
        assert!(merged.outcomes.iter().all(|o| o.verified));
        assert_eq!(merged.uncertainties.len(), 1, "identical uncertainties collapse");
        assert_eq!(merged.session_ids, vec!["s-a".to_string(), "s-b".into()]);
        assert!(merged.contribution.starts_with("Did Scoped"), "read in day order");
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
}
