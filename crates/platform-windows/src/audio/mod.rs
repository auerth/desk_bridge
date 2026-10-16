//! Windows audio backend: WASAPI (event-driven, shared mode), no `cpal`.
//!
//! * [`WasapiDevices`]: endpoint enumeration and `IMMNotificationClient` hot-plug events.
//! * [`WasapiCapture`]: microphone and render-endpoint loopback capture, 48 kHz f32.
//! * [`WasapiPlayback`]: low-latency render (`IAudioClient3` when available).
//! * [`set_default_device`]: default endpoint switching via the undocumented `IPolicyConfig`.
//! * [`VirtualAudioStub`]: placeholder until a signed virtual audio driver is bundled.
//! * [`mic_session_enumeration`]: which processes currently record from a microphone.
//!
//! Threading: every stream owns one worker thread that initialises COM (MTA), registers with
//! MMCSS ("Pro Audio") and creates/uses/releases all its COM objects itself. Dropping a stream
//! handle signals the worker and joins it. Streams never fall back to another device: when the
//! endpoint disappears they end (and report [`StreamEvent::DeviceLost`] if asked to), and the
//! caller restarts them when `DeviceEvent::Added` arrives.
//!
//! # Loopback with a muted / volume-0 render endpoint (plan 8.3, "to verify")
//! WASAPI loopback taps the mixed stream *inside the audio engine, before* the endpoint's
//! hardware stage, so Windows' master volume/mute normally do not influence it (this is why OBS
//! and similar tools keep recording while the speakers are muted). Drivers that implement the
//! master volume as a software APO *in the render path* can behave differently, and a loopback
//! stream only delivers packets while something is rendering. Treat as unverified on the laptop;
//! see `endpoint_volume` and the `audio_probe` example for the read-only inspection.

mod capture;
mod devices;
mod playback;
mod policy;
mod sessions;
mod stream;
pub mod util;
mod wasapi;

pub use capture::{CaptureHandle, WasapiCapture};
pub use devices::{DeviceDetails, EndpointVolume, WasapiDevices, endpoint_volume};
pub use playback::{PlaybackHandle, PlaybackInfo, WasapiPlayback};
pub use policy::{DefaultRole, policy_config_available, set_default_device};
pub use sessions::{MicSession, mic_session_enumeration};
pub use stream::StreamEvent;

use deskbridge_platform::audio::{AudioFrame, MicUsage, StreamHandle, VirtualAudio};
use deskbridge_platform::{PlatformError, Result, UnboundedSender};

/// [`VirtualAudio`] without a driver: `available()` is `false`, everything else is unsupported
/// or a no-op. Replaced by a real backend only if a signed virtual-audio driver is bundled.
#[derive(Debug, Default, Clone, Copy)]
pub struct VirtualAudioStub;

impl VirtualAudio for VirtualAudioStub {
    fn available(&self) -> bool {
        false
    }

    fn start_speaker_capture(
        &self,
        _sink: UnboundedSender<AudioFrame>,
    ) -> Result<Box<dyn StreamHandle>> {
        Err(PlatformError::Unsupported(
            "virtual audio driver is not installed",
        ))
    }

    fn write_mic(&self, _frames: &[f32]) {}

    fn mic_in_use(&self) -> bool {
        false
    }

    fn watch_mic_in_use(&self, _tx: UnboundedSender<MicUsage>) -> Result<()> {
        Err(PlatformError::Unsupported(
            "virtual audio driver is not installed",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_reports_unavailable() {
        let v = VirtualAudioStub;
        assert!(!v.available());
        assert!(!v.mic_in_use());
        v.write_mic(&[0.0; 480]);
        let (tx, _rx) = deskbridge_platform::unbounded_channel();
        assert!(matches!(
            v.start_speaker_capture(tx),
            Err(PlatformError::Unsupported(_))
        ));
        let (tx, _rx) = deskbridge_platform::unbounded_channel();
        assert!(matches!(
            v.watch_mic_in_use(tx),
            Err(PlatformError::Unsupported(_))
        ));
    }
}
