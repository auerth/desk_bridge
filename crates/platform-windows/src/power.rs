//! Power events, network change notifications, lid action and keep-awake.
//!
//! * `watch` runs a hidden *top-level* window (message-only windows do not get broadcasts such as
//!   `WM_POWERBROADCAST`) that translates suspend/resume, lid switch changes and IP changes into
//!   [`PowerEvent`]s. Network notifications (`NotifyIpInterfaceChange` + `NotifyUnicastIpAddressChange`,
//!   IPv4) arrive on thread-pool threads, are forwarded to the window thread with `PostMessage`
//!   and debounced there (500 ms).
//! * `set_lid_action_do_nothing` rewrites the lid-close action of the active power scheme (AC and DC)
//!   and keeps the original values in `%LOCALAPPDATA%\softwelop\DeskBridge\lid-backup.json`, so they
//!   can be restored even after a crash: call [`restore_lid_action_if_backup_exists`] on agent start-up
//!   and from the uninstaller.
//! * `set_keep_awake` needs a *persistent* thread because `SetThreadExecutionState(ES_CONTINUOUS)` is
//!   tied to the calling thread; a small dedicated thread owns it (system + display required).

use std::cell::RefCell;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use deskbridge_platform::power::{PowerEvent, PowerManager};
use deskbridge_platform::{PlatformError, Result, UnboundedSender};
use parking_lot::Mutex;
use windows::Win32::Foundation::{
    ERROR_SUCCESS, HANDLE, HLOCAL, HWND, LPARAM, LRESULT, LocalFree, WPARAM,
};
use windows::Win32::NetworkManagement::IpHelper::{
    CancelMibChangeNotify2, MIB_IPINTERFACE_ROW, MIB_NOTIFICATION_TYPE, MIB_UNICASTIPADDRESS_ROW,
    NotifyIpInterfaceChange, NotifyUnicastIpAddressChange,
};
use windows::Win32::Networking::WinSock::{AF_INET, AF_UNSPEC};
use windows::Win32::System::Power::{
    ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, HPOWERNOTIFY, POWERBROADCAST_SETTING,
    PowerGetActiveScheme, PowerReadACValueIndex, PowerReadDCValueIndex, PowerSetActiveScheme,
    PowerWriteACValueIndex, PowerWriteDCValueIndex, RegisterPowerSettingNotification,
    SetThreadExecutionState, UnregisterPowerSettingNotification,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DEVICE_NOTIFY_WINDOW_HANDLE, DefWindowProcW, DestroyWindow, KillTimer, PostMessageW,
    PostQuitMessage, SetTimer, WM_APP, WM_DESTROY, WM_NCCREATE, WM_POWERBROADCAST, WM_TIMER,
};
use windows::core::GUID;

use crate::input::{attach_userdata, create_hidden_window, run_message_loop, userdata};

const WM_APP_STOP: u32 = WM_APP + 1;
const WM_APP_NET: u32 = WM_APP + 2;
const TIMER_NET: usize = 1;
/// Quiet period for network change bursts.
pub const NET_DEBOUNCE_MS: u32 = 500;
/// Resume is announced twice by Windows (`RESUMEAUTOMATIC` and `RESUMESUSPEND`).
const RESUME_DEDUP: Duration = Duration::from_secs(3);

const PBT_APMSUSPEND: usize = 0x0004;
const PBT_APMRESUMESUSPEND: usize = 0x0007;
const PBT_APMRESUMEAUTOMATIC: usize = 0x0012;
const PBT_POWERSETTINGCHANGE: usize = 0x8013;

/// `GUID_LIDSWITCH_STATE_CHANGE` {BA3E0F4D-B817-4094-A2D1-D56379E6A0F3}: 0 = closed, 1 = open.
const GUID_LIDSWITCH_STATE_CHANGE: GUID = GUID::from_u128(0xBA3E0F4D_B817_4094_A2D1_D56379E6A0F3);
/// `GUID_SYSTEM_BUTTON_SUBGROUP` (SUB_BUTTONS) {4F971E89-EEBD-4455-A8DE-9E59040E7347}.
const GUID_SYSTEM_BUTTON_SUBGROUP: GUID = GUID::from_u128(0x4F971E89_EEBD_4455_A8DE_9E59040E7347);
/// `GUID_LIDCLOSE_ACTION` {5CA83367-6E45-459F-A27B-476B1D01C936}; value 0 = do nothing.
const GUID_LIDCLOSE_ACTION: GUID = GUID::from_u128(0x5CA83367_6E45_459F_A27B_476B1D01C936);

// ---------------------------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------------------------

/// Parses the payload of a `GUID_LIDSWITCH_STATE_CHANGE` notification; `Some(true)` = closed.
fn parse_lid_closed(data: &[u8]) -> Option<bool> {
    match data.len() {
        0 => None,
        1..=3 => Some(data[0] == 0),
        _ => Some(u32::from_le_bytes([data[0], data[1], data[2], data[3]]) == 0),
    }
}

/// Turns repeated lid states into events (initial assumption: lid open).
#[derive(Debug)]
struct LidTracker {
    closed: bool,
}

impl LidTracker {
    fn new() -> Self {
        Self { closed: false }
    }

    fn update(&mut self, closed: bool) -> Option<PowerEvent> {
        if closed == self.closed {
            return None;
        }
        self.closed = closed;
        Some(if closed {
            PowerEvent::LidClosed
        } else {
            PowerEvent::LidOpened
        })
    }
}

/// Drops the second of the two resume notifications.
#[derive(Debug, Default)]
struct ResumeDedup {
    last: Option<Instant>,
}

impl ResumeDedup {
    fn accept(&mut self, now: Instant) -> bool {
        if self
            .last
            .is_some_and(|l| now.duration_since(l) < RESUME_DEDUP)
        {
            return false;
        }
        self.last = Some(now);
        true
    }
}

// ---------------------------------------------------------------------------------------------
// Lid action with backup
// ---------------------------------------------------------------------------------------------

/// Original lid-close action values of one power scheme.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LidBackup {
    scheme: GUID,
    ac: u32,
    dc: u32,
}

fn guid_to_string(g: &GUID) -> String {
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        g.data1,
        g.data2,
        g.data3,
        g.data4[0],
        g.data4[1],
        g.data4[2],
        g.data4[3],
        g.data4[4],
        g.data4[5],
        g.data4[6],
        g.data4[7]
    )
}

fn guid_from_string(s: &str) -> Option<GUID> {
    let hex: String = s
        .chars()
        .filter(|c| !matches!(c, '{' | '}' | '-'))
        .collect();
    if hex.len() != 32 {
        return None;
    }
    u128::from_str_radix(&hex, 16).ok().map(GUID::from_u128)
}

impl LidBackup {
    fn to_json(&self) -> String {
        format!(
            "{{\"scheme\":\"{}\",\"ac\":{},\"dc\":{}}}\n",
            guid_to_string(&self.scheme),
            self.ac,
            self.dc
        )
    }

    fn from_json(s: &str) -> Option<Self> {
        let field = |name: &str| -> Option<String> {
            let pat = format!("\"{name}\"");
            let rest = &s[s.find(&pat)? + pat.len()..];
            let rest = rest[rest.find(':')? + 1..].trim_start();
            let end = rest.find([',', '}']).unwrap_or(rest.len());
            Some(rest[..end].trim().trim_matches('"').to_owned())
        };
        Some(Self {
            scheme: guid_from_string(&field("scheme")?)?,
            ac: field("ac")?.parse().ok()?,
            dc: field("dc")?.parse().ok()?,
        })
    }

    fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, self.to_json())?;
        std::fs::rename(&tmp, path)
    }

    fn load(path: &Path) -> Option<Self> {
        Self::from_json(&std::fs::read_to_string(path).ok()?)
    }
}

/// Location of the lid backup file.
#[must_use]
pub fn lid_backup_path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map_or_else(std::env::temp_dir, PathBuf::from);
    base.join("softwelop")
        .join("DeskBridge")
        .join("lid-backup.json")
}

/// Access to the power scheme settings (abstracted for tests).
trait LidOs {
    fn active_scheme(&self) -> Result<GUID>;
    fn read(&self, scheme: &GUID) -> Result<(u32, u32)>;
    fn write(&self, scheme: &GUID, ac: u32, dc: u32) -> Result<()>;
    /// Re-applies the scheme so changes take effect immediately.
    fn activate(&self, scheme: &GUID) -> Result<()>;
}

fn win_err(what: &str, code: u32) -> PlatformError {
    PlatformError::Os {
        code: i64::from(code),
        msg: format!("{what} failed"),
    }
}

struct RealLidOs;

impl LidOs for RealLidOs {
    fn active_scheme(&self) -> Result<GUID> {
        let mut p: *mut GUID = std::ptr::null_mut();
        // SAFETY: on success `p` points to a LocalAlloc'ed GUID that we copy and free.
        unsafe {
            let r = PowerGetActiveScheme(None, &mut p);
            if r != ERROR_SUCCESS || p.is_null() {
                return Err(win_err("PowerGetActiveScheme", r.0));
            }
            let g = *p;
            let _ = LocalFree(HLOCAL(p.cast()));
            Ok(g)
        }
    }

    fn read(&self, scheme: &GUID) -> Result<(u32, u32)> {
        let (mut ac, mut dc) = (0u32, 0u32);
        // SAFETY: valid GUID pointers and out values.
        unsafe {
            let r = PowerReadACValueIndex(
                None,
                Some(scheme),
                Some(&GUID_SYSTEM_BUTTON_SUBGROUP),
                Some(&GUID_LIDCLOSE_ACTION),
                &mut ac,
            );
            if r != ERROR_SUCCESS {
                return Err(win_err("PowerReadACValueIndex", r.0));
            }
            let r = PowerReadDCValueIndex(
                None,
                Some(scheme),
                Some(&GUID_SYSTEM_BUTTON_SUBGROUP),
                Some(&GUID_LIDCLOSE_ACTION),
                &mut dc,
            );
            if r != 0 {
                return Err(win_err("PowerReadDCValueIndex", r));
            }
        }
        Ok((ac, dc))
    }

    fn write(&self, scheme: &GUID, ac: u32, dc: u32) -> Result<()> {
        // SAFETY: valid GUID pointers.
        unsafe {
            let r = PowerWriteACValueIndex(
                None,
                scheme,
                Some(&GUID_SYSTEM_BUTTON_SUBGROUP),
                Some(&GUID_LIDCLOSE_ACTION),
                ac,
            );
            if r != ERROR_SUCCESS {
                return Err(win_err("PowerWriteACValueIndex", r.0));
            }
            let r = PowerWriteDCValueIndex(
                None,
                scheme,
                Some(&GUID_SYSTEM_BUTTON_SUBGROUP),
                Some(&GUID_LIDCLOSE_ACTION),
                dc,
            );
            if r != 0 {
                return Err(win_err("PowerWriteDCValueIndex", r));
            }
        }
        Ok(())
    }

    fn activate(&self, scheme: &GUID) -> Result<()> {
        // SAFETY: valid GUID pointer.
        let r = unsafe { PowerSetActiveScheme(None, Some(scheme)) };
        if r == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(win_err("PowerSetActiveScheme", r.0))
        }
    }
}

fn io_err(e: &std::io::Error) -> PlatformError {
    PlatformError::other(format!("lid backup file: {e}"))
}

/// Restores the values saved in the backup (if any) and deletes it.
fn restore_lid(path: &Path, os: &dyn LidOs) -> Result<()> {
    let Some(b) = LidBackup::load(path) else {
        if path.exists() {
            // Corrupt file: nothing sensible to restore, do not keep it around forever.
            tracing::warn!("ignoring unreadable lid backup {}", path.display());
            let _ = std::fs::remove_file(path);
        }
        return Ok(());
    };
    os.write(&b.scheme, b.ac, b.dc)?;
    if let Ok(active) = os.active_scheme() {
        os.activate(&active)?;
    }
    std::fs::remove_file(path).map_err(|e| io_err(&e))?;
    Ok(())
}

/// Sets (`on`) the lid-close action of the active scheme to "do nothing" or restores the backup.
fn apply_lid_action(on: bool, path: &Path, os: &dyn LidOs) -> Result<()> {
    if !on {
        return restore_lid(path, os);
    }
    let active = os.active_scheme()?;
    match LidBackup::load(path) {
        Some(b) if b.scheme == active => {} // already modified by us: keep the original backup
        Some(_) => {
            // The user switched schemes while we were active: put the old one back first.
            restore_lid(path, os)?;
        }
        None => {}
    }
    if !path.exists() {
        let (ac, dc) = os.read(&active)?;
        LidBackup {
            scheme: active,
            ac,
            dc,
        }
        .save(path)
        .map_err(|e| io_err(&e))?;
    }
    os.write(&active, 0, 0)?;
    os.activate(&active)
}

/// Restores the lid-close action if a previous run left a backup behind (crash, kill,
/// uninstall). Call at agent start-up and from the uninstaller; a no-op without a backup.
pub fn restore_lid_action_if_backup_exists() -> Result<()> {
    restore_lid(&lid_backup_path(), &RealLidOs)
}

// ---------------------------------------------------------------------------------------------
// Keep awake
// ---------------------------------------------------------------------------------------------

struct KeepAwake {
    tx: Sender<bool>,
    thread: JoinHandle<()>,
}

impl KeepAwake {
    fn spawn() -> std::io::Result<Self> {
        let (tx, rx) = std::sync::mpsc::channel::<bool>();
        let thread = std::thread::Builder::new()
            .name("db-keep-awake".into())
            .spawn(move || {
                while let Ok(on) = rx.recv() {
                    let flags = if on {
                        ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED
                    } else {
                        ES_CONTINUOUS
                    };
                    // SAFETY: no preconditions; ES_CONTINUOUS state belongs to this thread.
                    unsafe { SetThreadExecutionState(flags) };
                }
                // Channel closed: the state is cleared when the thread ends; do it explicitly.
                // SAFETY: as above.
                unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
            })?;
        Ok(Self { tx, thread })
    }
}

// ---------------------------------------------------------------------------------------------
// Watcher window
// ---------------------------------------------------------------------------------------------

struct PowerCtx {
    tx: UnboundedSender<PowerEvent>,
    lid: RefCell<LidTracker>,
    resume: RefCell<ResumeDedup>,
}

impl PowerCtx {
    fn send(&self, ev: PowerEvent) {
        let _ = self.tx.send(ev);
    }
}

unsafe extern "system" fn power_wndproc(
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
        WM_POWERBROADCAST => {
            // SAFETY: the ctx pointer outlives the window.
            let ctx = unsafe { userdata::<PowerCtx>(hwnd).as_ref() };
            if let Some(ctx) = ctx {
                match wparam.0 {
                    PBT_APMSUSPEND => ctx.send(PowerEvent::Suspend),
                    PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND => {
                        if ctx.resume.borrow_mut().accept(Instant::now()) {
                            ctx.send(PowerEvent::Resume);
                        }
                    }
                    PBT_POWERSETTINGCHANGE if lparam.0 != 0 => {
                        // SAFETY: for this event lParam points to a POWERBROADCAST_SETTING
                        // followed by `DataLength` bytes of payload.
                        let (guid, data) = unsafe {
                            let s = &*(lparam.0 as *const POWERBROADCAST_SETTING);
                            let data =
                                std::slice::from_raw_parts(s.Data.as_ptr(), s.DataLength as usize);
                            (s.PowerSetting, data)
                        };
                        if guid == GUID_LIDSWITCH_STATE_CHANGE {
                            if let Some(closed) = parse_lid_closed(data) {
                                if let Some(ev) = ctx.lid.borrow_mut().update(closed) {
                                    ctx.send(ev);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            LRESULT(1) // TRUE: grant the request
        }
        WM_APP_NET => {
            // (Re-)arm: only the last notification of a burst fires the event.
            // SAFETY: own window.
            unsafe { SetTimer(hwnd, TIMER_NET, NET_DEBOUNCE_MS, None) };
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_NET => {
            // SAFETY: own window / ctx pointer outlives it.
            unsafe {
                let _ = KillTimer(hwnd, TIMER_NET);
                if let Some(ctx) = userdata::<PowerCtx>(hwnd).as_ref() {
                    ctx.send(PowerEvent::NetworkChanged);
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

/// Called on a thread-pool thread: only forwards to the window (thread-safe `PostMessage`).
fn notify_net(context: *const c_void) {
    // SAFETY: the context is the HWND value; the notification is cancelled before the window dies.
    unsafe {
        let _ = PostMessageW(
            HWND(context as *mut c_void),
            WM_APP_NET,
            WPARAM(0),
            LPARAM(0),
        );
    }
}

unsafe extern "system" fn ip_interface_cb(
    ctx: *const c_void,
    _row: *const MIB_IPINTERFACE_ROW,
    _t: MIB_NOTIFICATION_TYPE,
) {
    notify_net(ctx);
}

unsafe extern "system" fn ip_address_cb(
    ctx: *const c_void,
    _row: *const MIB_UNICASTIPADDRESS_ROW,
    _t: MIB_NOTIFICATION_TYPE,
) {
    notify_net(ctx);
}

struct Watcher {
    hwnd: isize,
    thread: JoinHandle<()>,
}

fn watcher_thread(
    tx: UnboundedSender<PowerEvent>,
    ready: std::sync::mpsc::Sender<std::result::Result<isize, String>>,
) {
    let ctx = Box::into_raw(Box::new(PowerCtx {
        tx,
        lid: RefCell::new(LidTracker::new()),
        resume: RefCell::new(ResumeDedup::default()),
    }));
    // SAFETY: `ctx` is freed only after the window and all notification registrations are gone.
    unsafe {
        let hwnd = match create_hidden_window(
            "DeskBridgePowerWatch",
            power_wndproc,
            ctx as *const c_void,
            false,
        ) {
            Ok(h) => h,
            Err(e) => {
                let _ = ready.send(Err(format!("power watch window: {e}")));
                drop(Box::from_raw(ctx));
                return;
            }
        };
        let lid_reg: Option<HPOWERNOTIFY> = RegisterPowerSettingNotification(
            HANDLE(hwnd.0),
            &GUID_LIDSWITCH_STATE_CHANGE,
            DEVICE_NOTIFY_WINDOW_HANDLE,
        )
        .map_err(|e| tracing::warn!("lid notification unavailable (desktop PC?): {e}"))
        .ok();

        let hwnd_ctx = hwnd.0.cast_const();
        let mut h_if = HANDLE::default();
        let r = NotifyIpInterfaceChange(
            AF_UNSPEC,
            Some(ip_interface_cb),
            Some(hwnd_ctx),
            false,
            &mut h_if,
        );
        let if_reg = (r == ERROR_SUCCESS).then_some(h_if);
        if if_reg.is_none() {
            tracing::warn!("NotifyIpInterfaceChange failed: {}", r.0);
        }
        let mut h_ip = HANDLE::default();
        let r = NotifyUnicastIpAddressChange(
            AF_INET,
            Some(ip_address_cb),
            Some(hwnd_ctx),
            false,
            &mut h_ip,
        );
        let ip_reg = (r == ERROR_SUCCESS).then_some(h_ip);
        if ip_reg.is_none() {
            tracing::warn!("NotifyUnicastIpAddressChange failed: {}", r.0);
        }

        let _ = ready.send(Ok(hwnd.0 as isize));
        run_message_loop();

        // Cancel first: CancelMibChangeNotify2 waits for running callbacks.
        for h in [if_reg, ip_reg].into_iter().flatten() {
            let _ = CancelMibChangeNotify2(h);
        }
        if let Some(h) = lid_reg {
            let _ = UnregisterPowerSettingNotification(h);
        }
        drop(Box::from_raw(ctx));
    }
}

/// Windows implementation of [`PowerManager`].
pub struct WinPowerManager {
    watchers: Mutex<Vec<Watcher>>,
    keep_awake: Mutex<Option<KeepAwake>>,
    lid_backup: PathBuf,
}

impl Default for WinPowerManager {
    fn default() -> Self {
        Self::new()
    }
}

impl WinPowerManager {
    #[must_use]
    pub fn new() -> Self {
        Self {
            watchers: Mutex::new(Vec::new()),
            keep_awake: Mutex::new(None),
            lid_backup: lid_backup_path(),
        }
    }
}

impl PowerManager for WinPowerManager {
    fn watch(&self, tx: UnboundedSender<PowerEvent>) -> Result<()> {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("db-power-watch".into())
            .spawn(move || watcher_thread(tx, ready_tx))
            .map_err(|e| PlatformError::other(format!("spawn power watcher: {e}")))?;
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
                Err(PlatformError::other("power watcher died during setup"))
            }
        }
    }

    fn set_lid_action_do_nothing(&self, on: bool) -> Result<()> {
        apply_lid_action(on, &self.lid_backup, &RealLidOs)
    }

    fn set_keep_awake(&self, on: bool) {
        let mut slot = self.keep_awake.lock();
        if slot.is_none() {
            if !on {
                return;
            }
            match KeepAwake::spawn() {
                Ok(k) => *slot = Some(k),
                Err(e) => {
                    tracing::warn!("keep-awake thread failed to start: {e}");
                    return;
                }
            }
        }
        if let Some(k) = slot.as_ref() {
            let _ = k.tx.send(on);
        }
    }
}

impl Drop for WinPowerManager {
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
        if let Some(k) = self.keep_awake.lock().take() {
            drop(k.tx);
            let _ = k.thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[derive(Default)]
    struct FakeOs {
        active: RefCell<GUID>,
        /// scheme -> (ac, dc)
        values: RefCell<Vec<(GUID, (u32, u32))>>,
        activations: Cell<u32>,
    }

    impl FakeOs {
        fn with_scheme(scheme: GUID, ac: u32, dc: u32) -> Self {
            Self {
                active: RefCell::new(scheme),
                values: RefCell::new(vec![(scheme, (ac, dc))]),
                activations: Cell::new(0),
            }
        }

        fn get(&self, s: &GUID) -> (u32, u32) {
            self.values.borrow().iter().find(|(g, _)| g == s).unwrap().1
        }
    }

    impl LidOs for FakeOs {
        fn active_scheme(&self) -> Result<GUID> {
            Ok(*self.active.borrow())
        }

        fn read(&self, s: &GUID) -> Result<(u32, u32)> {
            Ok(self.get(s))
        }

        fn write(&self, s: &GUID, ac: u32, dc: u32) -> Result<()> {
            let mut v = self.values.borrow_mut();
            if let Some(e) = v.iter_mut().find(|(g, _)| g == s) {
                e.1 = (ac, dc);
            } else {
                v.push((*s, (ac, dc)));
            }
            Ok(())
        }

        fn activate(&self, _s: &GUID) -> Result<()> {
            self.activations.set(self.activations.get() + 1);
            Ok(())
        }
    }

    const S1: GUID = GUID::from_u128(0x381B4222_F694_41F0_9685_FF5BB260DF2E);
    const S2: GUID = GUID::from_u128(0x8C5E7FDA_E8BF_4A96_9A85_A6E23A8C635C);

    #[test]
    fn guid_string_round_trip() {
        let s = guid_to_string(&S1);
        assert_eq!(s, "{381B4222-F694-41F0-9685-FF5BB260DF2E}");
        assert_eq!(guid_from_string(&s), Some(S1));
        assert_eq!(
            guid_from_string("381b4222-f694-41f0-9685-ff5bb260df2e"),
            Some(S1)
        );
        assert_eq!(guid_from_string("nope"), None);
    }

    #[test]
    fn backup_json_round_trip_and_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a").join("b").join("lid-backup.json");
        let b = LidBackup {
            scheme: S1,
            ac: 1,
            dc: 3,
        };
        b.save(&path).unwrap();
        assert_eq!(LidBackup::load(&path), Some(b.clone()));
        assert_eq!(LidBackup::from_json(&b.to_json()), Some(b));
        assert_eq!(LidBackup::load(&dir.path().join("missing.json")), None);
        std::fs::write(&path, "garbage").unwrap();
        assert_eq!(LidBackup::load(&path), None);
    }

    #[test]
    fn lid_on_off_restores_original_values() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lid-backup.json");
        let os = FakeOs::with_scheme(S1, 1, 3);

        apply_lid_action(true, &path, &os).unwrap();
        assert_eq!(os.get(&S1), (0, 0));
        assert_eq!(
            LidBackup::load(&path),
            Some(LidBackup {
                scheme: S1,
                ac: 1,
                dc: 3
            })
        );

        // Second "on" must not overwrite the original backup with the modified values.
        apply_lid_action(true, &path, &os).unwrap();
        assert_eq!(
            LidBackup::load(&path),
            Some(LidBackup {
                scheme: S1,
                ac: 1,
                dc: 3
            })
        );

        apply_lid_action(false, &path, &os).unwrap();
        assert_eq!(os.get(&S1), (1, 3));
        assert!(!path.exists());
        assert!(os.activations.get() >= 3);

        // Off without backup is a no-op.
        apply_lid_action(false, &path, &os).unwrap();
        assert_eq!(os.get(&S1), (1, 3));
    }

    #[test]
    fn lid_survives_a_crash_via_the_backup_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lid-backup.json");
        let os = FakeOs::with_scheme(S1, 2, 2);
        apply_lid_action(true, &path, &os).unwrap();
        // "Crash": a new process only has the file and the OS state.
        restore_lid(&path, &os).unwrap();
        assert_eq!(os.get(&S1), (2, 2));
        assert!(!path.exists());
    }

    #[test]
    fn scheme_switch_restores_the_old_scheme_first() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lid-backup.json");
        let os = FakeOs::with_scheme(S1, 1, 1);
        os.write(&S2, 2, 2).unwrap();
        apply_lid_action(true, &path, &os).unwrap();
        *os.active.borrow_mut() = S2;
        apply_lid_action(true, &path, &os).unwrap();
        assert_eq!(os.get(&S1), (1, 1), "old scheme restored");
        assert_eq!(os.get(&S2), (0, 0));
        assert_eq!(
            LidBackup::load(&path),
            Some(LidBackup {
                scheme: S2,
                ac: 2,
                dc: 2
            })
        );
        apply_lid_action(false, &path, &os).unwrap();
        assert_eq!(os.get(&S2), (2, 2));
    }

    #[test]
    fn corrupt_backup_is_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lid-backup.json");
        std::fs::write(&path, "{broken").unwrap();
        let os = FakeOs::with_scheme(S1, 1, 1);
        restore_lid(&path, &os).unwrap();
        assert!(!path.exists());
        assert_eq!(os.get(&S1), (1, 1));
    }

    #[test]
    fn lid_payload_parsing_and_tracking() {
        assert_eq!(parse_lid_closed(&[]), None);
        assert_eq!(parse_lid_closed(&[0, 0, 0, 0]), Some(true));
        assert_eq!(parse_lid_closed(&[1, 0, 0, 0]), Some(false));
        assert_eq!(parse_lid_closed(&[0]), Some(true));
        let mut t = LidTracker::new();
        assert_eq!(t.update(false), None, "initial open state is silent");
        assert_eq!(t.update(true), Some(PowerEvent::LidClosed));
        assert_eq!(t.update(true), None);
        assert_eq!(t.update(false), Some(PowerEvent::LidOpened));
    }

    #[test]
    fn resume_is_deduplicated() {
        let mut d = ResumeDedup::default();
        let t0 = Instant::now();
        assert!(d.accept(t0));
        assert!(!d.accept(t0 + Duration::from_secs(1)));
        assert!(d.accept(t0 + Duration::from_secs(10)));
    }

    #[test]
    fn real_lid_scheme_is_readable() {
        // Read-only OS check: the active scheme and its lid action can be queried.
        let os = RealLidOs;
        let scheme = os.active_scheme().unwrap();
        let (ac, dc) = os.read(&scheme).unwrap();
        eprintln!(
            "lid action: scheme={} ac={ac} dc={dc}",
            guid_to_string(&scheme)
        );
        assert!(ac <= 4 && dc <= 4);
    }

    #[test]
    fn watcher_starts_and_stops() {
        let pm = WinPowerManager::new();
        let (tx, mut rx) = deskbridge_platform::unbounded_channel();
        pm.watch(tx).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        // The initial lid notification (open) is silent.
        while let Ok(ev) = rx.try_recv() {
            assert!(
                ev != PowerEvent::Suspend && ev != PowerEvent::LidClosed,
                "{ev:?}"
            );
        }
        drop(pm);
    }

    #[test]
    fn keep_awake_thread_lifecycle() {
        // ES_CONTINUOUS|ES_SYSTEM_REQUIRED is cleared again before the test ends.
        let pm = WinPowerManager::new();
        pm.set_keep_awake(false); // no thread yet: no-op
        pm.set_keep_awake(true);
        pm.set_keep_awake(false);
        drop(pm);
    }

    #[test]
    #[ignore = "modifies the active power scheme (restores it afterwards); run manually"]
    fn manual_lid_action_round_trip() {
        let pm = WinPowerManager::new();
        let os = RealLidOs;
        let scheme = os.active_scheme().unwrap();
        let before = os.read(&scheme).unwrap();
        pm.set_lid_action_do_nothing(true).unwrap();
        assert_eq!(os.read(&scheme).unwrap(), (0, 0));
        pm.set_lid_action_do_nothing(false).unwrap();
        assert_eq!(os.read(&scheme).unwrap(), before);
    }

    #[test]
    #[ignore = "needs a real suspend/resume or lid close; run manually and watch the log"]
    fn manual_watch_prints_events() {
        let pm = WinPowerManager::new();
        let (tx, mut rx) = deskbridge_platform::unbounded_channel();
        pm.watch(tx).unwrap();
        let end = Instant::now() + Duration::from_secs(60);
        while Instant::now() < end {
            while let Ok(ev) = rx.try_recv() {
                eprintln!("{ev:?}");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}
