//! Windows clipboard backend (plan 8.5 / 8.6).
//!
//! One dedicated STA thread owns a hidden message-only window. It
//! * listens for `WM_CLIPBOARDUPDATE` (via `AddClipboardFormatListener`) and turns changes into
//!   [`ClipboardEvent`]s (skipping our own writes: `DeskBridge.Origin` marker + sequence number),
//! * services commands from other threads (`mpsc` + `PostMessage` wake-up, synchronous replies),
//! * acts as clipboard owner for delayed rendering (`WM_RENDERFORMAT` -> `ClipboardProvider::fetch`),
//! * hosts the virtual-files `IDataObject` (`OleSetClipboard`), see [`dataobject`].
//!
//! Internal data representation is MIME based: UTF-8 text, a UTF-8 HTML document (CF_HTML wrapper
//! stripped), RTF bytes, PNG bytes (DIB/DIBV5/PNG converted) and `text/uri-list`.
//!
//! Do not call backend methods from inside `ClipboardProvider::fetch` / `VirtualFileProvider`
//! callbacks: those run on the clipboard thread and the call would wait for itself.

mod dataobject;
mod format;
mod image;
#[cfg(test)]
mod live_tests;

use deskbridge_platform::clipboard::{
    ClipFormatInfo, ClipboardBackend, ClipboardEvent, ClipboardProvider, FileOffer, VirtualFileProvider, mime,
};
use deskbridge_platform::{PlatformError, Result, UnboundedSender};
use format::{
    NAME_CAN_INCLUDE_HISTORY, NAME_CAN_UPLOAD_CLOUD, CF_DIB, CF_DIBV5, CF_HDROP, CF_REGISTERED_FIRST, CF_UNICODETEXT, EAGER_TEXT_LIMIT, Kind, NAME_FILE_CONTENTS, NAME_FILE_DESCRIPTOR, NAME_HTML, NAME_LOGICAL_PERFORMED_DROP_EFFECT, NAME_ORIGIN, NAME_PASTE_SUCCEEDED, NAME_PERFORMED_DROP_EFFECT,
    NAME_PNG, NAME_PNG_ALIAS, NAME_PREFERRED_DROP_EFFECT, NAME_RTF, RegisteredIds,
};
use parking_lot::Mutex;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Once, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;
use windows::Win32::Foundation::{SetLastError, WIN32_ERROR, CLIPBRD_E_CANT_OPEN, GlobalFree, HANDLE, HGLOBAL, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::{
    AddClipboardFormatListener, CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData, GetClipboardFormatNameW, GetClipboardOwner,
    GetClipboardSequenceNumber, IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW, RemoveClipboardFormatListener, SetClipboardData,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Ole::{DROPEFFECT_COPY, OleInitialize, OleIsCurrentClipboard, OleSetClipboard, OleUninitialize};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_USERDATA, GetMessageW, GetWindowLongPtrW, HWND_MESSAGE, MSG, PostMessageW,
    PostQuitMessage, RegisterClassW, SetWindowLongPtrW, TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLIPBOARDUPDATE, WM_DESTROY,
    WM_DESTROYCLIPBOARD, WM_RENDERALLFORMATS, WM_RENDERFORMAT, WNDCLASSW,
};
use windows::core::{PCWSTR, w};

/// Wake-up message for the command queue.
const WM_APP_CMD: u32 = WM_APP + 1;
/// `OpenClipboard` retry policy: another process may hold the clipboard briefly.
const OPEN_RETRIES: u32 = 10;
const OPEN_RETRY_SLEEP: Duration = Duration::from_millis(15);
/// How long a caller waits for the clipboard thread to answer a command.
const CALL_TIMEOUT: Duration = Duration::from_secs(60);
/// Safety net for `ClipboardProvider::fetch` during `WM_RENDERFORMAT`. The provider is expected
/// to time out on its own (plan: 5 s); this only keeps a hung provider from freezing the thread
/// (and the pasting application) forever.
const RENDER_TIMEOUT: Duration = Duration::from_secs(30);
/// Do not read text blocks larger than this eagerly (UTF-16 bytes).
const MAX_EAGER_TEXT_BYTES: usize = 4 * 1024 * 1024;

static NEXT_ORIGIN: AtomicU64 = AtomicU64::new(1);
static REGISTER_CLASS: Once = Once::new();

fn os_err(context: &str, e: &windows::core::Error) -> PlatformError {
    PlatformError::Os { code: i64::from(e.code().0), msg: format!("{context}: {e}") }
}

fn next_origin_payload() -> [u8; 8] {
    // Mix in the process id so markers from several Desk Bridge processes differ.
    let n = NEXT_ORIGIN.fetch_add(1, Ordering::Relaxed) | (u64::from(std::process::id()) << 32);
    n.to_le_bytes()
}

// ---------------------------------------------------------------------------------------------
// Public backend

type Reply<T> = mpsc::Sender<Result<T>>;

enum Cmd {
    Read { mime: String, reply: Reply<Option<Vec<u8>>> },
    SetText { text: String, reply: Reply<()> },
    Offer { formats: Vec<ClipFormatInfo>, provider: Arc<dyn ClipboardProvider>, reply: Reply<()> },
    OfferFiles { files: Vec<FileOffer>, provider: Arc<dyn VirtualFileProvider>, reply: Reply<()> },
    OfferLocalFiles { paths: Vec<PathBuf>, reply: Reply<()> },
    Stop,
}

struct Running {
    cmd_tx: mpsc::Sender<Cmd>,
    /// Window handle of the clipboard thread (as integer; `HWND` is not `Send`).
    hwnd: isize,
    join: JoinHandle<()>,
}

/// Windows implementation of [`ClipboardBackend`].
#[derive(Default)]
pub struct WinClipboard {
    running: Mutex<Option<Running>>,
}

impl WinClipboard {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn wake(hwnd: isize) -> Result<()> {
        // SAFETY: PostMessageW is thread safe; a stale handle merely fails.
        unsafe { PostMessageW(HWND(hwnd as *mut _), WM_APP_CMD, WPARAM(0), LPARAM(0)) }.map_err(|e| os_err("PostMessage", &e))
    }

    /// Sends a command to the clipboard thread and waits for the answer.
    fn call<T>(&self, make: impl FnOnce(Reply<T>) -> Cmd) -> Result<T> {
        let (cmd_tx, hwnd) = {
            let g = self.running.lock();
            let r = g.as_ref().ok_or_else(|| PlatformError::other("clipboard backend not started"))?;
            (r.cmd_tx.clone(), r.hwnd)
        };
        let (tx, rx) = mpsc::channel();
        cmd_tx.send(make(tx)).map_err(|_| PlatformError::other("clipboard thread has exited"))?;
        Self::wake(hwnd)?;
        match rx.recv_timeout(CALL_TIMEOUT) {
            Ok(r) => r,
            Err(mpsc::RecvTimeoutError::Timeout) => Err(PlatformError::other("clipboard command timed out")),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(PlatformError::other("clipboard thread has exited")),
        }
    }
}

impl ClipboardBackend for WinClipboard {
    fn start(&self, tx: UnboundedSender<ClipboardEvent>) -> Result<()> {
        let mut g = self.running.lock();
        if g.is_some() {
            return Err(PlatformError::other("clipboard backend already started"));
        }
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<isize>>(1);
        let join = std::thread::Builder::new()
            .name("deskbridge-clipboard".into())
            .spawn(move || thread_main(tx, cmd_rx, ready_tx))
            .map_err(|e| PlatformError::other(format!("spawn clipboard thread: {e}")))?;
        match ready_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(hwnd)) => {
                *g = Some(Running { cmd_tx, hwnd, join });
                Ok(())
            }
            Ok(Err(e)) => {
                let _ = join.join();
                Err(e)
            }
            Err(_) => Err(PlatformError::other("clipboard thread did not start")),
        }
    }

    fn stop(&self) {
        let Some(r) = self.running.lock().take() else { return };
        if r.cmd_tx.send(Cmd::Stop).is_ok() {
            let _ = Self::wake(r.hwnd);
        }
        let _ = r.join.join();
    }

    fn read(&self, mime: &str) -> Result<Option<Vec<u8>>> {
        self.call(|reply| Cmd::Read { mime: mime.to_owned(), reply })
    }

    fn set_text(&self, text: &str) -> Result<()> {
        self.call(|reply| Cmd::SetText { text: text.to_owned(), reply })
    }

    fn offer(&self, formats: Vec<ClipFormatInfo>, provider: Arc<dyn ClipboardProvider>) -> Result<()> {
        self.call(|reply| Cmd::Offer { formats, provider, reply })
    }

    fn offer_files(&self, files: Vec<FileOffer>, provider: Arc<dyn VirtualFileProvider>) -> Result<()> {
        self.call(|reply| Cmd::OfferFiles { files, provider, reply })
    }

    fn offer_local_files(&self, paths: Vec<PathBuf>) -> Result<()> {
        self.call(|reply| Cmd::OfferLocalFiles { paths, reply })
    }
}

impl Drop for WinClipboard {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---------------------------------------------------------------------------------------------
// Clipboard thread

/// Registered clipboard formats.
struct Fmt {
    ids: RegisteredIds,
    origin: u32,
    can_history: u32,
    can_cloud: u32,
    preferred_effect: u32,
    performed_effect: u32,
    logical_performed_effect: u32,
    paste_succeeded: u32,
    file_descriptor: u32,
    file_contents: u32,
}

fn register_format(name: &str) -> u32 {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` is NUL terminated and outlives the call.
    unsafe { RegisterClipboardFormatW(PCWSTR(wide.as_ptr())) }
}

impl Fmt {
    fn register() -> Self {
        Self {
            ids: RegisteredIds {
                html: register_format(NAME_HTML),
                rtf: register_format(NAME_RTF),
                png: register_format(NAME_PNG),
                png_alias: register_format(NAME_PNG_ALIAS),
            },
            origin: register_format(NAME_ORIGIN),
            can_history: register_format(NAME_CAN_INCLUDE_HISTORY),
            can_cloud: register_format(NAME_CAN_UPLOAD_CLOUD),
            preferred_effect: register_format(NAME_PREFERRED_DROP_EFFECT),
            performed_effect: register_format(NAME_PERFORMED_DROP_EFFECT),
            logical_performed_effect: register_format(NAME_LOGICAL_PERFORMED_DROP_EFFECT),
            paste_succeeded: register_format(NAME_PASTE_SUCCEEDED),
            file_descriptor: register_format(NAME_FILE_DESCRIPTOR),
            file_contents: register_format(NAME_FILE_CONTENTS),
        }
    }

    fn object_formats(&self) -> dataobject::ObjectFormats {
        dataobject::ObjectFormats {
            file_descriptor: self.file_descriptor as u16,
            file_contents: self.file_contents as u16,
            preferred_drop_effect: self.preferred_effect as u16,
            performed_drop_effect: self.performed_effect as u16,
            logical_performed_drop_effect: self.logical_performed_effect as u16,
            paste_succeeded: self.paste_succeeded as u16,
            origin: self.origin as u16,
        }
    }
}

/// What we currently own on the clipboard beyond plain, immediately available data.
enum Owned {
    None,
    /// Delayed-rendered formats backed by a provider.
    Delayed(DelayedOffer),
    /// Virtual files exposed through OLE.
    Ole(windows::Win32::System::Com::IDataObject),
}

struct DelayedOffer {
    provider: Arc<dyn ClipboardProvider>,
    /// Fetched data per MIME (PNG and DIBV5 share one fetch). Dropped with the offer.
    cache: HashMap<&'static str, Arc<Vec<u8>>>,
}

/// State of the clipboard thread. Lives in a `Box` whose address is stored in the window's
/// `GWLP_USERDATA`; the window procedure only ever gets `&Ctx`, so all mutation goes through
/// `Cell`/`RefCell` and no borrow is held across a Win32 call that may re-enter the window
/// procedure (`EmptyClipboard`, `OleSetClipboard`, `GetClipboardData`, ...).
struct Ctx {
    fmt: Fmt,
    hwnd: Cell<HWND>,
    tx: UnboundedSender<ClipboardEvent>,
    cmd_rx: mpsc::Receiver<Cmd>,
    /// Sequence number right after our latest write.
    own_seq: Cell<u32>,
    /// Last sequence number an update was processed for (de-duplicates repeated notifications).
    last_seen_seq: Cell<u32>,
    owned: RefCell<Owned>,
}

fn thread_main(tx: UnboundedSender<ClipboardEvent>, cmd_rx: mpsc::Receiver<Cmd>, ready: mpsc::SyncSender<Result<isize>>) {
    // SAFETY: plain Win32 setup on this thread; every handle created here is released below.
    unsafe {
        if let Err(e) = OleInitialize(None) {
            let _ = ready.send(Err(os_err("OleInitialize", &e)));
            return;
        }
        let ctx = Box::new(Ctx {
            fmt: Fmt::register(),
            hwnd: Cell::new(HWND::default()),
            tx,
            cmd_rx,
            own_seq: Cell::new(0),
            last_seen_seq: Cell::new(0),
            owned: RefCell::new(Owned::None),
        });
        let hwnd = match create_window() {
            Ok(h) => h,
            Err(e) => {
                let _ = ready.send(Err(e));
                drop(ctx);
                OleUninitialize();
                return;
            }
        };
        ctx.hwnd.set(hwnd);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, std::ptr::from_ref::<Ctx>(&ctx) as isize);
        if let Err(e) = AddClipboardFormatListener(hwnd) {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            let _ = DestroyWindow(hwnd);
            let _ = ready.send(Err(os_err("AddClipboardFormatListener", &e)));
            drop(ctx);
            OleUninitialize();
            return;
        }
        // Content that is already on the clipboard is not a "change".
        ctx.last_seen_seq.set(GetClipboardSequenceNumber());
        let _ = ready.send(Ok(hwnd.0 as isize));

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, HWND::default(), 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        // The window is gone (Stop command) -> no more callbacks into `ctx`.
        drop(ctx);
        OleUninitialize();
    }
}

fn create_window() -> Result<HWND> {
    // SAFETY: standard window class registration / creation; the class name is a static literal.
    unsafe {
        let module = GetModuleHandleW(None).map_err(|e| os_err("GetModuleHandle", &e))?;
        let hinstance = HINSTANCE(module.0);
        REGISTER_CLASS.call_once(|| {
            let wc = WNDCLASSW { lpfnWndProc: Some(wnd_proc), hInstance: hinstance, lpszClassName: w!("DeskBridgeClipboardWindow"), ..Default::default() };
            RegisterClassW(&wc);
        });
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("DeskBridgeClipboardWindow"),
            w!("Desk Bridge clipboard"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            None,
            hinstance,
            None,
        )
        .map_err(|e| os_err("CreateWindowEx", &e))
    }
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: GWLP_USERDATA is either 0 or the address of the `Ctx` box owned by the thread, which
    // outlives the window (cleared/dropped only after the message loop ended).
    let ctx = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const Ctx;
    if let Some(ctx) = unsafe { ctx.as_ref() } {
        // A panic must never unwind through the window procedure.
        let handled = catch_unwind(AssertUnwindSafe(|| ctx.handle(hwnd, msg, wparam)));
        if let Ok(true) = handled {
            return LRESULT(0);
        }
    }
    if msg == WM_DESTROY {
        // SAFETY: trivially safe; ends this thread's message loop.
        unsafe { PostQuitMessage(0) };
        return LRESULT(0);
    }
    // SAFETY: forwarding the original message parameters.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// RAII for an opened clipboard.
struct ClipGuard;

impl Drop for ClipGuard {
    fn drop(&mut self) {
        // SAFETY: only constructed after a successful OpenClipboard on this thread.
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

fn open_clipboard(hwnd: HWND) -> Result<ClipGuard> {
    let mut last = None;
    for attempt in 0..OPEN_RETRIES {
        // SAFETY: plain Win32 call; `hwnd` is our window.
        match unsafe { OpenClipboard(hwnd) } {
            Ok(()) => return Ok(ClipGuard),
            Err(e) => {
                last = Some(e);
                if attempt + 1 < OPEN_RETRIES {
                    std::thread::sleep(OPEN_RETRY_SLEEP);
                }
            }
        }
    }
    Err(last.map_or_else(|| PlatformError::other("OpenClipboard failed"), |e| os_err("OpenClipboard (clipboard busy)", &e)))
}

/// Copies the bytes of a global-memory clipboard handle. Clipboard must be open.
fn clipboard_bytes(fmt: u32) -> Option<Vec<u8>> {
    // SAFETY: the clipboard is open (caller contract); the handle is owned by the clipboard and
    // only read while locked.
    unsafe {
        IsClipboardFormatAvailable(fmt).ok()?;
        let h = GetClipboardData(fmt).ok()?;
        let hg = HGLOBAL(h.0);
        let size = GlobalSize(hg);
        let p = GlobalLock(hg).cast::<u8>();
        if p.is_null() {
            return None;
        }
        let v = std::slice::from_raw_parts(p, size).to_vec();
        let _ = GlobalUnlock(hg);
        Some(v)
    }
}

/// Size of the global-memory handle for `fmt` without copying. Clipboard must be open.
fn clipboard_size(fmt: u32) -> Option<usize> {
    // SAFETY: see `clipboard_bytes`.
    unsafe {
        let h = GetClipboardData(fmt).ok()?;
        Some(GlobalSize(HGLOBAL(h.0)))
    }
}

/// Places `bytes` on the (open) clipboard as `fmt`. Ownership of the memory passes to the
/// clipboard on success and is freed here on failure.
fn set_clipboard_bytes(fmt: u32, bytes: &[u8]) -> Result<()> {
    // SAFETY: the block is large enough for `bytes`, locked only while copying; on success the
    // system owns it (must not be freed), on failure we free it.
    unsafe {
        let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1)).map_err(|e| os_err("GlobalAlloc", &e))?;
        let p = GlobalLock(h).cast::<u8>();
        if p.is_null() {
            let e = windows::core::Error::from_win32();
            let _ = GlobalFree(h);
            return Err(os_err("GlobalLock", &e));
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
        let _ = GlobalUnlock(h);
        if let Err(e) = SetClipboardData(fmt, HANDLE(h.0)) {
            let _ = GlobalFree(h);
            return Err(os_err("SetClipboardData", &e));
        }
    }
    Ok(())
}

fn registered_name(id: u32) -> Option<String> {
    if id < CF_REGISTERED_FIRST {
        return None;
    }
    let mut buf = [0u16; 256];
    // SAFETY: `buf` is a valid, writable buffer.
    let n = unsafe { GetClipboardFormatNameW(id, &mut buf) };
    (n > 0).then(|| String::from_utf16_lossy(&buf[..n as usize]))
}

/// Runs `provider.fetch` on a helper thread with a safety timeout.
fn fetch_with_timeout(provider: Arc<dyn ClipboardProvider>, mime: &'static str) -> Result<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("deskbridge-clipboard-fetch".into())
        .spawn(move || {
            let r = catch_unwind(AssertUnwindSafe(|| provider.fetch(mime)));
            let _ = tx.send(r);
        })
        .map_err(|e| PlatformError::other(format!("spawn fetch thread: {e}")))?;
    match rx.recv_timeout(RENDER_TIMEOUT) {
        Ok(Ok(r)) => r,
        Ok(Err(_)) => Err(PlatformError::other("clipboard provider panicked")),
        Err(_) => Err(PlatformError::other("clipboard provider timed out")),
    }
}

/// Converts internal representation to the bytes for a clipboard format.
fn to_clipboard_bytes(kind: Kind, data: &[u8]) -> std::result::Result<Vec<u8>, String> {
    match kind {
        Kind::Text => Ok(format::string_to_utf16_bytes(&String::from_utf8_lossy(data))),
        Kind::Html => {
            let mut v = format::build_cf_html(&String::from_utf8_lossy(data));
            v.push(0);
            Ok(v)
        }
        Kind::Rtf => {
            let mut v = format::trim_trailing_nuls(data).to_vec();
            v.push(0);
            Ok(v)
        }
        Kind::Png => Ok(data.to_vec()),
        Kind::DibV5 => image::png_to_dibv5(data),
        Kind::Dib | Kind::HDrop => Err("format cannot be rendered".into()),
    }
}

/// What to hand to a pasting application when the provider failed, so it does not wait forever.
/// Images are left unset (`GetClipboardData` then fails fast) because a bogus DIB can crash
/// careless readers.
fn failure_placeholder(kind: Kind) -> Option<Vec<u8>> {
    match kind {
        Kind::Text => Some(format::string_to_utf16_bytes("")),
        Kind::Html => to_clipboard_bytes(Kind::Html, b"").ok(),
        Kind::Rtf => Some(b"{\\rtf1}\0".to_vec()),
        _ => None,
    }
}

impl Ctx {
    fn hwnd(&self) -> HWND {
        self.hwnd.get()
    }

    /// Returns `true` when the message was handled.
    fn handle(&self, hwnd: HWND, msg: u32, wparam: WPARAM) -> bool {
        match msg {
            WM_CLIPBOARDUPDATE => {
                self.on_update();
                true
            }
            WM_RENDERFORMAT => {
                self.render_format(wparam.0 as u32);
                true
            }
            WM_RENDERALLFORMATS => {
                // Only sent while we are being torn down. Rendering everything would block on the
                // remote peer for no benefit; `shutdown` already emptied delayed content.
                tracing::debug!("WM_RENDERALLFORMATS ignored");
                true
            }
            WM_DESTROYCLIPBOARD => {
                // Someone (maybe ourselves, before a new write) emptied the clipboard.
                let mut o = self.owned.borrow_mut();
                if matches!(*o, Owned::Delayed(_)) {
                    *o = Owned::None;
                }
                true
            }
            WM_APP_CMD => {
                self.drain_commands(hwnd);
                true
            }
            _ => false,
        }
    }

    // ------------------------------------------------------------------ events

    fn on_update(&self) {
        // SAFETY: plain Win32 query.
        let seq = unsafe { GetClipboardSequenceNumber() };
        if seq == self.last_seen_seq.get() {
            return; // duplicate notification
        }
        self.last_seen_seq.set(seq);
        if seq == self.own_seq.get() {
            return; // our own write
        }
        let guard = match open_clipboard(self.hwnd()) {
            Ok(g) => g,
            Err(e) => {
                tracing::warn!("clipboard changed but could not be opened: {e}");
                self.last_seen_seq.set(0); // let a repeated notification retry
                return;
            }
        };
        let event = self.snapshot();
        drop(guard);
        if let Some(ev) = event {
            let _ = self.tx.send(ev);
        }
    }

    /// Builds the change event from the (open) clipboard. `None` for our own content.
    fn snapshot(&self) -> Option<ClipboardEvent> {
        let mut formats: Vec<u32> = Vec::new();
        let mut names: Vec<(u32, String)> = Vec::new();
        let mut f = 0;
        loop {
            // SAFETY: the clipboard is open.
            f = unsafe { EnumClipboardFormats(f) };
            if f == 0 {
                break;
            }
            formats.push(f);
            if let Some(n) = registered_name(f) {
                names.push((f, n));
            }
        }
        if formats.contains(&self.fmt.origin) {
            return None; // written by us (also covers OLE content, whose marker is delayed)
        }
        // Foreign content replaced whatever we offered through OLE.
        {
            let mut o = self.owned.borrow_mut();
            if matches!(*o, Owned::Ole(_)) {
                *o = Owned::None;
            }
        }

        let name_list: Vec<&str> = names.iter().map(|(_, n)| n.as_str()).collect();
        let sensitive = format::is_sensitive(&name_list, |n| {
            let id = names.iter().find(|(_, x)| x.eq_ignore_ascii_case(n))?.0;
            let b = clipboard_bytes(id)?;
            Some(u32::from_le_bytes(b.get(..4)?.try_into().ok()?))
        });

        let mimes = format::mimes_of_formats(&self.fmt.ids, &formats);
        if mimes.is_empty() {
            return None; // cleared, or only formats we cannot transfer
        }
        let mut infos: Vec<ClipFormatInfo> = mimes.iter().map(|m| ClipFormatInfo { mime: (*m).to_owned(), size: 0 }).collect();
        let mut text = None;
        let mut files = Vec::new();
        if !sensitive {
            if mimes.contains(&mime::TEXT) {
                text = self.read_eager_text(&mut infos);
            }
            if mimes.contains(&mime::URI_LIST)
                && let Some(b) = clipboard_bytes(CF_HDROP)
            {
                files = format::parse_dropfiles(&b);
            }
        }
        Some(ClipboardEvent::Changed { formats: infos, sensitive, text, files })
    }

    /// Reads `CF_UNICODETEXT` if small enough and records the text size in `infos`.
    fn read_eager_text(&self, infos: &mut [ClipFormatInfo]) -> Option<String> {
        let size = clipboard_size(CF_UNICODETEXT)?;
        let set_size = |infos: &mut [ClipFormatInfo], s: u64| {
            if let Some(i) = infos.iter_mut().find(|i| i.mime == mime::TEXT) {
                i.size = s;
            }
        };
        if size > MAX_EAGER_TEXT_BYTES {
            set_size(infos, (size / 2) as u64); // lower bound of the UTF-8 length
            return None;
        }
        let bytes = clipboard_bytes(CF_UNICODETEXT)?;
        let text = format::utf16_bytes_to_string(&bytes);
        set_size(infos, text.len() as u64);
        (text.len() < EAGER_TEXT_LIMIT).then_some(text)
    }

    // ------------------------------------------------------------------ commands

    fn drain_commands(&self, hwnd: HWND) {
        while let Ok(cmd) = self.cmd_rx.try_recv() {
            let outcome = catch_unwind(AssertUnwindSafe(|| match cmd {
                Cmd::Read { mime, reply } => {
                    let _ = reply.send(self.read_mime(&mime));
                    false
                }
                Cmd::SetText { text, reply } => {
                    let _ = reply.send(self.set_text(&text));
                    false
                }
                Cmd::Offer { formats, provider, reply } => {
                    let _ = reply.send(self.offer(&formats, provider));
                    false
                }
                Cmd::OfferFiles { files, provider, reply } => {
                    let _ = reply.send(self.offer_files(&files, provider));
                    false
                }
                Cmd::OfferLocalFiles { paths, reply } => {
                    let _ = reply.send(self.offer_local_files(&paths));
                    false
                }
                Cmd::Stop => true,
            }));
            match outcome {
                Ok(true) => {
                    self.shutdown(hwnd);
                    return;
                }
                Ok(false) => {}
                Err(_) => tracing::error!("panic while handling a clipboard command"),
            }
        }
    }

    /// Removes the listener, drops what we still own and destroys the window (ends the pump).
    ///
    /// Plain data we wrote (text, `CF_HDROP`) stays on the clipboard. Delayed formats and virtual
    /// files cannot be served any more once we are gone, so they are removed instead of being
    /// flushed (flushing virtual files would pull every file over the network).
    fn shutdown(&self, hwnd: HWND) {
        let owned = std::mem::replace(&mut *self.owned.borrow_mut(), Owned::None);
        // SAFETY: all calls operate on our own window / clipboard state on this thread.
        unsafe {
            let _ = RemoveClipboardFormatListener(hwnd);
            match owned {
                Owned::None => {}
                Owned::Delayed(_) => {
                    if let Ok(_g) = open_clipboard(hwnd)
                        && GetClipboardOwner().is_ok_and(|o| o == hwnd)
                    {
                        let _ = EmptyClipboard();
                    }
                }
                Owned::Ole(obj) => {
                    if OleIsCurrentClipboard(&obj).is_ok() {
                        let _ = OleSetClipboard(None::<&windows::Win32::System::Com::IDataObject>);
                    }
                }
            }
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            let _ = DestroyWindow(hwnd);
        }
    }

    // ------------------------------------------------------------------ writing

    /// Opens and empties the clipboard with us as owner.
    fn begin_write(&self) -> Result<ClipGuard> {
        let guard = open_clipboard(self.hwnd())?;
        // SAFETY: the clipboard is open. May synchronously dispatch WM_DESTROYCLIPBOARD to us.
        unsafe { EmptyClipboard() }.map_err(|e| os_err("EmptyClipboard", &e))?;
        *self.owned.borrow_mut() = Owned::None;
        Ok(guard)
    }

    fn write_origin(&self) -> Result<()> {
        set_clipboard_bytes(self.fmt.origin, &next_origin_payload())
    }

    fn finish_write(&self, guard: ClipGuard) {
        drop(guard);
        // SAFETY: plain query.
        self.own_seq.set(unsafe { GetClipboardSequenceNumber() });
    }

    fn set_text(&self, text: &str) -> Result<()> {
        let guard = self.begin_write()?;
        set_clipboard_bytes(CF_UNICODETEXT, &format::string_to_utf16_bytes(text))?;
        self.write_origin()?;
        self.finish_write(guard);
        Ok(())
    }

    fn offer(&self, formats: &[ClipFormatInfo], provider: Arc<dyn ClipboardProvider>) -> Result<()> {
        let mut kinds: Vec<Kind> = Vec::new();
        for f in formats {
            let ks = format::kinds_for_mime(&f.mime);
            if ks.is_empty() {
                tracing::warn!("clipboard offer: unsupported mime {}", f.mime);
            }
            for &k in ks {
                if !kinds.contains(&k) {
                    kinds.push(k);
                }
            }
        }
        if kinds.is_empty() {
            return Err(PlatformError::other("clipboard offer contains no supported format"));
        }
        let guard = self.begin_write()?;
        *self.owned.borrow_mut() = Owned::Delayed(DelayedOffer { provider, cache: HashMap::new() });
        let mut registered = 0;
        for k in kinds {
            // SAFETY: the clipboard is open and owned by our window; NULL handle = delayed rendering.
            // The last-error value is cleared first because success is signalled by "NULL, no error".
            unsafe { SetLastError(WIN32_ERROR(0)) };
            match unsafe { SetClipboardData(self.fmt.ids.id_of(k), HANDLE::default()) } {
                Ok(_) => registered += 1,
                // Delayed rendering returns a NULL handle on success, which the binding reports
                // as an error with error code 0.
                Err(e) if e.code().is_ok() => registered += 1,
                Err(e) => tracing::warn!("delayed SetClipboardData({k:?}) failed: {e}"),
            }
        }
        if registered == 0 {
            *self.owned.borrow_mut() = Owned::None;
            return Err(PlatformError::other("SetClipboardData failed for all offered formats"));
        }
        self.write_origin()?;
        // Keep Windows clipboard history / cloud clipboard from rendering (= fetching over the
        // network) every delayed format the moment it appears.
        set_clipboard_bytes(self.fmt.can_history, &0u32.to_le_bytes())?;
        set_clipboard_bytes(self.fmt.can_cloud, &0u32.to_le_bytes())?;
        self.finish_write(guard);
        Ok(())
    }

    fn offer_local_files(&self, paths: &[PathBuf]) -> Result<()> {
        if paths.is_empty() {
            return Err(PlatformError::other("no files to offer"));
        }
        let guard = self.begin_write()?;
        set_clipboard_bytes(CF_HDROP, &format::build_dropfiles(paths))?;
        // Explorer must copy, not move, files that arrived from the other machine.
        set_clipboard_bytes(self.fmt.preferred_effect, &DROPEFFECT_COPY.0.to_le_bytes())?;
        self.write_origin()?;
        self.finish_write(guard);
        Ok(())
    }

    fn offer_files(&self, files: &[FileOffer], provider: Arc<dyn VirtualFileProvider>) -> Result<()> {
        if files.is_empty() {
            return Err(PlatformError::other("no files to offer"));
        }
        let obj = dataobject::create_virtual_files_object(files, provider, self.fmt.object_formats(), next_origin_payload().to_vec())
            .map_err(PlatformError::Other)?;
        let mut last = None;
        for attempt in 0..OPEN_RETRIES {
            // SAFETY: `obj` is a live COM object; OLE takes its own reference. May dispatch
            // WM_DESTROYCLIPBOARD for the previous content.
            match unsafe { OleSetClipboard(&obj) } {
                Ok(()) => {
                    *self.owned.borrow_mut() = Owned::Ole(obj);
                    // SAFETY: plain query.
                    self.own_seq.set(unsafe { GetClipboardSequenceNumber() });
                    return Ok(());
                }
                Err(e) => {
                    let busy = e.code() == CLIPBRD_E_CANT_OPEN;
                    last = Some(e);
                    if !busy {
                        break;
                    }
                    if attempt + 1 < OPEN_RETRIES {
                        std::thread::sleep(OPEN_RETRY_SLEEP);
                    }
                }
            }
        }
        Err(last.map_or_else(|| PlatformError::other("OleSetClipboard failed"), |e| os_err("OleSetClipboard", &e)))
    }

    // ------------------------------------------------------------------ reading

    fn read_mime(&self, m: &str) -> Result<Option<Vec<u8>>> {
        let _guard = open_clipboard(self.hwnd())?;
        let ids = &self.fmt.ids;
        Ok(match m {
            mime::TEXT => clipboard_bytes(CF_UNICODETEXT).map(|b| format::utf16_bytes_to_string(&b).into_bytes()),
            mime::HTML => clipboard_bytes(ids.html).and_then(|b| format::parse_cf_html(&b)).map(String::into_bytes),
            mime::RTF => clipboard_bytes(ids.rtf).map(|b| format::trim_trailing_nuls(&b).to_vec()),
            mime::URI_LIST => {
                clipboard_bytes(CF_HDROP).map(|b| format::paths_to_uri_list(&format::parse_dropfiles(&b)).into_bytes()).filter(|b| !b.is_empty())
            }
            mime::PNG => {
                if let Some(b) = clipboard_bytes(ids.png).or_else(|| clipboard_bytes(ids.png_alias)) {
                    Some(b)
                } else if let Some(dib) = clipboard_bytes(CF_DIBV5).or_else(|| clipboard_bytes(CF_DIB)) {
                    Some(image::dib_to_png(&dib).map_err(|e| PlatformError::other(format!("clipboard image conversion: {e}")))?)
                } else {
                    None
                }
            }
            _ => None,
        })
    }

    // ------------------------------------------------------------------ delayed rendering

    /// `WM_RENDERFORMAT`: the clipboard is already open by the requester; we must only call
    /// `SetClipboardData` (never `OpenClipboard`/`EmptyClipboard`).
    fn render_format(&self, fmt: u32) {
        let Some(kind) = self.fmt.ids.kind_of(fmt) else {
            tracing::debug!("WM_RENDERFORMAT for unknown format {fmt}");
            return;
        };
        let m = kind.mime();
        let (provider, cached) = match &*self.owned.borrow() {
            Owned::Delayed(d) => (d.provider.clone(), d.cache.get(m).cloned()),
            _ => return, // stale request (offer already replaced)
        };
        let data = match cached {
            Some(d) => Some(d),
            None => match fetch_with_timeout(provider, m) {
                Ok(v) => {
                    let a = Arc::new(v);
                    if let Owned::Delayed(d) = &mut *self.owned.borrow_mut() {
                        d.cache.insert(m, a.clone());
                    }
                    Some(a)
                }
                Err(e) => {
                    tracing::warn!("clipboard fetch({m}) failed: {e}");
                    None
                }
            },
        };
        let bytes = data
            .and_then(|d| match to_clipboard_bytes(kind, &d) {
                Ok(b) => Some(b),
                Err(e) => {
                    tracing::warn!("clipboard conversion for {m} failed: {e}");
                    None
                }
            })
            .or_else(|| failure_placeholder(kind));
        if let Some(b) = bytes
            && let Err(e) = set_clipboard_bytes(fmt, &b)
        {
            tracing::warn!("clipboard render of format {fmt} failed: {e}");
        }
    }
}
