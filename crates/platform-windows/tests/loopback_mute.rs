//! Manual experiment: does WASAPI loopback of the default render endpoint keep delivering the
//! signal while that endpoint is muted? Plays a quiet tone in the default output for a few
//! seconds. `cargo test -p deskbridge-platform-windows --test loopback_mute -- --ignored --nocapture`
#![cfg(windows)]

use deskbridge_platform::audio::{
    AudioCapture, AudioFrame, AudioPlayback, CaptureSource, PullSource,
};
use deskbridge_platform_windows::audio::{WasapiCapture, WasapiPlayback, set_render_mute};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedReceiver;

struct Tone(AtomicU64);

impl PullSource for Tone {
    fn fill(&self, out: &mut [f32]) {
        for f in out.chunks_exact_mut(2) {
            let n = self.0.fetch_add(1, Ordering::Relaxed);
            let v = 0.05 * (2.0 * std::f32::consts::PI * 440.0 * n as f32 / 48_000.0).sin();
            f[0] = v;
            f[1] = v;
        }
    }
}

fn peak_for(secs: u64, rx: &mut UnboundedReceiver<AudioFrame>) -> f32 {
    let end = std::time::Instant::now() + Duration::from_secs(secs);
    let mut peak = 0f32;
    while std::time::Instant::now() < end {
        while let Ok(f) = rx.try_recv() {
            peak = f.samples.iter().fold(peak, |p, s| p.max(s.abs()));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    peak
}

#[test]
#[ignore = "plays a quiet tone and briefly mutes the default output"]
fn loopback_while_endpoint_is_muted() {
    let _play = WasapiPlayback::new()
        .start(None, Arc::new(Tone(AtomicU64::new(0))))
        .expect("playback");
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let _cap = WasapiCapture::new()
        .start(CaptureSource::Loopback(None), tx)
        .expect("loopback");
    let before = peak_for(2, &mut rx);
    let (id, prev) = set_render_mute(None, true).expect("mute");
    let muted = peak_for(2, &mut rx);
    set_render_mute(Some(&id), prev).expect("restore");
    let after = peak_for(1, &mut rx);
    println!("PEAK before={before:.4} muted={muted:.4} after={after:.4}");
    assert!(before > 0.01, "no loopback signal at all");
    assert!(muted > 0.01, "loopback went silent while muted");
}
