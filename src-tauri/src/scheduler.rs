use crate::pipeline::{self, AppState};
use crate::updater;
use std::time::Duration;
use tauri::{AppHandle, Manager};

pub fn spawn(app: AppHandle) {
    std::thread::spawn(move || loop {
        tick(&app);
        std::thread::sleep(Duration::from_secs(30));
    });
}

fn tick(app: &AppHandle) {
    let state = app.state::<AppState>();
    let (settings, last_scan, last_zread_day, last_update_check) = {
        let store = state.store.lock().unwrap();
        (
            store.settings(),
            store.kv_get("last_scan_at"),
            store.kv_get("last_zread_day"),
            store.kv_get("last_update_check_at"),
        )
    };

    let now = chrono::Local::now();
    let elapsed = |stamp: Option<String>, min: chrono::Duration| {
        stamp
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
            .map(|t| now.signed_duration_since(t.with_timezone(&chrono::Local)) >= min)
            .unwrap_or(true)
    };

    if elapsed(
        last_scan,
        chrono::Duration::minutes(settings.scan_interval_min.max(1) as i64),
    ) {
        let _ = pipeline::scan(app);
    }

    if elapsed(last_update_check, chrono::Duration::hours(24)) {
        {
            let store = state.store.lock().unwrap();
            let _ = store.kv_set("last_update_check_at", &now.to_rfc3339());
        }
        updater::scheduled_check(app);
    }

    let today = pipeline::today();
    if last_zread_day.as_deref() == Some(today.as_str()) {
        return;
    }
    // Seed today so first launch cannot trigger immediate model usage.
    if last_zread_day.is_none() {
        let store = state.store.lock().unwrap();
        let _ = store.kv_set("last_zread_day", &today);
        return;
    }
    let due_time = chrono::NaiveTime::parse_from_str(&settings.zread_time, "%H:%M")
        .unwrap_or_else(|_| chrono::NaiveTime::from_hms_opt(18, 0, 0).unwrap());
    if now.time() < due_time {
        return;
    }
    {
        let store = state.store.lock().unwrap();
        let _ = store.kv_set("last_zread_day", &today);
    }
    let _ = pipeline::evaluate_pending(app, "zread");
}
