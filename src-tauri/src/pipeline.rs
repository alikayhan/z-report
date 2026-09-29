use crate::models::Candidate;
use anyhow::Result;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;
pub use z_report_core::engine::{today, Engine as AppState};
pub use z_report_core::pipeline::{link_related, merge_into_one};

pub fn scan(app: &AppHandle) -> Result<u32> {
    let outcome = z_report_core::engine::scan(&app.state::<AppState>())?;
    let _ = app.emit("zr:refresh", ());
    anyhow::ensure!(
        outcome.complete,
        "Some evidence could not be scanned: {}",
        outcome.diagnostics.join("; ")
    );
    Ok(outcome.updated)
}

pub fn evaluate_pending(app: &AppHandle, kind: &str) -> Result<usize> {
    let state = app.state::<AppState>();
    let id = z_report_core::engine::new_id();
    state.store.lock().unwrap().queue_read(
        &id,
        kind,
        "desktop",
        &chrono::Utc::now().to_rfc3339(),
    )?;
    let _ = app.emit("zr:evaluating", true);
    let result = z_report_core::engine::run_read(&state, &id, false, &|read| {
        let _ = app.emit("zr:read", read);
        let _ = app.emit("zr:refresh", ());
    });
    let _ = app.emit("zr:evaluating", false);
    let _ = app.emit("zr:refresh", ());
    let read = result?;
    if read.status == "failed" {
        let _ = app
            .notification()
            .builder()
            .title("Z Report")
            .body(&read.message)
            .show();
        anyhow::bail!("{}", read.message);
    }
    if read.candidate_count > 0 {
        let _ = app
            .notification()
            .builder()
            .title("Achievements ready for review")
            .body(format!(
                "{} candidates · open Z Report to review",
                read.candidate_count
            ))
            .show();
    }
    Ok(read.candidate_count as usize)
}

pub fn spawn_evaluation(app: AppHandle, kind: &'static str) {
    std::thread::spawn(move || {
        let _ = evaluate_pending(&app, kind);
    });
}

pub fn rewrite_merged_in_background(
    app: AppHandle,
    stitched: Candidate,
    parts: Vec<(String, String)>,
) {
    std::thread::spawn(move || {
        let state = app.state::<AppState>();
        z_report_core::engine::rewrite_merged(&state, &stitched, &parts, &|| {
            let _ = app.emit("zr:refresh", ());
        });
    });
}
