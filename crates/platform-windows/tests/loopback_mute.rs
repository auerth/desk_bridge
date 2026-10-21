//! Manual experiment: does WASAPI loopback of a render endpoint keep delivering the signal while
//! that endpoint is muted, or while its volume is 0? Plays a quiet tone in the output for a few
//! seconds. `cargo test -p deskbridge-platform-windows --test loopback_mute -- --ignored --nocapture`
//! (`DB_LOOPBACK_DEVICE=<part of the name>` picks an output other than the default).
#![cfg(windows)]

use deskbridge_platform::audio::{
    AudioCapture, AudioDevices, AudioFrame, AudioPlayback, CaptureSource, DeviceKind, PullSource,
};
use deskbridge_platform_windows::audio::{
    WasapiCapture, WasapiDevices, WasapiPlayback, endpoint_volume, set_render_mute,
    set_render_volume,
};
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
#[ignore = "plays a quiet tone and briefly mutes / turns down an output"]
fn loopback_while_endpoint_is_muted_or_at_volume_zero() {
    let id = match std::env::var("DB_LOOPBACK_DEVICE") {
        Ok(part) => {
            let list = WasapiDevices::new().list(DeviceKind::Render).expect("list");
            let dev = list
                .iter()
                .find(|d| d.name.to_lowercase().contains(&part.to_lowercase()))
                .unwrap_or_else(|| panic!("no output matching {part:?}"));
            println!("output: {}", dev.name);
            Some(dev.id.clone())
        }
        Err(_) => None,
    };
    let orig = endpoint_volume(DeviceKind::Render, id.as_deref()).expect("volume");
    let _play = WasapiPlayback::new()
        .start(id.as_deref(), Arc::new(Tone(AtomicU64::new(0))))
        .expect("playback");
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let _cap = WasapiCapture::new()
        .start(CaptureSource::Loopback(id.clone()), tx)
        .expect("loopback");
    let before = peak_for(2, &mut rx);
    let (id, _) = set_render_mute(id.as_deref(), true).expect("mute");
    let muted = peak_for(2, &mut rx);
    set_render_mute(Some(&id), false).expect("unmute");
    set_render_volume(Some(&id), 0.0).expect("volume 0");
    let zero = peak_for(2, &mut rx);
    set_render_volume(Some(&id), orig.scalar).expect("restore volume");
    set_render_mute(Some(&id), orig.muted).expect("restore mute");
    let after = peak_for(1, &mut rx);
    println!("PEAK before={before:.4} muted={muted:.4} volume0={zero:.4} after={after:.4}");
    assert!(before > 0.01, "no loopback signal at all");
    assert!(zero > 0.01, "loopback went silent at volume 0");
}
