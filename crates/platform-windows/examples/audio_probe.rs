//! Manual audio probe: devices, endpoint volume (read-only), loopback, test tone, microphone.
//!
//! `cargo run -p deskbridge-platform-windows --example audio_probe`
//!
//! Plays a very quiet (amplitude 0.05) 440 Hz tone for 1 s on the default output. Nothing in the
//! system configuration is changed.
#![cfg(windows)]

use deskbridge_platform::audio::{
    AudioDevices, AudioFrame, CaptureSource, DeviceKind, PullSource, RATE,
};
use deskbridge_platform::{UnboundedReceiver, unbounded_channel};
use deskbridge_platform_windows::audio::util::peak;
use deskbridge_platform_windows::audio::{
    StreamEvent, WasapiCapture, WasapiDevices, WasapiPlayback, endpoint_volume,
    mic_session_enumeration, policy_config_available,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const AMPLITUDE: f32 = 0.05;
const TONE_HZ: f32 = 440.0;
const TONE_FRAMES: u64 = RATE as u64; // 1 s

/// Sine tone with 10 ms fades; silent after `TONE_FRAMES`.
struct Tone {
    pos: AtomicU64,
}

impl PullSource for Tone {
    fn fill(&self, out: &mut [f32]) {
        let frames = (out.len() / 2) as u64;
        let start = self.pos.fetch_add(frames, Ordering::Relaxed);
        for (i, f) in out.chunks_exact_mut(2).enumerate() {
            let n = start + i as u64;
            let v = if n < TONE_FRAMES {
                let fade = 480.0;
                let env = ((n as f32) / fade)
                    .min(1.0)
                    .min(((TONE_FRAMES - n) as f32) / fade);
                AMPLITUDE
                    * env
                    * (2.0 * std::f32::consts::PI * TONE_HZ * n as f32 / RATE as f32).sin()
            } else {
                0.0
            };
            f[0] = v;
            f[1] = v;
        }
    }
}

#[derive(Default)]
struct Stats {
    frames: usize,
    samples: usize,
    peak: f32,
    channels: u16,
    nonzero_frames: usize,
    first_ts: Option<u64>,
    last_ts: u64,
    max_gap_us: u64,
}

fn drain(rx: &mut UnboundedReceiver<AudioFrame>, s: &mut Stats) {
    while let Ok(f) = rx.try_recv() {
        let p = peak(&f.samples);
        s.frames += 1;
        s.samples += f.samples.len();
        s.peak = s.peak.max(p);
        s.channels = f.channels;
        s.nonzero_frames += usize::from(p > 0.0);
        if s.first_ts.is_none() {
            s.first_ts = Some(f.ts_us);
        } else {
            s.max_gap_us = s.max_gap_us.max(f.ts_us.saturating_sub(s.last_ts));
        }
        s.last_ts = f.ts_us;
    }
}

fn report(label: &str, s: &Stats, secs: f32) {
    let span_ms = s.first_ts.map_or(0.0, |f| (s.last_ts - f) as f32 / 1000.0);
    let frame_samples = s.samples / s.frames.max(1) / usize::from(s.channels.max(1));
    println!(
        "{label}: {} frames ({:.0}/s, ~{} sample-frames each, {}ch), non-silent frames {}, peak {:.4}, ts span {:.0} ms, max ts step {:.1} ms",
        s.frames,
        s.frames as f32 / secs,
        frame_samples,
        s.channels,
        s.nonzero_frames,
        s.peak,
        span_ms,
        s.max_gap_us as f32 / 1000.0
    );
}

fn main() {
    let devices = WasapiDevices::new();
    for kind in [DeviceKind::Render, DeviceKind::Capture] {
        println!("== {kind:?} endpoints ==");
        match devices.list_detailed(kind) {
            Ok(list) => {
                for d in list {
                    println!(
                        "  {}{}{} {:?} [desc={:?} adapter={:?} enum={:?}]\n      id={}",
                        if d.info.is_default { "D" } else { "-" },
                        if d.info.is_default_communications {
                            "C"
                        } else {
                            "-"
                        },
                        if d.info.is_virtual { "V" } else { "-" },
                        d.info.name,
                        d.description,
                        d.adapter,
                        d.enumerator,
                        d.info.id
                    );
                }
            }
            Err(e) => println!("  error: {e}"),
        }
    }

    println!("== default render endpoint volume (read-only) ==");
    match endpoint_volume(DeviceKind::Render, None) {
        Ok(v) => println!("  muted={} volume={:.0}%", v.muted, v.scalar * 100.0),
        Err(e) => println!("  error: {e}"),
    }
    println!("IPolicyConfig instantiable: {}", policy_config_available());
    match mic_session_enumeration(None) {
        Ok(v) => println!("active capture sessions on default mic: {v:?}"),
        Err(e) => println!("mic sessions: error {e}"),
    }

    let (dev_tx, mut dev_rx) = unbounded_channel();
    match devices.watch(dev_tx) {
        Ok(()) => println!("device watcher started (plug/unplug devices now to see events)"),
        Err(e) => println!("device watcher failed: {e}"),
    }

    let (ev_tx, mut ev_rx) = unbounded_channel::<StreamEvent>();
    let capture = WasapiCapture::new();

    // 1. Loopback for 3 s (whatever currently plays on the default output).
    println!("== loopback 3 s (default render device) ==");
    let (tx, mut rx) = unbounded_channel();
    match capture.start_with_events(CaptureSource::Loopback(None), tx, ev_tx.clone()) {
        Ok(handle) => {
            let mut stats = Stats::default();
            let t0 = Instant::now();
            let mut last = Instant::now();
            while t0.elapsed() < Duration::from_secs(3) {
                std::thread::sleep(Duration::from_millis(20));
                drain(&mut rx, &mut stats);
                if last.elapsed() >= Duration::from_millis(500) {
                    last = Instant::now();
                    println!(
                        "  t={:.1}s frames={} peak={:.4}",
                        t0.elapsed().as_secs_f32(),
                        stats.frames,
                        stats.peak
                    );
                }
            }
            drop(handle);
            drain(&mut rx, &mut stats);
            report("loopback", &stats, 3.0);
        }
        Err(e) => println!("  loopback failed: {e}"),
    }

    // 2. Test tone for 1 s with a concurrent loopback to see real (non-synthetic) data.
    println!("== 440 Hz tone, 1 s, amplitude {AMPLITUDE} (with concurrent loopback) ==");
    let (ltx, mut lrx) = unbounded_channel();
    let loop_handle = capture.start_with_events(CaptureSource::Loopback(None), ltx, ev_tx.clone());
    if let Err(e) = &loop_handle {
        println!("  concurrent loopback failed: {e}");
    }
    let playback = WasapiPlayback::new();
    let tone = Arc::new(Tone {
        pos: AtomicU64::new(0),
    });
    match playback.start_concrete_with_events(None, tone, ev_tx.clone()) {
        Ok(h) => {
            println!("  device: {}", h.device_name());
            println!("  {:#?}", h.info());
            println!("  stream_latency_ms() = {:.2}", h.stream_latency_ms());
            let mut stats = Stats::default();
            let t0 = Instant::now();
            while t0.elapsed() < Duration::from_millis(1400) {
                std::thread::sleep(Duration::from_millis(20));
                drain(&mut lrx, &mut stats);
            }
            println!("  render underruns: {}", h.underruns());
            drop(h);
            report("loopback during tone", &stats, 1.4);
        }
        Err(e) => println!("  playback failed: {e}"),
    }
    drop(loop_handle);

    // 3. Microphone, 1 s (mono expected; only the peak level is looked at).
    println!("== microphone 1 s (default communications device) ==");
    let (mtx, mut mrx) = unbounded_channel();
    match capture.start_with_events(CaptureSource::Microphone(None), mtx, ev_tx) {
        Ok(handle) => {
            let mut stats = Stats::default();
            let t0 = Instant::now();
            while t0.elapsed() < Duration::from_secs(1) {
                std::thread::sleep(Duration::from_millis(20));
                drain(&mut mrx, &mut stats);
            }
            drop(handle);
            report("microphone", &stats, 1.0);
        }
        Err(e) => println!("  microphone failed: {e}"),
    }

    let mut events = Vec::new();
    while let Ok(e) = ev_rx.try_recv() {
        events.push(e);
    }
    println!("lifecycle events: {events:?}");
    while let Ok(e) = dev_rx.try_recv() {
        println!("device event: {e:?}");
    }
}
