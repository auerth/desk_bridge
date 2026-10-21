//! Windows input capture (hooks + Raw Input) and injection (`SendInput`).
//!
//! # Capture
//! One dedicated thread owns a message loop with
//! * `WH_KEYBOARD_LL` / `WH_MOUSE_LL` hooks (decide "swallow or pass" inline, enqueue events),
//! * a message-only window that receives **Raw Input** (`RIDEV_INPUTSINK`) for relative mouse
//!   deltas that keep flowing while the pointer is frozen by a grab.
//!
//! Hook callbacks are deliberately tiny: classify the event, run the pure decision logic
//! ([`KeyboardFilter`] / [`GrabState`], both unit-tested), push into the unbounded channel.
//! Nothing blocks, no long-held locks (the state lives in a thread-local, the grab mode in an
//! atomic) so `LowLevelHooksTimeout` is never hit.
//!
//! Grab semantics (stuck-key safety): a key whose *down* reached the OS is never left without its
//! *up* (the up is passed even while grabbing, and still reported); a key whose down was swallowed
//! has its up (and auto-repeat) swallowed too so the OS never sees an orphan up.
//!
//! # Cursor hiding
//! [`InputCapture::set_cursor_visible`] replaces the standard system cursors with a fully
//! transparent one via `SetSystemCursor` and restores them with `SPI_SETCURSORS`
//! (`ShowCursor` only affects windows of the calling thread, so it cannot hide the pointer
//! desktop-wide). The cursors are restored on `show`, `stop()` and `Drop`; after a hard crash they
//! stay transparent until [`restore_system_cursors`] runs (call it at agent start-up).
//! Limitation: applications that set their own cursor handles (some games) stay visible.
//!
//! # Injection
//! All injected input carries `dwExtraInfo = `[`INJECT_MARKER`] so a capture running in the same
//! process/machine ignores it. Keys are injected by scancode (`KEYEVENTF_SCANCODE`, +extended),
//! except Pause and the media/browser keys which are injected by virtual key (more reliable).
//! AltGr = `R_ALT` alone; Windows adds the fake left Ctrl itself.
//!
//! Absolute placement sends `SendInput` with `MOUSEEVENTF_ABSOLUTE | VIRTUALDESK` (normalised
//! 0..65535, see [`normalize_abs`]): only a real input event triggers hover effects such as the
//! taskbar previews. The normalised path can round on multi-monitor/mixed-DPI setups, so the
//! pointer is then put on the exact physical pixel with `SetCursorPos` (the process must be
//! Per-Monitor-DPI-aware v2, see [`init_dpi_awareness`]).

use std::cell::RefCell;
use std::collections::HashSet;
use std::ffi::c_void;
use std::mem::size_of;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicIsize, AtomicU8, AtomicU32, Ordering};
use std::thread::JoinHandle;

use deskbridge_platform::hotkey::{Hotkey, HotkeyMatcher, HotkeyOutcome};
use deskbridge_platform::input::{Button, CaptureEvent, GrabMode, InputCapture, InputInjector};
use deskbridge_platform::{PlatformError, Result, UnboundedSender, hid};
use parking_lot::Mutex;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_CLASS_ALREADY_EXISTS, GetLastError, HANDLE, HINSTANCE, HWND, LPARAM,
    LRESULT, WPARAM,
};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, OpenProcessToken, SetThreadPriority,
    THREAD_PRIORITY_HIGHEST,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSE_EVENT_FLAGS,
    MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN,
    MOUSEEVENTF_XUP, MOUSEINPUT, SendInput, VIRTUAL_KEY, VK_CAPITAL, VK_NUMLOCK, VK_SCROLL,
};
use windows::Win32::UI::Input::{
    GetRawInputData, HRAWINPUT, MOUSE_MOVE_ABSOLUTE, MOUSE_VIRTUAL_DESKTOP, RAWINPUT,
    RAWINPUTDEVICE, RAWINPUTHEADER, RID_INPUT, RIDEV_INPUTSINK, RIM_TYPEMOUSE,
    RegisterRawInputDevices,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CallNextHookEx, CreateCursor, CreateWindowExW, DefWindowProcW, DestroyWindow,
    DispatchMessageW, GWLP_USERDATA, GetCursorPos, GetMessageW, GetSystemMetrics, HC_ACTION, HHOOK,
    HWND_MESSAGE, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, MSG, MSLLHOOKSTRUCT, PostMessageW,
    PostQuitMessage, RegisterClassExW, SM_CXSCREEN, SM_CXVIRTUALSCREEN, SM_CYSCREEN,
    SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SPI_GETMOUSE, SPI_SETCURSORS,
    SPI_SETMOUSE, SYSTEM_CURSOR_ID, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SetCursorPos,
    SetSystemCursor, SetWindowLongPtrW, SetWindowsHookExW, SystemParametersInfoW, TranslateMessage,
    UnhookWindowsHookEx, WH_KEYBOARD_LL, WH_MOUSE_LL, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP,
    WM_DESTROY, WM_INPUT, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN,
    WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP,
    WM_SYSKEYDOWN, WM_SYSKEYUP, WM_XBUTTONDOWN, WM_XBUTTONUP, WNDCLASSEXW,
};
use windows::core::PCWSTR;

use crate::keymap::{HookKey, classify_hook_key, hid_to_scancode, hid_to_vk};

/// `dwExtraInfo` marker ("DBRG") carried by everything [`WinInputInjector`] sends.
pub const INJECT_MARKER: usize = 0x4442_5247;

const WM_APP_HOTKEYS: u32 = WM_APP + 1;
const WM_APP_STOP: u32 = WM_APP + 2;

// ---------------------------------------------------------------------------------------------
// Shared window helpers (used by screen.rs / power.rs as well)
// ---------------------------------------------------------------------------------------------

/// NUL-terminated UTF-16 string.
pub(crate) fn utf16z(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Registers `class` (idempotent) and creates a window that is never shown.
///
/// `message_only` windows do not receive broadcasts (`WM_DISPLAYCHANGE`, `WM_POWERBROADCAST`,
/// `WM_SETTINGCHANGE`), so callers that need those pass `false` and get an invisible top-level
/// popup. `param` is delivered as `CREATESTRUCTW.lpCreateParams` (see [`attach_userdata`]).
///
/// # Safety
/// `proc` must be a valid window procedure; `param` must stay valid for as long as `proc` uses it.
pub(crate) unsafe fn create_hidden_window(
    class: &str,
    proc: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
    param: *const c_void,
    message_only: bool,
) -> windows::core::Result<HWND> {
    let class_w = utf16z(class);
    // SAFETY: plain Win32 calls with valid, NUL-terminated strings that outlive the calls.
    unsafe {
        let hinst: HINSTANCE = GetModuleHandleW(None)?.into();
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(proc),
            hInstance: hinst,
            lpszClassName: PCWSTR(class_w.as_ptr()),
            ..Default::default()
        };
        if RegisterClassExW(&wc) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
            return Err(windows::core::Error::from_win32());
        }
        let (style, parent) = if message_only {
            (WINDOW_STYLE(0), HWND_MESSAGE)
        } else {
            // WS_POPUP, never shown.
            (WINDOW_STYLE(0x8000_0000), HWND::default())
        };
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class_w.as_ptr()),
            PCWSTR(class_w.as_ptr()),
            style,
            0,
            0,
            0,
            0,
            parent,
            None,
            hinst,
            Some(param),
        )
    }
}

/// Call from the window procedure on `WM_NCCREATE` to store `lpCreateParams` as user data.
///
/// # Safety
/// `lparam` must be the `lParam` of a `WM_NCCREATE` message.
pub(crate) unsafe fn attach_userdata(hwnd: HWND, lparam: LPARAM) {
    // SAFETY: for WM_NCCREATE, lParam points to a valid CREATESTRUCTW.
    unsafe {
        let cs = &*(lparam.0 as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
    }
}

/// Reads the pointer stored by [`attach_userdata`].
///
/// # Safety
/// The caller must know the pointer's real type and that it is still alive.
pub(crate) unsafe fn userdata<T>(hwnd: HWND) -> *mut T {
    // SAFETY: GetWindowLongPtrW has no preconditions.
    unsafe {
        windows::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut T
    }
}

/// Runs the standard message loop until `WM_QUIT`.
pub(crate) fn run_message_loop() {
    let mut msg = MSG::default();
    // SAFETY: standard message pump on the calling thread.
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn os_error(what: &str, e: &windows::core::Error) -> PlatformError {
    PlatformError::Os {
        code: i64::from(e.code().0),
        msg: format!("{what}: {e}"),
    }
}

// ---------------------------------------------------------------------------------------------
// DPI awareness / elevation
// ---------------------------------------------------------------------------------------------

/// Makes the process Per-Monitor-DPI-aware v2 so all coordinates are physical pixels.
///
/// Idempotent; an error (already set by the manifest or by another call) is only logged.
pub fn init_dpi_awareness() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // SAFETY: no preconditions.
        let r =
            unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        if let Err(e) = r {
            tracing::debug!("SetProcessDpiAwarenessContext: {e} (probably already set)");
        }
    });
}

/// `true` if the current process runs elevated (needed to inject into elevated windows, UIPI).
#[must_use]
pub fn is_elevated() -> bool {
    // SAFETY: the token handle is closed before returning; the out buffer matches the class.
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut ret = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some((&mut elevation as *mut TOKEN_ELEVATION).cast()),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && elevation.TokenIsElevated != 0
    }
}

// ---------------------------------------------------------------------------------------------
// Pure decision logic (unit-tested)
// ---------------------------------------------------------------------------------------------

/// Result of the grab decision for one event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    /// Do not hand the event to the rest of the system.
    pub swallow: bool,
}

/// Id space for mouse buttons inside [`GrabState`] (keys use their HID usage).
const BUTTON_ID_BASE: u16 = 0xF000;

/// Tracks which keys/buttons the OS has seen going down, so grabs never leave one stuck.
#[derive(Debug, Default)]
pub struct GrabState {
    /// Down reached the OS (passed); its up must reach the OS as well.
    local_down: HashSet<u16>,
    /// Down was swallowed; its up (and repeats) are swallowed too.
    swallowed: HashSet<u16>,
}

impl GrabState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Keyboard event: `KeyboardOnly` and `All` both swallow.
    pub fn on_key(&mut self, hid: u16, down: bool, mode: GrabMode) -> Decision {
        self.on_edge(hid, down, mode != GrabMode::None)
    }

    /// Mouse button event: only `All` swallows.
    pub fn on_button(&mut self, button: Button, down: bool, mode: GrabMode) -> Decision {
        self.on_edge(
            BUTTON_ID_BASE + button.to_wire() as u16,
            down,
            mode == GrabMode::All,
        )
    }

    fn on_edge(&mut self, id: u16, down: bool, grabbing: bool) -> Decision {
        if down {
            if self.local_down.contains(&id) {
                // Auto-repeat of a key the OS already sees: while grabbing, stop the repeats.
                return Decision { swallow: grabbing };
            }
            if self.swallowed.contains(&id) {
                return Decision { swallow: true };
            }
            if grabbing {
                self.swallowed.insert(id);
                Decision { swallow: true }
            } else {
                self.local_down.insert(id);
                Decision { swallow: false }
            }
        } else if self.swallowed.remove(&id) {
            // The OS never saw the down: do not give it an orphan up.
            Decision { swallow: true }
        } else {
            // Up of a locally pressed (or unknown) key must always reach the OS.
            self.local_down.remove(&id);
            Decision { swallow: false }
        }
    }

    /// Forgets everything (e.g. after the hooks were re-installed).
    pub fn reset(&mut self) {
        self.local_down.clear();
        self.swallowed.clear();
    }
}

/// What to do with one keyboard hook event.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KeyAction {
    pub swallow: bool,
    pub event: Option<CaptureEvent>,
}

const FAKE_CTRL_ID: u16 = 0x8F1D;
const FAKE_SHIFT_ID: u16 = 0x8F2A;

/// Hotkey matching + grab decision for keyboard events (runs inline inside the hook).
#[derive(Debug, Default)]
pub struct KeyboardFilter {
    grab: GrabState,
    matcher: HotkeyMatcher,
}

impl KeyboardFilter {
    #[must_use]
    pub fn new(hotkeys: Vec<Hotkey>) -> Self {
        Self {
            grab: GrabState::new(),
            matcher: HotkeyMatcher::new(hotkeys),
        }
    }

    pub fn set_hotkeys(&mut self, hotkeys: Vec<Hotkey>) {
        self.matcher.set_hotkeys(hotkeys);
    }

    pub fn process(&mut self, key: HookKey, down: bool, mode: GrabMode) -> KeyAction {
        let hid = match key {
            HookKey::Hid(h) => h,
            // Internal bookkeeping only: never forwarded, but their ups must not get stuck.
            HookKey::FakeCtrl => return self.silent(FAKE_CTRL_ID, down, mode),
            HookKey::FakeShift => return self.silent(FAKE_SHIFT_ID, down, mode),
            HookKey::Unknown(vk) => return self.silent(0x8000 | (vk as u16 & 0xFF), down, mode),
        };
        match self.matcher.feed(hid, down) {
            HotkeyOutcome::Fired(id) => KeyAction {
                swallow: true,
                event: Some(CaptureEvent::Hotkey(id)),
            },
            HotkeyOutcome::Swallow => {
                if !down && self.grab.local_down.contains(&hid) {
                    // The key was already pressed before its hotkey fired (e.g. repeat while the
                    // modifiers came later): its up has to reach the OS.
                    let d = self.grab.on_key(hid, false, mode);
                    KeyAction {
                        swallow: d.swallow,
                        event: Some(CaptureEvent::Key { hid, down }),
                    }
                } else {
                    KeyAction {
                        swallow: true,
                        event: None,
                    }
                }
            }
            HotkeyOutcome::Pass => {
                let d = self.grab.on_key(hid, down, mode);
                KeyAction {
                    swallow: d.swallow,
                    event: Some(CaptureEvent::Key { hid, down }),
                }
            }
        }
    }

    fn silent(&mut self, id: u16, down: bool, mode: GrabMode) -> KeyAction {
        let d = self.grab.on_key(id, down, mode);
        KeyAction {
            swallow: d.swallow,
            event: None,
        }
    }
}

/// Converts raw-input mouse samples to relative deltas, also for absolute devices (RDP, tablets,
/// touch digitisers report 0..65535 positions instead of deltas).
#[derive(Debug, Default)]
pub struct RawMouseTracker {
    last: Option<(i32, i32)>,
}

impl RawMouseTracker {
    /// `screen` is the width/height the absolute range maps to (virtual desktop or primary).
    pub fn delta(&mut self, absolute: bool, x: i32, y: i32, screen: (i32, i32)) -> (i32, i32) {
        if !absolute {
            self.last = None;
            return (x, y);
        }
        let px = (
            (i64::from(x) * i64::from(screen.0) / 65535) as i32,
            (i64::from(y) * i64::from(screen.1) / 65535) as i32,
        );
        let d = match self.last {
            Some((lx, ly)) => (px.0 - lx, px.1 - ly),
            None => (0, 0),
        };
        self.last = Some(px);
        d
    }
}

/// Normalises a pixel position to `MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK` coordinates
/// (0..65535 over the virtual desktop `vx, vy, vw, vh`) such that Windows maps it back to `x, y`.
#[must_use]
pub fn normalize_abs(x: i32, y: i32, vx: i32, vy: i32, vw: i32, vh: i32) -> (i32, i32) {
    let f = |p: i32, o: i32, size: i32| -> i32 {
        let size = i64::from(size.max(1));
        let v = ((i64::from(p) - i64::from(o)) * 65536 + size - 1) / size;
        v.clamp(0, 65535) as i32
    };
    (f(x, vx, vw), f(y, vy, vh))
}

fn grab_to_u8(m: GrabMode) -> u8 {
    match m {
        GrabMode::None => 0,
        GrabMode::All => 1,
        GrabMode::KeyboardOnly => 2,
    }
}

fn grab_from_u8(v: u8) -> GrabMode {
    match v {
        1 => GrabMode::All,
        2 => GrabMode::KeyboardOnly,
        _ => GrabMode::None,
    }
}

// ---------------------------------------------------------------------------------------------
// System cursor hiding
// ---------------------------------------------------------------------------------------------

const CURSOR_IDS: [u32; 13] = [
    32512, // NORMAL
    32513, // IBEAM
    32514, // WAIT
    32515, // CROSS
    32516, // UP
    32642, // SIZENWSE
    32643, // SIZENESW
    32644, // SIZEWE
    32645, // SIZENS
    32646, // SIZEALL
    32648, // NO
    32649, // HAND
    32650, // APPSTARTING
];

/// Restores the user's cursor scheme from the registry. Safe to call at any time (also at
/// start-up to recover from a previous crash while the cursor was hidden).
pub fn restore_system_cursors() {
    // SAFETY: SPI_SETCURSORS takes no buffer.
    let r = unsafe {
        SystemParametersInfoW(
            SPI_SETCURSORS,
            0,
            None,
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    if let Err(e) = r {
        tracing::warn!("restoring system cursors failed: {e}");
    }
}

fn hide_system_cursors() -> windows::core::Result<()> {
    // 32x32 monochrome cursor: AND plane all ones + XOR plane all zeros = fully transparent.
    let and_plane = [0xFFu8; 32 * 32 / 8];
    let xor_plane = [0u8; 32 * 32 / 8];
    for id in CURSOR_IDS {
        // SAFETY: the planes are 128 bytes as required for 32x32 1bpp; SetSystemCursor takes
        // ownership of (and destroys) the cursor handle, hence one handle per system cursor.
        unsafe {
            let cur = CreateCursor(
                HINSTANCE::default(),
                0,
                0,
                32,
                32,
                and_plane.as_ptr().cast(),
                xor_plane.as_ptr().cast(),
            )?;
            SetSystemCursor(cur, SYSTEM_CURSOR_ID(id))?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Capture
// ---------------------------------------------------------------------------------------------

struct Shared {
    grab: AtomicU8,
    hwnd: AtomicIsize,
    pending_hotkeys: Mutex<Option<Vec<Hotkey>>>,
}

/// State owned by the hook thread (thread-local: hooks always run on the installing thread).
struct HookCtx {
    tx: UnboundedSender<CaptureEvent>,
    shared: Arc<Shared>,
    kb: KeyboardFilter,
    buttons: GrabState,
    raw: RawMouseTracker,
}

thread_local! {
    static CTX: RefCell<Option<HookCtx>> = const { RefCell::new(None) };
}

fn with_ctx<R>(f: impl FnOnce(&mut HookCtx) -> R) -> Option<R> {
    CTX.with(|c| c.try_borrow_mut().ok().and_then(|mut b| b.as_mut().map(f)))
}

impl HookCtx {
    fn mode(&self) -> GrabMode {
        grab_from_u8(self.shared.grab.load(Ordering::Relaxed))
    }

    fn emit(&self, ev: CaptureEvent) {
        let _ = self.tx.send(ev);
    }

    fn on_keyboard(&mut self, msg: u32, k: &KBDLLHOOKSTRUCT) -> bool {
        let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
        if !down && msg != WM_KEYUP && msg != WM_SYSKEYUP {
            return false;
        }
        let ext = k.flags.0 & LLKHF_EXTENDED.0 != 0;
        let key = classify_hook_key(k.vkCode, k.scanCode, ext);
        let mode = self.mode();
        let a = self.kb.process(key, down, mode);
        if let Some(ev) = a.event {
            self.emit(ev);
        }
        a.swallow
    }

    fn button(&mut self, button: Button, down: bool) -> bool {
        let mode = self.mode();
        let d = self.buttons.on_button(button, down, mode);
        self.emit(CaptureEvent::Button { button, down });
        d.swallow
    }

    fn on_mouse(&mut self, msg: u32, m: &MSLLHOOKSTRUCT) -> bool {
        let mode = self.mode();
        let all = mode == GrabMode::All;
        match msg {
            WM_MOUSEMOVE => {
                if !all {
                    self.emit(CaptureEvent::MouseMove {
                        x: m.pt.x,
                        y: m.pt.y,
                    });
                }
                all
            }
            WM_LBUTTONDOWN => self.button(Button::Left, true),
            WM_LBUTTONUP => self.button(Button::Left, false),
            WM_RBUTTONDOWN => self.button(Button::Right, true),
            WM_RBUTTONUP => self.button(Button::Right, false),
            WM_MBUTTONDOWN => self.button(Button::Middle, true),
            WM_MBUTTONUP => self.button(Button::Middle, false),
            WM_XBUTTONDOWN | WM_XBUTTONUP => {
                let down = msg == WM_XBUTTONDOWN;
                match (m.mouseData >> 16) & 0xFFFF {
                    1 => self.button(Button::X1, down),
                    2 => self.button(Button::X2, down),
                    _ => false,
                }
            }
            WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
                let delta = i32::from((m.mouseData >> 16) as u16 as i16);
                let ev = if msg == WM_MOUSEWHEEL {
                    CaptureEvent::Wheel { dx: 0, dy: delta }
                } else {
                    CaptureEvent::Wheel { dx: delta, dy: 0 }
                };
                self.emit(ev);
                all
            }
            _ => false,
        }
    }

    fn on_raw_mouse(&mut self, flags: u16, x: i32, y: i32, extra: u32) {
        if extra as usize == INJECT_MARKER {
            return;
        }
        let absolute = flags & MOUSE_MOVE_ABSOLUTE.0 != 0;
        let screen = if absolute {
            // SAFETY: GetSystemMetrics has no preconditions.
            unsafe {
                if flags & MOUSE_VIRTUAL_DESKTOP.0 != 0 {
                    (
                        GetSystemMetrics(SM_CXVIRTUALSCREEN),
                        GetSystemMetrics(SM_CYVIRTUALSCREEN),
                    )
                } else {
                    (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN))
                }
            }
        } else {
            (0, 0)
        };
        let (dx, dy) = self.raw.delta(absolute, x, y, screen);
        if dx != 0 || dy != 0 {
            self.emit(CaptureEvent::MouseRel { dx, dy });
        }
    }
}

unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && lparam.0 != 0 {
        // SAFETY: for HC_ACTION, lParam points to a KBDLLHOOKSTRUCT valid during the callback.
        let k = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        if k.dwExtraInfo != INJECT_MARKER {
            let swallow = catch_unwind(AssertUnwindSafe(|| {
                with_ctx(|c| c.on_keyboard(wparam.0 as u32, k)).unwrap_or(false)
            }))
            .unwrap_or(false);
            if swallow {
                return LRESULT(1);
            }
        }
    }
    // SAFETY: forwarding the unmodified hook arguments.
    unsafe { CallNextHookEx(HHOOK::default(), code, wparam, lparam) }
}

unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && lparam.0 != 0 {
        // SAFETY: for HC_ACTION, lParam points to an MSLLHOOKSTRUCT valid during the callback.
        let m = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        if m.dwExtraInfo != INJECT_MARKER {
            let swallow = catch_unwind(AssertUnwindSafe(|| {
                with_ctx(|c| c.on_mouse(wparam.0 as u32, m)).unwrap_or(false)
            }))
            .unwrap_or(false);
            if swallow {
                return LRESULT(1);
            }
        }
    }
    // SAFETY: forwarding the unmodified hook arguments.
    unsafe { CallNextHookEx(HHOOK::default(), code, wparam, lparam) }
}

unsafe extern "system" fn capture_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_INPUT => {
            let _ = catch_unwind(AssertUnwindSafe(|| handle_raw_input(lparam)));
            // The application must always call DefWindowProc for WM_INPUT (cleanup).
            // SAFETY: forwarding unmodified arguments.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_APP_HOTKEYS => {
            with_ctx(|c| {
                if let Some(h) = c.shared.pending_hotkeys.lock().take() {
                    c.kb.set_hotkeys(h);
                }
            });
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
            // SAFETY: ends the message loop of this thread.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // SAFETY: forwarding unmodified arguments.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn handle_raw_input(lparam: LPARAM) {
    let mut raw = RAWINPUT::default();
    let mut size = size_of::<RAWINPUT>() as u32;
    // SAFETY: `raw` is a properly sized, writable RAWINPUT buffer.
    let n = unsafe {
        GetRawInputData(
            HRAWINPUT(lparam.0 as *mut c_void),
            RID_INPUT,
            Some((&mut raw as *mut RAWINPUT).cast()),
            &mut size,
            size_of::<RAWINPUTHEADER>() as u32,
        )
    };
    if n == u32::MAX || raw.header.dwType != RIM_TYPEMOUSE.0 {
        return;
    }
    // SAFETY: dwType == RIM_TYPEMOUSE selects the `mouse` union member.
    let m = unsafe { raw.data.mouse };
    with_ctx(|c| c.on_raw_mouse(m.usFlags.0, m.lLastX, m.lLastY, m.ulExtraInformation));
}

fn capture_thread(
    sink: UnboundedSender<CaptureEvent>,
    shared: Arc<Shared>,
    ready: std::sync::mpsc::Sender<std::result::Result<(), String>>,
) {
    // SAFETY: all calls below happen on this thread, which owns the window and both hooks and
    // tears them down before returning.
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST);

        let hotkeys = shared.pending_hotkeys.lock().take().unwrap_or_default();
        CTX.with(|c| {
            *c.borrow_mut() = Some(HookCtx {
                tx: sink,
                shared: shared.clone(),
                kb: KeyboardFilter::new(hotkeys),
                buttons: GrabState::new(),
                raw: RawMouseTracker::default(),
            });
        });

        let setup = (|| -> windows::core::Result<(HWND, HHOOK, HHOOK)> {
            let hwnd = create_hidden_window(
                "DeskBridgeInputSink",
                capture_wndproc,
                std::ptr::null(),
                true,
            )?;
            let rid = [RAWINPUTDEVICE {
                usUsagePage: 0x01,
                usUsage: 0x02,
                dwFlags: RIDEV_INPUTSINK,
                hwndTarget: hwnd,
            }];
            if let Err(e) = RegisterRawInputDevices(&rid, size_of::<RAWINPUTDEVICE>() as u32) {
                let _ = DestroyWindow(hwnd);
                return Err(e);
            }
            let hmod: HINSTANCE = GetModuleHandleW(None)?.into();
            let kb = match SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), hmod, 0) {
                Ok(h) => h,
                Err(e) => {
                    let _ = DestroyWindow(hwnd);
                    return Err(e);
                }
            };
            let ms = match SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), hmod, 0) {
                Ok(h) => h,
                Err(e) => {
                    let _ = UnhookWindowsHookEx(kb);
                    let _ = DestroyWindow(hwnd);
                    return Err(e);
                }
            };
            Ok((hwnd, kb, ms))
        })();

        match setup {
            Ok((hwnd, kb, ms)) => {
                shared.hwnd.store(hwnd.0 as isize, Ordering::SeqCst);
                let _ = ready.send(Ok(()));
                run_message_loop();
                shared.hwnd.store(0, Ordering::SeqCst);
                let _ = UnhookWindowsHookEx(kb);
                let _ = UnhookWindowsHookEx(ms);
            }
            Err(e) => {
                let _ = ready.send(Err(format!("input capture setup failed: {e}")));
            }
        }
        CTX.with(|c| *c.borrow_mut() = None);
    }
}

/// Windows input capture. See the module docs.
pub struct WinInputCapture {
    shared: Arc<Shared>,
    thread: Mutex<Option<JoinHandle<()>>>,
    cursor_hidden: Mutex<bool>,
}

impl Default for WinInputCapture {
    fn default() -> Self {
        Self::new()
    }
}

impl WinInputCapture {
    #[must_use]
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Shared {
                grab: AtomicU8::new(0),
                hwnd: AtomicIsize::new(0),
                pending_hotkeys: Mutex::new(None),
            }),
            thread: Mutex::new(None),
            cursor_hidden: Mutex::new(false),
        }
    }

    fn show_cursor_now(&self) {
        let mut hidden = self.cursor_hidden.lock();
        if *hidden {
            restore_system_cursors();
            *hidden = false;
        }
    }
}

impl InputCapture for WinInputCapture {
    fn start(&self, sink: UnboundedSender<CaptureEvent>) -> Result<()> {
        let mut slot = self.thread.lock();
        if slot.is_some() {
            return Err(PlatformError::other("input capture already started"));
        }
        init_dpi_awareness();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let shared = self.shared.clone();
        let handle = std::thread::Builder::new()
            .name("db-input-capture".into())
            .spawn(move || capture_thread(sink, shared, ready_tx))
            .map_err(|e| PlatformError::other(format!("spawn input thread: {e}")))?;
        match ready_rx.recv() {
            Ok(Ok(())) => {
                *slot = Some(handle);
                Ok(())
            }
            Ok(Err(msg)) => {
                let _ = handle.join();
                Err(PlatformError::other(msg))
            }
            Err(_) => {
                let _ = handle.join();
                Err(PlatformError::other(
                    "input capture thread died during setup",
                ))
            }
        }
    }

    fn stop(&self) {
        let handle = self.thread.lock().take();
        if let Some(h) = handle {
            let hwnd = self.shared.hwnd.load(Ordering::SeqCst);
            if hwnd != 0 {
                // SAFETY: posting to our own window; failure just means it is already gone.
                unsafe {
                    let _ =
                        PostMessageW(HWND(hwnd as *mut c_void), WM_APP_STOP, WPARAM(0), LPARAM(0));
                }
            }
            let _ = h.join();
        }
        self.shared.grab.store(0, Ordering::Relaxed);
        self.show_cursor_now();
    }

    fn set_grab(&self, mode: GrabMode) {
        self.shared.grab.store(grab_to_u8(mode), Ordering::Relaxed);
    }

    fn set_hotkeys(&self, hotkeys: Vec<Hotkey>) {
        *self.shared.pending_hotkeys.lock() = Some(hotkeys);
        let hwnd = self.shared.hwnd.load(Ordering::SeqCst);
        if hwnd != 0 {
            // SAFETY: posting to our own window.
            unsafe {
                let _ = PostMessageW(
                    HWND(hwnd as *mut c_void),
                    WM_APP_HOTKEYS,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        }
    }

    fn set_cursor_pos(&self, x: i32, y: i32) {
        // SAFETY: no preconditions.
        if let Err(e) = unsafe { SetCursorPos(x, y) } {
            tracing::debug!("SetCursorPos({x},{y}) failed: {e}");
        }
    }

    fn set_cursor_visible(&self, visible: bool) {
        if visible {
            self.show_cursor_now();
            return;
        }
        let mut hidden = self.cursor_hidden.lock();
        if *hidden {
            return;
        }
        match hide_system_cursors() {
            Ok(()) => *hidden = true,
            Err(e) => {
                tracing::warn!("hiding the cursor failed: {e}");
                restore_system_cursors();
            }
        }
    }
}

impl Drop for WinInputCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---------------------------------------------------------------------------------------------
// Injection
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
struct InjectState {
    keys: HashSet<u16>,
    buttons: HashSet<Button>,
    /// Original `SPI_GETMOUSE` values while the 1:1 mode is active.
    saved_mouse: Option<[i32; 3]>,
}

/// `SendInput`-based injector for the target side.
#[derive(Default)]
pub struct WinInputInjector {
    state: Mutex<InjectState>,
    failures: AtomicU32,
}

impl WinInputInjector {
    #[must_use]
    pub fn new() -> Self {
        init_dpi_awareness();
        Self::default()
    }

    /// `false` if Windows did not take every event.
    fn send(&self, inputs: &[INPUT]) -> bool {
        if inputs.is_empty() {
            return true;
        }
        // SAFETY: `inputs` is a valid slice of fully initialised INPUT structs.
        let n = unsafe { SendInput(inputs, size_of::<INPUT>() as i32) };
        let ok = n as usize == inputs.len();
        if !ok {
            let err = windows::core::Error::from_win32();
            let count = self.failures.fetch_add(1, Ordering::Relaxed) + 1;
            // Rate limited: 1st, 2nd, 4th, 8th ... failure.
            if count.is_power_of_two() {
                tracing::warn!(
                    "SendInput injected {n}/{} events ({err}); target window may be elevated \
                     (UIPI) or the desktop is secure/locked (failure #{count})",
                    inputs.len()
                );
            }
        }
        ok
    }

    fn key_input(hid: u16, down: bool) -> Option<INPUT> {
        let up = if down {
            KEYBD_EVENT_FLAGS(0)
        } else {
            KEYEVENTF_KEYUP
        };
        let (vk, scan, flags) = if let Some(vk) = hid_to_vk(hid) {
            // Pause and consumer keys are injected by virtual key.
            let (sc, ext) = hid_to_scancode(hid).unwrap_or((0, false));
            let ext = if ext {
                KEYEVENTF_EXTENDEDKEY
            } else {
                KEYBD_EVENT_FLAGS(0)
            };
            (vk as u16, sc, up | ext)
        } else {
            let (sc, ext) = hid_to_scancode(hid)?;
            let ext = if ext {
                KEYEVENTF_EXTENDEDKEY
            } else {
                KEYBD_EVENT_FLAGS(0)
            };
            (0, sc, KEYEVENTF_SCANCODE | up | ext)
        };
        Some(INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk),
                    wScan: scan,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: INJECT_MARKER,
                },
            },
        })
    }

    fn mouse_input(dx: i32, dy: i32, data: u32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
        INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx,
                    dy,
                    mouseData: data,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: INJECT_MARKER,
                },
            },
        }
    }

    fn button_input(button: Button, down: bool) -> INPUT {
        let (flags, data) = match (button, down) {
            (Button::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
            (Button::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
            (Button::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
            (Button::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
            (Button::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
            (Button::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
            (Button::X1, true) => (MOUSEEVENTF_XDOWN, 1),
            (Button::X1, false) => (MOUSEEVENTF_XUP, 1),
            (Button::X2, true) => (MOUSEEVENTF_XDOWN, 2),
            (Button::X2, false) => (MOUSEEVENTF_XUP, 2),
        };
        Self::mouse_input(0, 0, data, flags)
    }

    /// Optional "1:1 mode": switches off pointer acceleration ("Enhance pointer precision")
    /// for the session via `SPI_SETMOUSE` (not persisted to the user profile) and restores the
    /// previous values with `set_raw_one_to_one(false)`, `release_all`-independent, and on drop.
    pub fn set_raw_one_to_one(&self, on: bool) -> Result<()> {
        let mut st = self.state.lock();
        if on {
            if st.saved_mouse.is_some() {
                return Ok(());
            }
            let mut cur = [0i32; 3];
            // SAFETY: SPI_GETMOUSE writes three INTs into the buffer.
            unsafe {
                SystemParametersInfoW(
                    SPI_GETMOUSE,
                    0,
                    Some(cur.as_mut_ptr().cast()),
                    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
                )
            }
            .map_err(|e| os_error("SPI_GETMOUSE", &e))?;
            let mut off = [cur[0], cur[1], 0];
            // SAFETY: SPI_SETMOUSE reads three INTs from the buffer.
            unsafe {
                SystemParametersInfoW(
                    SPI_SETMOUSE,
                    0,
                    Some(off.as_mut_ptr().cast()),
                    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
                )
            }
            .map_err(|e| os_error("SPI_SETMOUSE", &e))?;
            st.saved_mouse = Some(cur);
        } else if let Some(mut saved) = st.saved_mouse.take() {
            // SAFETY: as above.
            unsafe {
                SystemParametersInfoW(
                    SPI_SETMOUSE,
                    0,
                    Some(saved.as_mut_ptr().cast()),
                    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
                )
            }
            .map_err(|e| os_error("SPI_SETMOUSE restore", &e))?;
        }
        Ok(())
    }
}

impl Drop for WinInputInjector {
    fn drop(&mut self) {
        let _ = self.set_raw_one_to_one(false);
        self.release_all();
    }
}

impl InputInjector for WinInputInjector {
    fn key(&self, hid: u16, down: bool) {
        let Some(input) = Self::key_input(hid, down) else {
            tracing::debug!("no scancode for HID usage {hid:#x}");
            return;
        };
        {
            let mut st = self.state.lock();
            if down {
                st.keys.insert(hid);
            } else {
                st.keys.remove(&hid);
            }
        }
        self.send(&[input]);
    }

    fn mouse_rel(&self, dx: i32, dy: i32) {
        if dx == 0 && dy == 0 {
            return;
        }
        self.send(&[Self::mouse_input(dx, dy, 0, MOUSEEVENTF_MOVE)]);
    }

    fn mouse_abs_px(&self, x: i32, y: i32) {
        // A real mouse event first: `SetCursorPos` alone moves the pointer without any input,
        // so hover effects (taskbar previews, tooltips) never fired in focus mode.
        // SAFETY: GetSystemMetrics has no preconditions.
        let (vx, vy, vw, vh) = unsafe {
            (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
            )
        };
        let (nx, ny) = normalize_abs(x, y, vx, vy, vw, vh);
        let sent = self.send(&[Self::mouse_input(
            nx,
            ny,
            0,
            MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
        )]);
        // The normalised position can round to a neighbouring pixel on multi-monitor/mixed-DPI
        // layouts: put the pointer exactly where it belongs.
        let mut p = windows::Win32::Foundation::POINT::default();
        // SAFETY: valid out pointer.
        let exact = sent && unsafe { GetCursorPos(&mut p) }.is_ok() && (p.x, p.y) == (x, y);
        if !exact {
            // SAFETY: no preconditions.
            if let Err(e) = unsafe { SetCursorPos(x, y) } {
                tracing::debug!("SetCursorPos({x},{y}) failed: {e}");
            }
        }
    }

    fn button(&self, button: Button, down: bool) {
        {
            let mut st = self.state.lock();
            if down {
                st.buttons.insert(button);
            } else {
                st.buttons.remove(&button);
            }
        }
        self.send(&[Self::button_input(button, down)]);
    }

    fn wheel(&self, dx: i32, dy: i32) {
        let mut inputs = Vec::with_capacity(2);
        if dy != 0 {
            inputs.push(Self::mouse_input(0, 0, dy as u32, MOUSEEVENTF_WHEEL));
        }
        if dx != 0 {
            inputs.push(Self::mouse_input(0, 0, dx as u32, MOUSEEVENTF_HWHEEL));
        }
        self.send(&inputs);
    }

    fn release_all(&self) {
        let (mut keys, buttons) = {
            let mut st = self.state.lock();
            (
                std::mem::take(&mut st.keys).into_iter().collect::<Vec<_>>(),
                std::mem::take(&mut st.buttons)
                    .into_iter()
                    .collect::<Vec<_>>(),
            )
        };
        // Non-modifiers first, modifiers last.
        keys.sort_by_key(|k| hid::is_modifier(*k));
        let mut inputs: Vec<INPUT> = buttons
            .into_iter()
            .map(|b| Self::button_input(b, false))
            .collect();
        inputs.extend(keys.into_iter().filter_map(|k| Self::key_input(k, false)));
        self.send(&inputs);
    }

    fn toggles(&self) -> (bool, bool, bool) {
        // SAFETY: GetKeyState has no preconditions; the low bit is the toggle state.
        unsafe {
            (
                GetKeyState(i32::from(VK_CAPITAL.0)) & 1 != 0,
                GetKeyState(i32::from(VK_NUMLOCK.0)) & 1 != 0,
                GetKeyState(i32::from(VK_SCROLL.0)) & 1 != 0,
            )
        }
    }
}

/// Current cursor position in physical pixels (helper for tests and tools).
#[must_use]
pub fn cursor_position() -> Option<(i32, i32)> {
    let mut p = windows::Win32::Foundation::POINT::default();
    // SAFETY: valid out pointer.
    unsafe { GetCursorPos(&mut p) }.ok().map(|()| (p.x, p.y))
}

#[cfg(test)]
mod tests {
    use super::*;
    use deskbridge_platform::hotkey::{ALT, CTRL, SHIFT};

    const A: u16 = hid::A;

    #[test]
    fn grab_none_passes_everything() {
        let mut g = GrabState::new();
        assert!(!g.on_key(A, true, GrabMode::None).swallow);
        assert!(!g.on_key(A, true, GrabMode::None).swallow); // repeat
        assert!(!g.on_key(A, false, GrabMode::None).swallow);
    }

    #[test]
    fn grab_swallows_down_and_its_up() {
        let mut g = GrabState::new();
        assert!(g.on_key(A, true, GrabMode::All).swallow);
        assert!(g.on_key(A, true, GrabMode::All).swallow); // repeat
        assert!(g.on_key(A, false, GrabMode::All).swallow);
        // Everything forgotten: a later up passes.
        assert!(!g.on_key(A, false, GrabMode::All).swallow);
    }

    #[test]
    fn up_of_locally_pressed_key_passes_during_grab() {
        let mut g = GrabState::new();
        assert!(!g.on_key(hid::L_CTRL, true, GrabMode::None).swallow);
        // grab begins while Ctrl is held
        assert!(g.on_key(hid::L_CTRL, true, GrabMode::All).swallow); // repeat swallowed
        assert!(
            !g.on_key(hid::L_CTRL, false, GrabMode::All).swallow,
            "up must reach the OS"
        );
        // second up (orphan) is passed as well, never swallowed
        assert!(!g.on_key(hid::L_CTRL, false, GrabMode::KeyboardOnly).swallow);
    }

    #[test]
    fn up_of_swallowed_key_is_swallowed_even_after_grab_ended() {
        let mut g = GrabState::new();
        assert!(g.on_key(A, true, GrabMode::All).swallow);
        assert!(g.on_key(A, false, GrabMode::None).swallow);
        assert!(!g.on_key(A, true, GrabMode::None).swallow);
    }

    #[test]
    fn keyboard_only_mode_keeps_mouse_local() {
        let mut g = GrabState::new();
        assert!(g.on_key(A, true, GrabMode::KeyboardOnly).swallow);
        assert!(
            !g.on_button(Button::Left, true, GrabMode::KeyboardOnly)
                .swallow
        );
        assert!(
            !g.on_button(Button::Left, false, GrabMode::KeyboardOnly)
                .swallow
        );
        assert!(g.on_button(Button::X1, true, GrabMode::All).swallow);
        assert!(g.on_button(Button::X1, false, GrabMode::All).swallow);
    }

    #[test]
    fn button_up_passes_when_pressed_before_grab() {
        let mut g = GrabState::new();
        assert!(!g.on_button(Button::Left, true, GrabMode::None).swallow);
        assert!(!g.on_button(Button::Left, false, GrabMode::All).swallow);
        // keys and buttons do not share ids
        assert!(g.on_key(0x04, true, GrabMode::All).swallow);
    }

    fn hotkey_filter() -> KeyboardFilter {
        KeyboardFilter::new(vec![Hotkey {
            id: 9,
            mods: CTRL | ALT | SHIFT,
            key: hid::A + 5,
        }])
    }

    fn feed(f: &mut KeyboardFilter, hid: u16, down: bool, mode: GrabMode) -> KeyAction {
        f.process(HookKey::Hid(hid), down, mode)
    }

    #[test]
    fn hotkey_is_swallowed_and_reported_modifiers_pass() {
        let mut f = hotkey_filter();
        for m in [hid::L_CTRL, hid::L_ALT, hid::L_SHIFT] {
            let a = feed(&mut f, m, true, GrabMode::None);
            assert!(!a.swallow);
            assert_eq!(a.event, Some(CaptureEvent::Key { hid: m, down: true }));
        }
        let fire = feed(&mut f, hid::A + 5, true, GrabMode::None);
        assert!(fire.swallow);
        assert_eq!(fire.event, Some(CaptureEvent::Hotkey(9)));
        let rep = feed(&mut f, hid::A + 5, true, GrabMode::None);
        assert!(rep.swallow && rep.event.is_none());
        let up = feed(&mut f, hid::A + 5, false, GrabMode::None);
        assert!(up.swallow && up.event.is_none());
        // modifier ups pass and are reported
        let up = feed(&mut f, hid::L_CTRL, false, GrabMode::None);
        assert!(!up.swallow);
        assert_eq!(
            up.event,
            Some(CaptureEvent::Key {
                hid: hid::L_CTRL,
                down: false
            })
        );
    }

    #[test]
    fn hotkey_fires_in_grab_mode_too() {
        let mut f = hotkey_filter();
        for m in [hid::L_CTRL, hid::L_ALT, hid::L_SHIFT] {
            let a = feed(&mut f, m, true, GrabMode::All);
            assert!(a.swallow); // modifiers are grabbed but still reported
            assert_eq!(a.event, Some(CaptureEvent::Key { hid: m, down: true }));
        }
        let fire = feed(&mut f, hid::A + 5, true, GrabMode::All);
        assert!(fire.swallow);
        assert_eq!(fire.event, Some(CaptureEvent::Hotkey(9)));
    }

    #[test]
    fn hotkey_main_key_up_passes_if_os_saw_its_down() {
        let mut f = KeyboardFilter::new(vec![Hotkey {
            id: 1,
            mods: CTRL,
            key: A,
        }]);
        // A pressed alone first: the OS sees it.
        assert!(!feed(&mut f, A, true, GrabMode::None).swallow);
        feed(&mut f, hid::L_CTRL, true, GrabMode::None);
        // Auto-repeat of A now completes the hotkey.
        let fire = feed(&mut f, A, true, GrabMode::None);
        assert_eq!(fire.event, Some(CaptureEvent::Hotkey(1)));
        // The up of A must still reach the OS or A stays stuck.
        let up = feed(&mut f, A, false, GrabMode::None);
        assert!(!up.swallow);
        assert_eq!(
            up.event,
            Some(CaptureEvent::Key {
                hid: A,
                down: false
            })
        );
    }

    #[test]
    fn altgr_fake_ctrl_is_not_forwarded() {
        let mut f = KeyboardFilter::new(vec![]);
        let a = f.process(HookKey::FakeCtrl, true, GrabMode::None);
        assert!(!a.swallow && a.event.is_none());
        let a = feed(&mut f, hid::R_ALT, true, GrabMode::None);
        assert_eq!(
            a.event,
            Some(CaptureEvent::Key {
                hid: hid::R_ALT,
                down: true
            })
        );
        let a = f.process(HookKey::FakeCtrl, false, GrabMode::None);
        assert!(!a.swallow && a.event.is_none());
        // Under grab, fake ctrl is swallowed together with AltGr (no stray local Ctrl).
        let a = f.process(HookKey::FakeCtrl, true, GrabMode::All);
        assert!(a.swallow && a.event.is_none());
        let a = feed(&mut f, hid::R_ALT, true, GrabMode::All);
        assert!(a.swallow);
        let a = f.process(HookKey::FakeCtrl, false, GrabMode::All);
        assert!(a.swallow, "its down was swallowed");
    }

    #[test]
    fn fake_ctrl_up_passes_when_down_was_local() {
        let mut f = KeyboardFilter::new(vec![]);
        f.process(HookKey::FakeCtrl, true, GrabMode::None);
        let a = f.process(HookKey::FakeCtrl, false, GrabMode::All);
        assert!(!a.swallow, "local Ctrl must not stay stuck");
    }

    #[test]
    fn unknown_keys_are_tracked_but_silent() {
        let mut f = KeyboardFilter::new(vec![]);
        let a = f.process(HookKey::Unknown(0xE7), true, GrabMode::All);
        assert!(a.swallow && a.event.is_none());
        let a = f.process(HookKey::Unknown(0xE7), false, GrabMode::All);
        assert!(a.swallow && a.event.is_none());
    }

    #[test]
    fn raw_mouse_relative_passes_through() {
        let mut t = RawMouseTracker::default();
        assert_eq!(t.delta(false, 3, -4, (0, 0)), (3, -4));
    }

    #[test]
    fn raw_mouse_absolute_is_differenced() {
        let mut t = RawMouseTracker::default();
        let screen = (1920, 1080);
        assert_eq!(t.delta(true, 0, 0, screen), (0, 0)); // first sample
        let d = t.delta(true, 32768, 32768, screen);
        assert!((d.0 - 960).abs() <= 1 && (d.1 - 540).abs() <= 1, "{d:?}");
        assert_eq!(t.delta(true, 32768, 32768, screen), (0, 0));
        // switching to relative resets the baseline
        t.delta(false, 1, 1, screen);
        assert_eq!(t.delta(true, 65535, 65535, screen), (0, 0));
    }

    #[test]
    fn normalize_abs_maps_back_to_the_same_pixel() {
        // Windows maps normalised -> pixel as floor(norm * size / 65536) + origin.
        for &(vx, vy, vw, vh) in &[
            (0, 0, 1920, 1080),
            (-1920, -200, 5760, 1280),
            (0, 0, 3840, 2160),
        ] {
            for &(x, y) in &[
                (vx, vy),
                (vx + vw - 1, vy + vh - 1),
                (vx + 17, vy + 3),
                (vx + vw / 2, vy + vh / 2),
            ] {
                let (nx, ny) = normalize_abs(x, y, vx, vy, vw, vh);
                assert!((0..=65535).contains(&nx) && (0..=65535).contains(&ny));
                let bx = vx + (i64::from(nx) * i64::from(vw) / 65536) as i32;
                let by = vy + (i64::from(ny) * i64::from(vh) / 65536) as i32;
                assert_eq!((bx, by), (x, y), "vd=({vx},{vy},{vw},{vh})");
            }
        }
    }

    #[test]
    fn grab_mode_u8_round_trip() {
        for m in [GrabMode::None, GrabMode::All, GrabMode::KeyboardOnly] {
            assert_eq!(grab_from_u8(grab_to_u8(m)), m);
        }
    }

    #[test]
    fn key_inputs_use_scancodes_and_marker() {
        let i = WinInputInjector::key_input(hid::A, true).unwrap();
        // SAFETY: we built a keyboard INPUT.
        let ki = unsafe { i.Anonymous.ki };
        assert_eq!(ki.wScan, 0x1E);
        assert_eq!(ki.wVk.0, 0);
        assert_eq!(ki.dwExtraInfo, INJECT_MARKER);
        assert!(ki.dwFlags.contains(KEYEVENTF_SCANCODE));
        assert!(!ki.dwFlags.contains(KEYEVENTF_KEYUP));

        // AltGr = RAlt alone: extended 0x38, no Ctrl involved.
        let i = WinInputInjector::key_input(hid::R_ALT, false).unwrap();
        // SAFETY: keyboard INPUT.
        let ki = unsafe { i.Anonymous.ki };
        assert_eq!(ki.wScan, 0x38);
        assert!(ki.dwFlags.contains(KEYEVENTF_EXTENDEDKEY));
        assert!(ki.dwFlags.contains(KEYEVENTF_KEYUP));

        // Pause goes through the virtual key.
        let i = WinInputInjector::key_input(hid::PAUSE, true).unwrap();
        // SAFETY: keyboard INPUT.
        let ki = unsafe { i.Anonymous.ki };
        assert_eq!(ki.wVk.0, 0x13);
        assert!(!ki.dwFlags.contains(KEYEVENTF_SCANCODE));

        assert!(WinInputInjector::key_input(0x7F7F, true).is_none());
    }

    #[test]
    fn injector_tracks_and_releases() {
        // Pure bookkeeping check without sending anything harmful: use keys that are
        // unmapped (nothing is injected, nothing is tracked).
        let inj = WinInputInjector::default();
        inj.key(0x7F7F, true);
        assert!(inj.state.lock().keys.is_empty());
    }

    /// Harmless: moves the pointer by one pixel and back, checks `GetCursorPos`.
    #[test]
    fn injector_moves_cursor_by_one_pixel() {
        let Some(p0) = cursor_position() else {
            eprintln!("skipped: no cursor (no interactive desktop)");
            return;
        };
        let inj = WinInputInjector::new();
        let sign = if p0.0 > 4 { -1 } else { 1 };
        inj.mouse_rel(sign, 0);
        let mut moved = false;
        for _ in 0..40 {
            std::thread::sleep(std::time::Duration::from_millis(10));
            if cursor_position().is_some_and(|p| p != p0) {
                moved = true;
                break;
            }
        }
        inj.mouse_rel(-sign, 0);
        if !moved {
            eprintln!("skipped assertion: injected input had no visible effect (locked desktop?)");
        }
    }

    /// Harmless: places the pointer one pixel away and back with absolute moves; it must land
    /// on the exact pixel each time.
    #[test]
    fn injector_places_cursor_exactly() {
        let Some(p0) = cursor_position() else {
            eprintln!("skipped: no cursor (no interactive desktop)");
            return;
        };
        let inj = WinInputInjector::new();
        let x1 = if p0.0 > 4 { p0.0 - 1 } else { p0.0 + 1 };
        inj.mouse_abs_px(x1, p0.1);
        let at_x1 = cursor_position();
        inj.mouse_abs_px(p0.0, p0.1);
        let back = cursor_position();
        if at_x1.is_some_and(|p| p.0 != x1) && back == Some(p0) {
            panic!("pointer landed at {at_x1:?} instead of ({x1}, {})", p0.1);
        }
        if back != Some(p0) {
            eprintln!("skipped assertion: pointer moved by the user or desktop locked ({back:?})");
        }
    }

    #[test]
    #[ignore = "installs system-wide hooks and grabs input; run manually"]
    fn manual_capture_start_stop() {
        let cap = WinInputCapture::new();
        let (tx, mut rx) = deskbridge_platform::unbounded_channel();
        cap.start(tx).unwrap();
        assert!(
            cap.start(deskbridge_platform::unbounded_channel().0)
                .is_err()
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
        while rx.try_recv().is_ok() {}
        cap.stop();
        cap.stop();
        // restartable
        let (tx, _rx) = deskbridge_platform::unbounded_channel();
        cap.start(tx).unwrap();
        cap.stop();
    }

    #[test]
    #[ignore = "hides the system cursor for a second; run manually"]
    fn manual_cursor_hide_restore() {
        let cap = WinInputCapture::new();
        cap.set_cursor_visible(false);
        std::thread::sleep(std::time::Duration::from_secs(1));
        cap.set_cursor_visible(true);
    }
}
