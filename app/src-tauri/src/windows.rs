//! Window management: main window (Mica, hide-to-tray), OSD pill and tray flyout.

use crate::Shared;
use serde_json::json;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tauri::{
    AppHandle, Emitter, Manager, Monitor, PhysicalPosition, PhysicalSize, Rect, WebviewUrl,
    WebviewWindow, WebviewWindowBuilder,
};

/// Logical size of the OSD window (the pill is drawn centred inside, with room for a shadow).
const OSD_SIZE: (f64, f64) = (360.0, 96.0);
/// Logical size of the tray flyout.
const FLYOUT_SIZE: (f64, f64) = (344.0, 452.0);
/// How long the OSD window stays visible (frontend fades out at ~600 ms).
const OSD_VISIBLE: Duration = Duration::from_millis(900);

static OSD_GENERATION: AtomicU64 = AtomicU64::new(0);
static MAIN_SHOWN_ONCE: AtomicBool = AtomicBool::new(false);
static FLYOUT_HIDDEN_AT: Mutex<Option<Instant>> = Mutex::new(None);

/// Applies the Mica backdrop to the main window (falls back silently on Windows 10).
pub fn setup_main(app: &AppHandle) {
    let Some(win) = app.get_webview_window("main") else {
        return;
    };
    apply_backdrop(app, &win, None);
}

pub fn apply_backdrop(app: &AppHandle, win: &WebviewWindow, dark: Option<bool>) {
    match window_vibrancy::apply_mica(win, dark) {
        Ok(()) => {
            if let Some(shared) = app.try_state::<Shared>() {
                shared.mica.store(true, Ordering::Relaxed);
            }
        }
        Err(e) => tracing::info!("Mica not available, using a solid background: {e}"),
    }
}

/// Creates the hidden OSD and flyout windows.
pub fn create_aux_windows(app: &AppHandle) -> tauri::Result<()> {
    let osd = WebviewWindowBuilder::new(app, "osd", WebviewUrl::App("index.html#/osd".into()))
        .title("Desk Bridge OSD")
        .inner_size(OSD_SIZE.0, OSD_SIZE.1)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .focused(false)
        .focusable(false)
        .visible(false)
        .build()?;
    // Click-through: the pill must never steal a click from the app below it.
    osd.set_ignore_cursor_events(true)?;

    WebviewWindowBuilder::new(app, "flyout", WebviewUrl::App("index.html#/flyout".into()))
        .title("Desk Bridge")
        .inner_size(FLYOUT_SIZE.0, FLYOUT_SIZE.1)
        .decorations(false)
        .transparent(true)
        // The CSS card draws its own soft shadow inside the transparent window padding.
        .shadow(false)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .visible(false)
        .build()?;
    Ok(())
}

pub fn show_main(app: &AppHandle) {
    hide_flyout(app);
    if let Some(win) = app.get_webview_window("main") {
        MAIN_SHOWN_ONCE.store(true, Ordering::Relaxed);
        let _ = win.unminimize();
        let _ = win.show();
        let _ = win.set_focus();
    }
}

/// Called once after the frontend painted its first frame.
pub fn show_main_after_ready(app: &AppHandle) {
    if app.state::<Shared>().start_hidden {
        return;
    }
    show_main(app);
}

/// Safety net for a frontend that never reports ready.
pub fn show_main_if_hidden_at_startup(app: &AppHandle) {
    if !MAIN_SHOWN_ONCE.load(Ordering::Relaxed) {
        show_main(app);
    }
}

fn monitor_at(app: &AppHandle, x: f64, y: f64) -> Option<Monitor> {
    app.monitor_from_point(x, y)
        .ok()
        .flatten()
        .or_else(|| app.primary_monitor().ok().flatten())
}

/// Shows the OSD pill top-centre on the monitor with the mouse cursor.
pub fn show_osd(app: &AppHandle, icon: &str, text: &str) {
    let Some(win) = app.get_webview_window("osd") else {
        return;
    };
    let cursor = app
        .cursor_position()
        .unwrap_or(PhysicalPosition::new(0.0, 0.0));
    if let Some(mon) = monitor_at(app, cursor.x, cursor.y) {
        let scale = mon.scale_factor();
        let w = (OSD_SIZE.0 * scale).round() as u32;
        let h = (OSD_SIZE.1 * scale).round() as u32;
        let area = mon.work_area();
        let x = area.position.x + ((area.size.width.saturating_sub(w)) / 2) as i32;
        let y = area.position.y + (24.0 * scale).round() as i32;
        let _ = win.set_size(PhysicalSize::new(w, h));
        let _ = win.set_position(PhysicalPosition::new(x, y));
    }
    let _ = app.emit_to("osd", "osd://show", json!({ "icon": icon, "text": text }));
    let _ = win.show();

    let generation = OSD_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(OSD_VISIBLE).await;
        // A newer OSD request restarts the timer; only the latest one hides the window.
        if OSD_GENERATION.load(Ordering::SeqCst) == generation {
            if let Some(win) = app.get_webview_window("osd") {
                let _ = win.hide();
            }
        }
    });
}

pub fn hide_flyout(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("flyout") {
        if win.is_visible().unwrap_or(false) {
            if let Ok(mut t) = FLYOUT_HIDDEN_AT.lock() {
                *t = Some(Instant::now());
            }
            let _ = win.hide();
        }
    }
}

/// Tray left click: toggle the compact flyout next to the tray icon.
pub fn toggle_flyout(app: &AppHandle, tray_rect: &Rect) {
    let Some(win) = app.get_webview_window("flyout") else {
        return;
    };
    if win.is_visible().unwrap_or(false) {
        hide_flyout(app);
        return;
    }
    // The click that made the flyout lose focus must not reopen it right away.
    let just_closed = FLYOUT_HIDDEN_AT
        .lock()
        .ok()
        .and_then(|t| *t)
        .is_some_and(|t| t.elapsed() < Duration::from_millis(300));
    if just_closed {
        return;
    }

    let icon_pos = tray_rect.position.to_physical::<f64>(1.0);
    let icon_size = tray_rect.size.to_physical::<f64>(1.0);
    if let Some(mon) = monitor_at(app, icon_pos.x, icon_pos.y) {
        let scale = mon.scale_factor();
        let w = (FLYOUT_SIZE.0 * scale).round();
        let h = (FLYOUT_SIZE.1 * scale).round();
        let area = mon.work_area();
        let (ax, ay) = (f64::from(area.position.x), f64::from(area.position.y));
        let (aw, ah) = (f64::from(area.size.width), f64::from(area.size.height));
        let margin = 8.0 * scale;
        let cx = icon_pos.x + icon_size.width / 2.0;
        let x = (cx - w / 2.0).clamp(ax + margin, (ax + aw - w - margin).max(ax + margin));
        let taskbar_on_top = icon_pos.y + icon_size.height / 2.0 < ay + ah / 2.0 && icon_pos.y < ay;
        let y = if taskbar_on_top {
            ay + margin
        } else {
            (ay + ah - h - margin).max(ay + margin)
        };
        let _ = win.set_size(PhysicalSize::new(w as u32, h as u32));
        let _ = win.set_position(PhysicalPosition::new(x as i32, y as i32));
    }
    let _ = app.emit_to("flyout", "flyout://opened", ());
    let _ = win.show();
    let _ = win.set_focus();
}
