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
    pub model: String,
    pub claude_found: bool,
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
    let (pending, session_count, settings, last_scan_at) = {
        let store = state.store.lock().unwrap();
        let (pending, session_count) = store.overview_counts().map_err(err)?;
        (
            pending,
            session_count,
            store.settings(),
            store.kv_get("last_scan_at"),
        )
    };
    Ok(Overview {
        pending,
        session_count,
        evaluating: state.lifecycle.evaluating(),
        last_scan_at,
        zread_time: settings.zread_time,
        today: pipeline::today(),
        model: evaluator::EVAL_MODEL.into(),
        claude_found: state.claude_found.load(Ordering::SeqCst),
        metered: (*state.metered.lock().unwrap()).unwrap_or(false),
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
) -> CmdResult<()> {
    let store = state.store.lock().unwrap();
    store
        .update_candidate_fields(&id, &title, &contribution, &outcomes)
        .map_err(err)?;
    relink(&store);
    Ok(())
}

fn relink(store: &store::Store) {
    let _ = pipeline::link_related(store);
}

#[tauri::command]
pub fn approve_candidate(state: State<AppState>, id: String, edited: bool) -> CmdResult<()> {
    let store = state.store.lock().unwrap();
    let c = store.candidate(&id).map_err(err)?;
    let entry = JournalEntry::from_candidate(&c, chrono::Local::now().to_rfc3339(), edited);
    store.insert_journal(&entry).map_err(err)?;
    store
        .set_candidate_status(&id, "approved", None)
        .map_err(err)?;
    relink(&store);
    Ok(())
}

fn transition_candidate(state: &AppState, id: &str, status: &str) -> CmdResult<()> {
    let store = state.store.lock().unwrap();
    store.set_candidate_status(id, status, None).map_err(err)?;
    relink(&store);
    Ok(())
}

#[tauri::command]
pub fn discard_candidate(state: State<AppState>, id: String) -> CmdResult<()> {
    transition_candidate(&state, &id, "discarded")
}

#[tauri::command]
pub fn restore_candidate(state: State<AppState>, id: String) -> CmdResult<()> {
    transition_candidate(&state, &id, "pending")
}

#[tauri::command]
pub fn merge_candidates(
    app: AppHandle,
    state: State<AppState>,
    ids: Vec<String>,
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
            let (found, metered) = evaluator::probe_status(&settings, &busy);
            state.claude_found.store(found, Ordering::SeqCst);
            *state.metered.lock().unwrap() = Some(metered);
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
pub async fn check_for_updates(app: AppHandle) -> CmdResult<Option<UpdateInfo>> {
    updater::check(&app).await
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
    state.store.lock().unwrap().wipe_all().map_err(err)?;
    let eval_dir = store::data_dir().join("eval");
    if eval_dir.exists() {
        std::fs::remove_dir_all(&eval_dir).map_err(err)?;
    }
    Ok(())
}
