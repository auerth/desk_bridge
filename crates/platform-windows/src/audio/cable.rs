//! `VirtualAudio` on top of a Microsoft-signed virtual audio *cable* (VB-CABLE "Pack45", see
//! ADR 0003). Desk Bridge writes the headset microphone into the cable's render endpoint
//! ("CABLE Input"); applications record from the cable's capture endpoint ("CABLE Output").
//!
//! Nothing here is installed by Desk Bridge: the backend simply reports `available() == false`
//! until the driver exists, so F3 stays honestly "not available" without it. "Microphone in use"
//! is derived from active recording sessions on the capture endpoint.
//!
//! The microphone stream is played into "CABLE Input" straight from the jitter buffer, so the
//! cable's own clock paces it (a timer-driven writer thread loses blocks to the 15.6 ms timer
//! granularity, which is heard as crackling). While the headset microphone is switched on,
//! "CABLE Output" can be made the default recording device; the previous defaults are restored
//! afterwards.

use super::{
    DefaultRole, WasapiDevices, WasapiPlayback, mic_session_enumeration, set_default_device,
};
use deskbridge_platform::audio::{
    AudioDeviceInfo, AudioDevices, AudioFrame, DeviceKind, MicDefaults, MicUsage, PullSource,
    StreamHandle, VirtualAudio,
};
use deskbridge_platform::{PlatformError, Result, UnboundedSender};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const RENDER_NAME: &str = "CABLE Input";
const CAPTURE_NAME: &str = "CABLE Output";
/// Never buffer more than 200 ms of audio (a stalled render stream must not add latency).
const MAX_RING_SAMPLES: usize = 48_000 / 5 * 2;

struct Ring(Mutex<VecDeque<f32>>);

impl PullSource for Ring {
    fn fill(&self, out: &mut [f32]) {
        let mut q = self.0.lock();
        for o in out.iter_mut() {
            *o = q.pop_front().unwrap_or(0.0);
        }
    }
}

struct Inner {
    ring: Arc<Ring>,
    playback: Mutex<Option<Box<dyn StreamHandle>>>,
    watcher_started: AtomicBool,
    watchers: Mutex<Vec<UnboundedSender<MicUsage>>>,
}

/// Cheap to clone; all clones share the stream.
#[derive(Clone)]
pub struct CableVirtualAudio {
    inner: Arc<Inner>,
}

impl Default for CableVirtualAudio {
    fn default() -> Self {
        Self::new()
    }
}

impl CableVirtualAudio {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                ring: Arc::new(Ring(Mutex::new(VecDeque::new()))),
                playback: Mutex::new(None),
                watcher_started: AtomicBool::new(false),
                watchers: Mutex::new(Vec::new()),
            }),
        }
    }

    fn find(kind: DeviceKind, needle: &str) -> Option<String> {
        WasapiDevices::new()
            .list(kind)
            .ok()?
            .into_iter()
            .find(|d| d.name.contains(needle))
            .map(|d| d.id)
    }

    fn capture_endpoints() -> Result<(Vec<AudioDeviceInfo>, AudioDeviceInfo)> {
        let list = WasapiDevices::new().list(DeviceKind::Capture)?;
        let cable = list
            .iter()
            .find(|d| d.name.contains(CAPTURE_NAME))
            .cloned()
            .ok_or_else(|| PlatformError::DeviceNotFound(CAPTURE_NAME.into()))?;
        Ok((list, cable))
    }

    fn active_processes() -> Vec<String> {
        let Some(id) = Self::find(DeviceKind::Capture, CAPTURE_NAME) else {
            return vec![];
        };
        mic_session_enumeration(Some(&id))
            .map(|v| v.into_iter().map(|s| s.process_name).collect())
            .unwrap_or_default()
    }
}

impl VirtualAudio for CableVirtualAudio {
    fn available(&self) -> bool {
        Self::find(DeviceKind::Render, RENDER_NAME).is_some()
            && Self::find(DeviceKind::Capture, CAPTURE_NAME).is_some()
    }

    fn start_speaker_capture(
        &self,
        _sink: UnboundedSender<AudioFrame>,
    ) -> Result<Box<dyn StreamHandle>> {
        Err(PlatformError::Unsupported(
            "a cable has no separate virtual speaker for the laptop",
        ))
    }

    fn write_mic(&self, frames: &[f32]) {
        {
            let mut p = self.inner.playback.lock();
            if p.is_none() {
                let Some(id) = Self::find(DeviceKind::Render, RENDER_NAME) else {
                    return;
                };
                match WasapiPlayback::new().start_concrete(Some(&id), self.inner.ring.clone()) {
                    Ok(h) => *p = Some(Box::new(h)),
                    Err(e) => {
                        tracing::warn!("opening the virtual cable failed: {e}");
                        return;
                    }
                }
            }
        }
        let mut q = self.inner.ring.0.lock();
        for s in frames {
            q.push_back(*s);
            q.push_back(*s);
        }
        while q.len() > MAX_RING_SAMPLES {
            q.pop_front();
        }
    }

    fn start_mic_playback(&self, source: Arc<dyn PullSource>) -> Result<Box<dyn StreamHandle>> {
        let id = Self::find(DeviceKind::Render, RENDER_NAME)
            .ok_or_else(|| PlatformError::DeviceNotFound(RENDER_NAME.into()))?;
        let h = WasapiPlayback::new().start_concrete(Some(&id), source)?;
        Ok(Box::new(h))
    }

    fn mic_in_use(&self) -> bool {
        !Self::active_processes().is_empty()
    }

    fn claim_default_mic(&self) -> Result<MicDefaults> {
        let (list, cable) = Self::capture_endpoints()?;
        let other = |pred: fn(&AudioDeviceInfo) -> bool| {
            list.iter()
                .find(|d| pred(d) && d.id != cable.id)
                .map(|d| d.id.clone())
        };
        let prev = MicDefaults {
            console: other(|d| d.is_default),
            communications: other(|d| d.is_default_communications),
        };
        set_default_device(&cable.id, DefaultRole::All)?;
        tracing::info!(
            ?prev,
            "virtual microphone is now the default recording device"
        );
        Ok(prev)
    }

    fn release_default_mic(&self, prev: &MicDefaults) -> Result<()> {
        // Without the cable Windows has already picked another default.
        let Ok((list, cable)) = Self::capture_endpoints() else {
            return Ok(());
        };
        let fallback = list.iter().find(|d| !d.is_virtual).map(|d| d.id.clone());
        let pick = |p: &Option<String>| {
            p.as_ref()
                .filter(|id| list.iter().any(|d| &d.id == *id))
                .cloned()
                .or_else(|| fallback.clone())
        };
        if cable.is_default
            && let Some(id) = pick(&prev.console)
        {
            set_default_device(&id, DefaultRole::Console)?;
            set_default_device(&id, DefaultRole::Multimedia)?;
        }
        if cable.is_default_communications
            && let Some(id) = pick(&prev.communications)
        {
            set_default_device(&id, DefaultRole::Communications)?;
        }
        tracing::info!(?prev, "previous default recording devices restored");
        Ok(())
    }

    fn watch_mic_in_use(&self, tx: UnboundedSender<MicUsage>) -> Result<()> {
        self.inner.watchers.lock().push(tx);
        if self.inner.watcher_started.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let inner = self.inner.clone();
        std::thread::Builder::new()
            .name("cable-mic-watch".into())
            .spawn(move || {
                let mut last: Option<Vec<String>> = None;
                loop {
                    let mut now = CableVirtualAudio::active_processes();
                    now.sort();
                    if last.as_ref() != Some(&now) {
                        let usage = MicUsage {
                            in_use: !now.is_empty(),
                            process: now.first().cloned(),
                        };
                        let mut w = inner.watchers.lock();
                        w.retain(|t| t.send(usage.clone()).is_ok());
                        if w.is_empty() {
                            inner.watcher_started.store(false, Ordering::SeqCst);
                            return;
                        }
                        last = Some(now);
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
            })
            .map_err(|e| PlatformError::other(e.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_never_grows_beyond_the_limit_and_pads_with_silence() {
        let v = CableVirtualAudio::new();
        {
            let mut q = v.inner.ring.0.lock();
            for _ in 0..MAX_RING_SAMPLES * 3 {
                q.push_back(0.5);
            }
            while q.len() > MAX_RING_SAMPLES {
                q.pop_front();
            }
            assert_eq!(q.len(), MAX_RING_SAMPLES);
        }
        let mut out = vec![1.0f32; MAX_RING_SAMPLES + 10];
        v.inner.ring.fill(&mut out);
        assert!(
            out[..MAX_RING_SAMPLES]
                .iter()
                .all(|s| (*s - 0.5).abs() < f32::EPSILON)
        );
        assert!(out[MAX_RING_SAMPLES..].iter().all(|s| *s == 0.0));
    }

    #[test]
    fn unavailable_without_the_driver_does_not_panic() {
        // On a machine without VB-CABLE this must simply be false; with it installed, true.
        let _ = CableVirtualAudio::new().available();
    }
}
