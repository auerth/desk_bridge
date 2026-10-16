use crate::{Result, UnboundedSender};
use std::sync::Arc;

pub const RATE: u32 = 48_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeviceKind {
    Render,
    Capture,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDeviceInfo {
    pub id: String,
    /// Friendly name; wireless dongles often expose several ("Game", "Chat").
    pub name: String,
    pub kind: DeviceKind,
    pub is_default: bool,
    pub is_default_communications: bool,
    /// Our own virtual device or another virtual cable (not offered as headset).
    pub is_virtual: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceEvent {
    Added(AudioDeviceInfo),
    Removed {
        id: String,
    },
    DefaultChanged {
        kind: DeviceKind,
        id: Option<String>,
    },
}

pub trait AudioDevices: Send + Sync {
    fn list(&self, kind: DeviceKind) -> Result<Vec<AudioDeviceInfo>>;
    /// Hot-plug and default-device notifications (`IMMNotificationClient`).
    fn watch(&self, tx: UnboundedSender<DeviceEvent>) -> Result<()>;
}

/// Interleaved f32 frame, 48 kHz.
#[derive(Debug, Clone)]
pub struct AudioFrame {
    pub samples: Vec<f32>,
    pub channels: u16,
    pub ts_us: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureSource {
    /// WASAPI loopback of a render endpoint (`None` = default render device).
    Loopback(Option<String>),
    /// Microphone endpoint (`None` = default communications device).
    Microphone(Option<String>),
}

/// Dropping the handle stops the stream.
pub trait StreamHandle: Send {}

pub trait AudioCapture: Send + Sync {
    /// Starts capturing; frames of ~5–10 ms are sent to `sink`. `Ok(None)` is never returned.
    fn start(
        &self,
        source: CaptureSource,
        sink: UnboundedSender<AudioFrame>,
    ) -> Result<Box<dyn StreamHandle>>;
}

/// Pull-based render source (the jitter buffer implements this).
pub trait PullSource: Send + Sync {
    /// Fill interleaved stereo f32. Must not block; underruns are filled with silence.
    fn fill(&self, out: &mut [f32]);
}

pub trait AudioPlayback: Send + Sync {
    fn start(
        &self,
        device_id: Option<&str>,
        source: Arc<dyn PullSource>,
    ) -> Result<Box<dyn StreamHandle>>;
}

/// Virtual speaker + microphone of the bundled driver (M3b).
pub trait VirtualAudio: Send + Sync {
    fn available(&self) -> bool;
    /// Frames played by apps into "Desk Bridge Lautsprecher".
    fn start_speaker_capture(
        &self,
        sink: UnboundedSender<AudioFrame>,
    ) -> Result<Box<dyn StreamHandle>>;
    /// Writes audio into "Desk Bridge Mikrofon".
    fn write_mic(&self, frames: &[f32]);
    fn mic_in_use(&self) -> bool;
    fn watch_mic_in_use(&self, tx: UnboundedSender<MicUsage>) -> Result<()>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicUsage {
    pub in_use: bool,
    pub process: Option<String>,
}
