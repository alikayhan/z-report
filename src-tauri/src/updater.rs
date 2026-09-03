use crate::commands::err;
use crate::pipeline::AppState;
use crate::store::Store;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_updater::UpdaterExt;

#[derive(Clone, Deserialize, Serialize, PartialEq)]
pub struct UpdateInfo {
    pub version: String,
}

#[derive(Default)]
pub struct UpdaterState {
    pub info: Mutex<Option<UpdateInfo>>,
    pub ready: AtomicBool,
}

impl UpdaterState {
    // The cache outlives the install it announced, so a restart into the new
    // version must not keep offering it.
    pub fn restore(store: &Store, current: &semver::Version) -> Self {
        let info = store
            .kv_get("available_update")
            .and_then(|value| serde_json::from_str::<UpdateInfo>(&value).ok())
            .filter(|u| semver::Version::parse(&u.version).is_ok_and(|v| v > *current));
        if info.is_none() {
            let _ = store.kv_delete("available_update");
        }
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

// Returns the update and whether it differs from the last check; the UI is refreshed when it does.
pub async fn check(app: &AppHandle) -> Result<(Option<UpdateInfo>, bool), String> {
    let info = fetch_update(app).await?.map(|u| UpdateInfo {
        version: u.version.clone(),
    });
    let previous = std::mem::replace(
        &mut *app.state::<UpdaterState>().info.lock().unwrap(),
        info.clone(),
    );
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
    let changed = previous != info;
    if changed {
        let _ = app.emit("zr:refresh", ());
    }
    Ok((info, changed))
}

pub fn scheduled_check(app: &AppHandle) -> Result<(), String> {
    if let (Some(info), true) = tauri::async_runtime::block_on(check(app))? {
        notify(app, available_notice(&info));
    }
    Ok(())
}

pub fn manual_check(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let notice = match check(&app).await {
            Ok((Some(info), _)) => available_notice(&info),
            Ok((None, _)) => (
                "Z Report is up to date",
                format!("You're on version {}.", app.package_info().version),
            ),
            Err(error) => ("Z Report", error),
        };
        notify(&app, notice);
    });
}

fn available_notice(info: &UpdateInfo) -> (&'static str, String) {
    (
        "Z Report update available",
        format!(
            "Version {} is ready to download from the Z Report sidebar.",
            info.version
        ),
    )
}

fn notify(app: &AppHandle, (title, body): (&str, String)) {
    let _ = app.notification().builder().title(title).body(body).show();
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
