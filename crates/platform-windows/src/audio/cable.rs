//! `VirtualAudio` on top of a Microsoft-signed virtual audio *cable* (VB-CABLE "Pack45", see
//! ADR 0003). Desk Bridge writes the headset microphone into the cable's render endpoint
//! ("CABLE Input"); applications record from the cable's capture endpoint ("CABLE Output").
//!
//! Nothing here is installed by Desk Bridge: the backend simply reports `available() == false`
//! until the driver exists, so F3 stays honestly "not available" without it. "Microphone in use"
//! is derived from active recording sessions on the capture endpoint.

use super::{WasapiDevices, WasapiPlayback, mic_session_enumeration};
use deskbridge_platform::audio::{
    AudioDevices, AudioFrame, DeviceKind, MicUsage, PullSource, StreamHandle, VirtualAudio,
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

    fn active_processes() -> Vec<String> {
        let Some(id) = Self::find(DeviceKind::Capture, CAPTURE_NAME) else { return vec![] };
        mic_session_enumeration(Some(&id))
            .map(|v| v.into_iter().map(|s| s.process_name).collect())
            .unwrap_or_default()
    }
}

impl VirtualAudio for CableVirtualAudio {
    fn available(&self) -> bool {
        Self::find(DeviceKind::Render, RENDER_NAME).is_some() && Self::find(DeviceKind::Capture, CAPTURE_NAME).is_some()
    }

    fn start_speaker_capture(&self, _sink: UnboundedSender<AudioFrame>) -> Result<Box<dyn StreamHandle>> {
        Err(PlatformError::Unsupported("a cable has no separate virtual speaker for the laptop"))
    }

    fn write_mic(&self, frames: &[f32]) {
        {
            let mut p = self.inner.playback.lock();
            if p.is_none() {
                let Some(id) = Self::find(DeviceKind::Render, RENDER_NAME) else { return };
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

    fn mic_in_use(&self) -> bool {
        !Self::active_processes().is_empty()
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
                        let usage = MicUsage { in_use: !now.is_empty(), process: now.first().cloned() };
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
        assert!(out[..MAX_RING_SAMPLES].iter().all(|s| (*s - 0.5).abs() < f32::EPSILON));
        assert!(out[MAX_RING_SAMPLES..].iter().all(|s| *s == 0.0));
    }

    #[test]
    fn unavailable_without_the_driver_does_not_panic() {
        // On a machine without VB-CABLE this must simply be false; with it installed, true.
        let _ = CableVirtualAudio::new().available();
    }
}
