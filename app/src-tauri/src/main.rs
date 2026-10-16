//! Desk Bridge UI: Tauri shell with tray, flyout, OSD and the named-pipe client to the agent.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod brand;
mod commands;
mod ipc_client;
mod logging;
mod tray;
mod windows;

use std::sync::Arc;
use tauri::{Manager, WindowEvent};

/// Process-wide flags shared between setup, commands and the tray.
pub struct Shared {
    /// Started with `--minimized` (autostart): stay in the tray until the user opens the window.
    pub start_hidden: bool,
    /// Mica backdrop was applied to the main window.
    pub mica: std::sync::atomic::AtomicBool,
}

fn main() {
    let _log_guard = logging::init();
    tracing::info!(
        "{} UI {} starting",
        brand::PRODUCT_NAME,
        env!("CARGO_PKG_VERSION")
    );

    let start_hidden = std::env::args().any(|a| a == "--minimized" || a == "--hidden");

    let result = tauri::Builder::default()
        // Must be the first plugin: a second launch just brings the existing window to the front.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            windows::show_main(app);
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(Shared {
            start_hidden,
            mica: std::sync::atomic::AtomicBool::new(false),
        })
        .invoke_handler(tauri::generate_handler![
            commands::agent_request,
            commands::frontend_ready,
            commands::app_info,
            commands::open_main,
            commands::launch_installer,
            commands::open_logs_folder,
            commands::reveal_path,
            commands::hide_flyout,
            commands::set_window_theme,
            commands::updater_check,
            commands::updater_install,
        ])
        .on_window_event(|window, event| match (window.label(), event) {
            // Closing the main window only hides it: the app lives in the tray.
            ("main", WindowEvent::CloseRequested { api, .. }) => {
                api.prevent_close();
                let _ = window.hide();
            }
            // The flyout behaves like a menu: it closes when it loses focus.
            ("flyout", WindowEvent::Focused(false)) => windows::hide_flyout(window.app_handle()),
            _ => {}
        })
        .setup(move |app| {
            let handle = app.handle().clone();

            // The updater must never stop the app from starting (endpoint/pubkey are placeholders until
            // the first signed release exists).
            #[cfg(feature = "updater")]
            if let Err(e) = handle.plugin(tauri_plugin_updater::Builder::new().build()) {
                tracing::warn!("updater plugin not available: {e}");
            }

            windows::setup_main(&handle);
            windows::create_aux_windows(&handle)?;

            let tray = tray::Tray::build(&handle)?;
            app.manage(tray);

            let client = Arc::new(ipc_client::IpcClient::new());
            app.manage(client.clone());
            ipc_client::spawn(handle.clone(), client);

            // Safety net: if the frontend never reports ready (JS error) show the window anyway.
            if !start_hidden {
                let h = handle.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(6)).await;
                    windows::show_main_if_hidden_at_startup(&h);
                });
            }
            Ok(())
        })
        .run(tauri::generate_context!());

    if let Err(e) = result {
        tracing::error!("fatal: {e}");
        eprintln!("Desk Bridge UI failed to start: {e}");
        std::process::exit(1);
    }
}
