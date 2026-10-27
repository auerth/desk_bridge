//! DXGI Desktop Duplication capturer (`ScreenCapturer`).
//!
//! * runs on its own time-critical thread; the duplicated frame is copied into a small pool of GPU
//!   textures and released at once (`ReleaseFrame`), so the compositor is never held up;
//! * only real picture changes produce `CaptureOutput::Frame` (`LastPresentTime != 0`); pointer
//!   movement alone produces `CaptureOutput::Cursor`;
//! * pointer position and shape are delivered separately from the image (never burnt in);
//! * `DXGI_ERROR_ACCESS_LOST` (mode change, UAC, lock screen) re-creates the duplication; an
//!   interruption that lasts is reported as `CaptureOutput::Status`;
//! * `reset` re-creates it on request (the engine saw the picture stand still during input);
//! * while the secure desktop (UAC, lock screen, logon) is up the duplication is refused with
//!   `E_ACCESSDENIED`. With a [`SecureFrameSource`] the picture then comes from the SYSTEM helper
//!   as GPU-shared textures (ADR 0007) and takes the same `CaptureOutput` path; without one, or
//!   when the helper cannot capture, the viewer keeps the "interruption" placeholder.
//!
//! IddCx virtual outputs are plain DXGI outputs on the render adapter selected in the driver
//! settings, so the same code captures them (not testable on a machine without that driver).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use deskbridge_platform::screen::ScreenProvider;
use deskbridge_platform::video::{
    CaptureOutput, CursorShapeData, CursorState, Frame, FrameData, ScreenCapturer,
};
use deskbridge_platform::{PlatformError, Result, UnboundedSender};
use windows::Win32::Foundation::E_ACCESSDENIED;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT, ID3D11Device, ID3D11DeviceContext, ID3D11Resource, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
    DXGI_OUTDUPL_POINTER_SHAPE_INFO, IDXGIAdapter1, IDXGIOutput, IDXGIOutput1,
    IDXGIOutputDuplication, IDXGIResource,
};
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::System::Threading::{
    GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL,
};
use windows::core::Interface;

use super::common::{
    D3dDevice, GpuFrame, OutputInfo, adapter_by_index, adapter_by_luid, create_device,
    list_outputs, luid_to_i64, now_us, os_err, qpc_to_us,
};
use super::secure::{CopyOutcome, SecureConsumer, SecureFrameSource, SecurePoll};

const SHAPE_MONOCHROME: u32 = 1;
const SHAPE_COLOR: u32 = 2;
const SHAPE_MASKED_COLOR: u32 = 4;
/// Pool of copy textures. The encoder holds at most one while converting; a few spare slots absorb
/// scheduling jitter. When all are in use, new frames are dropped (back-pressure) instead of
/// overwriting a picture that is still being read.
const POOL_SIZE: usize = 4;

/// Which output to capture.
#[derive(Clone)]
enum Target {
    /// Resolve through the `ScreenProvider` id given to `start`.
    ScreenId,
    /// Explicit DXGI adapter/output index.
    Output { adapter: u32, output: u32 },
}

/// DXGI Desktop Duplication capturer.
pub struct DxgiCapturer {
    resolver: Option<Arc<dyn ScreenProvider>>,
    target: Target,
    adapter_name: Arc<Mutex<String>>,
    stop: Arc<AtomicBool>,
    /// Set by `reset`, taken by the capture thread.
    reset: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    drops: Arc<std::sync::atomic::AtomicU64>,
    /// Where the picture of the secure desktop comes from (`None`: placeholder only).
    secure: Option<Arc<dyn SecureFrameSource>>,
}

impl DxgiCapturer {
    /// Shows the secure desktop (UAC, lock screen, logon) through `source` instead of the
    /// "interruption" placeholder.
    #[must_use]
    pub fn with_secure_source(mut self, source: Arc<dyn SecureFrameSource>) -> Self {
        self.secure = Some(source);
        self
    }

    /// Captures the output that backs the `ScreenProvider` screen id passed to `start`. Ids are
    /// resolved through `resolver` (use the engine's long-lived provider so ids match).
    #[must_use]
    pub fn new(resolver: Arc<dyn ScreenProvider>) -> Self {
        Self::with_target(Some(resolver), Target::ScreenId)
    }

    /// Captures a DXGI output by adapter and output index (see [`list_outputs`]); the
    /// `screen_id` argument of `start` is ignored.
    #[must_use]
    pub fn for_output(adapter_index: u32, output_index: u32) -> Self {
        Self::with_target(
            None,
            Target::Output {
                adapter: adapter_index,
                output: output_index,
            },
        )
    }

    fn with_target(resolver: Option<Arc<dyn ScreenProvider>>, target: Target) -> Self {
        Self {
            resolver,
            target,
            adapter_name: Arc::new(Mutex::new(String::new())),
            stop: Arc::new(AtomicBool::new(false)),
            reset: Arc::new(AtomicBool::new(false)),
            thread: None,
            drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            secure: None,
        }
    }

    /// Number of frames dropped because every pool texture was still in use downstream.
    #[must_use]
    pub fn dropped_frames(&self) -> u64 {
        self.drops.load(Ordering::Relaxed)
    }

    fn resolve(&self, screen_id: u32) -> Result<OutputInfo> {
        let outputs = list_outputs()?;
        match &self.target {
            Target::Output { adapter, output } => outputs
                .into_iter()
                .find(|o| o.adapter_index == *adapter && o.output_index == *output)
                .ok_or_else(|| PlatformError::DeviceNotFound(format!("output {adapter}/{output}"))),
            Target::ScreenId => {
                let resolver = self.resolver.as_ref().ok_or_else(|| {
                    PlatformError::other("no screen provider to resolve screen ids")
                })?;
                let screens = resolver.screens();
                let s = screens
                    .iter()
                    .find(|s| s.id == screen_id)
                    .ok_or_else(|| PlatformError::DeviceNotFound(format!("screen {screen_id}")))?;
                super::common::find_output_by_rect(&outputs, s.x, s.y, s.w, s.h)
                    .cloned()
                    .ok_or_else(|| {
                        PlatformError::DeviceNotFound(format!(
                            "no DXGI output for screen {screen_id} ({}x{} at {},{})",
                            s.w, s.h, s.x, s.y
                        ))
                    })
            }
        }
    }
}

impl ScreenCapturer for DxgiCapturer {
    fn start(
        &mut self,
        screen_id: u32,
        fps: u32,
        sink: UnboundedSender<CaptureOutput>,
    ) -> Result<()> {
        self.stop();
        crate::input::init_dpi_awareness();
        let out = self.resolve(screen_id)?;
        if out.software {
            return Err(PlatformError::Unsupported(
                "software adapter cannot be captured",
            ));
        }
        *self.adapter_name.lock().expect("adapter name lock") = out.adapter_name.clone();
        self.stop = Arc::new(AtomicBool::new(false));
        self.reset.store(false, Ordering::SeqCst);
        let flags = ThreadFlags {
            stop: self.stop.clone(),
            reset: self.reset.clone(),
            drops: self.drops.clone(),
            secure: self.secure.clone().map(|source| SecureCtx {
                source,
                interval_us: 1_000_000 / u64::from(fps.max(1)),
            }),
        };
        let (ready_tx, ready_rx) = mpsc::channel::<std::result::Result<(), String>>();
        let handle = std::thread::Builder::new()
            .name("db-dxgi-capture".into())
            .spawn(move || capture_thread(out, fps.max(1), sink, flags, ready_tx))
            .map_err(|e| PlatformError::other(format!("spawn capture thread: {e}")))?;
        match ready_rx.recv() {
            Ok(Ok(())) => {
                self.thread = Some(handle);
                Ok(())
            }
            Ok(Err(msg)) => {
                let _ = handle.join();
                Err(PlatformError::other(msg))
            }
            Err(_) => {
                let _ = handle.join();
                Err(PlatformError::other("capture thread died during setup"))
            }
        }
    }

    fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }

    fn adapter_name(&self) -> String {
        self.adapter_name.lock().expect("adapter name lock").clone()
    }

    fn reset(&self) {
        self.reset.store(true, Ordering::SeqCst);
    }
}

impl Drop for DxgiCapturer {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---------------------------------------------------------------------------------------------
// Texture pool
// ---------------------------------------------------------------------------------------------

struct Pool {
    free: Mutex<Vec<ID3D11Texture2D>>,
    size: (u32, u32),
}

/// A texture taken from the pool; returns itself on drop.
struct PoolSlot {
    tex: ID3D11Texture2D,
    pool: Arc<Pool>,
}

impl Drop for PoolSlot {
    fn drop(&mut self) {
        if let Ok(mut f) = self.pool.free.lock() {
            f.push(self.tex.clone());
        }
    }
}

impl Pool {
    fn new(device: &ID3D11Device, w: u32, h: u32) -> Result<Arc<Self>> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut v = Vec::with_capacity(POOL_SIZE);
        for _ in 0..POOL_SIZE {
            let mut t: Option<ID3D11Texture2D> = None;
            // SAFETY: valid descriptor and out pointer.
            unsafe { device.CreateTexture2D(&desc, None, Some(&mut t)) }
                .map_err(|e| os_err("CreateTexture2D (pool)", &e))?;
            v.push(t.ok_or_else(|| PlatformError::other("CreateTexture2D returned null"))?);
        }
        Ok(Arc::new(Self {
            free: Mutex::new(v),
            size: (w, h),
        }))
    }

    fn take(self: &Arc<Self>) -> Option<PoolSlot> {
        let tex = self.free.lock().ok()?.pop()?;
        Some(PoolSlot {
            tex,
            pool: self.clone(),
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Capture session
// ---------------------------------------------------------------------------------------------

struct Session {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    luid: i64,
    dup: IDXGIOutputDuplication,
    pool: Arc<Pool>,
    width: u32,
    height: u32,
}

fn open_session(out: &OutputInfo) -> Result<Session> {
    open_session_on(out, None)
}

/// Finds `out` again after a display change: Windows renumbers adapters/outputs when the
/// topology changes (the virtual display becoming the only one), so the indices saved at start
/// may point to nothing or to another screen. The GDI device name stays the same.
fn refresh_output(out: &OutputInfo) -> Option<OutputInfo> {
    let all = list_outputs().ok()?;
    all.iter()
        .find(|o| o.attached && !o.device_name.is_empty() && o.device_name == out.device_name)
        .or_else(|| {
            all.iter().find(|o| {
                o.attached
                    && o.adapter_luid == out.adapter_luid
                    && (o.w, o.h) == (out.w, out.h)
                    && (o.x, o.y) == (out.x, out.y)
            })
        })
        .or_else(|| {
            // Last resort: the only attached output of that size (the virtual display can come
            // back under a new name on a re-enumerated adapter).
            let mut same = all
                .iter()
                .filter(|o| o.attached && (o.w, o.h) == (out.w, out.h));
            match (same.next(), same.next()) {
                (Some(o), None) => Some(o),
                _ => None,
            }
        })
        .cloned()
}

/// The device of a closed session, kept for the next one (the zero-copy encoder is bound to it).
struct KeptDevice {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    luid: i64,
}

/// Re-opens the duplication after `ACCESS_LOST` & co.: re-resolves the output and reuses the kept
/// D3D11 device while it is alive and on the same adapter.
fn reopen_session(out: &mut OutputInfo, kept: Option<&KeptDevice>) -> Result<Session> {
    if let Some(fresh) = refresh_output(out) {
        if (fresh.adapter_index, fresh.output_index) != (out.adapter_index, out.output_index) {
            tracing::info!(
                name = %fresh.device_name,
                adapter = fresh.adapter_index,
                output = fresh.output_index,
                "capture output was renumbered after a display change"
            );
        }
        *out = fresh;
    }
    let keep = kept.filter(|k| {
        // SAFETY: plain COM call on a live device.
        let alive = unsafe { k.device.GetDeviceRemovedReason() }.is_ok();
        alive && k.luid == out.adapter_luid
    });
    open_session_on(out, keep.map(|k| (k.device.clone(), k.context.clone())))
}

/// Closes `old` and re-creates the duplication until it works (`None`: stopped meanwhile). The
/// old duplication is released first: a second one on the same output is refused while it lives.
/// `fresh_device`: do not reuse the old D3D11 device (it may look alive but be the problem).
/// While the duplication is refused with `E_ACCESSDENIED` (secure desktop) and `secure` is given,
/// the helper's picture is shown until the normal duplication works again.
fn recover(
    out: &mut OutputInfo,
    old: Session,
    fresh_device: bool,
    stop: &AtomicBool,
    sink: &UnboundedSender<CaptureOutput>,
    secure: Option<&SecureCtx>,
) -> Option<Session> {
    let kept = (!fresh_device).then(|| KeptDevice {
        device: old.device.clone(),
        context: old.context.clone(),
        luid: old.luid,
    });
    drop(old);
    let started = Instant::now();
    let mut last_report: Option<Instant> = None;
    let mut reported = false;
    let mut attempts = 0u32;
    let mut next_secure_try = Instant::now();
    loop {
        if stop.load(Ordering::Relaxed) {
            return None;
        }
        std::thread::sleep(Duration::from_millis(150));
        attempts += 1;
        // A device that only looks alive can be why the duplication keeps failing: every 10th
        // attempt starts over with a new one.
        let fresh = fresh_device || attempts.is_multiple_of(10);
        match reopen_session(out, if fresh { None } else { kept.as_ref() }) {
            Ok(s) => {
                if attempts > 1 || fresh_device {
                    tracing::info!(attempts, new_device = fresh, "capture re-opened");
                }
                if reported {
                    let _ = sink.send(CaptureOutput::Status(None));
                }
                return Some(s);
            }
            Err(e) => {
                if let Some(ctx) = secure
                    && is_access_denied(&e)
                    && Instant::now() >= next_secure_try
                {
                    let keep = kept.as_ref().filter(|k| k.luid == out.adapter_luid);
                    match run_secure(out, keep, ctx, stop, sink, &mut reported) {
                        SecureOutcome::Stopped => return None,
                        SecureOutcome::Resumed(s) => {
                            tracing::info!("secure desktop is gone, normal capture resumes");
                            return Some(s);
                        }
                        SecureOutcome::Unavailable(why) => {
                            tracing::info!("no secure-desktop picture: {why}");
                            next_secure_try = Instant::now() + SECURE_RETRY;
                            // `reported` reflects what run_secure actually left on screen: if it
                            // never cleared the placeholder it is still shown (no re-send); if it
                            // did, the loop re-announces one after the usual delay.
                        }
                    }
                }
                if last_report.is_none_or(|t| t.elapsed() > Duration::from_secs(2)) {
                    last_report = Some(Instant::now());
                    tracing::warn!(attempts, "capture re-open failed: {e}");
                }
                if !reported && started.elapsed() > Duration::from_secs(1) {
                    reported = true;
                    let _ = sink.send(CaptureOutput::Status(Some(interruption_reason(&e))));
                }
            }
        }
    }
}

/// What the viewer shows while the duplication cannot be re-opened.
fn interruption_reason(e: &PlatformError) -> String {
    if is_access_denied(e) {
        "Der Laptop zeigt gerade den Sperrbildschirm oder eine Sicherheitsabfrage (UAC)".into()
    } else {
        format!("Die Bildschirmaufnahme am Laptop ist unterbrochen ({e})")
    }
}

/// `DuplicateOutput` is refused while the secure desktop is the input desktop.
fn is_access_denied(e: &PlatformError) -> bool {
    matches!(e, PlatformError::Os { code, .. } if *code == i64::from(E_ACCESSDENIED.0))
}

fn open_session_on(
    out: &OutputInfo,
    existing: Option<(ID3D11Device, ID3D11DeviceContext)>,
) -> Result<Session> {
    let adapter: IDXGIAdapter1 = adapter_by_index(out.adapter_index)?;
    let dev = match existing {
        Some((device, context)) => D3dDevice {
            device,
            context,
            adapter_luid: out.adapter_luid,
            adapter_name: out.adapter_name.clone(),
        },
        None => create_device(Some(&adapter))?,
    };
    // SAFETY: plain COM calls on interfaces owned here.
    let (dup, width, height) = unsafe {
        let output: IDXGIOutput = adapter
            .EnumOutputs(out.output_index)
            .map_err(|e| os_err("EnumOutputs", &e))?;
        let output1: IDXGIOutput1 = output.cast().map_err(|e| os_err("IDXGIOutput1", &e))?;
        let dup = output1
            .DuplicateOutput(&dev.device)
            .map_err(|e| os_err("DuplicateOutput", &e))?;
        let d = dup.GetDesc();
        (dup, d.ModeDesc.Width, d.ModeDesc.Height)
    };
    let pool = Pool::new(&dev.device, width, height)?;
    Ok(Session {
        device: dev.device,
        context: dev.context,
        luid: if dev.adapter_luid != 0 {
            dev.adapter_luid
        } else {
            luid_to_i64_from(&adapter)
        },
        dup,
        pool,
        width,
        height,
    })
}

fn luid_to_i64_from(a: &IDXGIAdapter1) -> i64 {
    // SAFETY: plain COM call.
    unsafe {
        a.GetDesc1()
            .map(|d| luid_to_i64(d.AdapterLuid))
            .unwrap_or(0)
    }
}

/// Pointer state tracked across frames.
#[derive(Default)]
struct CursorTracker {
    serial: u32,
    shape: Option<CursorShapeData>,
    last_sent: Option<(f32, f32, bool, u32)>,
    have_shape_for_serial: u32,
}

impl CursorTracker {
    fn update_shape(&mut self, sess: &Session, info: &DXGI_OUTDUPL_FRAME_INFO) {
        if info.PointerShapeBufferSize == 0 {
            return;
        }
        let mut buf = vec![0u8; info.PointerShapeBufferSize as usize];
        let mut required = 0u32;
        let mut si = DXGI_OUTDUPL_POINTER_SHAPE_INFO::default();
        // SAFETY: the buffer is `PointerShapeBufferSize` bytes; out pointers are valid.
        let r = unsafe {
            sess.dup.GetFramePointerShape(
                buf.len() as u32,
                buf.as_mut_ptr().cast(),
                &mut required,
                &mut si,
            )
        };
        if r.is_err() {
            return;
        }
        if let Some(shape) = convert_pointer_shape(&buf, &si) {
            self.shape = Some(shape);
            self.serial = self.serial.wrapping_add(1);
        }
    }

    /// Builds the output for the current frame info, if anything changed.
    fn output(&mut self, sess: &Session, info: &DXGI_OUTDUPL_FRAME_INFO) -> Option<CursorState> {
        if info.LastMouseUpdateTime == 0 && self.last_sent.is_some() {
            return None;
        }
        if info.PointerShapeBufferSize > 0 {
            self.update_shape(sess, info);
        }
        let p = info.PointerPosition;
        let x = p.Position.x as f32 / sess.width.max(1) as f32;
        let y = p.Position.y as f32 / sess.height.max(1) as f32;
        let visible = p.Visible.as_bool();
        let key = (x, y, visible, self.serial);
        if self.last_sent == Some(key) {
            return None;
        }
        self.last_sent = Some(key);
        let shape = if self.have_shape_for_serial != self.serial {
            self.have_shape_for_serial = self.serial;
            self.shape.clone()
        } else {
            None
        };
        Some(CursorState {
            x,
            y,
            visible,
            shape_serial: self.serial,
            shape,
        })
    }
}

/// Converts a DXGI pointer shape buffer to straight-alpha RGBA8.
///
/// * `COLOR`: BGRA straight alpha.
/// * `MASKED_COLOR`: alpha byte 0 = opaque colour, 0xFF = XOR with the screen (approximated: the
///   XOR colour is drawn opaque when non-black, transparent otherwise).
/// * `MONOCHROME`: AND mask + XOR mask (1 bpp, height doubled). Pixels that would invert the
///   screen are drawn mid-grey (readable on both light and dark backgrounds).
fn convert_pointer_shape(
    buf: &[u8],
    si: &DXGI_OUTDUPL_POINTER_SHAPE_INFO,
) -> Option<CursorShapeData> {
    let w = si.Width as usize;
    let pitch = si.Pitch as usize;
    let hot = (si.HotSpot.x, si.HotSpot.y);
    match si.Type {
        SHAPE_COLOR | SHAPE_MASKED_COLOR => {
            let h = si.Height as usize;
            if w == 0 || h == 0 || buf.len() < pitch * h || pitch < w * 4 {
                return None;
            }
            let mut rgba = vec![0u8; w * h * 4];
            for y in 0..h {
                for x in 0..w {
                    let s = &buf[y * pitch + x * 4..y * pitch + x * 4 + 4];
                    let (b, g, r, a) = (s[0], s[1], s[2], s[3]);
                    let o = (y * w + x) * 4;
                    if si.Type == SHAPE_COLOR {
                        rgba[o..o + 4].copy_from_slice(&[r, g, b, a]);
                    } else if a == 0 || r != 0 || g != 0 || b != 0 {
                        rgba[o..o + 4].copy_from_slice(&[r, g, b, 255]);
                    }
                }
            }
            Some(CursorShapeData {
                width: w as u32,
                height: h as u32,
                hot_x: hot.0,
                hot_y: hot.1,
                rgba,
            })
        }
        SHAPE_MONOCHROME => {
            let h = (si.Height / 2) as usize;
            if w == 0 || h == 0 || buf.len() < pitch * h * 2 {
                return None;
            }
            let bit = |row: usize, x: usize| -> bool {
                buf[row * pitch + x / 8] & (0x80 >> (x % 8)) != 0
            };
            let mut rgba = vec![0u8; w * h * 4];
            for y in 0..h {
                for x in 0..w {
                    let and = bit(y, x);
                    let xor = bit(y + h, x);
                    let o = (y * w + x) * 4;
                    let px: [u8; 4] = match (and, xor) {
                        (false, false) => [0, 0, 0, 255],
                        (false, true) => [255, 255, 255, 255],
                        (true, false) => [0, 0, 0, 0],
                        (true, true) => [128, 128, 128, 255],
                    };
                    rgba[o..o + 4].copy_from_slice(&px);
                }
            }
            Some(CursorShapeData {
                width: w as u32,
                height: h as u32,
                hot_x: hot.0,
                hot_y: hot.1,
                rgba,
            })
        }
        _ => None,
    }
}

struct Pending {
    slot: PoolSlot,
    ts_us: u64,
}

/// Flags shared between a `DxgiCapturer` and its thread.
struct ThreadFlags {
    stop: Arc<AtomicBool>,
    reset: Arc<AtomicBool>,
    drops: Arc<std::sync::atomic::AtomicU64>,
    secure: Option<SecureCtx>,
}

fn capture_thread(
    mut out: OutputInfo,
    fps: u32,
    sink: UnboundedSender<CaptureOutput>,
    flags: ThreadFlags,
    ready: mpsc::Sender<std::result::Result<(), String>>,
) {
    let ThreadFlags {
        stop,
        reset,
        drops,
        secure,
    } = flags;
    // SAFETY: plain call on the current thread.
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
    }
    let mut session = match open_session(&out) {
        Ok(s) => {
            let _ = ready.send(Ok(()));
            s
        }
        Err(e) => {
            let _ = ready.send(Err(format!("start DXGI capture: {e}")));
            return;
        }
    };
    drop(ready);

    let interval_us = 1_000_000 / u64::from(fps);
    let mut next_emit_us = 0u64;
    let mut pending: Option<Pending> = None;
    let mut cursor = CursorTracker::default();
    let mut last_error = Instant::now() - Duration::from_secs(10);
    // A still desktop produces no "new picture" events at all, so a focus session started in
    // front of an idle laptop never got its first frame. Until the first picture: take whatever
    // the duplication hands out and ask Windows to repaint now and then.
    let mut have_picture = false;
    let mut last_nudge = Instant::now();

    while !stop.load(Ordering::Relaxed) {
        if reset.swap(false, Ordering::SeqCst) {
            tracing::warn!("capture reset requested: the picture stood still during input");
            drop(std::mem::take(&mut pending));
            let Some(s) = recover(&mut out, session, true, &stop, &sink, secure.as_ref()) else {
                return;
            };
            session = s;
            cursor = CursorTracker::default();
            have_picture = false;
            continue;
        }
        // Poll instead of waiting inside AcquireNextFrame: the encoder works on the same
        // (multithread-protected) device, and a blocking wait held its lock for up to the whole
        // timeout. Every encode then queued behind it: 50-200 ms per frame at 5120x1440.
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut res: Option<IDXGIResource> = None;
        // SAFETY: valid out pointers.
        let r = unsafe { session.dup.AcquireNextFrame(0, &mut info, &mut res) };
        match r {
            Ok(()) => {
                let new_picture = info.LastPresentTime != 0 || (!have_picture && res.is_some());
                let cursor_out = cursor.output(&session, &info);
                if new_picture {
                    match copy_frame(&session, res.as_ref()) {
                        Ok(Some(slot)) => {
                            have_picture = true;
                            let ts = qpc_to_us(info.LastPresentTime);
                            // A newer picture replaces an older deferred one.
                            pending = Some(Pending {
                                slot,
                                ts_us: if ts == 0 { now_us() } else { ts },
                            });
                        }
                        Ok(None) => {
                            drops.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(e) => tracing::warn!("capture copy failed: {e}"),
                    }
                }
                // SAFETY: a frame is currently acquired.
                let _ = unsafe { session.dup.ReleaseFrame() };
                drop(res);
                if let Some(c) = cursor_out
                    && sink.send(CaptureOutput::Cursor(c)).is_err()
                {
                    return;
                }
            }
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => {
                if !have_picture && last_nudge.elapsed() > Duration::from_millis(250) {
                    last_nudge = Instant::now();
                    // SAFETY: NULL window = invalidate and repaint every window; no pointers.
                    let _ = unsafe { InvalidateRect(None, None, true) };
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(e) => {
                let access_lost = e.code() == DXGI_ERROR_ACCESS_LOST;
                if !access_lost || last_error.elapsed() > Duration::from_secs(5) {
                    tracing::warn!("AcquireNextFrame: {e}; re-creating duplication");
                }
                last_error = Instant::now();
                // Re-create (duplication, pool, the device only if it is gone); retry until it
                // works or we are stopped.
                drop(std::mem::take(&mut pending));
                let Some(s) = recover(&mut out, session, false, &stop, &sink, secure.as_ref())
                else {
                    return;
                };
                session = s;
                cursor = CursorTracker::default();
                have_picture = false;
                continue;
            }
        }

        // Emit the deferred picture when its slot has come up (rate limit to `fps`).
        if pending.is_some() {
            let now = now_us();
            if now >= next_emit_us {
                let p = pending.take().expect("checked is_some");
                let frame = Frame {
                    width: session.width,
                    height: session.height,
                    ts_us: p.ts_us,
                    data: FrameData::Gpu(Box::new(GpuFrame {
                        texture: p.slot.tex.clone(),
                        device: session.device.clone(),
                        adapter_luid: session.luid,
                        subresource: 0,
                        format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        width: session.width,
                        height: session.height,
                        hold: Some(Box::new(p.slot)),
                    })),
                };
                if sink.send(CaptureOutput::Frame(frame)).is_err() {
                    return;
                }
                next_emit_us = if now.saturating_sub(next_emit_us) > interval_us {
                    now + interval_us
                } else {
                    next_emit_us + interval_us
                };
            }
        }
    }
}

/// Copies the acquired desktop image into a pool texture. `Ok(None)` = pool exhausted.
fn copy_frame(sess: &Session, res: Option<&IDXGIResource>) -> Result<Option<PoolSlot>> {
    let res = res.ok_or_else(|| PlatformError::other("AcquireNextFrame returned no resource"))?;
    let src: ID3D11Texture2D = res
        .cast()
        .map_err(|e| os_err("desktop resource -> texture", &e))?;
    let Some(slot) = sess.pool.take() else {
        return Ok(None);
    };
    let src_res: ID3D11Resource = src.cast().map_err(|e| os_err("texture -> resource", &e))?;
    let dst_res: ID3D11Resource = slot
        .tex
        .cast()
        .map_err(|e| os_err("texture -> resource", &e))?;
    // SAFETY: both resources live on `sess.device`; same size and format (pool created from the
    // duplication description).
    unsafe { sess.context.CopyResource(&dst_res, &src_res) };
    debug_assert_eq!(sess.pool.size, (sess.width, sess.height));
    Ok(Some(slot))
}

// ---------------------------------------------------------------------------------------------
// Secure desktop (UAC, lock screen, logon)
// ---------------------------------------------------------------------------------------------

/// The secure-desktop source plus the frame pacing it has to keep.
struct SecureCtx {
    source: Arc<dyn SecureFrameSource>,
    interval_us: u64,
}

enum SecureOutcome {
    /// The user's desktop is back: this is the normal duplication, opened again.
    Resumed(Session),
    /// Stopped meanwhile.
    Stopped,
    /// No picture of the secure desktop (no helper, old helper, setup failed, helper gave up):
    /// the placeholder stays.
    Unavailable(String),
}

/// How long one poll waits in the helper; also the latency of the first picture of a still desktop.
const SECURE_POLL_WAIT_MS: u16 = 30;
/// How often the normal duplication is tried while the secure picture runs.
const NORMAL_PROBE: Duration = Duration::from_millis(400);
/// Pause before the helper is asked again after it could not capture.
const SECURE_RETRY: Duration = Duration::from_secs(2);
/// Consecutive failures to read a shared texture before the picture is given up.
const MAX_SECURE_ERRORS: u32 = 20;
/// Serial of the synthetic pointer shown on the secure desktop (its real shape is never captured,
/// so the shape is sent once under this fixed serial and the viewer caches it).
const SECURE_CURSOR_SERIAL: u32 = 0xE5EC_0001;

/// A plain white arrow with a black outline, handed to the viewer once per secure session so the
/// operator sees a pointer on the secure desktop even though its real shape is not captured. The
/// hotspot is the tip at (0, 0). `rgba` is RGBA like every other [`CursorShapeData`].
fn default_secure_cursor() -> CursorShapeData {
    const ART: &[&str] = &[
        "B",
        "BB",
        "BWB",
        "BWWB",
        "BWWWB",
        "BWWWWB",
        "BWWWWWB",
        "BWWWWWWB",
        "BWWWWWWWB",
        "BWWWWWWWWB",
        "BWWWWWBBBBB",
        "BWWBWWB",
        "BWB BWWB",
        "BB  BWWB",
        "B    BWWB",
        "     BWWB",
        "      BB",
    ];
    let h = ART.len() as u32;
    let w = ART.iter().map(|r| r.len()).max().unwrap_or(0) as u32;
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for (y, row) in ART.iter().enumerate() {
        for (x, ch) in row.bytes().enumerate() {
            let color = match ch {
                b'B' => [0u8, 0, 0, 255],
                b'W' => [255, 255, 255, 255],
                _ => continue, // leave transparent
            };
            let px = ((y as u32 * w + x as u32) * 4) as usize;
            rgba[px..px + 4].copy_from_slice(&color);
        }
    }
    CursorShapeData {
        width: w,
        height: h,
        hot_x: 0,
        hot_y: 0,
        rgba,
    }
}

/// Tells the helper to drop its textures however the loop ends.
struct SecureGuard<'a>(&'a dyn SecureFrameSource);

impl Drop for SecureGuard<'_> {
    fn drop(&mut self) {
        self.0.stop();
    }
}

/// A device on the adapter the helper's textures live on: the kept one if it fits, else a new one.
fn secure_device(kept: Option<&KeptDevice>, luid: i64) -> Result<D3dDevice> {
    if let Some(k) = kept
        && k.luid == luid
        // SAFETY: plain COM call on a live device.
        && unsafe { k.device.GetDeviceRemovedReason() }.is_ok()
    {
        return Ok(D3dDevice {
            device: k.device.clone(),
            context: k.context.clone(),
            adapter_luid: luid,
            adapter_name: String::new(),
        });
    }
    create_device(Some(&adapter_by_luid(luid)?))
}

/// Streams the secure desktop from the helper into `sink` until the normal duplication works
/// again. The pictures take the same path as local ones (a pool texture in a `GpuFrame`), the
/// pointer goes out as `CaptureOutput::Cursor` without a shape (the viewer's own cursor is drawn).
/// `reported`: whether the "interruption" placeholder is currently shown. It is cleared (and the
/// flag set to `false`) with the first picture handed over, so the caller never re-announces a
/// placeholder this function already withdrew, and keeps showing one this function never cleared.
fn run_secure(
    out: &mut OutputInfo,
    kept: Option<&KeptDevice>,
    ctx: &SecureCtx,
    stop: &AtomicBool,
    sink: &UnboundedSender<CaptureOutput>,
    reported: &mut bool,
) -> SecureOutcome {
    let info = match ctx.source.start((out.x, out.y, out.w, out.h)) {
        Ok(i) => i,
        Err(e) => return SecureOutcome::Unavailable(e),
    };
    let _stop_helper = SecureGuard(ctx.source.as_ref());
    let setup = || -> Result<(D3dDevice, SecureConsumer, Arc<Pool>)> {
        let dev = secure_device(kept, info.luid)?;
        let consumer = SecureConsumer::new(&dev.device, &dev.context)?;
        let pool = Pool::new(&dev.device, info.w, info.h)?;
        Ok((dev, consumer, pool))
    };
    let (dev, mut consumer, mut pool) = match setup() {
        Ok(s) => s,
        Err(e) => {
            return SecureOutcome::Unavailable(format!("device for the helper's adapter: {e}"));
        }
    };
    // Frames name the adapter they live on; keep the kept device's own identity when reused.
    let keep_for_reopen = KeptDevice {
        device: dev.device.clone(),
        context: dev.context.clone(),
        luid: info.luid,
    };
    tracing::info!(
        w = info.w,
        h = info.h,
        "secure desktop: showing the helper's picture"
    );

    let mut last_generation = 0u32;
    let mut last_probe = Instant::now();
    let mut last_cursor: Option<(i32, i32, bool)> = None;
    let mut size = (info.w, info.h);
    let mut pending: Option<Pending> = None;
    let mut next_emit_us = 0u64;
    let mut errors = 0u32;
    let mut sent_cursor_shape = false;
    loop {
        if stop.load(Ordering::Relaxed) {
            return SecureOutcome::Stopped;
        }
        if last_probe.elapsed() >= NORMAL_PROBE {
            last_probe = Instant::now();
            if let Ok(s) = reopen_session(out, Some(&keep_for_reopen)) {
                return SecureOutcome::Resumed(s);
            }
        }
        match ctx.source.poll(last_generation, SECURE_POLL_WAIT_MS) {
            Err(e) => return SecureOutcome::Unavailable(format!("helper: {e}")),
            Ok(SecurePoll::Idle) => {}
            Ok(SecurePoll::Update(u)) => {
                let mut consumed = true;
                if u.image {
                    if (u.w, u.h) != size {
                        match Pool::new(&dev.device, u.w, u.h) {
                            Ok(p) => {
                                pool = p;
                                size = (u.w, u.h);
                                pending = None;
                            }
                            Err(e) => {
                                return SecureOutcome::Unavailable(format!("texture pool: {e}"));
                            }
                        }
                    }
                    match pool.take() {
                        // Every pool texture is still downstream. A still desktop sends this
                        // picture only once, so ask for it again rather than lose it.
                        None => consumed = false,
                        Some(slot) => match consumer.copy_to(&u.name, &slot.tex, size, 5) {
                            Ok(CopyOutcome::Copied) => {
                                errors = 0;
                                pending = Some(Pending {
                                    slot,
                                    ts_us: now_us(),
                                });
                            }
                            Ok(CopyOutcome::NotReady) => consumed = false,
                            Err(e) => {
                                consumed = false;
                                errors += 1;
                                tracing::debug!("secure texture {}: {e}", u.name);
                                if errors >= MAX_SECURE_ERRORS {
                                    return SecureOutcome::Unavailable(format!(
                                        "cannot read the shared texture: {e}"
                                    ));
                                }
                            }
                        },
                    }
                }
                let cursor = (u.pointer_x, u.pointer_y, u.pointer_visible);
                if last_cursor != Some(cursor) {
                    last_cursor = Some(cursor);
                    // The real shape is not captured; hand over a default arrow once so the
                    // viewer has a pointer to draw (otherwise a focus session that starts on the
                    // secure desktop would show no cursor at all).
                    let shape = (!sent_cursor_shape).then(|| {
                        sent_cursor_shape = true;
                        default_secure_cursor()
                    });
                    let state = CursorState {
                        x: u.pointer_x as f32 / size.0.max(1) as f32,
                        y: u.pointer_y as f32 / size.1.max(1) as f32,
                        visible: u.pointer_visible,
                        shape_serial: SECURE_CURSOR_SERIAL,
                        shape,
                    };
                    if sink.send(CaptureOutput::Cursor(state)).is_err() {
                        return SecureOutcome::Stopped;
                    }
                }
                if consumed {
                    last_generation = u.generation;
                } else {
                    // The picture is still wanted; do not spin on it.
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
        if pending.is_some() && now_us() >= next_emit_us {
            let p = pending.take().expect("checked is_some");
            let frame = Frame {
                width: size.0,
                height: size.1,
                ts_us: p.ts_us,
                data: FrameData::Gpu(Box::new(GpuFrame {
                    texture: p.slot.tex.clone(),
                    device: dev.device.clone(),
                    adapter_luid: info.luid,
                    subresource: 0,
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    width: size.0,
                    height: size.1,
                    hold: Some(Box::new(p.slot)),
                })),
            };
            if sink.send(CaptureOutput::Frame(frame)).is_err() {
                return SecureOutcome::Stopped;
            }
            if *reported {
                *reported = false;
                let _ = sink.send(CaptureOutput::Status(None));
            }
            let now = now_us();
            next_emit_us = if now.saturating_sub(next_emit_us) > ctx.interval_us {
                now + ctx.interval_us
            } else {
                next_emit_us + ctx.interval_us
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::secure::{Pump, SecureProducer, SecureUpdate};
    use super::*;
    use windows::Win32::Foundation::POINT;

    fn si(ty: u32, w: u32, h: u32, pitch: u32) -> DXGI_OUTDUPL_POINTER_SHAPE_INFO {
        DXGI_OUTDUPL_POINTER_SHAPE_INFO {
            Type: ty,
            Width: w,
            Height: h,
            Pitch: pitch,
            HotSpot: POINT { x: 1, y: 2 },
        }
    }

    /// A [`SecureFrameSource`] backed by a producer in this process (instead of the helper).
    #[derive(Default)]
    struct LoopbackSource {
        producer: Mutex<Option<ProducerBox>>,
        latest: Mutex<(u32, String)>,
        /// A picture went out: the "secure desktop" ends (the duplication is released, as the
        /// helper does when the input desktop returns to the user), which in this one process
        /// also frees the output for the normal duplication.
        delivered: AtomicBool,
        stopped: AtomicBool,
    }
    struct ProducerBox(SecureProducer);
    // SAFETY: the test drives the source from one thread only; the producer holds a raw handle.
    unsafe impl Send for ProducerBox {}

    impl SecureFrameSource for LoopbackSource {
        fn start(
            &self,
            rect: (i32, i32, u32, u32),
        ) -> std::result::Result<super::super::secure::SecureStartInfo, String> {
            let prefix = format!("DeskBridgeSecureCapTest-{}", std::process::id());
            let p = SecureProducer::open(rect, &prefix, None).map_err(|e| e.to_string())?;
            let i = p.info();
            *self.producer.lock().unwrap() = Some(ProducerBox(p));
            Ok(super::super::secure::SecureStartInfo {
                luid: i.luid,
                x: i.x,
                y: i.y,
                w: i.w,
                h: i.h,
            })
        }

        fn poll(&self, _generation: u32, wait_ms: u16) -> std::result::Result<SecurePoll, String> {
            let mut g = self.producer.lock().unwrap();
            if self.delivered.load(Ordering::SeqCst) {
                *g = None;
                std::thread::sleep(Duration::from_millis(u64::from(wait_ms)));
                return Ok(SecurePoll::Idle);
            }
            let p = g.as_mut().ok_or("not started")?;
            match p.0.pump(u32::from(wait_ms)).map_err(|e| e.to_string())? {
                Pump::Idle => Ok(SecurePoll::Idle),
                Pump::Lost => Err("lost".into()),
                Pump::Update { image, pointer } => {
                    let i = p.0.info();
                    let mut l = self.latest.lock().unwrap();
                    l.0 += 1;
                    if let Some(n) = &image {
                        l.1.clone_from(n);
                        self.delivered.store(true, Ordering::SeqCst);
                    }
                    let ptr = pointer.unwrap_or_default();
                    Ok(SecurePoll::Update(SecureUpdate {
                        generation: l.0,
                        name: l.1.clone(),
                        w: i.w,
                        h: i.h,
                        pointer_x: ptr.x,
                        pointer_y: ptr.y,
                        pointer_visible: ptr.visible,
                        image: image.is_some(),
                    }))
                }
            }
        }

        fn stop(&self) {
            *self.producer.lock().unwrap() = None;
            self.stopped.store(true, Ordering::SeqCst);
        }
    }

    /// The whole agent-side path with the helper replaced by an in-process producer: pictures
    /// reach the sink as GPU frames, and once the normal duplication works (it does on the user's
    /// desktop) the loop hands back a regular session and tells the source to stop.
    #[test]
    fn secure_pictures_reach_the_sink_until_the_normal_duplication_returns() {
        let _one_at_a_time = super::super::common::DUPLICATION_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(mut out) = list_outputs()
            .ok()
            .and_then(|v| v.into_iter().find(|o| o.attached && !o.software))
        else {
            eprintln!("skipped: no output");
            return;
        };
        let src = Arc::new(LoopbackSource::default());
        let ctx = SecureCtx {
            source: src.clone(),
            interval_us: 16_000,
        };
        let stop = AtomicBool::new(false);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        // Watchdog: never hang the test run if the normal duplication does not come back.
        let outcome = std::thread::scope(|s| {
            s.spawn(|| {
                let end = Instant::now() + Duration::from_secs(8);
                while !stop.load(Ordering::SeqCst) && Instant::now() < end {
                    std::thread::sleep(Duration::from_millis(20));
                }
                stop.store(true, Ordering::SeqCst);
            });
            let mut reported = true;
            let r = run_secure(&mut out, None, &ctx, &stop, &tx, &mut reported);
            stop.store(true, Ordering::SeqCst);
            r
        });
        match outcome {
            SecureOutcome::Resumed(_) => {}
            SecureOutcome::Unavailable(why) => {
                eprintln!("skipped: {why}");
                return;
            }
            SecureOutcome::Stopped => panic!("the normal duplication did not come back in 8 s"),
        }
        let (mut frames, mut cleared) = (0, false);
        while let Ok(o) = rx.try_recv() {
            match o {
                CaptureOutput::Frame(f) => {
                    assert_eq!((f.width, f.height), (out.w, out.h));
                    assert!(matches!(f.data, FrameData::Gpu(_)));
                    frames += 1;
                }
                CaptureOutput::Status(None) => cleared = true,
                _ => {}
            }
        }
        assert!(frames >= 1, "no picture reached the sink");
        assert!(cleared, "the placeholder status must be withdrawn");
        assert!(
            src.stopped.load(Ordering::SeqCst),
            "the helper must be told to stop"
        );
    }

    #[test]
    fn color_shape_bgra_to_rgba() {
        // 2x1: red (BGRA = 0,0,255,255), half-transparent green
        let buf = [0, 0, 255, 255, 0, 255, 0, 128];
        let s = convert_pointer_shape(&buf, &si(SHAPE_COLOR, 2, 1, 8)).unwrap();
        assert_eq!((s.width, s.height, s.hot_x, s.hot_y), (2, 1, 1, 2));
        assert_eq!(s.rgba, vec![255, 0, 0, 255, 0, 255, 0, 128]);
    }

    #[test]
    fn masked_color_shape() {
        // opaque blue, XOR black (transparent), XOR white (drawn opaque)
        let buf = [255, 0, 0, 0, 0, 0, 0, 255, 255, 255, 255, 255];
        let s = convert_pointer_shape(&buf, &si(SHAPE_MASKED_COLOR, 3, 1, 12)).unwrap();
        assert_eq!(&s.rgba[0..4], &[0, 0, 255, 255]);
        assert_eq!(&s.rgba[4..8], &[0, 0, 0, 0]);
        assert_eq!(&s.rgba[8..12], &[255, 255, 255, 255]);
    }

    #[test]
    fn monochrome_shape() {
        // 4x1 cursor, AND row then XOR row (1 byte each): pixels (and,xor):
        // x0 = (0,0) black, x1 = (0,1) white, x2 = (1,0) transparent, x3 = (1,1) invert
        let and_row = 0b0011_0000u8;
        let xor_row = 0b0101_0000u8;
        let buf = [and_row, xor_row];
        let s = convert_pointer_shape(&buf, &si(SHAPE_MONOCHROME, 4, 2, 1)).unwrap();
        assert_eq!((s.width, s.height), (4, 1));
        assert_eq!(&s.rgba[0..4], &[0, 0, 0, 255]);
        assert_eq!(&s.rgba[4..8], &[255, 255, 255, 255]);
        assert_eq!(&s.rgba[8..12], &[0, 0, 0, 0]);
        assert_eq!(&s.rgba[12..16], &[128, 128, 128, 255]);
    }

    #[test]
    fn truncated_buffers_are_rejected() {
        assert!(convert_pointer_shape(&[0; 4], &si(SHAPE_COLOR, 8, 8, 32)).is_none());
        assert!(convert_pointer_shape(&[0; 1], &si(SHAPE_MONOCHROME, 32, 64, 4)).is_none());
        assert!(convert_pointer_shape(&[0; 64], &si(99, 4, 4, 16)).is_none());
    }
}
