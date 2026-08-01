mod commands;
pub mod evaluator;
pub mod export;
pub mod gitfacts;
pub mod ingest;
pub mod models;
mod pipeline;
pub mod related;
mod scheduler;
pub mod store;

use pipeline::AppState;
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, WindowEvent};

// With the window hidden the app is menu-bar only: no Dock icon, no Cmd-Tab entry.
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

fn show_window(app: &AppHandle) {
    set_dock_visible(app, true);
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
        .setup(|app| {
            let store = store::Store::open_default()?;
            app.manage(AppState {
                store: Mutex::new(store),
                evaluating: AtomicBool::new(false),
                metered: Mutex::new(None),
            });

            // Probe billing mode off-thread so launch never blocks on the CLI.
            let detect = app.handle().clone();
            std::thread::spawn(move || {
                let settings = detect.state::<AppState>().store.lock().unwrap().settings();
                let metered = evaluator::is_metered(&settings);
                *detect.state::<AppState>().metered.lock().unwrap() = Some(metered);
                let _ = detect.emit("zr:refresh", ());
            });

            let open = MenuItem::with_id(app, "open", "Open Z Report", true, None::<&str>)?;
            let xread = MenuItem::with_id(app, "xread", "Review now (X-read)", true, None::<&str>)?;
            let sep = PredefinedMenuItem::separator(app)?;
            let quit = MenuItem::with_id(app, "quit", "Quit Z Report", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &xread, &sep, &quit])?;

            TrayIconBuilder::with_id("zreport-tray")
                .icon(Image::from_bytes(include_bytes!("../icons/tray.png"))?)
                .icon_as_template(true)
                .tooltip("Z Report")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => show_window(app),
                    "xread" => {
                        let handle = app.clone();
                        std::thread::spawn(move || {
                            let _ = pipeline::evaluate_pending(&handle, "xread");
                        });
                    }
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
        // Closing the window keeps the app alive in the menu bar.
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
                set_dock_visible(window.app_handle(), false);
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::overview,
            commands::scan_now,
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
            commands::export_markdown,
            commands::write_file,
            commands::get_settings,
            commands::set_settings,
            commands::eval_runs,
            commands::delete_all_data,
        ])
        .build(tauri::generate_context!())
        .expect("error while running Z Report")
        .run(|app, event| {
            if let tauri::RunEvent::Reopen { .. } = event {
                show_window(app);
            }
        });
}
