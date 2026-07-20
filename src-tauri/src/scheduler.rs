use crate::pipeline::{self, AppState};
use std::time::Duration;
use tauri::{AppHandle, Manager};

/// Background loop: periodic collection scans plus the once-daily Z-read.
pub fn spawn(app: AppHandle) {
    std::thread::spawn(move || loop {
        tick(&app);
        std::thread::sleep(Duration::from_secs(30));
    });
}

fn tick(app: &AppHandle) {
    let state = app.state::<AppState>();
    let (settings, last_scan, last_zread_day) = {
        let store = state.store.lock().unwrap();
        (
            store.settings(),
            store.kv_get("last_scan_at"),
            store.kv_get("last_zread_day"),
        )
    };

    let now = chrono::Local::now();
    let scan_due = last_scan
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
        .map(|t| {
            now.signed_duration_since(t.with_timezone(&chrono::Local))
                >= chrono::Duration::minutes(settings.scan_interval_min.max(1) as i64)
        })
        .unwrap_or(true);
    if scan_due {
        let _ = pipeline::scan(app);
    }

    let today = pipeline::today();
    if last_zread_day.as_deref() == Some(today.as_str()) {
        return;
    }
    // First launch: start the ledger today, close the books tomorrow.
    // Prevents a surprise evaluation (and model spend) minutes after install.
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
