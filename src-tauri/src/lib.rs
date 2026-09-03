mod calendar;
mod commands;
pub mod evaluator;
pub mod export;
pub mod gitfacts;
pub mod ingest;
pub mod lifecycle;
pub mod models;
pub mod pipeline;
pub mod related;
mod scheduler;
pub mod store;
mod text;
mod updater;

use pipeline::AppState;
use std::sync::Mutex;
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, WindowEvent};

fn set_dock_visible(app: &AppHandle, visible: bool) {
    #[cfg(target_os = "macos")]
    {
        use tauri::ActivationPolicy;
        let _ = app.set_activation_policy(if visible {
            ActivationPolicy::Regular
        } else {
            ActivationPolicy::Accessory
        });
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, visible);
}

// Returning from the Accessory policy makes macOS re-derive the Dock icon and
// it usually picks a generic one; hand it the real icon explicitly.
#[cfg(target_os = "macos")]
fn restore_dock_icon(app: &AppHandle) {
    use objc2::{AllocAnyThread, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;
    let _ = app.run_on_main_thread(|| {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let data = NSData::with_bytes(include_bytes!("../icons/icon.png"));
        if let Some(icon) = NSImage::initWithData(NSImage::alloc(), &data) {
            unsafe { NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&icon)) };
        }
    });
}

fn show_window(app: &AppHandle) {
    set_dock_visible(app, true);
    #[cfg(target_os = "macos")]
    restore_dock_icon(app);
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.set_focus();
    }
}

fn toggle_window(app: &AppHandle) {
    let Some(win) = app.get_webview_window("main") else {
        return;
    };
    if win.is_visible().unwrap_or(false) && win.is_focused().unwrap_or(false) {
        let _ = win.hide();
        set_dock_visible(app, false);
    } else {
        show_window(app);
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let store = store::Store::open_default()?;
            let cached_update = store
                .kv_get("available_update")
                .and_then(|value| serde_json::from_str(&value).ok());
            app.manage(AppState {
                store: Mutex::new(store),
                lifecycle: lifecycle::Lifecycle::default(),
                availability: Mutex::new(evaluator::Availability::default()),
            });
            app.manage(updater::UpdaterState::new(cached_update));

            let detect = app.handle().clone();
            std::thread::spawn(move || {
                let state = detect.state::<AppState>();
                let Some(busy) = state.lifecycle.begin_probe() else {
                    return;
                };
                let settings = state.store.lock().unwrap().settings();
                let availability = evaluator::probe(&settings, &busy);
                *detect.state::<AppState>().availability.lock().unwrap() = availability;
                let _ = detect.emit("zr:refresh", ());
            });

            let open = MenuItem::with_id(app, "open", "Open Z Report", true, None::<&str>)?;
            let xread = MenuItem::with_id(app, "xread", "Review now (X-read)", true, None::<&str>)?;
            let updates = MenuItem::with_id(app, "updates", "Check for Updates…", true, None::<&str>)?;
            let sep = PredefinedMenuItem::separator(app)?;
            let quit = MenuItem::with_id(app, "quit", "Quit Z Report", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &xread, &updates, &sep, &quit])?;

            TrayIconBuilder::with_id("zreport-tray")
                .icon(Image::from_bytes(include_bytes!("../icons/tray.png"))?)
                .icon_as_template(true)
                .tooltip("Z Report")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => show_window(app),
                    "xread" => {
                        pipeline::spawn_evaluation(app.clone(), "xread");
                    }
                    "updates" => updater::manual_check(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        toggle_window(tray.app_handle());
                    }
                })
                .build(app)?;

            scheduler::spawn(app.handle().clone());
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
                set_dock_visible(window.app_handle(), false);
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::overview,
            commands::run_xread,
            commands::candidates,
            commands::update_candidate,
            commands::approve_candidate,
            commands::discard_candidate,
            commands::restore_candidate,
            commands::merge_candidates,
            commands::dismiss_related,
            commands::journal,
            commands::confirm_impact,
            commands::delete_journal_entry,
            commands::export_data,
            commands::write_file,
            commands::get_settings,
            commands::set_settings,
            commands::eval_runs,
            commands::delete_all_data,
            commands::install_update,
            commands::restart_app,
        ])
        .build(tauri::generate_context!())
        .expect("error while running Z Report")
        .run(|app, event| {
            if let tauri::RunEvent::Reopen { .. } = event {
                show_window(app);
            }
        });
}
