//! The seam between the engine (`core`) and the platform specific video stack.
//!
//! `core` only talks to [`VideoBackend`]. The Windows implementation (DXGI capture, FFmpeg
//! encode/decode, D3D11 presenter) lives in this crate under `#[cfg(windows)]`
//! (`WindowsVideoBackend`); tests use [`mock::MockVideoBackend`], which runs the *whole* focus
//! pipeline with synthetic frames and a pass-through "codec".

use deskbridge_platform::Result;
use deskbridge_platform::video::{
    CursorState, EncoderConfig, Frame, ScreenCapturer, VideoDecoder, VideoEncoder, VideoPresenter,
};
use deskbridge_proto::pb;

/// Where and how the PC shows the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresenterMode {
    /// Borderless on a whole monitor (virtual-desktop pixels).
    Fullscreen { x: i32, y: i32, w: u32, h: u32 },
    /// Borderless on part of a monitor (split mode); `appbar` shrinks the work area.
    Region { x: i32, y: i32, w: u32, h: u32, appbar: bool },
    /// Normal resizable window.
    Window { w: u32, h: u32 },
}

impl PresenterMode {
    /// Rectangle on the PC virtual desktop that shows the picture (for pointer mapping).
    #[must_use]
    pub fn rect(&self) -> (i32, i32, u32, u32) {
        match *self {
            Self::Fullscreen { x, y, w, h } | Self::Region { x, y, w, h, .. } => (x, y, w, h),
            Self::Window { w, h } => (0, 0, w, h),
        }
    }
}

pub trait VideoBackend: Send + Sync {
    /// What this machine can encode (`encode`, `encode_yuv444`, `max_*`, `encoder_name`).
    fn encoder_caps(&self) -> pb::VideoCaps;
    /// What this machine can decode.
    fn decoder_caps(&self) -> pb::VideoCaps;
    fn create_capturer(&self) -> Result<Box<dyn ScreenCapturer>>;
    /// `adapter` is `ScreenCapturer::adapter_name()` so the encoder lands on the capture GPU.
    fn create_encoder(&self, cfg: &EncoderConfig, adapter: &str) -> Result<Box<dyn VideoEncoder>>;
    fn create_decoder(&self, cfg: &EncoderConfig) -> Result<Box<dyn VideoDecoder>>;
    fn create_presenter(&self, mode: PresenterMode, cfg: &EncoderConfig) -> Result<Box<dyn VideoPresenter>>;
}

/// No video support (platform without a video stack): focus mode is reported unavailable.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullVideoBackend;

impl VideoBackend for NullVideoBackend {
    fn encoder_caps(&self) -> pb::VideoCaps {
        pb::VideoCaps::default()
    }

    fn decoder_caps(&self) -> pb::VideoCaps {
        pb::VideoCaps::default()
    }

    fn create_capturer(&self) -> Result<Box<dyn ScreenCapturer>> {
        Err(deskbridge_platform::PlatformError::Unsupported("video capture"))
    }

    fn create_encoder(&self, _: &EncoderConfig, _: &str) -> Result<Box<dyn VideoEncoder>> {
        Err(deskbridge_platform::PlatformError::Unsupported("video encoder"))
    }

    fn create_decoder(&self, _: &EncoderConfig) -> Result<Box<dyn VideoDecoder>> {
        Err(deskbridge_platform::PlatformError::Unsupported("video decoder"))
    }

    fn create_presenter(&self, _: PresenterMode, _: &EncoderConfig) -> Result<Box<dyn VideoPresenter>> {
        Err(deskbridge_platform::PlatformError::Unsupported("video presenter"))
    }
}

pub mod mock {
    //! Synthetic backend for tests: moving gradient frames, pass-through "codec".

    use super::{PresenterMode, VideoBackend};
    use deskbridge_platform::video::{
        CaptureOutput, CursorShapeData, CursorState, EncodedPacket, EncoderConfig, Frame, FrameData, ScreenCapturer,
        VideoDecoder, VideoEncoder, VideoPresenter,
    };
    use deskbridge_platform::{Result, UnboundedSender};
    use deskbridge_proto::pb;
    use parking_lot::Mutex;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    /// Counters the tests can inspect.
    #[derive(Default)]
    pub struct MockVideoStats {
        pub frames_captured: AtomicU64,
        pub frames_encoded: AtomicU64,
        pub keyframes_encoded: AtomicU64,
        pub frames_decoded: AtomicU64,
        pub frames_presented: AtomicU64,
        pub last_presented_pixel: AtomicU64,
        pub presenter_open: AtomicBool,
        pub capturer_running: AtomicBool,
        pub presenter_modes: Mutex<Vec<PresenterMode>>,
        pub last_bitrate_kbps: AtomicU64,
        pub cursor_updates: AtomicU64,
    }

    pub struct MockVideoBackend {
        pub stats: Arc<MockVideoStats>,
        pub encode: bool,
        pub decode: bool,
        /// Size of the synthetic captured screen.
        pub size: (u32, u32),
    }

    impl MockVideoBackend {
        #[must_use]
        pub fn new(size: (u32, u32)) -> Self {
            Self { stats: Arc::default(), encode: true, decode: true, size }
        }
    }

    fn caps(on: bool, size: (u32, u32), name: &str) -> pb::VideoCaps {
        if !on {
            return pb::VideoCaps::default();
        }
        pb::VideoCaps {
            encode: vec![pb::Codec::Hevc as i32, pb::Codec::H264 as i32],
            decode: vec![pb::Codec::Hevc as i32, pb::Codec::H264 as i32],
            encode_yuv444: false,
            decode_yuv444: false,
            max_width: size.0.max(8192),
            max_height: size.1.max(4320),
            max_fps: 240,
            encoder_name: name.to_string(),
        }
    }

    impl VideoBackend for MockVideoBackend {
        fn encoder_caps(&self) -> pb::VideoCaps {
            caps(self.encode, self.size, "mock-encoder")
        }

        fn decoder_caps(&self) -> pb::VideoCaps {
            caps(self.decode, self.size, "mock-decoder")
        }

        fn create_capturer(&self) -> Result<Box<dyn ScreenCapturer>> {
            Ok(Box::new(MockCapturer { stats: self.stats.clone(), stop: Arc::new(AtomicBool::new(false)), size: self.size }))
        }

        fn create_encoder(&self, cfg: &EncoderConfig, _adapter: &str) -> Result<Box<dyn VideoEncoder>> {
            self.stats.last_bitrate_kbps.store(u64::from(cfg.bitrate_kbps), Ordering::SeqCst);
            Ok(Box::new(MockEncoder { stats: self.stats.clone(), first: true }))
        }

        fn create_decoder(&self, _cfg: &EncoderConfig) -> Result<Box<dyn VideoDecoder>> {
            Ok(Box::new(MockDecoder { stats: self.stats.clone(), have_key: false }))
        }

        fn create_presenter(&self, mode: PresenterMode, _cfg: &EncoderConfig) -> Result<Box<dyn VideoPresenter>> {
            self.stats.presenter_open.store(true, Ordering::SeqCst);
            self.stats.presenter_modes.lock().push(mode);
            Ok(Box::new(MockPresenter { stats: self.stats.clone() }))
        }
    }

    struct MockCapturer {
        stats: Arc<MockVideoStats>,
        stop: Arc<AtomicBool>,
        size: (u32, u32),
    }

    impl ScreenCapturer for MockCapturer {
        fn start(&mut self, _screen_id: u32, fps: u32, sink: UnboundedSender<CaptureOutput>) -> Result<()> {
            let (stats, stop, (w, h)) = (self.stats.clone(), self.stop.clone(), self.size);
            stop.store(false, Ordering::SeqCst);
            stats.capturer_running.store(true, Ordering::SeqCst);
            std::thread::spawn(move || {
                let period = std::time::Duration::from_secs_f64(1.0 / f64::from(fps.max(1)));
                let mut n = 0u64;
                let _ = sink.send(CaptureOutput::Cursor(CursorState {
                    x: 0.5,
                    y: 0.5,
                    visible: true,
                    shape_serial: 1,
                    shape: Some(CursorShapeData { width: 2, height: 2, hot_x: 0, hot_y: 0, rgba: vec![255; 16] }),
                }));
                while !stop.load(Ordering::SeqCst) {
                    let mut data = vec![0u8; (w * h * 4) as usize];
                    for px in data.chunks_exact_mut(4) {
                        px.copy_from_slice(&[(n % 251) as u8, 64, 128, 255]);
                    }
                    let f = Frame { width: w, height: h, ts_us: n * 1000, data: FrameData::Bgra { data, stride: (w * 4) as usize } };
                    stats.frames_captured.fetch_add(1, Ordering::SeqCst);
                    if sink.send(CaptureOutput::Frame(f)).is_err() {
                        break;
                    }
                    n += 1;
                    std::thread::sleep(period);
                }
                stats.capturer_running.store(false, Ordering::SeqCst);
            });
            Ok(())
        }

        fn stop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
        }

        fn adapter_name(&self) -> String {
            "Mock GPU".into()
        }
    }

    struct MockEncoder {
        stats: Arc<MockVideoStats>,
        first: bool,
    }

    impl VideoEncoder for MockEncoder {
        fn encode(&mut self, frame: &Frame, force_key: bool) -> Result<Vec<EncodedPacket>> {
            let FrameData::Bgra { data, .. } = &frame.data else { return Ok(vec![]) };
            let key = self.first || force_key;
            self.first = false;
            self.stats.frames_encoded.fetch_add(1, Ordering::SeqCst);
            if key {
                self.stats.keyframes_encoded.fetch_add(1, Ordering::SeqCst);
            }
            // payload: width, height, first pixel (pass-through "codec")
            let mut p = Vec::with_capacity(12 + data.len().min(64));
            p.extend_from_slice(&frame.width.to_le_bytes());
            p.extend_from_slice(&frame.height.to_le_bytes());
            p.extend_from_slice(&data[..4]);
            Ok(vec![EncodedPacket { data: p, key, ts_us: frame.ts_us, encode_us: 100 }])
        }

        fn set_bitrate(&mut self, kbps: u32) -> Result<()> {
            self.stats.last_bitrate_kbps.store(u64::from(kbps), Ordering::SeqCst);
            Ok(())
        }

        fn name(&self) -> String {
            "mock-encoder".into()
        }
    }

    struct MockDecoder {
        stats: Arc<MockVideoStats>,
        have_key: bool,
    }

    impl VideoDecoder for MockDecoder {
        fn decode(&mut self, data: &[u8], key: bool) -> Result<Option<Frame>> {
            if key {
                self.have_key = true;
            }
            if !self.have_key || data.len() < 12 {
                return Ok(None);
            }
            self.stats.frames_decoded.fetch_add(1, Ordering::SeqCst);
            let w = u32::from_le_bytes(data[0..4].try_into().unwrap());
            let h = u32::from_le_bytes(data[4..8].try_into().unwrap());
            let px = u32::from_le_bytes(data[8..12].try_into().unwrap());
            self.stats.last_presented_pixel.store(u64::from(px), Ordering::SeqCst);
            Ok(Some(Frame { width: w, height: h, ts_us: 0, data: FrameData::Bgra { data: data[8..12].to_vec(), stride: 4 } }))
        }

        fn name(&self) -> String {
            "mock-decoder".into()
        }
    }

    struct MockPresenter {
        stats: Arc<MockVideoStats>,
    }

    impl VideoPresenter for MockPresenter {
        fn present(&mut self, _frame: &Frame) -> Result<()> {
            self.stats.frames_presented.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn set_cursor(&mut self, _cursor: &CursorState) {
            self.stats.cursor_updates.fetch_add(1, Ordering::SeqCst);
        }

        fn close(&mut self) {
            self.stats.presenter_open.store(false, Ordering::SeqCst);
        }
    }

    impl Drop for MockPresenter {
        fn drop(&mut self) {
            self.stats.presenter_open.store(false, Ordering::SeqCst);
        }
    }
}
