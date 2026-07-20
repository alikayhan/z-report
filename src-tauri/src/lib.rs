mod commands;
pub mod evaluator;
pub mod export;
pub mod gitfacts;
pub mod ingest;
pub mod models;
mod pipeline;
mod scheduler;
pub mod store;

use pipeline::AppState;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, WindowEvent};
use tauri_plugin_positioner::{Position, WindowExt};

fn toggle_window(app: &AppHandle) {
    let Some(win) = app.get_webview_window("main") else {
        return;
    };
    if win.is_visible().unwrap_or(false) {
        let _ = win.hide();
    } else {
        let _ = win.move_window(Position::TrayBottomCenter);
        let _ = win.show();
        let _ = win.set_focus();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_positioner::init())
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let store = store::Store::open_default()?;
            app.manage(AppState {
                store: Mutex::new(store),
                evaluating: AtomicBool::new(false),
                pinned: AtomicBool::new(false),
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
                    "open" => toggle_window(app),
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
                    tauri_plugin_positioner::on_tray_event(tray.app_handle(), &event);
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
            if let WindowEvent::Focused(false) = event {
                let state = window.app_handle().state::<AppState>();
                if !state.pinned.load(Ordering::SeqCst) {
                    let _ = window.hide();
                }
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
            commands::journal,
            commands::confirm_impact,
            commands::delete_journal_entry,
            commands::export_markdown,
            commands::write_file,
            commands::get_settings,
            commands::set_settings,
            commands::eval_runs,
            commands::set_pinned,
            commands::hide_window,
            commands::delete_all_data,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Z Report");
}
