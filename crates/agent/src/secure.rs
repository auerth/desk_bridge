//! Agent side of the Windows service (milestone M6).
//!
//! * [`ServiceClient`] talks to `\\.\pipe\deskbridge-service` (`ping`, `send_sas`).
//! * [`SecureDesktopInjector`] injects with the normal injector while the user's desktop has the
//!   input focus and forwards to the SYSTEM helper (`\\.\pipe\deskbridge-helper`) while the
//!   secure desktop (UAC consent, lock screen, logon) is active.
//! * [`release_stuck_keys`] clears keys that a crashed predecessor left pressed.

use deskbridge_platform::input::{Button, InputInjector};
use deskbridge_platform::system::SystemServices;
use deskbridge_platform::{PlatformError, Result};
use deskbridge_platform_windows::input::INJECT_MARKER;
use deskbridge_service::client::PipeClient;
use deskbridge_service::desktop::{self, DesktopKind};
use deskbridge_service::helper::apply;
use deskbridge_service::protocol::{ErrorCode, HELPER_PIPE, Request, Response, SERVICE_PIPE};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, MAP_VIRTUAL_KEY_TYPE, MOUSE_EVENT_FLAGS,
    MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_XUP, MOUSEINPUT,
    MapVirtualKeyW, SendInput, VIRTUAL_KEY,
};

/// How long a desktop probe result is trusted (the probe opens a desktop handle).
const DESKTOP_CACHE: Duration = Duration::from_millis(100);
/// Pipe timeouts: injection must never stall the engine for long.
const SEND_TIMEOUT: Duration = Duration::from_millis(250);
const CONNECT_TIMEOUT: Duration = Duration::from_millis(100);
const CALL_TIMEOUT: Duration = Duration::from_millis(2500);
/// Minimum spacing of helper reconnect attempts while it is unreachable.
const RECONNECT_SPACING: Duration = Duration::from_secs(2);
/// `available()` answers are cached this long.
const AVAILABLE_CACHE: Duration = Duration::from_secs(3);

// ---------------------------------------------------------------------------------------------
// Service client
// ---------------------------------------------------------------------------------------------

/// Blocking client of the service control pipe; implements [`SystemServices`].
pub struct ServiceClient {
    pipe: String,
    conn: Mutex<Option<PipeClient>>,
    available: Mutex<Option<(Instant, bool)>>,
}

impl ServiceClient {
    #[must_use]
    pub fn new() -> Self {
        Self::with_pipe(SERVICE_PIPE)
    }

    #[must_use]
    pub fn with_pipe(pipe: &str) -> Self {
        Self {
            pipe: pipe.to_string(),
            conn: Mutex::new(None),
            available: Mutex::new(None),
        }
    }

    /// One request/response; reconnects once if the cached connection went stale.
    fn call(&self, req: &Request) -> std::io::Result<Response> {
        let mut guard = self
            .conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for attempt in 0..2 {
            if guard.is_none() {
                *guard = Some(PipeClient::connect(&self.pipe, CONNECT_TIMEOUT)?);
            }
            let Some(c) = guard.as_mut() else { continue };
            match c.call(req, CALL_TIMEOUT) {
                Ok(r) => return Ok(r),
                Err(e) => {
                    *guard = None;
                    if attempt == 1 || e.kind() == std::io::ErrorKind::TimedOut {
                        return Err(e);
                    }
                }
            }
        }
        Err(std::io::ErrorKind::BrokenPipe.into())
    }
}

impl Default for ServiceClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Maps a service error answer to the platform error the engine shows.
fn sas_error(code: ErrorCode, text: &str) -> PlatformError {
    match code {
        ErrorCode::SasNotAllowed => PlatformError::Other(
            "Strg+Alt+Entf ist nicht erlaubt: Richtlinie SoftwareSASGeneration muss 1 oder 3 sein"
                .to_string(),
        ),
        ErrorCode::Unsupported | ErrorCode::Disabled => {
            PlatformError::Other(format!("Strg+Alt+Entf nicht verfügbar: {text}"))
        }
        ErrorCode::BadRequest | ErrorCode::Failed => PlatformError::Other(text.to_string()),
    }
}

impl SystemServices for ServiceClient {
    fn available(&self) -> bool {
        let cache = self
            .available
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((at, v)) = *cache
            && at.elapsed() < AVAILABLE_CACHE
        {
            return v;
        }
        drop(cache);
        let ok = matches!(self.call(&Request::Ping), Ok(Response::Pong { .. }));
        *self
            .available
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((Instant::now(), ok));
        ok
    }

    fn send_sas(&self) -> Result<()> {
        match self.call(&Request::SendSas) {
            Ok(Response::Ok) => Ok(()),
            Ok(Response::Error { code, text }) => Err(sas_error(code, &text)),
            Ok(other) => Err(PlatformError::Other(format!(
                "unexpected service answer: {other:?}"
            ))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(PlatformError::Unsupported(
                "the Desk Bridge service is not running",
            )),
            Err(e) => Err(PlatformError::Other(format!("service pipe: {e}"))),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Secure-desktop injector
// ---------------------------------------------------------------------------------------------

/// Which desktop currently receives input (mockable).
pub trait DesktopProbe: Send + Sync {
    fn kind(&self) -> DesktopKind;
}

/// Real probe: `OpenInputDesktop` + name.
pub struct SystemProbe;

impl DesktopProbe for SystemProbe {
    fn kind(&self) -> DesktopKind {
        desktop::probe_input_desktop().kind
    }
}

/// Connection to the helper (mockable).
pub trait HelperLink: Send + Sync {
    /// `false` when the helper cannot be reached.
    fn send(&self, req: &Request) -> bool;
}

/// Real link over `\\.\pipe\deskbridge-helper`, reconnecting lazily with a spacing between
/// attempts so an absent helper costs nothing on the hot path.
pub struct PipeHelperLink {
    pipe: String,
    conn: Mutex<(Option<PipeClient>, Option<Instant>)>,
}

impl PipeHelperLink {
    #[must_use]
    pub fn new() -> Self {
        Self::with_pipe(HELPER_PIPE)
    }

    #[must_use]
    pub fn with_pipe(pipe: &str) -> Self {
        Self {
            pipe: pipe.to_string(),
            conn: Mutex::new((None, None)),
        }
    }
}

impl Default for PipeHelperLink {
    fn default() -> Self {
        Self::new()
    }
}

impl HelperLink for PipeHelperLink {
    fn send(&self, req: &Request) -> bool {
        let mut g = self
            .conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for _ in 0..2 {
            if g.0.is_none() {
                if g.1.is_some_and(|t| t.elapsed() < RECONNECT_SPACING) {
                    return false;
                }
                match PipeClient::connect(&self.pipe, CONNECT_TIMEOUT) {
                    Ok(c) => g.0 = Some(c),
                    Err(e) => {
                        tracing::debug!("helper pipe unreachable: {e}");
                        g.1 = Some(Instant::now());
                        return false;
                    }
                }
            }
            let Some(c) = g.0.as_mut() else { continue };
            match c.send(req, SEND_TIMEOUT) {
                Ok(()) => return true,
                Err(e) => {
                    tracing::debug!("helper pipe write failed: {e}");
                    g.0 = None;
                }
            }
        }
        g.1 = Some(Instant::now());
        false
    }
}

struct InjectState {
    kind: DesktopKind,
    checked: Option<Instant>,
    /// The helper received something since the last `release_all` (so it may hold keys).
    helper_dirty: bool,
}

/// [`InputInjector`] that follows the input desktop.
pub struct SecureDesktopInjector {
    inner: std::sync::Arc<dyn InputInjector>,
    probe: Box<dyn DesktopProbe>,
    helper: Box<dyn HelperLink>,
    state: Mutex<InjectState>,
    warned: AtomicBool,
}

impl SecureDesktopInjector {
    /// Production wiring around `inner` (the normal `SendInput` injector).
    #[must_use]
    pub fn new(inner: std::sync::Arc<dyn InputInjector>) -> Self {
        Self::with_parts(
            inner,
            Box::new(SystemProbe),
            Box::new(PipeHelperLink::new()),
        )
    }

    #[must_use]
    pub fn with_parts(
        inner: std::sync::Arc<dyn InputInjector>,
        probe: Box<dyn DesktopProbe>,
        helper: Box<dyn HelperLink>,
    ) -> Self {
        Self {
            inner,
            probe,
            helper,
            state: Mutex::new(InjectState {
                kind: DesktopKind::Normal,
                checked: None,
                helper_dirty: false,
            }),
            warned: AtomicBool::new(false),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, InjectState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Current desktop kind; releases everything on the side that is being left when it changes.
    fn current(&self) -> DesktopKind {
        let mut st = self.lock();
        if st.checked.is_none_or(|t| t.elapsed() >= DESKTOP_CACHE) {
            st.checked = Some(Instant::now());
            let now = self.probe.kind();
            if now != st.kind {
                let left = std::mem::replace(&mut st.kind, now);
                tracing::info!(from = ?left, to = ?now, "input desktop switched");
                match left {
                    DesktopKind::Normal => self.inner.release_all(),
                    DesktopKind::Secure => {
                        if std::mem::take(&mut st.helper_dirty) {
                            self.helper.send(&Request::ReleaseAll);
                        }
                    }
                }
            }
        }
        st.kind
    }

    fn forward(&self, req: &Request) {
        if self.current() == DesktopKind::Secure {
            if self.helper.send(req) {
                self.lock().helper_dirty = true;
                return;
            }
            if !self.warned.swap(true, Ordering::Relaxed) {
                tracing::warn!(
                    "secure desktop is active but the Desk Bridge helper is unreachable \
                     (service not running?); falling back to normal injection, which cannot \
                     reach the UAC prompt or the lock screen"
                );
            }
        } else {
            // Re-arm the one-time warning for the next secure-desktop episode.
            self.warned.store(false, Ordering::Relaxed);
        }
        apply(self.inner.as_ref(), req);
    }
}

impl InputInjector for SecureDesktopInjector {
    fn key(&self, hid: u16, down: bool) {
        self.forward(&Request::Key { hid, down });
    }

    fn mouse_rel(&self, dx: i32, dy: i32) {
        self.forward(&Request::MouseRel { dx, dy });
    }

    fn mouse_abs_px(&self, x: i32, y: i32) {
        self.forward(&Request::MouseAbs { x, y });
    }

    fn button(&self, button: Button, down: bool) {
        self.forward(&Request::Button { button, down });
    }

    fn wheel(&self, dx: i32, dy: i32) {
        self.forward(&Request::Wheel { dx, dy });
    }

    fn release_all(&self) {
        self.inner.release_all();
        let dirty = std::mem::take(&mut self.lock().helper_dirty);
        if dirty {
            self.helper.send(&Request::ReleaseAll);
        }
    }

    fn toggles(&self) -> (bool, bool, bool) {
        self.inner.toggles()
    }
}

// ---------------------------------------------------------------------------------------------
// Stuck keys after an agent crash
// ---------------------------------------------------------------------------------------------

const VK_LBUTTON: u16 = 0x01;
const VK_RBUTTON: u16 = 0x02;
const VK_MBUTTON: u16 = 0x04;
const VK_XBUTTON1: u16 = 0x05;
const VK_XBUTTON2: u16 = 0x06;

/// Virtual keys to inspect: everything except the generic modifier aliases (the left/right
/// variants report the same physical keys) and non-key codes.
fn candidate_vks() -> impl Iterator<Item = u16> {
    (1u16..=254).filter(|vk| !matches!(vk, 0x10..=0x12 | 0xE5 | 0xE7))
}

/// Virtual keys (including mouse buttons) that `is_down` reports as pressed.
#[must_use]
pub fn stuck_vks(is_down: impl Fn(u16) -> bool) -> Vec<u16> {
    candidate_vks().filter(|&vk| is_down(vk)).collect()
}

/// Keys that need `KEYEVENTF_EXTENDEDKEY` even though `MapVirtualKeyW` does not always say so
/// (navigation cluster, Win/Apps, right Ctrl/Alt, Num Lock, numpad divide, Print Screen).
#[must_use]
pub fn is_extended_vk(vk: u16) -> bool {
    matches!(vk, 0x21..=0x28 | 0x2C..=0x2E | 0x5B..=0x5D | 0x6F | 0x90 | 0xA3 | 0xA5)
}

/// Key-up / button-up input for `vk`, carrying our marker so the local hook ignores it.
#[must_use]
pub fn release_input(vk: u16) -> INPUT {
    let mouse = |flags: MOUSE_EVENT_FLAGS, data: u32| INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: INJECT_MARKER,
            },
        },
    };
    match vk {
        VK_LBUTTON => mouse(MOUSEEVENTF_LEFTUP, 0),
        VK_RBUTTON => mouse(MOUSEEVENTF_RIGHTUP, 0),
        VK_MBUTTON => mouse(MOUSEEVENTF_MIDDLEUP, 0),
        VK_XBUTTON1 => mouse(MOUSEEVENTF_XUP, 1),
        VK_XBUTTON2 => mouse(MOUSEEVENTF_XUP, 2),
        _ => {
            // MAPVK_VK_TO_VSC_EX = 4: high byte 0xE0/0xE1 marks extended keys.
            // SAFETY: MapVirtualKeyW has no preconditions.
            let sc = unsafe { MapVirtualKeyW(u32::from(vk), MAP_VIRTUAL_KEY_TYPE(4)) };
            let extended = (sc >> 8) == 0xE0 || is_extended_vk(vk);
            let mut flags = KEYEVENTF_KEYUP;
            if extended {
                flags |= KEYEVENTF_EXTENDEDKEY;
            }
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VIRTUAL_KEY(vk),
                        wScan: (sc & 0xFF) as u16,
                        dwFlags: flags | KEYBD_EVENT_FLAGS(0),
                        time: 0,
                        dwExtraInfo: INJECT_MARKER,
                    },
                },
            }
        }
    }
}

/// Sends a key-up for every key and button the system reports as down, so a crashed previous
/// agent (or one killed while the PC keyboard held a key) cannot leave anything stuck on this
/// target. Modifiers go last. Returns the number of released keys/buttons.
///
/// Call at startup on devices that are *not* an input source: there, no physical user normally
/// holds a key at that instant, while a stuck remote key is the exact failure this repairs.
pub fn release_stuck_keys() -> usize {
    // SAFETY: GetAsyncKeyState has no preconditions; the high bit means "down".
    let mut down = stuck_vks(|vk| unsafe { GetAsyncKeyState(i32::from(vk)) } < 0);
    if down.is_empty() {
        return 0;
    }
    // Non-modifiers first, modifiers (0xA0..=0xA5 Shift/Ctrl/Alt, 0x5B/0x5C Win) last.
    down.sort_by_key(|vk| matches!(vk, 0xA0..=0xA5 | 0x5B | 0x5C));
    let inputs: Vec<INPUT> = down.iter().map(|&vk| release_input(vk)).collect();
    // SAFETY: `inputs` is a valid slice of initialised INPUT structs.
    let sent = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
    tracing::info!(count = down.len(), sent, keys = ?down, "released stuck keys at startup");
    down.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use deskbridge_platform::mock::{InjectedOp, MockInjector};
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    // --- scripted fakes ---------------------------------------------------------------------

    struct ScriptedProbe(Arc<Mutex<DesktopKind>>);
    impl DesktopProbe for ScriptedProbe {
        fn kind(&self) -> DesktopKind {
            *self.0.lock().unwrap()
        }
    }

    #[derive(Default)]
    struct FakeHelper {
        sent: Mutex<Vec<Request>>,
        reachable: AtomicBool,
        calls: AtomicUsize,
    }
    struct SharedHelper(Arc<FakeHelper>);
    impl HelperLink for SharedHelper {
        fn send(&self, req: &Request) -> bool {
            self.0.calls.fetch_add(1, Ordering::SeqCst);
            if !self.0.reachable.load(Ordering::SeqCst) {
                return false;
            }
            self.0.sent.lock().unwrap().push(*req);
            true
        }
    }

    struct Rig {
        inj: SecureDesktopInjector,
        inner: Arc<MockInjector>,
        helper: Arc<FakeHelper>,
        desktop: Arc<Mutex<DesktopKind>>,
    }

    fn rig(reachable: bool) -> Rig {
        let inner = Arc::new(MockInjector::default());
        let helper = Arc::new(FakeHelper::default());
        helper.reachable.store(reachable, Ordering::SeqCst);
        let desktop = Arc::new(Mutex::new(DesktopKind::Normal));
        let inj = SecureDesktopInjector::with_parts(
            inner.clone(),
            Box::new(ScriptedProbe(desktop.clone())),
            Box::new(SharedHelper(helper.clone())),
        );
        Rig {
            inj,
            inner,
            helper,
            desktop,
        }
    }

    /// Lets the 100 ms probe cache expire.
    fn expire_cache(r: &Rig) {
        r.inj.lock().checked = Some(Instant::now() - Duration::from_secs(1));
    }

    #[test]
    fn normal_desktop_uses_the_normal_injector() {
        let r = rig(true);
        r.inj.key(4, true);
        r.inj.mouse_rel(1, 2);
        assert_eq!(
            r.inner.ops(),
            vec![
                InjectedOp::Key { hid: 4, down: true },
                InjectedOp::MouseRel { dx: 1, dy: 2 }
            ]
        );
        assert!(r.helper.sent.lock().unwrap().is_empty());
        assert_eq!(
            r.helper.calls.load(Ordering::SeqCst),
            0,
            "the helper is never touched"
        );
    }

    #[test]
    fn secure_desktop_goes_to_the_helper_and_switching_releases_both_sides() {
        let r = rig(true);
        r.inj.key(0xE0, true); // ctrl held on the normal desktop
        assert_eq!(r.inner.pressed_keys(), vec![0xE0]);

        *r.desktop.lock().unwrap() = DesktopKind::Secure;
        expire_cache(&r);
        r.inj.key(4, true);
        // leaving the normal desktop released what was pressed there
        assert!(r.inner.pressed_keys().is_empty());
        assert_eq!(*r.inner.release_all_calls.lock(), 1);
        assert_eq!(
            *r.helper.sent.lock().unwrap(),
            vec![Request::Key { hid: 4, down: true }]
        );

        r.inj.button(Button::Left, true);
        r.inj.mouse_abs_px(5, 6);
        r.inj.wheel(0, 120);
        assert_eq!(r.helper.sent.lock().unwrap().len(), 4);

        // back to normal: the helper is told to release, the normal injector takes over
        *r.desktop.lock().unwrap() = DesktopKind::Normal;
        expire_cache(&r);
        r.inj.key(5, true);
        assert_eq!(
            r.helper.sent.lock().unwrap().last(),
            Some(&Request::ReleaseAll)
        );
        assert_eq!(r.inner.pressed_keys(), vec![5]);
    }

    #[test]
    fn desktop_probe_is_cached() {
        let r = rig(true);
        r.inj.key(4, true); // first call probes
        *r.desktop.lock().unwrap() = DesktopKind::Secure;
        r.inj.key(4, false); // within the cache window: still routed as normal
        assert_eq!(r.inner.ops().len(), 2);
        assert!(r.helper.sent.lock().unwrap().is_empty());
    }

    #[test]
    fn unreachable_helper_falls_back_to_normal_injection() {
        let r = rig(false);
        *r.desktop.lock().unwrap() = DesktopKind::Secure;
        r.inj.key(4, true);
        r.inj.key(4, false);
        assert_eq!(r.inner.ops().len(), 2, "fallback keeps the keyboard usable");
        assert!(r.inj.warned.load(Ordering::Relaxed), "logged once");
    }

    #[test]
    fn release_all_reaches_the_helper_only_if_it_was_used() {
        let r = rig(true);
        r.inj.release_all();
        assert_eq!(r.helper.calls.load(Ordering::SeqCst), 0);
        *r.desktop.lock().unwrap() = DesktopKind::Secure;
        r.inj.key(4, true);
        r.inj.release_all();
        assert_eq!(
            r.helper.sent.lock().unwrap().last(),
            Some(&Request::ReleaseAll)
        );
        assert_eq!(*r.inner.release_all_calls.lock(), 3); // explicit, on switch, explicit
        // second release_all has nothing to tell the helper
        let before = r.helper.calls.load(Ordering::SeqCst);
        r.inj.release_all();
        assert_eq!(r.helper.calls.load(Ordering::SeqCst), before);
    }

    // --- service client ---------------------------------------------------------------------

    #[test]
    fn service_client_without_service_is_unavailable() {
        let c = ServiceClient::with_pipe(r"\\.\pipe\deskbridge-agent-test-no-such-pipe");
        assert!(!c.available());
        match c.send_sas() {
            Err(PlatformError::Unsupported(_)) => {}
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn sas_error_mapping() {
        assert!(
            matches!(sas_error(ErrorCode::SasNotAllowed, ""), PlatformError::Other(m) if m.contains("SoftwareSASGeneration"))
        );
        assert!(matches!(sas_error(ErrorCode::Failed, "x"), PlatformError::Other(m) if m == "x"));
    }

    // --- end to end over a test pipe ----------------------------------------------------------

    struct FixedSas(std::result::Result<(), deskbridge_service::control::SasError>);
    impl deskbridge_service::control::SasSender for FixedSas {
        fn send(&self) -> std::result::Result<(), deskbridge_service::control::SasError> {
            self.0.clone()
        }
    }

    fn spawn_control_server(name: &str, sas: FixedSas) -> tokio::sync::watch::Sender<bool> {
        use deskbridge_service::control::{ControlHandler, HelperStatus};
        use deskbridge_service::pipe_security::PipeAccess;
        let (stop, rx) = tokio::sync::watch::channel(false);
        let sid = deskbridge_service::winutil::current_user_sid();
        let access: deskbridge_service::pipe_server::AccessFn = Arc::new(move || PipeAccess {
            user_sid: sid.clone(),
            require_high_integrity: false,
        });
        let handler = Arc::new(ControlHandler {
            sas: Arc::new(sas),
            helper: Arc::new(parking_lot::Mutex::new(HelperStatus::default())),
        });
        tokio::spawn(deskbridge_service::pipe_server::serve(
            name.to_string(),
            access,
            handler,
            rx,
            Arc::new(tokio::sync::Notify::new()),
        ));
        stop
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn service_client_end_to_end() {
        use deskbridge_service::control::SasError;
        let ok = format!(r"\\.\pipe\deskbridge-agent-test-ok-{}", std::process::id());
        let denied = format!(
            r"\\.\pipe\deskbridge-agent-test-denied-{}",
            std::process::id()
        );
        let s1 = spawn_control_server(&ok, FixedSas(Ok(())));
        let s2 = spawn_control_server(&denied, FixedSas(Err(SasError::NotAllowedByPolicy)));
        tokio::task::spawn_blocking(move || {
            // The server tasks need a moment to create their first instance.
            std::thread::sleep(Duration::from_millis(200));
            let c = ServiceClient::with_pipe(&ok);
            assert!(c.available());
            c.send_sas().expect("SendSAS accepted");
            let c = ServiceClient::with_pipe(&denied);
            assert!(c.available());
            match c.send_sas() {
                Err(PlatformError::Other(m)) => assert!(m.contains("SoftwareSASGeneration"), "{m}"),
                other => panic!("{other:?}"),
            }
        })
        .await
        .unwrap();
        let _ = (s1.send(true), s2.send(true));
    }

    // --- stuck keys -------------------------------------------------------------------------

    #[test]
    fn stuck_vk_selection() {
        let down = |vk: u16| matches!(vk, 0x41 | 0xA2 | 0x11 | 0x01 | 0xE7);
        // generic CONTROL (0x11) and VK_PACKET are skipped; left ctrl and the mouse button count
        assert_eq!(stuck_vks(down), vec![0x01, 0x41, 0xA2]);
        assert!(stuck_vks(|_| false).is_empty());
    }

    #[test]
    fn extended_key_set() {
        for vk in [0x25u16, 0x27, 0x2E, 0x5B, 0xA3, 0xA5, 0x6F] {
            assert!(is_extended_vk(vk), "{vk:#x}");
        }
        for vk in [0x41u16, 0x0D, 0xA2, 0xA4, 0x10, 0x70] {
            assert!(!is_extended_vk(vk), "{vk:#x}");
        }
    }

    #[test]
    fn release_inputs_are_key_ups_with_our_marker() {
        let a = release_input(0x41);
        let right = release_input(0x27); // VK_RIGHT: extended
        let lbutton = release_input(VK_LBUTTON);
        // SAFETY: reading the union member that matches the `type` we constructed.
        unsafe {
            assert_eq!(a.r#type, INPUT_KEYBOARD);
            assert!(a.Anonymous.ki.dwFlags.contains(KEYEVENTF_KEYUP));
            assert!(!a.Anonymous.ki.dwFlags.contains(KEYEVENTF_EXTENDEDKEY));
            assert_eq!(a.Anonymous.ki.dwExtraInfo, INJECT_MARKER);
            assert!(right.Anonymous.ki.dwFlags.contains(KEYEVENTF_EXTENDEDKEY));
            assert_eq!(lbutton.r#type, INPUT_MOUSE);
            assert_eq!(lbutton.Anonymous.mi.dwFlags, MOUSEEVENTF_LEFTUP);
            assert_eq!(lbutton.Anonymous.mi.dwExtraInfo, INJECT_MARKER);
        }
    }
}
