//! Tauri commands called from the frontend.

use crate::ipc_client::IpcClient;
use crate::{Shared, tray, windows};
use serde::Serialize;
use serde_json::Value;
#[cfg(feature = "updater")]
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tauri::{AppHandle, Manager, State, Theme};

/// Forwards a `UiRequest` (raw JSON, see `crates/core/src/ipc.rs`) to the agent and returns its reply data.
/// Errors: `agent_offline`, `timeout`, or the agent's own error text.
#[tauri::command]
pub async fn agent_request(
    app: AppHandle,
    client: State<'_, Arc<IpcClient>>,
    req: Value,
) -> Result<Value, String> {
    let kind = req
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let reply = client.request(req).await?;
    if kind == "get_config" || kind == "set_config" {
        client.remember_config(&reply);
        tray::refresh(&app, client.inner());
    }
    Ok(reply)
}

/// The frontend painted its first frame: show the (initially hidden) main window.
#[tauri::command]
pub fn frontend_ready(app: AppHandle) {
    windows::show_main_after_ready(&app);
}

#[derive(Serialize)]
pub struct AppInfo {
    version: &'static str,
    mica: bool,
    start_hidden: bool,
}

#[tauri::command]
pub fn app_info(shared: State<'_, Shared>) -> AppInfo {
    AppInfo {
        version: env!("CARGO_PKG_VERSION"),
        mica: shared.mica.load(Ordering::Relaxed),
        start_hidden: shared.start_hidden,
    }
}

/// Opens `%LOCALAPPDATA%\softwelop\DeskBridge\logs` in Explorer.
#[tauri::command]
pub fn open_logs_folder() -> Result<(), String> {
    let dir = directories::BaseDirs::new()
        .map(|d| {
            d.data_local_dir()
                .join(crate::brand::COMPANY_DIR)
                .join(crate::brand::APP_DIR)
                .join("logs")
        })
        .ok_or("no local data directory")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::process::Command::new("explorer.exe")
        .arg(dir)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Shows a file in Explorer (selected). Directories are opened.
#[tauri::command]
pub fn reveal_path(path: String) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let p = std::path::Path::new(&path);
    let mut cmd = std::process::Command::new("explorer.exe");
    if p.is_dir() {
        cmd.arg(p);
    } else {
        // `/select,<path>` must be passed as one raw argument.
        cmd.raw_arg(format!("/select,\"{}\"", p.display()));
    }
    cmd.spawn().map(|_| ()).map_err(|e| e.to_string())
}

/// Starts a received partner installer. The frontend only calls this after the user confirmed the dialog;
/// the path must be an existing `.exe` named like `DeskBridge-Setup-*.exe`.
#[tauri::command]
pub fn launch_installer(app: AppHandle, path: String) -> Result<(), String> {
    let p = std::path::Path::new(&path);
    let name = p
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !p.is_file() || !name.starts_with("deskbridge-setup-") || !name.ends_with(".exe") {
        return Err("not a Desk Bridge installer".into());
    }
    std::process::Command::new(p)
        .spawn()
        .map_err(|e| e.to_string())?;
    // The installer replaces files of this app: get out of its way.
    app.exit(0);
    Ok(())
}

#[tauri::command]
pub fn open_main(app: AppHandle) {
    windows::show_main(&app);
}

#[tauri::command]
pub fn hide_flyout(app: AppHandle) {
    windows::hide_flyout(&app);
}

/// Applies the user's theme choice (`light` | `dark` | `system`) to the native window frame / Mica tint.
#[tauri::command]
pub fn set_window_theme(app: AppHandle, theme: String) {
    let Some(win) = app.get_webview_window("main") else {
        return;
    };
    let (t, dark) = match theme.as_str() {
        "light" => (Some(Theme::Light), Some(false)),
        "dark" => (Some(Theme::Dark), Some(true)),
        _ => (None, None),
    };
    let _ = win.set_theme(t);
    windows::apply_backdrop(&app, &win, dark);
}

#[derive(Serialize)]
#[cfg_attr(not(feature = "updater"), allow(dead_code))]
pub struct UpdateInfo {
    version: String,
    notes: String,
}

/// Looks for a new UI version via the Tauri updater (GitHub releases). Returns `None` when up to date.
#[cfg(feature = "updater")]
#[tauri::command]
pub async fn updater_check(app: AppHandle) -> Result<Option<UpdateInfo>, String> {
    use tauri_plugin_updater::UpdaterExt;
    let updater = app
        .updater()
        .map_err(|e| format!("updater_unavailable: {e}"))?;
    match updater.check().await {
        Ok(Some(u)) => Ok(Some(UpdateInfo {
            version: u.version.clone(),
            notes: u.body.clone().unwrap_or_default(),
        })),
        Ok(None) => Ok(None),
        Err(e) => Err(format!("updater_error: {e}")),
    }
}

/// Downloads and installs the update found by [`updater_check`], then restarts the UI.
#[cfg(feature = "updater")]
#[tauri::command]
pub async fn updater_install(app: AppHandle) -> Result<Value, String> {
    use tauri_plugin_updater::UpdaterExt;
    let updater = app
        .updater()
        .map_err(|e| format!("updater_unavailable: {e}"))?;
    let Some(update) = updater
        .check()
        .await
        .map_err(|e| format!("updater_error: {e}"))?
    else {
        return Ok(json!({ "installed": false }));
    };
    update
        .download_and_install(|_chunk, _total| {}, || {})
        .await
        .map_err(|e| format!("updater_error: {e}"))?;
    app.restart()
}

/// Updater is compiled out (see Cargo.toml TODO): the frontend shows "update server not set up".
#[cfg(not(feature = "updater"))]
#[tauri::command]
pub async fn updater_check() -> Result<Option<UpdateInfo>, String> {
    Err("updater_unavailable: built without the updater feature".into())
}

#[cfg(not(feature = "updater"))]
#[tauri::command]
pub async fn updater_install() -> Result<Value, String> {
    Err("updater_unavailable: built without the updater feature".into())
}
