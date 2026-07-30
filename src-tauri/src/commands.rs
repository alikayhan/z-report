use crate::models::*;
use crate::pipeline::{self, AppState};
use crate::{evaluator, export, store};
use serde::Serialize;
use std::sync::atomic::Ordering;
use tauri::{AppHandle, Manager, State};

type CmdResult<T> = Result<T, String>;

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

#[derive(Serialize)]
pub struct Overview {
    pub pending: i64,
    pub journal_count: i64,
    pub session_count: i64,
    pub evaluating: bool,
    pub last_scan_at: Option<String>,
    pub last_zread_day: Option<String>,
    pub zread_time: String,
    pub today: String,
    pub model: String,
    pub claude_found: bool,
    pub metered: bool,
}

#[tauri::command]
pub fn overview(state: State<AppState>) -> CmdResult<Overview> {
    let store = state.store.lock().unwrap();
    let (pending, journal_count, session_count) = store.counts().map_err(err)?;
    let settings = store.settings();
    Ok(Overview {
        pending,
        journal_count,
        session_count,
        evaluating: state.evaluating.load(Ordering::SeqCst),
        last_scan_at: store.kv_get("last_scan_at"),
        last_zread_day: store.kv_get("last_zread_day"),
        zread_time: settings.zread_time.clone(),
        today: pipeline::today(),
        model: evaluator::EVAL_MODEL.into(),
        claude_found: evaluator::find_claude(&settings).is_ok(),
        metered: (*state.metered.lock().unwrap()).unwrap_or(false),
    })
}

#[tauri::command]
pub fn scan_now(app: AppHandle) -> CmdResult<u32> {
    pipeline::scan(&app).map_err(err)
}

#[tauri::command]
pub fn run_xread(app: AppHandle) -> CmdResult<()> {
    std::thread::spawn(move || {
        let _ = pipeline::evaluate_pending(&app, "xread");
    });
    Ok(())
}

#[tauri::command]
pub fn candidates(state: State<AppState>, status: String) -> CmdResult<Vec<Candidate>> {
    state
        .store
        .lock()
        .unwrap()
        .candidates_by_status(&status)
        .map_err(err)
}

#[tauri::command]
pub fn update_candidate(
    state: State<AppState>,
    id: String,
    title: String,
    contribution: String,
    outcomes: Vec<Outcome>,
) -> CmdResult<()> {
    state
        .store
        .lock()
        .unwrap()
        .update_candidate_fields(&id, &title, &contribution, &outcomes)
        .map_err(err)
}

/// Suggestions point at other pending cards, so any status change can strand
/// one. Re-deriving is pure scoring over what is already in SQLite.
fn relink(store: &store::Store) {
    let _ = pipeline::link_related(store);
}

#[tauri::command]
pub fn approve_candidate(state: State<AppState>, id: String, edited: bool) -> CmdResult<()> {
    let store = state.store.lock().unwrap();
    let c = store.candidate(&id).map_err(err)?;
    let entry = JournalEntry {
        id: format!("j-{}", c.id),
        day: c.day.clone(),
        day_end: c.day_end.clone(),
        title: c.title.clone(),
        contribution: c.contribution.clone(),
        outcomes: c.outcomes.clone(),
        evidence_level: c.evidence_level,
        session_ids: c.session_ids.clone(),
        pr_links: c.pr_links.clone(),
        repo: c.repo.clone(),
        model: c.model.clone(),
        approved_at: chrono::Local::now().to_rfc3339(),
        edited,
    };
    store.insert_journal(&entry).map_err(err)?;
    store
        .set_candidate_status(&id, "approved", None)
        .map_err(err)?;
    relink(&store);
    Ok(())
}

#[tauri::command]
pub fn discard_candidate(state: State<AppState>, id: String) -> CmdResult<()> {
    let store = state.store.lock().unwrap();
    store
        .set_candidate_status(&id, "discarded", None)
        .map_err(err)?;
    relink(&store);
    Ok(())
}

#[tauri::command]
pub fn restore_candidate(state: State<AppState>, id: String) -> CmdResult<()> {
    let store = state.store.lock().unwrap();
    store
        .set_candidate_status(&id, "pending", None)
        .map_err(err)?;
    relink(&store);
    Ok(())
}

#[tauri::command]
pub fn merge_candidates(app: AppHandle, state: State<AppState>, ids: Vec<String>) -> CmdResult<String> {
    if ids.len() < 2 {
        return Err("select at least two candidates to merge".into());
    }
    let (new, parts) = {
        let store = state.store.lock().unwrap();
        let mut merged: Vec<Candidate> = Vec::new();
        for id in &ids {
            merged.push(store.candidate(id).map_err(err)?);
        }
        // A suggestion can outlive its target being approved or discarded.
        if merged.iter().any(|c| c.status != "pending") {
            return Err("only cards still in the review queue can be merged".into());
        }
        let new = pipeline::merge_into_one(&merged);
        store.insert_candidate(&new).map_err(err)?;
        for id in &ids {
            store
                .set_candidate_status(id, "merged", Some(&new.id))
                .map_err(err)?;
        }
        let parts: Vec<(String, String)> = merged
            .iter()
            .map(|c| (c.title.clone(), c.contribution.clone()))
            .collect();
        relink(&store);
        (new, parts)
    };
    let new_id = new.id.clone();
    pipeline::rewrite_merged_in_background(app, new, parts);
    Ok(new_id)
}

#[tauri::command]
pub fn dismiss_related(state: State<AppState>, id: String) -> CmdResult<()> {
    let store = state.store.lock().unwrap();
    let c = store.candidate(&id).map_err(err)?;
    let Some(link) = &c.related else {
        return Ok(());
    };
    // Links stored before pair_key existed deserialize empty; the next relink
    // rewrites them, so there is nothing durable to record yet.
    if !link.pair_key.is_empty() {
        store.dismiss_link(&link.pair_key).map_err(err)?;
    }
    store.set_candidate_related(&id, None).map_err(err)
}

#[tauri::command]
pub fn journal(
    state: State<AppState>,
    from: String,
    to: String,
    query: Option<String>,
) -> CmdResult<Vec<JournalEntry>> {
    state
        .store
        .lock()
        .unwrap()
        .journal_range(&from, &to, query.as_deref().filter(|q| !q.is_empty()))
        .map_err(err)
}

#[tauri::command]
pub fn confirm_impact(state: State<AppState>, id: String, note: String) -> CmdResult<()> {
    let note = if note.trim().is_empty() {
        "Impact confirmed by developer".to_string()
    } else {
        note
    };
    state
        .store
        .lock()
        .unwrap()
        .confirm_impact(&id, &note)
        .map_err(err)
}

#[tauri::command]
pub fn delete_journal_entry(state: State<AppState>, id: String) -> CmdResult<()> {
    state
        .store
        .lock()
        .unwrap()
        .delete_journal_entry(&id)
        .map_err(err)
}

#[tauri::command]
pub fn export_markdown(state: State<AppState>, from: String, to: String) -> CmdResult<String> {
    let entries = state
        .store
        .lock()
        .unwrap()
        .journal_range(&from, &to, None)
        .map_err(err)?;
    Ok(export::to_markdown(&entries, &from, &to))
}

#[tauri::command]
pub fn write_file(path: String, content: String) -> CmdResult<()> {
    std::fs::write(&path, content).map_err(err)
}

#[tauri::command]
pub fn get_settings(state: State<AppState>) -> CmdResult<Settings> {
    Ok(state.store.lock().unwrap().settings())
}

#[tauri::command]
pub fn set_settings(state: State<AppState>, settings: Settings) -> CmdResult<()> {
    state
        .store
        .lock()
        .unwrap()
        .save_settings(&settings)
        .map_err(err)
}

#[tauri::command]
pub fn eval_runs(state: State<AppState>) -> CmdResult<Vec<EvalRun>> {
    state.store.lock().unwrap().recent_eval_runs(20).map_err(err)
}

#[tauri::command]
pub fn set_pinned(state: State<AppState>, pinned: bool) -> CmdResult<()> {
    state.pinned.store(pinned, Ordering::SeqCst);
    Ok(())
}

#[tauri::command]
pub fn hide_window(app: AppHandle) -> CmdResult<()> {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.hide();
    }
    Ok(())
}

#[tauri::command]
pub fn delete_all_data(state: State<AppState>) -> CmdResult<()> {
    state.store.lock().unwrap().wipe_all().map_err(err)?;
    let eval_dir = store::data_dir().join("eval");
    if eval_dir.exists() {
        std::fs::remove_dir_all(&eval_dir).map_err(err)?;
    }
    Ok(())
}
