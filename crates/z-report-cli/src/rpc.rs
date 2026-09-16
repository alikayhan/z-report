use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use z_report_core::{
    engine::{self, Engine},
    evaluator, export,
    models::*,
    pipeline,
};

#[derive(Deserialize)]
pub struct Request {
    pub protocol: u32,
    #[serde(flatten)]
    pub action: Action,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    Overview,
    Scan,
    ReadStart {
        owner: String,
        #[serde(default)]
        automatic: bool,
        #[serde(default)]
        interactive: bool,
    },
    ReadStatus {
        id: Option<String>,
    },
    ReadHeartbeat {
        id: String,
        owner: String,
    },
    ReadCancel {
        id: String,
    },
    Candidates {
        status: String,
    },
    Evidence {
        ids: Vec<String>,
    },
    EditCandidate {
        id: String,
        revision: i64,
        title: String,
        contribution: String,
        outcomes: Vec<Outcome>,
    },
    ApproveCandidate {
        id: String,
        revision: i64,
        #[serde(default)]
        edited: bool,
    },
    DiscardCandidate {
        id: String,
        revision: i64,
    },
    RestoreCandidate {
        id: String,
        revision: i64,
    },
    MergeCandidates {
        ids: Vec<String>,
        revisions: Vec<i64>,
    },
    DismissRelated {
        id: String,
    },
    Journal {
        from: String,
        to: String,
        query: Option<String>,
    },
    Export {
        from: String,
        to: String,
    },
    SaveExport {
        from: String,
        to: String,
        path: String,
    },
    Settings,
    UpdateSettings {
        patch: Value,
    },
    EvalRuns,
}

fn dates(from: &str, to: &str) -> Result<()> {
    chrono::NaiveDate::parse_from_str(from, "%Y-%m-%d")?;
    chrono::NaiveDate::parse_from_str(to, "%Y-%m-%d")?;
    anyhow::ensure!(from <= to, "Start date must precede end date");
    Ok(())
}

pub fn dispatch(engine: &Engine, request: Request) -> Result<Value> {
    match request.action {
        Action::ReadStart {
            owner,
            automatic,
            interactive,
        } => return crate::start(engine, &owner, automatic, interactive),
        Action::Scan => return Ok(serde_json::to_value(engine::scan(engine)?)?),
        _ => {}
    }
    let store = engine.store.lock().unwrap();
    let value = match request.action {
        Action::Overview => {
            store.recover_reads()?;
            let (pending, sessions) = store.overview_counts()?;
            let settings = store.settings();
            // Availability discovery is local and never launches a model.
            let evaluators: Vec<_> = evaluator::discover(&settings)
                .into_iter()
                .map(|info| json!({"agent": info.agent, "found": info.found, "model": info.model}))
                .collect();
            json!({"pending":pending,"session_count":sessions,"today":engine::today(),"coverage_days":engine::EVAL_WINDOW_DAYS,
                "last_successful_read_at":store.kv_get("last_successful_read_at"),"last_scan_at":store.kv_get("last_scan_at"),"initialized":store.kv_get("read_initialized").as_deref()==Some("true"),"read":store.read_cycle(None)?,"evaluators":evaluators,"settings":settings})
        }
        Action::ReadStatus { id } => {
            store.recover_reads()?;
            json!(store.read_cycle(id.as_deref())?)
        }
        Action::ReadHeartbeat { id, owner } => {
            store.heartbeat_read(&id, &owner)?;
            json!(store.read_cycle(Some(&id))?)
        }
        Action::ReadCancel { id } => {
            store.cancel_read(&id)?;
            json!(store.read_cycle(Some(&id))?)
        }
        Action::Candidates { status } => {
            anyhow::ensure!(
                ["pending", "discarded", "approved"].contains(&status.as_str()),
                "Invalid candidate status"
            );
            json!(store.candidates_by_status(&status)?)
        }
        Action::Evidence { ids } => {
            anyhow::ensure!(ids.len() <= 100, "Too many sessions");
            json!(store.sessions_by_ids(&ids)?)
        }
        Action::EditCandidate {
            id,
            revision,
            title,
            contribution,
            outcomes,
        } => {
            store.edit_candidate(&id, Some(revision), &title, &contribution, &outcomes)?;
            pipeline::link_related(&store)?;
            json!(store.candidate(&id)?)
        }
        Action::ApproveCandidate {
            id,
            revision,
            edited,
        } => {
            let id = store.approve(&id, Some(revision), edited)?;
            pipeline::link_related(&store)?;
            json!({"id":id})
        }
        Action::DiscardCandidate { id, revision } => {
            store.transition(&id, Some(revision), "discarded")?;
            pipeline::link_related(&store)?;
            json!(null)
        }
        Action::RestoreCandidate { id, revision } => {
            store.transition(&id, Some(revision), "pending")?;
            pipeline::link_related(&store)?;
            json!(null)
        }
        Action::MergeCandidates { ids, revisions } => {
            let c = store.merge(&ids, Some(&revisions))?;
            pipeline::link_related(&store)?;
            json!(c)
        }
        Action::DismissRelated { id } => {
            if let Some(link) = store.candidate(&id)?.related {
                store.dismiss_link(&link.pair_key)?;
            }
            store.set_candidate_related(&id, None)?;
            json!(null)
        }
        Action::Journal { from, to, query } => {
            dates(&from, &to)?;
            json!(store.journal_range(&from, &to, query.as_deref().filter(|q| !q.is_empty()))?)
        }
        Action::Export { from, to } => {
            dates(&from, &to)?;
            let entries = store.journal_range(&from, &to, None)?;
            json!({"markdown":export::to_markdown(&entries,&from,&to),"entries":entries})
        }
        Action::SaveExport { from, to, path } => {
            dates(&from, &to)?;
            let path = std::path::PathBuf::from(path);
            anyhow::ensure!(
                path.is_absolute() && path.extension().is_some_and(|s| s == "md"),
                "Choose an absolute .md file path"
            );
            let entries = store.journal_range(&from, &to, None)?;
            use std::io::Write;
            // Existing files require another name; exporting cannot erase work.
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .context("Cannot create export file (it may already exist)")?;
            file.write_all(export::to_markdown(&entries, &from, &to).as_bytes())?;
            json!({"path":path})
        }
        Action::Settings => json!(store.settings()),
        Action::UpdateSettings { patch } => {
            let mut value = serde_json::to_value(store.settings())?;
            let values = patch.as_object().context("Settings must be an object")?;
            for (key, v) in values {
                anyhow::ensure!(value.get(key).is_some(), "Unknown setting: {key}");
                value[key] = v.clone();
            }
            let settings: Settings = serde_json::from_value(value)?;
            store.save_settings(&settings)?;
            json!(settings)
        }
        Action::EvalRuns => json!(store.recent_eval_runs(50)?),
        Action::ReadStart { .. } | Action::Scan => unreachable!(),
    };
    Ok(value)
}
