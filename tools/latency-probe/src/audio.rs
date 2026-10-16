//! Audio click test helpers.

use deskbridge_platform::audio::{
    AudioCapture, AudioFrame, AudioPlayback, CaptureSource, PullSource,
};
use deskbridge_platform_windows::audio::{WasapiCapture, WasapiPlayback};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// QPC time in microseconds (the same clock the WASAPI capture timestamps use).
fn qpc_us() -> u64 {
    let (mut c, mut f) = (0i64, 0i64);
    // SAFETY: plain out-parameter calls.
    unsafe {
        let _ = QueryPerformanceCounter(&mut c);
        let _ = QueryPerformanceFrequency(&mut f);
    }
    (c as u128 * 1_000_000 / f.max(1) as u128) as u64
}

struct ClickSource {
    next_in_frames: AtomicU64,
    interval_frames: u64,
    burst_left: AtomicU64,
    log: std::sync::Mutex<Vec<u64>>,
    remaining: AtomicU64,
}

impl PullSource for ClickSource {
    fn fill(&self, out: &mut [f32]) {
        let frames = out.len() / 2;
        for i in 0..frames {
            let mut v = 0.0f32;
            let left = self.burst_left.load(Ordering::Relaxed);
            if left > 0 {
                // 3 ms 1 kHz burst
                let n = 144 - left;
                v = 0.6 * (2.0 * std::f32::consts::PI * 1000.0 * n as f32 / 48_000.0).sin();
                self.burst_left.store(left - 1, Ordering::Relaxed);
            } else {
                let n = self.next_in_frames.load(Ordering::Relaxed);
                if n == 0 && self.remaining.load(Ordering::Relaxed) > 0 {
                    self.remaining.fetch_sub(1, Ordering::Relaxed);
                    self.burst_left.store(144, Ordering::Relaxed);
                    self.next_in_frames
                        .store(self.interval_frames, Ordering::Relaxed);
                    // timestamp of the first burst sample as handed to the audio engine
                    self.log
                        .lock()
                        .expect("log")
                        .push(qpc_us() + (i as u64) * 1_000_000 / 48_000);
                } else if n > 0 {
                    self.next_in_frames.store(n - 1, Ordering::Relaxed);
                }
            }
            out[i * 2] = v;
            out[i * 2 + 1] = v;
        }
    }
}

pub async fn emit(args: &[String]) -> i32 {
    let count: u64 = arg(args, "--count")
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);
    let interval_ms: u64 = arg(args, "--interval-ms")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1000);
    let src = Arc::new(ClickSource {
        next_in_frames: AtomicU64::new(0),
        interval_frames: interval_ms * 48,
        burst_left: AtomicU64::new(0),
        log: std::sync::Mutex::new(Vec::new()),
        remaining: AtomicU64::new(count),
    });
    let pb = WasapiPlayback::new();
    let _h = match pb.start(None, src.clone()) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("cannot open output: {e}");
            return 1;
        }
    };
    tokio::time::sleep(Duration::from_millis(interval_ms * count + 1500)).await;
    for t in src.log.lock().expect("log").iter() {
        println!("{t}");
    }
    0
}

pub async fn detect(args: &[String]) -> i32 {
    let secs: u64 = arg(args, "--seconds")
        .and_then(|s| s.parse().ok())
        .unwrap_or(12);
    let source = if args.iter().any(|a| a == "--loopback") {
        CaptureSource::Loopback(arg(args, "--device"))
    } else {
        CaptureSource::Microphone(arg(args, "--device"))
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AudioFrame>();
    let cap = WasapiCapture::new();
    let _h = match cap.start(source, tx) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("cannot open capture: {e}");
            return 1;
        }
    };
    let end = tokio::time::Instant::now() + Duration::from_secs(secs);
    let mut quiet_until: u64 = 0;
    while let Ok(Some(f)) = tokio::time::timeout_at(end, rx.recv()).await {
        let ch = usize::from(f.channels.max(1));
        for (i, chunk) in f.samples.chunks(ch).enumerate() {
            let a = chunk.iter().fold(0f32, |m, v| m.max(v.abs()));
            let t = f.ts_us + (i as u64) * 1_000_000 / 48_000;
            if a > 0.1 && t >= quiet_until {
                println!("{t}");
                quiet_until = t + 300_000;
            }
        }
    }
    0
}

pub fn compare(args: &[String]) -> i32 {
    let (Some(e), Some(d)) = (args.first(), args.get(1)) else {
        eprintln!("usage: audio-compare EMIT.txt DETECT.txt [--offset-us N]");
        return 2;
    };
    let offset: i64 = arg(args, "--offset-us")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let read = |p: &str| -> Vec<i64> {
        std::fs::read_to_string(p)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.trim().parse().ok())
            .collect()
    };
    let (em, de) = (read(e), read(d));
    let mut delays = Vec::new();
    for t in &em {
        if let Some(x) = de.iter().find(|x| **x - offset >= *t) {
            delays.push((*x - offset - *t) as f64 / 1000.0);
        }
    }
    if delays.is_empty() {
        eprintln!("no click pairs found");
        return 1;
    }
    delays.sort_by(|a, b| a.partial_cmp(b).expect("finite"));
    let mean = delays.iter().sum::<f64>() / delays.len() as f64;
    println!(
        "pairs={} min={:.1} ms median={:.1} ms mean={:.1} ms max={:.1} ms (target < 30 ms wired, minus headset latency)",
        delays.len(),
        delays[0],
        delays[delays.len() / 2],
        mean,
        delays[delays.len() - 1]
    );
    0
}
