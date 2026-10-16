use crate::{Result, UnboundedSender};
use std::any::Any;

pub enum FrameData {
    /// CPU frame, BGRA8, `stride` bytes per row.
    Bgra { data: Vec<u8>, stride: usize },
    /// GPU texture (platform specific, e.g. `ID3D11Texture2D` + adapter), zero-copy path.
    Gpu(Box<dyn Any + Send>),
}

pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub ts_us: u64,
    pub data: FrameData,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CursorState {
    /// Normalised 0..1 on the captured screen.
    pub x: f32,
    pub y: f32,
    pub visible: bool,
    /// Bumped whenever the shape changes.
    pub shape_serial: u32,
    pub shape: Option<CursorShapeData>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorShapeData {
    pub width: u32,
    pub height: u32,
    pub hot_x: i32,
    pub hot_y: i32,
    pub rgba: Vec<u8>,
}

pub enum CaptureOutput {
    Frame(Frame),
    Cursor(CursorState),
}

pub trait ScreenCapturer: Send {
    /// Starts capturing the given screen (id from `ScreenProvider`). Frames are only emitted when
    /// the picture changed; the stream layer adds keep-alives.
    fn start(
        &mut self,
        screen_id: u32,
        fps: u32,
        sink: UnboundedSender<CaptureOutput>,
    ) -> Result<()>;
    fn stop(&mut self);
    /// Name of the GPU adapter the frames live on.
    fn adapter_name(&self) -> String;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodec {
    H264,
    Hevc,
    Av1,
}

#[derive(Debug, Clone)]
pub struct EncoderConfig {
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub yuv444: bool,
}

pub struct EncodedPacket {
    pub data: Vec<u8>,
    pub key: bool,
    pub ts_us: u64,
    pub encode_us: u32,
}

pub trait VideoEncoder: Send {
    fn encode(&mut self, frame: &Frame, force_key: bool) -> Result<Vec<EncodedPacket>>;
    fn set_bitrate(&mut self, kbps: u32) -> Result<()>;
    fn name(&self) -> String;
    /// Encoder plus the path the pictures take (zero-copy or read-back), for diagnostics.
    fn describe(&self) -> String {
        self.name()
    }
}

pub trait VideoDecoder: Send {
    /// Returns the decoded picture (GPU texture where possible).
    fn decode(&mut self, data: &[u8], key: bool) -> Result<Option<Frame>>;
    /// Like [`VideoDecoder::decode`] but stamps the picture with the sender timestamp from the
    /// video header (decoders cannot know it; there are no B-frames, so output order = input order).
    fn decode_with_ts(&mut self, data: &[u8], key: bool, ts_us: u64) -> Result<Option<Frame>> {
        let mut f = self.decode(data, key)?;
        if let Some(fr) = f.as_mut() {
            fr.ts_us = ts_us;
        }
        Ok(f)
    }
    fn name(&self) -> String;
}

pub trait VideoPresenter: Send {
    fn present(&mut self, frame: &Frame) -> Result<()>;
    fn set_cursor(&mut self, cursor: &CursorState);
    fn close(&mut self);
    /// Current rectangle of the picture on the virtual desktop (window mode can be moved).
    fn window_rect(&self) -> Option<(i32, i32, u32, u32)> {
        None
    }
    /// Shows or hides the stats overlay with the given text (Ctrl+Alt+Shift+I).
    fn set_stats_overlay(&mut self, _on: bool, _text: &str) {}
    /// The user closed the window.
    fn is_closed(&self) -> bool {
        false
    }
    /// Whether the pointer is over the window itself, i.e. not over a window on top of it
    /// (`None` = not tracked).
    fn pointer_over(&self) -> Option<bool> {
        None
    }
}
