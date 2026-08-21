use crate::commands::err;
use crate::pipeline::AppState;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_updater::UpdaterExt;

#[derive(Clone, Serialize)]
pub struct UpdateInfo {
    pub version: String,
    pub notes: Option<String>,
}

#[derive(Default)]
pub struct UpdaterState {
    pub info: Mutex<Option<UpdateInfo>>,
    pub ready: AtomicBool,
}

async fn fetch_update(app: &AppHandle) -> Result<Option<tauri_plugin_updater::Update>, String> {
    app.updater().map_err(err)?.check().await.map_err(err)
}

pub async fn check(app: &AppHandle) -> Result<Option<UpdateInfo>, String> {
    let info = fetch_update(app).await?.map(|u| UpdateInfo {
        version: u.version.clone(),
        notes: u.body.clone(),
    });
    *app.state::<UpdaterState>().info.lock().unwrap() = info.clone();
    Ok(info)
}

pub fn scheduled_check(app: &AppHandle) {
    let previous = {
        let state = app.state::<UpdaterState>();
        let info = state.info.lock().unwrap();
        info.as_ref().map(|u| u.version.clone())
    };
    let Ok(Some(info)) = tauri::async_runtime::block_on(check(app)) else {
        return;
    };
    if previous.as_deref() == Some(info.version.as_str()) {
        return;
    }
    let _ = app
        .notification()
        .builder()
        .title("Z Report update available")
        .body(format!(
            "Version {} can be installed from Settings.",
            info.version
        ))
        .show();
    let _ = app.emit("zr:refresh", ());
}

pub async fn install(app: AppHandle) -> Result<(), String> {
    let state = app.state::<UpdaterState>();
    if state.ready.load(Ordering::SeqCst) {
        return Err("The update is already installed. Restart to finish.".into());
    }
    let _busy = app.state::<AppState>().lifecycle.begin_update()?;
    let update = fetch_update(&app)
        .await?
        .ok_or("You're already on the latest version.")?;

    let progress = app.clone();
    let mut downloaded: u64 = 0;
    let mut last_milestone = None;
    update
        .download_and_install(
            move |chunk, total| {
                downloaded += chunk as u64;
                let milestone = match total {
                    Some(total) if total > 0 => downloaded * 100 / total,
                    _ => downloaded / 1_048_576,
                };
                if last_milestone != Some(milestone) {
                    last_milestone = Some(milestone);
                    let _ = progress.emit(
                        "zr:update-progress",
                        serde_json::json!({ "downloaded": downloaded, "total": total }),
                    );
                }
            },
            || {},
        )
        .await
        .map_err(err)?;

    state.ready.store(true, Ordering::SeqCst);
    Ok(())
}
