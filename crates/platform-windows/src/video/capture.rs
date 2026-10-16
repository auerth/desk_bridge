//! DXGI Desktop Duplication capturer (`ScreenCapturer`).
//!
//! * runs on its own time-critical thread; the duplicated frame is copied into a small pool of GPU
//!   textures and released at once (`ReleaseFrame`), so the compositor is never held up;
//! * only real picture changes produce `CaptureOutput::Frame` (`LastPresentTime != 0`); pointer
//!   movement alone produces `CaptureOutput::Cursor`;
//! * pointer position and shape are delivered separately from the image (never burnt in);
//! * `DXGI_ERROR_ACCESS_LOST` (mode change, UAC, lock screen) re-creates the duplication.
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
use windows::Win32::System::Threading::{
    GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL,
};
use windows::core::Interface;

use super::common::{
    GpuFrame, OutputInfo, adapter_by_index, create_device, list_outputs, luid_to_i64, now_us,
    os_err, qpc_to_us,
};

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
    thread: Option<JoinHandle<()>>,
    drops: Arc<std::sync::atomic::AtomicU64>,
}

impl DxgiCapturer {
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
        Self::with_target(None, Target::Output { adapter: adapter_index, output: output_index })
    }

    fn with_target(resolver: Option<Arc<dyn ScreenProvider>>, target: Target) -> Self {
        Self {
            resolver,
            target,
            adapter_name: Arc::new(Mutex::new(String::new())),
            stop: Arc::new(AtomicBool::new(false)),
            thread: None,
            drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
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
                let resolver = self
                    .resolver
                    .as_ref()
                    .ok_or_else(|| PlatformError::other("no screen provider to resolve screen ids"))?;
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
    fn start(&mut self, screen_id: u32, fps: u32, sink: UnboundedSender<CaptureOutput>) -> Result<()> {
        self.stop();
        crate::input::init_dpi_awareness();
        let out = self.resolve(screen_id)?;
        if out.software {
            return Err(PlatformError::Unsupported("software adapter cannot be captured"));
        }
        *self.adapter_name.lock().expect("adapter name lock") = out.adapter_name.clone();
        self.stop = Arc::new(AtomicBool::new(false));
        let stop = self.stop.clone();
        let drops = self.drops.clone();
        let (ready_tx, ready_rx) = mpsc::channel::<std::result::Result<(), String>>();
        let handle = std::thread::Builder::new()
            .name("db-dxgi-capture".into())
            .spawn(move || capture_thread(out, fps.max(1), sink, stop, drops, ready_tx))
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
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
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
        Ok(Arc::new(Self { free: Mutex::new(v), size: (w, h) }))
    }

    fn take(self: &Arc<Self>) -> Option<PoolSlot> {
        let tex = self.free.lock().ok()?.pop()?;
        Some(PoolSlot { tex, pool: self.clone() })
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
    let adapter: IDXGIAdapter1 = adapter_by_index(out.adapter_index)?;
    let dev = create_device(Some(&adapter))?;
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
        luid: if dev.adapter_luid != 0 { dev.adapter_luid } else { luid_to_i64_from(&adapter) },
        dup,
        pool,
        width,
        height,
    })
}

fn luid_to_i64_from(a: &IDXGIAdapter1) -> i64 {
    // SAFETY: plain COM call.
    unsafe { a.GetDesc1().map(|d| luid_to_i64(d.AdapterLuid)).unwrap_or(0) }
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
        Some(CursorState { x, y, visible, shape_serial: self.serial, shape })
    }
}

/// Converts a DXGI pointer shape buffer to straight-alpha RGBA8.
///
/// * `COLOR`: BGRA straight alpha.
/// * `MASKED_COLOR`: alpha byte 0 = opaque colour, 0xFF = XOR with the screen (approximated: the
///   XOR colour is drawn opaque when non-black, transparent otherwise).
/// * `MONOCHROME`: AND mask + XOR mask (1 bpp, height doubled). Pixels that would invert the
///   screen are drawn mid-grey (readable on both light and dark backgrounds).
fn convert_pointer_shape(buf: &[u8], si: &DXGI_OUTDUPL_POINTER_SHAPE_INFO) -> Option<CursorShapeData> {
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
            Some(CursorShapeData { width: w as u32, height: h as u32, hot_x: hot.0, hot_y: hot.1, rgba })
        }
        SHAPE_MONOCHROME => {
            let h = (si.Height / 2) as usize;
            if w == 0 || h == 0 || buf.len() < pitch * h * 2 {
                return None;
            }
            let bit = |row: usize, x: usize| -> bool { buf[row * pitch + x / 8] & (0x80 >> (x % 8)) != 0 };
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
            Some(CursorShapeData { width: w as u32, height: h as u32, hot_x: hot.0, hot_y: hot.1, rgba })
        }
        _ => None,
    }
}

struct Pending {
    slot: PoolSlot,
    ts_us: u64,
}

fn capture_thread(
    out: OutputInfo,
    fps: u32,
    sink: UnboundedSender<CaptureOutput>,
    stop: Arc<AtomicBool>,
    drops: Arc<std::sync::atomic::AtomicU64>,
    ready: mpsc::Sender<std::result::Result<(), String>>,
) {
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

    while !stop.load(Ordering::Relaxed) {
        // Wait for the next change, but not past the moment a deferred frame becomes due.
        let wait_ms = match &pending {
            Some(_) => {
                let now = now_us();
                (next_emit_us.saturating_sub(now) / 1000 + 1).min(100) as u32
            }
            None => 100,
        };

        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut res: Option<IDXGIResource> = None;
        // SAFETY: valid out pointers.
        let r = unsafe { session.dup.AcquireNextFrame(wait_ms, &mut info, &mut res) };
        match r {
            Ok(()) => {
                let new_picture = info.LastPresentTime != 0;
                let cursor_out = cursor.output(&session, &info);
                if new_picture {
                    match copy_frame(&session, res.as_ref()) {
                        Ok(Some(slot)) => {
                            let ts = qpc_to_us(info.LastPresentTime);
                            // A newer picture replaces an older deferred one.
                            pending = Some(Pending { slot, ts_us: if ts == 0 { now_us() } else { ts } });
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
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => {}
            Err(e) => {
                let access_lost = e.code() == DXGI_ERROR_ACCESS_LOST;
                if !access_lost || last_error.elapsed() > Duration::from_secs(5) {
                    tracing::warn!("AcquireNextFrame: {e}; re-creating duplication");
                }
                last_error = Instant::now();
                // Re-create (device, duplication, pool); retry until it works or we are stopped.
                drop(std::mem::take(&mut pending));
                loop {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(150));
                    match open_session(&out) {
                        Ok(s) => {
                            session = s;
                            cursor = CursorTracker::default();
                            break;
                        }
                        Err(e) => tracing::debug!("capture re-open failed: {e}"),
                    }
                }
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
    let src: ID3D11Texture2D = res.cast().map_err(|e| os_err("desktop resource -> texture", &e))?;
    let Some(slot) = sess.pool.take() else {
        return Ok(None);
    };
    let src_res: ID3D11Resource = src.cast().map_err(|e| os_err("texture -> resource", &e))?;
    let dst_res: ID3D11Resource = slot.tex.cast().map_err(|e| os_err("texture -> resource", &e))?;
    // SAFETY: both resources live on `sess.device`; same size and format (pool created from the
    // duplication description).
    unsafe { sess.context.CopyResource(&dst_res, &src_res) };
    debug_assert_eq!(sess.pool.size, (sess.width, sess.height));
    Ok(Some(slot))
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::POINT;

    fn si(ty: u32, w: u32, h: u32, pitch: u32) -> DXGI_OUTDUPL_POINTER_SHAPE_INFO {
        DXGI_OUTDUPL_POINTER_SHAPE_INFO { Type: ty, Width: w, Height: h, Pitch: pitch, HotSpot: POINT { x: 1, y: 2 } }
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
