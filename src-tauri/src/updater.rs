use crate::commands::err;
use crate::pipeline::AppState;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_updater::UpdaterExt;

#[derive(Clone, Deserialize, Serialize)]
pub struct UpdateInfo {
    pub version: String,
    pub notes: Option<String>,
}

#[derive(Default)]
pub struct UpdaterState {
    pub info: Mutex<Option<UpdateInfo>>,
    pub ready: AtomicBool,
}

impl UpdaterState {
    pub fn new(info: Option<UpdateInfo>) -> Self {
        Self {
            info: Mutex::new(info),
            ready: AtomicBool::new(false),
        }
    }
}

async fn fetch_update(app: &AppHandle) -> Result<Option<tauri_plugin_updater::Update>, String> {
    app.updater().map_err(err)?.check().await.map_err(|error| {
        format!(
            "Couldn't check for updates. Check your internet connection and try again. ({error})"
        )
    })
}

pub async fn check(app: &AppHandle) -> Result<Option<UpdateInfo>, String> {
    let info = fetch_update(app).await?.map(|u| UpdateInfo {
        version: u.version.clone(),
        notes: u.body.clone(),
    });
    *app.state::<UpdaterState>().info.lock().unwrap() = info.clone();
    let state = app.state::<AppState>();
    let store = state.store.lock().unwrap();
    match &info {
        Some(info) => store
            .kv_set(
                "available_update",
                &serde_json::to_string(info).map_err(err)?,
            )
            .map_err(err)?,
        None => store.kv_delete("available_update").map_err(err)?,
    }
    Ok(info)
}

pub fn scheduled_check(app: &AppHandle) -> Result<(), String> {
    let previous = {
        let state = app.state::<UpdaterState>();
        let info = state.info.lock().unwrap();
        info.as_ref().map(|u| u.version.clone())
    };
    let Some(info) = tauri::async_runtime::block_on(check(app))? else {
        return Ok(());
    };
    if previous.as_deref() == Some(info.version.as_str()) {
        return Ok(());
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
    Ok(())
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
        .map_err(|error| {
            format!(
                "Couldn't download or install the update. Your current installation is still available; try again. ({error})"
            )
        })?;

    state.ready.store(true, Ordering::SeqCst);
    Ok(())
}
