//! Monitor enumeration and display-change notifications.
//!
//! Geometry comes from `EnumDisplaySettingsW(ENUM_CURRENT_SETTINGS)` (true physical pixels and
//! virtual-desktop position, independent of the process DPI awareness); `EnumDisplayMonitors` /
//! `GetMonitorInfoW` provide the monitor handles, the primary flag and the GDI device names.
//!
//! Ids are stable across restarts: every physical monitor (identified by its PnP device id,
//! falling back to the GDI name) gets the next free small id the first time it is seen and keeps
//! it when monitors are re-ordered or re-plugged. The table lives in
//! `%LOCALAPPDATA%\softwelop\DeskBridge\screen-ids.txt`, because the layout placements are keyed
//! by these ids: an id that depended on start-up order (e.g. a virtual monitor still present at
//! start) moved the screen to its default spot after every update. Output order is "primary
//! first, then by position".

use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::size_of;
use std::sync::Arc;
use std::thread::JoinHandle;

use deskbridge_platform::screen::{Point, ScreenInfo, ScreenProvider};
use deskbridge_platform::{PlatformError, Result, UnboundedSender};
use parking_lot::Mutex;
use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
    DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME,
    DISPLAYCONFIG_TARGET_DEVICE_NAME, DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes,
    QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, LRESULT, POINT, RECT, TRUE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    DEVMODEW, DISPLAY_DEVICEW, ENUM_CURRENT_SETTINGS, EnumDisplayDevicesW, EnumDisplayMonitors,
    EnumDisplaySettingsW, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, DestroyWindow, GetCursorPos, KillTimer, PostMessageW, PostQuitMessage,
    SetTimer, WM_APP, WM_DESTROY, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_NCCREATE, WM_SETTINGCHANGE,
    WM_TIMER,
};
use windows::core::PCWSTR;

use crate::input::{
    attach_userdata, create_hidden_window, init_dpi_awareness, run_message_loop, userdata,
};

const WM_APP_STOP: u32 = WM_APP + 1;
const TIMER_DEBOUNCE: usize = 1;
/// Quiet period after the last change message before `()` is sent.
pub const DEBOUNCE_MS: u32 = 300;

/// A monitor as read from the OS, before id assignment.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RawScreen {
    /// Stable identity (PnP device id or GDI name).
    key: String,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    dpi: u32,
    refresh_mhz: u32,
    primary: bool,
    name: String,
}

/// Assigns small, process-lifetime-stable ids to monitor identities.
#[derive(Debug, Default)]
struct IdTable {
    keys: Vec<String>,
}

impl IdTable {
    #[cfg(test)]
    const fn new() -> Self {
        Self { keys: Vec::new() }
    }

    fn assign(&mut self, key: &str) -> u32 {
        if let Some(i) = self.keys.iter().position(|k| k == key) {
            return i as u32;
        }
        self.keys.push(key.to_owned());
        (self.keys.len() - 1) as u32
    }
}

static IDS: Mutex<Option<IdTable>> = Mutex::new(None);

/// Location of the persisted id table.
fn ids_path() -> std::path::PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map_or_else(std::env::temp_dir, std::path::PathBuf::from);
    base.join("softwelop")
        .join("DeskBridge")
        .join("screen-ids.txt")
}

impl IdTable {
    /// One monitor key per line; the line number is the id.
    fn load(path: &std::path::Path) -> Self {
        let keys = std::fs::read_to_string(path)
            .map(|t| t.lines().map(str::to_owned).collect())
            .unwrap_or_default();
        Self { keys }
    }

    fn save(&self, path: &std::path::Path) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let mut text = self.keys.join("\n");
        text.push('\n');
        if let Err(e) = std::fs::write(path, text) {
            tracing::warn!("could not save the screen ids: {e}");
        }
    }
}

/// Orders (primary first, then left-to-right, top-to-bottom) and assigns ids.
fn finalize(mut raw: Vec<RawScreen>, ids: &mut IdTable) -> Vec<ScreenInfo> {
    raw.sort_by_key(|r| (!r.primary, r.x, r.y, r.key.clone()));
    raw.into_iter()
        .map(|r| ScreenInfo {
            id: ids.assign(&r.key),
            x: r.x,
            y: r.y,
            w: r.w,
            h: r.h,
            dpi: r.dpi,
            refresh_mhz: r.refresh_mhz,
            primary: r.primary,
            name: r.name,
        })
        .collect()
}

/// `DEVMODE` frequency 0/1 means "hardware default"; report 60 Hz then.
fn refresh_to_mhz(hz: u32) -> u32 {
    if hz <= 1 { 60_000 } else { hz * 1000 }
}

fn wide_to_string(w: &[u16]) -> String {
    let end = w.iter().position(|c| *c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..end])
}

/// Maps GDI device names (`\\.\DISPLAY1`) to the monitor's friendly name via the display
/// configuration API ("DELL U2723QE"). Empty on any failure.
fn friendly_names() -> HashMap<String, String> {
    let mut map = HashMap::new();
    // SAFETY: standard two-call QueryDisplayConfig pattern with correctly sized buffers; the
    // device-info structs are fully initialised with their header size/type before the call.
    unsafe {
        let (mut np, mut nm) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm).is_err() {
            return map;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
        if QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut np,
            paths.as_mut_ptr(),
            &mut nm,
            modes.as_mut_ptr(),
            None,
        )
        .is_err()
        {
            return map;
        }
        for p in paths.iter().take(np as usize) {
            let mut src = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
            src.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
            src.header.size = size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
            src.header.adapterId = p.sourceInfo.adapterId;
            src.header.id = p.sourceInfo.id;
            if DisplayConfigGetDeviceInfo(&mut src.header) != 0 {
                continue;
            }
            let mut tgt = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
            tgt.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
            tgt.header.size = size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
            tgt.header.adapterId = p.targetInfo.adapterId;
            tgt.header.id = p.targetInfo.id;
            if DisplayConfigGetDeviceInfo(&mut tgt.header) != 0 {
                continue;
            }
            let gdi = wide_to_string(&src.viewGdiDeviceName);
            let friendly = wide_to_string(&tgt.monitorFriendlyDeviceName);
            if !gdi.is_empty() && !friendly.is_empty() {
                map.insert(gdi, friendly);
            }
        }
    }
    map
}

unsafe extern "system" fn monitor_enum(
    hmon: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> BOOL {
    // SAFETY: `data` is the `&mut Vec<HMONITOR>` passed by `read_raw_screens`.
    let list = unsafe { &mut *(data.0 as *mut Vec<HMONITOR>) };
    list.push(hmon);
    TRUE
}

fn read_raw_screens() -> Vec<RawScreen> {
    let mut mons: Vec<HMONITOR> = Vec::new();
    // SAFETY: the callback only pushes into `mons`, which outlives the call.
    unsafe {
        let _ = EnumDisplayMonitors(
            HDC::default(),
            None,
            Some(monitor_enum),
            LPARAM((&mut mons as *mut Vec<HMONITOR>) as isize),
        );
    }
    let names = friendly_names();
    let mut out = Vec::new();
    for hmon in mons {
        let mut mi = MONITORINFOEXW::default();
        mi.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        // SAFETY: MONITORINFOEXW starts with a MONITORINFO and cbSize announces the full size.
        let ok = unsafe {
            GetMonitorInfoW(hmon, (&mut mi as *mut MONITORINFOEXW).cast::<MONITORINFO>())
        };
        if !ok.as_bool() {
            continue;
        }
        let gdi = wide_to_string(&mi.szDevice);
        let rc = mi.monitorInfo.rcMonitor;
        let (mut x, mut y) = (rc.left, rc.top);
        let (mut w, mut h) = (
            (rc.right - rc.left).max(0) as u32,
            (rc.bottom - rc.top).max(0) as u32,
        );
        let mut hz = 0u32;

        let mut dm = DEVMODEW {
            dmSize: size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        // SAFETY: `mi.szDevice` is NUL-terminated; `dm` has dmSize set.
        if unsafe {
            EnumDisplaySettingsW(PCWSTR(mi.szDevice.as_ptr()), ENUM_CURRENT_SETTINGS, &mut dm)
        }
        .as_bool()
            && dm.dmPelsWidth > 0
            && dm.dmPelsHeight > 0
        {
            // SAFETY: the display variant of the DEVMODE union is the active one for displays.
            let pos = unsafe { dm.Anonymous1.Anonymous2.dmPosition };
            x = pos.x;
            y = pos.y;
            w = dm.dmPelsWidth;
            h = dm.dmPelsHeight;
            hz = dm.dmDisplayFrequency;
        }

        let (mut dx, mut dy) = (0u32, 0u32);
        // SAFETY: valid out pointers.
        let dpi = if unsafe { GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) }.is_ok()
            && dx > 0
        {
            dx
        } else {
            96
        };

        // Monitor-level device entry: stable PnP id + generic name.
        let mut dd = DISPLAY_DEVICEW {
            cb: size_of::<DISPLAY_DEVICEW>() as u32,
            ..Default::default()
        };
        // SAFETY: valid NUL-terminated device name and initialised out struct.
        let have_dd =
            unsafe { EnumDisplayDevicesW(PCWSTR(mi.szDevice.as_ptr()), 0, &mut dd, 0) }.as_bool();
        let pnp_id = if have_dd {
            wide_to_string(&dd.DeviceID)
        } else {
            String::new()
        };
        let generic = if have_dd {
            wide_to_string(&dd.DeviceString)
        } else {
            String::new()
        };
        let key = if pnp_id.is_empty() {
            gdi.clone()
        } else {
            pnp_id
        };
        let name = names
            .get(&gdi)
            .cloned()
            .or_else(|| (!generic.is_empty()).then_some(generic))
            .unwrap_or_else(|| gdi.clone());

        out.push(RawScreen {
            key,
            x,
            y,
            w,
            h,
            dpi,
            refresh_mhz: refresh_to_mhz(hz),
            primary: mi.monitorInfo.dwFlags & 1 != 0, // MONITORINFOF_PRIMARY
            name,
        });
    }
    out
}

fn snapshot() -> Vec<ScreenInfo> {
    let raw = read_raw_screens();
    let mut guard = IDS.lock();
    let path = ids_path();
    let ids = guard.get_or_insert_with(|| IdTable::load(&path));
    let known = ids.keys.len();
    let out = finalize(raw, ids);
    if ids.keys.len() != known {
        ids.save(&path);
    }
    out
}

struct WatchCtx {
    tx: UnboundedSender<()>,
    last: Vec<ScreenInfo>,
}

unsafe extern "system" fn watch_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_NCCREATE => {
            // SAFETY: lParam belongs to this WM_NCCREATE.
            unsafe { attach_userdata(hwnd, lparam) };
            // SAFETY: forwarding unmodified arguments.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_DISPLAYCHANGE | WM_DPICHANGED | WM_SETTINGCHANGE => {
            // (Re-)arm the debounce timer: only the last message of a burst triggers.
            // SAFETY: own window.
            unsafe { SetTimer(hwnd, TIMER_DEBOUNCE, DEBOUNCE_MS, None) };
            // SAFETY: forwarding unmodified arguments.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_TIMER if wparam.0 == TIMER_DEBOUNCE => {
            // SAFETY: own window; the ctx pointer was stored at creation and outlives the window.
            unsafe {
                let _ = KillTimer(hwnd, TIMER_DEBOUNCE);
                let ctx = userdata::<WatchCtx>(hwnd);
                if !ctx.is_null() {
                    // WM_SETTINGCHANGE also fires for unrelated settings: only report real changes.
                    let now = snapshot();
                    if now != (*ctx).last {
                        (*ctx).last = now;
                        let _ = (*ctx).tx.send(());
                    }
                }
            }
            LRESULT(0)
        }
        WM_APP_STOP => {
            // SAFETY: destroying our own window from its thread.
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            // SAFETY: ends this thread's message loop.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // SAFETY: forwarding unmodified arguments.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

struct Watcher {
    hwnd: isize,
    thread: JoinHandle<()>,
}

/// Windows implementation of [`ScreenProvider`].
#[derive(Default)]
pub struct WinScreenProvider {
    watchers: Arc<Mutex<Vec<Watcher>>>,
}

impl WinScreenProvider {
    #[must_use]
    pub fn new() -> Self {
        init_dpi_awareness();
        Self::default()
    }
}

impl ScreenProvider for WinScreenProvider {
    fn screens(&self) -> Vec<ScreenInfo> {
        snapshot()
    }

    fn cursor_pos(&self) -> Point {
        let mut p = POINT::default();
        // SAFETY: valid out pointer.
        if unsafe { GetCursorPos(&mut p) }.is_ok() {
            Point { x: p.x, y: p.y }
        } else {
            Point::default()
        }
    }

    fn watch(&self, tx: UnboundedSender<()>) -> Result<()> {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<std::result::Result<isize, String>>();
        let thread = std::thread::Builder::new()
            .name("db-display-watch".into())
            .spawn(move || {
                let ctx = Box::into_raw(Box::new(WatchCtx {
                    tx,
                    last: snapshot(),
                }));
                // SAFETY: `ctx` stays alive until after the message loop ended (freed below).
                let hwnd = unsafe {
                    create_hidden_window(
                        "DeskBridgeDisplayWatch",
                        watch_wndproc,
                        ctx as *const c_void,
                        false,
                    )
                };
                match hwnd {
                    Ok(h) => {
                        let _ = ready_tx.send(Ok(h.0 as isize));
                        run_message_loop();
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(format!("display watch window: {e}")));
                    }
                }
                // SAFETY: the window is gone, nobody uses the pointer any more.
                drop(unsafe { Box::from_raw(ctx) });
            })
            .map_err(|e| PlatformError::other(format!("spawn display watcher: {e}")))?;
        match ready_rx.recv() {
            Ok(Ok(hwnd)) => {
                self.watchers.lock().push(Watcher { hwnd, thread });
                Ok(())
            }
            Ok(Err(msg)) => {
                let _ = thread.join();
                Err(PlatformError::other(msg))
            }
            Err(_) => {
                let _ = thread.join();
                Err(PlatformError::other("display watcher died during setup"))
            }
        }
    }
}

impl Drop for WinScreenProvider {
    fn drop(&mut self) {
        for w in self.watchers.lock().drain(..) {
            // SAFETY: posting to our own window; failure means it is already gone.
            unsafe {
                let _ = PostMessageW(
                    HWND(w.hwnd as *mut c_void),
                    WM_APP_STOP,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
            let _ = w.thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(key: &str, x: i32, y: i32, primary: bool) -> RawScreen {
        RawScreen {
            key: key.into(),
            x,
            y,
            w: 1920,
            h: 1080,
            dpi: 96,
            refresh_mhz: 60_000,
            primary,
            name: key.into(),
        }
    }

    #[test]
    fn primary_first_then_by_position() {
        let mut ids = IdTable::new();
        let out = finalize(
            vec![
                raw("c", 3840, 0, false),
                raw("a", 0, 0, true),
                raw("b", -1920, 0, false),
            ],
            &mut ids,
        );
        let order: Vec<_> = out.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(order, ["a", "b", "c"]);
        assert!(out[0].primary);
        assert_eq!(out.iter().map(|s| s.id).collect::<Vec<_>>(), [0, 1, 2]);
    }

    #[test]
    fn ids_stay_stable_when_monitors_are_reordered() {
        let mut ids = IdTable::new();
        let first = finalize(
            vec![raw("a", 0, 0, true), raw("b", 1920, 0, false)],
            &mut ids,
        );
        assert_eq!((first[0].id, first[1].id), (0, 1));
        // b becomes primary and moves left of a.
        let second = finalize(
            vec![raw("a", 1920, 0, false), raw("b", 0, 0, true)],
            &mut ids,
        );
        let id_of = |name: &str| second.iter().find(|s| s.name == name).unwrap().id;
        assert_eq!(id_of("a"), 0);
        assert_eq!(id_of("b"), 1);
        // a new monitor gets the next free id; an unplugged one keeps its slot reserved
        let third = finalize(vec![raw("c", 0, 0, true)], &mut ids);
        assert_eq!(third[0].id, 2);
    }

    #[test]
    fn refresh_conversion() {
        assert_eq!(refresh_to_mhz(144), 144_000);
        assert_eq!(refresh_to_mhz(60), 60_000);
        assert_eq!(refresh_to_mhz(0), 60_000);
        assert_eq!(refresh_to_mhz(1), 60_000);
    }

    #[test]
    fn wide_string_helper() {
        let mut buf = [0u16; 8];
        for (i, c) in "AB".encode_utf16().enumerate() {
            buf[i] = c;
        }
        assert_eq!(wide_to_string(&buf), "AB");
        assert_eq!(wide_to_string(&[]), "");
    }

    #[test]
    fn real_screens_are_plausible() {
        let p = WinScreenProvider::new();
        let screens = p.screens();
        assert!(!screens.is_empty(), "no monitors reported");
        assert_eq!(screens.iter().filter(|s| s.primary).count(), 1);
        assert!(screens[0].primary, "primary must come first");
        let mut ids: Vec<_> = screens.iter().map(|s| s.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), screens.len(), "ids must be unique");
        for s in &screens {
            assert!(s.w >= 320 && s.w <= 16384, "{s:?}");
            assert!(s.h >= 200 && s.h <= 16384, "{s:?}");
            assert!((48..=960).contains(&s.dpi), "{s:?}");
            assert!((24_000..=1_000_000).contains(&s.refresh_mhz), "{s:?}");
            assert!(!s.name.is_empty());
        }
        eprintln!("screens: {screens:#?}");
        // Ids are stable across calls.
        assert_eq!(screens, p.screens());
        // The cursor lies on some screen (when there is an interactive desktop).
        let c = p.cursor_pos();
        if screens.iter().any(|s| s.contains(c)) {
            return;
        }
        eprintln!("cursor {c:?} not on a known screen (headless session?)");
    }

    #[test]
    fn watcher_starts_and_stops() {
        let p = WinScreenProvider::new();
        let (tx, mut rx) = deskbridge_platform::unbounded_channel();
        p.watch(tx).unwrap();
        // No display change happens: nothing is reported.
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(rx.try_recv().is_err());
        drop(p); // joins the watcher thread
    }
}
