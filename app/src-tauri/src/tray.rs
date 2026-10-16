//! System tray: icon states, menu (plan 9.4) and left-click flyout.
//!
//! The menu is built once; [`refresh`] updates labels, check marks, enabled flags and the icon from the
//! latest `AppState` snapshot pushed by the agent.

use crate::ipc_client::{self, IpcClient};
use crate::windows;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::image::Image;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::DialogExt;

const TRAY_ID: &str = "main";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Connected,
    Disconnected,
    MicLive,
    Focus,
}

pub struct Tray {
    icon: TrayIcon,
    status: MenuItem<tauri::Wry>,
    focus: CheckMenuItem<tauri::Wry>,
    mute_laptop: CheckMenuItem<tauri::Wry>,
    mute_mic: CheckMenuItem<tauri::Wry>,
    lock: CheckMenuItem<tauri::Wry>,
    send: MenuItem<tauri::Wry>,
    open: MenuItem<tauri::Wry>,
    pause: CheckMenuItem<tauri::Wry>,
    quit: MenuItem<tauri::Wry>,
    quit_agent: MenuItem<tauri::Wry>,
    last: Mutex<Option<(Kind, bool, bool)>>,
    last_texts: Mutex<String>,
}

struct Labels {
    focus: &'static str,
    mute_laptop: &'static str,
    mute_mic: &'static str,
    lock: &'static str,
    send: &'static str,
    open: &'static str,
    pause: &'static str,
    quit: &'static str,
    quit_agent: &'static str,
    offline: &'static str,
    paused: &'static str,
    idle: &'static str,
    connecting: &'static str,
    searching: &'static str,
    unstable: &'static str,
    connected_with: &'static str,
    device: &'static str,
}

const DE: Labels = Labels {
    focus: "Fokusmodus",
    mute_laptop: "Laptop-Ton im Headset stumm",
    mute_mic: "Mikro-Weiterleitung stumm",
    lock: "Cursor sperren",
    send: "Datei senden…",
    open: "Öffnen",
    pause: "Pausieren (alles trennen)",
    quit: "Beenden",
    quit_agent: "Beenden und Agent stoppen",
    offline: "Agent nicht erreichbar",
    paused: "Pausiert",
    idle: "Nicht verbunden",
    connecting: "Verbindung wird aufgebaut…",
    searching: "Suche nach dem Partnergerät…",
    unstable: "Verbindung instabil",
    connected_with: "Verbunden mit",
    device: "Partnergerät",
};

const EN: Labels = Labels {
    focus: "Focus mode",
    mute_laptop: "Mute laptop sound in the headset",
    mute_mic: "Mute microphone forwarding",
    lock: "Lock cursor",
    send: "Send file…",
    open: "Open",
    pause: "Pause (disconnect everything)",
    quit: "Quit",
    quit_agent: "Quit and stop agent",
    offline: "Agent not reachable",
    paused: "Paused",
    idle: "Not connected",
    connecting: "Connecting…",
    searching: "Looking for the other device…",
    unstable: "Connection unstable",
    connected_with: "Connected to",
    device: "other device",
};

fn labels_for(language: &str) -> &'static Labels {
    let lang = match language {
        "de" | "en" => language.to_owned(),
        _ => sys_locale::get_locale().unwrap_or_default(),
    };
    if lang.to_ascii_lowercase().starts_with("de") {
        &DE
    } else {
        &EN
    }
}

macro_rules! tray_png {
    ($name:literal) => {
        (
            include_bytes!(concat!("../icons/tray/", $name, ".png")) as &[u8],
            include_bytes!(concat!("../icons/tray/", $name, "@2x.png")) as &[u8],
        )
    };
}

fn tray_image(kind: Kind, light_taskbar: bool, hi_dpi: bool) -> Option<Image<'static>> {
    let (small, large) = match (kind, light_taskbar) {
        (Kind::Connected, false) => tray_png!("connected-dark"),
        (Kind::Connected, true) => tray_png!("connected-light"),
        (Kind::Disconnected, false) => tray_png!("disconnected-dark"),
        (Kind::Disconnected, true) => tray_png!("disconnected-light"),
        (Kind::MicLive, false) => tray_png!("mic-live-dark"),
        (Kind::MicLive, true) => tray_png!("mic-live-light"),
        (Kind::Focus, false) => tray_png!("focus-dark"),
        (Kind::Focus, true) => tray_png!("focus-light"),
    };
    Image::from_bytes(if hi_dpi { large } else { small }).ok()
}

/// `true` when the Windows taskbar uses the light theme (dark glyph needed).
fn taskbar_is_light() -> bool {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
        .and_then(|k| k.get_value::<u32, _>("SystemUsesLightTheme"))
        .map(|v| v == 1)
        .unwrap_or(false)
}

impl Tray {
    pub fn build(app: &AppHandle) -> tauri::Result<Self> {
        let l = &DE;
        let status = MenuItem::with_id(app, "status", l.idle, false, None::<&str>)?;
        let focus = CheckMenuItem::with_id(app, "focus", l.focus, true, false, None::<&str>)?;
        let mute_laptop =
            CheckMenuItem::with_id(app, "mute_laptop", l.mute_laptop, true, false, None::<&str>)?;
        let mute_mic =
            CheckMenuItem::with_id(app, "mute_mic", l.mute_mic, true, false, None::<&str>)?;
        let lock = CheckMenuItem::with_id(app, "lock", l.lock, true, false, None::<&str>)?;
        let send = MenuItem::with_id(app, "send", l.send, true, None::<&str>)?;
        let open = MenuItem::with_id(app, "open", l.open, true, None::<&str>)?;
        let pause = CheckMenuItem::with_id(app, "pause", l.pause, true, false, None::<&str>)?;
        let quit = MenuItem::with_id(app, "quit", l.quit, true, None::<&str>)?;
        let quit_agent = MenuItem::with_id(app, "quit_agent", l.quit_agent, true, None::<&str>)?;

        let sep = |app: &AppHandle| PredefinedMenuItem::separator(app);
        let (s1, s2, s3, s4) = (sep(app)?, sep(app)?, sep(app)?, sep(app)?);
        let menu = Menu::with_items(
            app,
            &[
                &status,
                &s1,
                &focus,
                &mute_laptop,
                &mute_mic,
                &lock,
                &send,
                &s2,
                &open,
                &pause,
                &s3,
                &quit,
                &s4,
                &quit_agent,
            ],
        )?;

        let light = taskbar_is_light();
        let hi_dpi = app
            .primary_monitor()
            .ok()
            .flatten()
            .is_some_and(|m| m.scale_factor() >= 1.25);
        let mut builder = TrayIconBuilder::with_id(TRAY_ID)
            .tooltip("Desk Bridge")
            .menu(&menu)
            .show_menu_on_left_click(false)
            .on_menu_event(|app, event| on_menu(app, event.id().as_ref()))
            .on_tray_icon_event(|tray, event| match event {
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    rect,
                    ..
                } => {
                    windows::toggle_flyout(tray.app_handle(), &rect);
                }
                TrayIconEvent::DoubleClick {
                    button: MouseButton::Left,
                    ..
                } => {
                    windows::show_main(tray.app_handle());
                }
                _ => {}
            });
        if let Some(img) = tray_image(Kind::Disconnected, light, hi_dpi) {
            builder = builder.icon(img);
        }
        let icon = builder.build(app)?;

        // The taskbar theme can change at any time; re-evaluate the icon every few seconds.
        let poll_app = app.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                if let Some(client) = poll_app.try_state::<Arc<IpcClient>>() {
                    refresh(&poll_app, client.inner());
                }
            }
        });

        Ok(Self {
            icon,
            status,
            focus,
            mute_laptop,
            mute_mic,
            lock,
            send,
            open,
            pause,
            quit,
            quit_agent,
            last: Mutex::new(None),
            last_texts: Mutex::new(String::new()),
        })
    }
}

fn str_at<'a>(v: &'a Value, ptr: &str) -> &'a str {
    v.pointer(ptr).and_then(Value::as_str).unwrap_or("")
}

fn bool_at(v: &Value, ptr: &str) -> bool {
    v.pointer(ptr).and_then(Value::as_bool).unwrap_or(false)
}

fn status_line(l: &Labels, online: bool, state: Option<&Value>) -> String {
    let Some(state) = state.filter(|_| online) else {
        return l.offline.to_owned();
    };
    if bool_at(state, "/paused") || str_at(state, "/connection/state") == "paused" {
        return l.paused.to_owned();
    }
    let peer = state
        .pointer("/connection/peer/name")
        .and_then(Value::as_str)
        .unwrap_or(l.device);
    match str_at(state, "/connection/state") {
        "connected" => {
            let rtt = state
                .pointer("/connection/rtt_us")
                .and_then(Value::as_u64)
                .map(|us| us as f64 / 1000.0);
            match rtt {
                Some(ms) if ms < 10.0 => format!("{} {peer} · {ms:.1} ms", l.connected_with),
                Some(ms) => format!("{} {peer} · {ms:.0} ms", l.connected_with),
                None => format!("{} {peer}", l.connected_with),
            }
        }
        "degraded" => format!("{} · {peer}", l.unstable),
        "discovering" => l.searching.to_owned(),
        "connecting" | "handshaking" | "reconnecting" => l.connecting.to_owned(),
        _ => l.idle.to_owned(),
    }
}

/// Re-reads the latest state and updates menu and icon. Cheap; called on every state push.
pub fn refresh(app: &AppHandle, client: &Arc<IpcClient>) {
    let Some(tray) = app.try_state::<Tray>() else {
        return;
    };
    let online = client.is_connected();
    let state = client.latest_state();
    let prefs = client.prefs();
    let l = labels_for(&prefs.language);
    let s = state.as_ref().filter(|_| online);

    let conn = s.map(|s| str_at(s, "/connection/state")).unwrap_or("");
    let connected = matches!(conn, "connected" | "degraded");
    let focus_phase = s
        .map(|s| str_at(s, "/features/focus/phase"))
        .unwrap_or("off");
    let focus_active = matches!(focus_phase, "preparing" | "streaming");
    let mic_state = s.map(|s| str_at(s, "/features/mic/state")).unwrap_or("off");
    let mic_live = connected && mic_state == "live";
    let laptop_muted = s.is_some_and(|s| bool_at(s, "/features/laptop_audio/muted"));
    let mic_muted = s.is_some_and(|s| bool_at(s, "/features/mic/muted"));
    let locked = s.is_some_and(|s| bool_at(s, "/cursor_locked"));
    let paused = s.is_some_and(|s| bool_at(s, "/paused"));
    let focus_unavailable = s.is_some_and(|s| str_at(s, "/features/focus/state") == "unavailable");
    let mic_unavailable = mic_state == "unavailable";

    let mut status = status_line(l, online, s);
    if mic_live
        && let Some(d) = s
            .and_then(|s| s.pointer("/features/mic/detail"))
            .and_then(Value::as_str)
    {
        status = format!("{status} · {d}");
    }
    let signature = format!(
        "{}|{status}|{focus_active}{laptop_muted}{mic_muted}{locked}{paused}{online}{connected}{focus_unavailable}{mic_unavailable}",
        prefs.language
    );
    let texts_changed = {
        let mut last = tray.last_texts.lock().expect("tray lock");
        let changed = *last != signature;
        if changed {
            *last = signature;
        }
        changed
    };
    if texts_changed {
        let _ = tray.status.set_text(&status);
        let _ = tray.focus.set_text(l.focus);
        let _ = tray.mute_laptop.set_text(l.mute_laptop);
        let _ = tray.mute_mic.set_text(l.mute_mic);
        let _ = tray.lock.set_text(l.lock);
        let _ = tray.send.set_text(l.send);
        let _ = tray.open.set_text(l.open);
        let _ = tray.pause.set_text(l.pause);
        let _ = tray.quit.set_text(l.quit);
        let _ = tray.quit_agent.set_text(l.quit_agent);

        let _ = tray.focus.set_checked(focus_active);
        let _ = tray.mute_laptop.set_checked(laptop_muted);
        let _ = tray.mute_mic.set_checked(mic_muted);
        let _ = tray.lock.set_checked(locked);
        let _ = tray.pause.set_checked(paused);

        let _ = tray
            .focus
            .set_enabled(online && connected && !focus_unavailable);
        let _ = tray.mute_laptop.set_enabled(online);
        let _ = tray.mute_mic.set_enabled(online && !mic_unavailable);
        let _ = tray.lock.set_enabled(online);
        let _ = tray.send.set_enabled(online && connected);
        let _ = tray.pause.set_enabled(online);
        let _ = tray.quit_agent.set_enabled(online);
        let _ = tray
            .icon
            .set_tooltip(Some(format!("Desk Bridge – {status}")));
    }

    let kind = if !connected {
        Kind::Disconnected
    } else if mic_live {
        Kind::MicLive
    } else if focus_active {
        Kind::Focus
    } else {
        Kind::Connected
    };
    let light = taskbar_is_light();
    let hi_dpi = app
        .primary_monitor()
        .ok()
        .flatten()
        .is_some_and(|m| m.scale_factor() >= 1.25);
    let wanted = (kind, light, hi_dpi);
    let mut last = tray.last.lock().expect("tray lock");
    if *last != Some(wanted) {
        if let Some(img) = tray_image(kind, light, hi_dpi) {
            let _ = tray.icon.set_icon(Some(img));
        }
        *last = Some(wanted);
    }
}

fn on_menu(app: &AppHandle, id: &str) {
    let Some(client) = app.try_state::<Arc<IpcClient>>() else {
        return;
    };
    let client = client.inner().clone();
    let state = client.latest_state().unwrap_or(Value::Null);
    match id {
        "open" => windows::show_main(app),
        "focus" => {
            let phase = str_at(&state, "/features/focus/phase");
            let mode = if matches!(phase, "preparing" | "streaming") {
                "off".to_owned()
            } else {
                client.prefs().focus_mode
            };
            ipc_client::send_detached(app, json!({ "type": "set_focus", "mode": mode }));
        }
        // `enabled: true` on the mute features means "mute is on".
        "mute_laptop" => {
            let muted = bool_at(&state, "/features/laptop_audio/muted");
            ipc_client::send_detached(
                app,
                json!({ "type": "set_feature", "feature": "mute_laptop_audio", "enabled": !muted }),
            );
        }
        "mute_mic" => {
            let muted = bool_at(&state, "/features/mic/muted");
            ipc_client::send_detached(
                app,
                json!({ "type": "set_feature", "feature": "mute_mic", "enabled": !muted }),
            );
        }
        "lock" => {
            let locked = bool_at(&state, "/cursor_locked");
            ipc_client::send_detached(app, json!({ "type": "set_cursor_lock", "locked": !locked }));
        }
        "pause" => {
            let paused = bool_at(&state, "/paused");
            ipc_client::send_detached(app, json!({ "type": "set_paused", "paused": !paused }));
        }
        "send" => {
            let app2 = app.clone();
            app.dialog().file().pick_files(move |picked| {
                let paths: Vec<String> = picked
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|p| p.into_path().ok())
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect();
                if !paths.is_empty() {
                    ipc_client::send_detached(
                        &app2,
                        json!({ "type": "send_files", "paths": paths }),
                    );
                }
            });
        }
        "quit" => app.exit(0),
        "quit_agent" => {
            let app2 = app.clone();
            tauri::async_runtime::spawn(async move {
                let _ = tokio::time::timeout(
                    Duration::from_secs(2),
                    client.request(json!({ "type": "shutdown_agent" })),
                )
                .await;
                app2.exit(0);
            });
        }
        _ => {}
    }
}
