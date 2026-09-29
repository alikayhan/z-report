use crate::models::*;
use crate::pipeline::{self, AppState};
use crate::updater::{self, UpdateInfo, UpdaterState};
use crate::{evaluator, export, store};
use serde::Serialize;
use std::sync::atomic::Ordering;
use tauri::{AppHandle, Manager, State};

type CmdResult<T> = Result<T, String>;

pub(crate) fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

#[derive(Serialize)]
pub struct Overview {
    pub pending: i64,
    pub session_count: i64,
    pub evaluating: bool,
    pub last_scan_at: Option<String>,
    pub zread_time: String,
    pub today: String,
    pub evaluators: Vec<evaluator::EvaluatorInfo>,
    pub metered: bool,
    pub app_version: String,
    pub update: Option<UpdateInfo>,
    pub update_ready: bool,
}

#[derive(Serialize)]
pub struct ExportData {
    pub markdown: String,
    pub entries: Vec<JournalEntry>,
}

#[tauri::command]
pub fn overview(app: AppHandle, state: State<AppState>) -> CmdResult<Overview> {
    let (pending, session_count, settings, last_scan_at, shared_read) = {
        let store = state.store.lock().unwrap();
        store.recover_reads().map_err(err)?;
        let (pending, session_count) = store.overview_counts().map_err(err)?;
        (
            pending,
            session_count,
            store.settings(),
            store.kv_get("last_scan_at"),
            store
                .read_cycle(None)
                .map_err(err)?
                .is_some_and(|r| matches!(r.status.as_str(), "queued" | "running")),
        )
    };
    let availability = state.availability.lock().unwrap().clone();
    Ok(Overview {
        pending,
        session_count,
        evaluating: state.lifecycle.evaluating() || shared_read,
        last_scan_at,
        zread_time: settings.zread_time,
        today: pipeline::today(),
        evaluators: availability.evaluators,
        metered: availability.metered,
        app_version: app.package_info().version.to_string(),
        update: app.state::<UpdaterState>().info.lock().unwrap().clone(),
        update_ready: app.state::<UpdaterState>().ready.load(Ordering::SeqCst),
    })
}

#[tauri::command]
pub fn run_xread(app: AppHandle) {
    pipeline::spawn_evaluation(app, "xread");
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
    revision: Option<i64>,
) -> CmdResult<()> {
    let store = state.store.lock().unwrap();
    store
        .edit_candidate(&id, revision, &title, &contribution, &outcomes)
        .map_err(err)?;
    relink(&store);
    Ok(())
}

fn relink(store: &store::Store) {
    let _ = pipeline::link_related(store);
}

#[tauri::command]
pub fn approve_candidate(
    state: State<AppState>,
    id: String,
    edited: bool,
    revision: Option<i64>,
) -> CmdResult<()> {
    let store = state.store.lock().unwrap();
    store.approve(&id, revision, edited).map_err(err)?;
    relink(&store);
    Ok(())
}

fn transition_candidate(
    state: &AppState,
    id: &str,
    status: &str,
    revision: Option<i64>,
) -> CmdResult<()> {
    let store = state.store.lock().unwrap();
    store.transition(id, revision, status).map_err(err)?;
    relink(&store);
    Ok(())
}

#[tauri::command]
pub fn discard_candidate(
    state: State<AppState>,
    id: String,
    revision: Option<i64>,
) -> CmdResult<()> {
    transition_candidate(&state, &id, "discarded", revision)
}

#[tauri::command]
pub fn restore_candidate(
    state: State<AppState>,
    id: String,
    revision: Option<i64>,
) -> CmdResult<()> {
    transition_candidate(&state, &id, "pending", revision)
}

#[tauri::command]
pub fn merge_candidates(
    app: AppHandle,
    state: State<AppState>,
    ids: Vec<String>,
    revisions: Option<Vec<i64>>,
) -> CmdResult<String> {
    if ids.len() < 2 {
        return Err("select at least two candidates to merge".into());
    }
    let (new, parts) = {
        let store = state.store.lock().unwrap();
        let merged: Vec<Candidate> = ids
            .iter()
            .map(|id| store.candidate(id))
            .collect::<Result<_, _>>()
            .map_err(err)?;
        if merged.iter().any(|c| c.status != "pending") {
            return Err("only cards still in the review queue can be merged".into());
        }
        let new = store.merge(&ids, revisions.as_deref()).map_err(err)?;
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
    // Legacy links without a pair key cannot persist a dismissal safely.
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
    let store = state.store.lock().unwrap();
    store.delete_journal_entry(&id).map_err(err)?;
    relink(&store);
    Ok(())
}

#[tauri::command]
pub fn export_data(state: State<AppState>, from: String, to: String) -> CmdResult<ExportData> {
    let entries = state
        .store
        .lock()
        .unwrap()
        .journal_range(&from, &to, None)
        .map_err(err)?;
    Ok(ExportData {
        markdown: export::to_markdown(&entries, &from, &to),
        entries,
    })
}

#[tauri::command]
pub fn write_file(path: String, content: String) -> CmdResult<()> {
    std::fs::write(&path, content).map_err(err)
}

#[tauri::command]
pub fn get_settings(state: State<AppState>) -> Settings {
    state.store.lock().unwrap().settings()
}

#[tauri::command]
pub fn set_settings(state: State<AppState>, settings: Settings) -> CmdResult<()> {
    let path_changed = {
        let store = state.store.lock().unwrap();
        let path_changed = store.settings().claude_path != settings.claude_path;
        store.save_settings(&settings).map_err(err)?;
        path_changed
    };
    if path_changed {
        if let Some(busy) = state.lifecycle.begin_probe() {
            let availability = evaluator::probe(&settings, &busy);
            *state.availability.lock().unwrap() = availability;
        }
    }
    Ok(())
}

#[tauri::command]
pub fn eval_runs(state: State<AppState>) -> CmdResult<Vec<EvalRun>> {
    state
        .store
        .lock()
        .unwrap()
        .recent_eval_runs(20)
        .map_err(err)
}

#[tauri::command]
pub async fn install_update(app: AppHandle) -> CmdResult<()> {
    updater::install(app).await
}

#[tauri::command]
pub fn restart_app(app: AppHandle) {
    app.restart();
}

#[tauri::command]
pub fn delete_all_data(state: State<AppState>) -> CmdResult<()> {
    let store = state.store.lock().unwrap();
    let _lock = z_report_core::engine::lock(&store, "read").map_err(err)?;
    store.wipe_all().map_err(err)?;
    let eval_dir = store::data_dir().join("eval");
    if eval_dir.exists() {
        std::fs::remove_dir_all(&eval_dir).map_err(err)?;
    }
    Ok(())
}

#[tauri::command]
pub fn cancel_xread(state: State<AppState>) -> CmdResult<()> {
    state.lifecycle.cancel();
    let store = state.store.lock().unwrap();
    if let Some(read) = store.read_cycle(None).map_err(err)? {
        store.cancel_read(&read.id).map_err(err)?;
    }
    Ok(())
}
