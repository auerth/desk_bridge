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

/// Mutes the machine's *own* speakers while its sound is forwarded to the other device.
pub trait LocalOutput: Send + Sync {
    /// Mutes or unmutes every render endpoint whose sound is forwarded (the default device, the
    /// default communications device Teams & co. use, the other real outputs). Returns each
    /// endpoint id with the mute state it had before the call (so it can be restored exactly).
    fn set_forwarded_render_mute(&self, mute: bool) -> Result<Vec<(String, bool)>>;
    /// Sets the mute state of a specific render endpoint (used to restore).
    fn set_render_mute(&self, device_id: &str, mute: bool) -> Result<()>;
}

/// No local control (platforms without an implementation).
#[derive(Debug, Default, Clone, Copy)]
pub struct NullLocalOutput;

impl LocalOutput for NullLocalOutput {
    fn set_forwarded_render_mute(&self, _mute: bool) -> Result<Vec<(String, bool)>> {
        Err(crate::PlatformError::Unsupported("local output muting"))
    }

    fn set_render_mute(&self, _device_id: &str, _mute: bool) -> Result<()> {
        Err(crate::PlatformError::Unsupported("local output muting"))
    }
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
    /// Plays `source` (interleaved stereo, normally the jitter buffer) into the virtual
    /// microphone, clocked by the device itself. `Unsupported` → the caller feeds
    /// [`write_mic`](Self::write_mic) in 10 ms blocks instead.
    fn start_mic_playback(&self, _source: Arc<dyn PullSource>) -> Result<Box<dyn StreamHandle>> {
        Err(crate::PlatformError::Unsupported(
            "device-clocked virtual microphone",
        ))
    }
    fn mic_in_use(&self) -> bool;
    fn watch_mic_in_use(&self, tx: UnboundedSender<MicUsage>) -> Result<()>;
    /// Makes the virtual microphone the system default recording device (all roles). Returns the
    /// defaults it replaced.
    fn claim_default_mic(&self) -> Result<MicDefaults> {
        Err(crate::PlatformError::Unsupported(
            "default microphone switching",
        ))
    }
    /// Gives every role its previous default back, unless the user picked another device in the
    /// meantime (only roles still pointing at the virtual microphone are changed).
    fn release_default_mic(&self, _prev: &MicDefaults) -> Result<()> {
        Err(crate::PlatformError::Unsupported(
            "default microphone switching",
        ))
    }
}

/// Default recording devices before the virtual microphone took over.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MicDefaults {
    /// "Default device" (`eConsole`).
    pub console: Option<String>,
    /// "Default communication device" (Teams & co.).
    pub communications: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicUsage {
    pub in_use: bool,
    pub process: Option<String>,
}
