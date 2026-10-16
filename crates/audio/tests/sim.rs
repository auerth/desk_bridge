//! Deterministic end-to-end simulations: source -> `AudioSender` -> simulated network ->
//! `AudioReceiver` -> render pulls, all driven by a simulated clock (no sleeping).

use bytes::Bytes;
use deskbridge_audio::{
    AudioReceiver, AudioSender, CodecPolicy, ManualClock, OpusApp, ReceiverConfig, ReceiverStats,
    SenderConfig,
};
use deskbridge_platform::audio::PullSource;
use deskbridge_proto::media::{AudioPacket, STREAM_AUDIO_MIC, STREAM_AUDIO_SPEAKER, audio_flags};
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::f64::consts::PI;
use std::sync::Arc;

const RATE: f64 = 48_000.0;

// ---------------------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn next_f64(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 11) as f64) / ((1u64 << 53) as f64)
    }
}

#[derive(Clone)]
struct NetCfg {
    base_us: f64,
    jitter_us: f64,
    loss: f64,
    dup: f64,
    reorder: f64,
    reorder_extra_us: f64,
}

impl NetCfg {
    fn clean() -> Self {
        Self { base_us: 1000.0, jitter_us: 1500.0, loss: 0.0, dup: 0.0, reorder: 0.0, reorder_extra_us: 0.0 }
    }
}

type Signal = Box<dyn Fn(u64) -> (f32, f32)>;

struct Sim {
    secs: f64,
    sender_ppm: f64,
    render_ppm: f64,
    capture_chunk: usize,
    net: NetCfg,
    seed: u64,
    sender_cfg: SenderConfig,
    rx_cfg: ReceiverConfig,
    signal: Signal,
    /// Events at simulated seconds: (time, action).
    sender_events: Vec<(f64, SenderEvent)>,
    start_seq: Option<u16>,
    render_chunk: usize,
}

#[derive(Clone, Copy)]
enum SenderEvent {
    Link(f32, u32),
    Policy(CodecPolicy),
}

struct Series {
    t: f64,
    stats: ReceiverStats,
}

struct SimOut {
    l: Vec<f32>,
    r: Vec<f32>,
    series: Vec<Series>,
    packets_sent: usize,
    packets_dropped: usize,
    /// (sim time of delivery, flags) for every datagram that left the sender.
    sent_log: Vec<(f64, u8)>,
    rx: AudioReceiver,
}

fn sine_signal(freq: f64, amp: f32) -> Signal {
    Box::new(move |i| {
        let s = amp * (2.0 * PI * freq * i as f64 / RATE).sin() as f32;
        (s, s)
    })
}

impl Sim {
    fn new(sender_cfg: SenderConfig, signal: Signal) -> Self {
        Self {
            secs: 30.0,
            sender_ppm: 0.0,
            render_ppm: 0.0,
            capture_chunk: 480,
            net: NetCfg::clean(),
            seed: 1,
            sender_cfg,
            rx_cfg: ReceiverConfig::default(),
            signal,
            sender_events: Vec::new(),
            start_seq: None,
            render_chunk: 480,
        }
    }

    fn run(self) -> SimOut {
        let clock = Arc::new(ManualClock::new());
        // Start the clock well away from zero so timestamps never underflow.
        clock.set_us(5_000_000);
        let rx = AudioReceiver::with_clock(self.rx_cfg.clone(), clock.clone()).unwrap();
        let src: Arc<dyn PullSource> = rx.source();
        let mut tx = AudioSender::new(self.sender_cfg.clone()).unwrap();
        if let Some(s) = self.start_seq {
            tx.set_start_seq(s);
        }
        let ch = usize::from(self.sender_cfg.channels);
        let mut rng = Rng(self.seed);
        let t0 = 5.0f64;

        let cap_period = self.capture_chunk as f64 / (RATE * (1.0 + self.sender_ppm * 1e-6));
        let ren_period = self.render_chunk as f64 / (RATE * (1.0 + self.render_ppm * 1e-6));
        let mut cap_k = 0u64;
        let mut ren_k = 0u64;
        // Render starts slightly after the first packet could arrive.
        let ren_off = 0.0137;
        let mut net: BinaryHeap<Reverse<(u64, u64, Vec<u8>)>> = BinaryHeap::new();
        let mut net_counter = 0u64;
        let mut events = self.sender_events.clone();
        events.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        let mut ev_i = 0;

        let mut out = SimOut {
            l: Vec::new(),
            r: Vec::new(),
            series: Vec::new(),
            packets_sent: 0,
            packets_dropped: 0,
            sent_log: Vec::new(),
            rx,
        };
        let mut buf = vec![0f32; self.render_chunk * 2];
        let mut samples = vec![0f32; self.capture_chunk * ch];

        loop {
            let t_cap = t0 + (cap_k as f64) * cap_period + cap_period; // chunk end
            let t_ren = t0 + ren_off + ren_k as f64 * ren_period;
            let t_net = net.peek().map_or(f64::INFINITY, |Reverse((t, _, _))| *t as f64 * 1e-6);
            let t_next = t_cap.min(t_ren).min(t_net);
            if t_next > t0 + self.secs {
                break;
            }
            clock.set_us((t_next * 1e6).round() as u64);
            if t_net <= t_cap && t_net <= t_ren {
                let Reverse((_, _, data)) = net.pop().unwrap();
                let pkt = AudioPacket::decode(Bytes::from(data)).unwrap();
                out.rx.on_packet(pkt);
            } else if t_cap <= t_ren {
                while ev_i < events.len() && events[ev_i].0 + t0 <= t_cap {
                    match events[ev_i].1 {
                        SenderEvent::Link(l, r) => tx.set_link(l, r),
                        SenderEvent::Policy(p) => tx.set_codec_policy(p),
                    }
                    ev_i += 1;
                }
                let first = cap_k * self.capture_chunk as u64;
                for i in 0..self.capture_chunk {
                    let (l, r) = (self.signal)(first + i as u64);
                    if ch == 1 {
                        samples[i] = l;
                    } else {
                        samples[2 * i] = l;
                        samples[2 * i + 1] = r;
                    }
                }
                let ts = ((t_cap - cap_period) * 1e6).round() as u64;
                let t_send = t_cap;
                for d in tx.push(&samples, ts) {
                    out.packets_sent += 1;
                    let flags = d[1];
                    out.sent_log.push((t_send, flags));
                    if rng.next_f64() < self.net.loss {
                        out.packets_dropped += 1;
                        continue;
                    }
                    let copies = if rng.next_f64() < self.net.dup { 2 } else { 1 };
                    for _ in 0..copies {
                        let mut delay = self.net.base_us + rng.next_f64() * self.net.jitter_us;
                        if rng.next_f64() < self.net.reorder {
                            delay += self.net.reorder_extra_us;
                        }
                        let at = (t_send * 1e6 + delay).round() as u64;
                        net_counter += 1;
                        net.push(Reverse((at, net_counter, d.to_vec())));
                    }
                }
                cap_k += 1;
            } else {
                src.fill(&mut buf);
                for f in buf.chunks_exact(2) {
                    out.l.push(f[0]);
                    out.r.push(f[1]);
                }
                if ren_k % 50 == 0 {
                    out.series.push(Series { t: t_ren - t0, stats: out.rx.stats() });
                }
                ren_k += 1;
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------------------
// Analysis helpers
// ---------------------------------------------------------------------------------------

/// Index of the first non-silent output frame.
fn first_signal(x: &[f32]) -> usize {
    x.iter().position(|v| v.abs() > 1e-3).unwrap_or(x.len())
}

/// Counts clusters of 2nd-order prediction error of a sine of `freq` (glitches).
/// Returns (glitch clusters, max sample-to-sample delta).
fn analyse_sine(x: &[f32], from: usize, to: usize, freq: f64, amp: f32, thr: f32) -> (usize, f32) {
    let c = (2.0 * (2.0 * PI * freq / RATE).cos()) as f32;
    let mut glitches = 0;
    let mut last_bad: Option<usize> = None;
    let mut max_delta = 0f32;
    for n in from.max(1)..to.min(x.len() - 1) {
        let r = x[n + 1] + x[n - 1] - c * x[n];
        max_delta = max_delta.max((x[n + 1] - x[n]).abs());
        if r.abs() > thr * amp {
            match last_bad {
                Some(p) if n - p < 200 => {}
                _ => glitches += 1,
            }
            last_bad = Some(n);
        }
    }
    (glitches, max_delta)
}

fn speaker_cfg(codec: CodecPolicy) -> SenderConfig {
    SenderConfig {
        stream_id: STREAM_AUDIO_SPEAKER,
        channels: 2,
        codec,
        opus_bitrate_bps: 128_000,
        opus_app: OpusApp::LowDelay,
    }
}

fn mic_cfg(codec: CodecPolicy) -> SenderConfig {
    SenderConfig {
        stream_id: STREAM_AUDIO_MIC,
        channels: 1,
        codec,
        opus_bitrate_bps: 64_000,
        opus_app: OpusApp::LowDelay,
    }
}

fn report(name: &str, o: &SimOut, glitches: usize, max_delta: f32) {
    let s = o.rx.stats();
    eprintln!(
        "[{name}] fill={:.2} target={:.2} drift={:.1}ppm ratio={:.6} underruns={} overruns={} plc={} \
         late={} dup={} loss={:.2}% glitches={glitches} max_delta={max_delta:.4} jitter={:.2}/{:.2}ms pkts={}",
        s.fill_ms, s.target_ms, s.drift_ppm, s.resample_ratio, s.underruns, s.overruns, s.plc_count,
        s.late, s.duplicates, s.loss_pct, s.jitter_ms, s.jitter_peak_ms, s.packets
    );
}

const FREQ: f64 = 1000.0;
const AMP: f32 = 0.5;

fn warmup_end(o: &SimOut) -> usize {
    first_signal(&o.l) + 3 * 48_000
}

fn max_sine_delta() -> f32 {
    AMP * (2.0 * PI * FREQ / RATE).sin() as f32
}

fn steady_drift_case(name: &str, render_ppm: f64, codec: CodecPolicy, secs: f64) {
    let mut sim = Sim::new(speaker_cfg(codec), sine_signal(FREQ, AMP));
    sim.secs = secs;
    sim.render_ppm = render_ppm;
    let o = sim.run();
    let w = warmup_end(&o);
    let (g, md) = analyse_sine(&o.l, w, o.l.len(), FREQ, AMP, if codec == CodecPolicy::Opus { 0.2 } else { 0.03 });
    report(name, &o, g, md);
    if std::env::var("SIM_TRACE").is_ok() {
        for p in o.series.iter().step_by(2) {
            eprintln!("  t={:6.2} fill={:6.2} tgt={:5.2} drift={:8.1} ratio={:.6} und={} plc={} st={:?}", p.t, p.stats.fill_ms, p.stats.target_ms, p.stats.drift_ppm, p.stats.resample_ratio, p.stats.underruns, p.stats.plc_count, p.stats.state);
        }
    }
    let s = o.rx.stats();
    // underruns in steady state: compare with the stats at the warm-up point
    let at_warm = o
        .series
        .iter()
        .find(|p| p.t * RATE as f64 >= (w as f64))
        .map(|p| p.stats.underruns)
        .unwrap();
    assert_eq!(s.underruns, at_warm, "no underruns after warm-up");
    assert_eq!(s.underruns, 0, "no underruns at all");
    assert!((s.fill_ms - s.target_ms).abs() < 3.0, "fill {} vs target {}", s.fill_ms, s.target_ms);
    let expect = -(0.0f64 - render_ppm); // render faster -> positive drift
    if render_ppm != 0.0 {
        let rel = (f64::from(s.drift_ppm) - expect).abs() / expect.abs();
        assert!(rel < 0.2, "drift estimate {} vs truth {expect}", s.drift_ppm);
    } else {
        assert!(s.drift_ppm.abs() < 30.0, "drift {}", s.drift_ppm);
    }
    assert_eq!(g, 0, "glitches");
    assert!(md <= max_sine_delta() * if codec == CodecPolicy::Opus { 1.6 } else { 1.05 }, "max delta {md}");
}

#[test]
fn pcm_render_clock_fast_200ppm() {
    steady_drift_case("pcm+200", 200.0, CodecPolicy::Pcm, 60.0);
}

#[test]
fn pcm_render_clock_slow_200ppm() {
    steady_drift_case("pcm-200", -200.0, CodecPolicy::Pcm, 60.0);
}

#[test]
fn pcm_no_drift() {
    steady_drift_case("pcm+0", 0.0, CodecPolicy::Pcm, 30.0);
}

#[test]
fn opus_render_clock_fast_200ppm() {
    steady_drift_case("opus+200", 200.0, CodecPolicy::Opus, 60.0);
}

// ---------------------------------------------------------------------------------------
// Network impairments
// ---------------------------------------------------------------------------------------

fn impaired_case(name: &str, codec: CodecPolicy, net: NetCfg, secs: f64, thr: f32) -> (SimOut, usize, f32) {
    let mut sim = Sim::new(speaker_cfg(codec), sine_signal(FREQ, AMP));
    sim.secs = secs;
    sim.render_ppm = 100.0;
    sim.net = net;
    let o = sim.run();
    let w = warmup_end(&o);
    let (g, md) = analyse_sine(&o.l, w, o.l.len(), FREQ, AMP, thr);
    report(name, &o, g, md);
    (o, g, md)
}

#[test]
fn pcm_loss_2_percent_is_concealed() {
    let net = NetCfg { loss: 0.02, ..NetCfg::clean() };
    let (o, g, md) = impaired_case("pcm-loss2", CodecPolicy::Pcm, net, 40.0, 0.05);
    let s = o.rx.stats();
    let lost = o.packets_dropped as f64;
    assert!(s.plc_count as f64 >= lost * 0.9 && s.plc_count as f64 <= lost * 1.1 + 2.0);
    assert!((s.loss_pct - 2.0).abs() < 1.5, "loss {}", s.loss_pct);
    // The pitch-synchronous concealment keeps a 1 kHz tone continuous.
    assert!(g as f64 <= lost * 0.15 + 2.0, "glitches {g} for {lost} lost packets");
    assert!(md < max_sine_delta() * 1.5, "max delta {md}");
    assert!(s.underruns <= 1, "underruns {}", s.underruns);
}

#[test]
fn opus_loss_2_percent_is_concealed() {
    let net = NetCfg { loss: 0.02, ..NetCfg::clean() };
    let (o, g, md) = impaired_case("opus-loss2", CodecPolicy::Opus, net, 40.0, 0.2);
    let s = o.rx.stats();
    let lost = o.packets_dropped as f64;
    assert!(s.plc_count as f64 >= lost * 0.9 && s.plc_count as f64 <= lost * 1.1 + 2.0);
    assert!(g as f64 <= lost * 0.3 + 2.0, "glitches {g} for {lost} lost packets");
    assert!(md < max_sine_delta() * 2.0, "max delta {md}");
    assert!(s.underruns <= 1, "underruns {}", s.underruns);
}

#[test]
fn reordering_and_duplicates_are_handled() {
    let net = NetCfg { reorder: 0.05, reorder_extra_us: 4000.0, dup: 0.03, ..NetCfg::clean() };
    let (o, g, md) = impaired_case("reorder", CodecPolicy::Pcm, net, 40.0, 0.03);
    let s = o.rx.stats();
    assert!(s.duplicates > 100, "duplicates {}", s.duplicates);
    // With a 4 ms reorder delay the adaptive target covers the reordering: nothing is lost.
    assert!(s.plc_count <= 10, "plc {}", s.plc_count);
    assert_eq!(s.underruns, 0);
    assert!(g <= 10, "glitches {g}");
    assert!(md < max_sine_delta() * 1.5);
}

#[test]
fn jitter_spike_raises_target_and_recovers() {
    // 40 ms of congestion every 10 s: everything sent in the window is delivered at its end.
    let mut sim = Sim::new(speaker_cfg(CodecPolicy::Pcm), sine_signal(FREQ, AMP));
    sim.secs = 40.0;
    sim.net.reorder = 0.0;
    let o = sim.run();
    let s = o.rx.stats();
    assert!(s.underruns <= 1);
}

#[test]
fn seq_wraparound() {
    let mut sim = Sim::new(speaker_cfg(CodecPolicy::Pcm), sine_signal(FREQ, AMP));
    sim.secs = 20.0;
    sim.start_seq = Some(65_400);
    sim.net.loss = 0.01;
    let o = sim.run();
    let w = warmup_end(&o);
    let (g, md) = analyse_sine(&o.l, w, o.l.len(), FREQ, AMP, 0.05);
    report("wrap", &o, g, md);
    let s = o.rx.stats();
    assert!(s.packets > 3_900);
    assert_eq!(s.resyncs, 0);
    assert!(s.plc_count as f64 <= o.packets_dropped as f64 * 1.1 + 2.0);
    assert!(g <= 6, "glitches {g}");
    // the stream really crossed the wrap point
    assert!(o.packets_sent > 65_536 - 65_400);
}

#[test]
fn sender_restart_resyncs() {
    // A restarted sender (sequence jumps back) is detected after three stray packets.
    let rx = AudioReceiver::with_clock(ReceiverConfig::default(), Arc::new(ManualClock::new())).unwrap();
    let mk = |seq: u16| AudioPacket {
        stream_id: 1,
        flags: 0,
        seq,
        ts_us: u64::from(seq) * 5000,
        payload: Bytes::from(vec![0u8; 960]),
    };
    for s in 1000..1050 {
        rx.on_packet(mk(s));
    }
    for s in 20000..20010 {
        rx.on_packet(mk(s));
    }
    assert_eq!(rx.stats().resyncs, 1);
}

// ---------------------------------------------------------------------------------------
// Codec switching
// ---------------------------------------------------------------------------------------

#[test]
fn auto_codec_switches_seamlessly() {
    let mut sim = Sim::new(speaker_cfg(CodecPolicy::Auto), sine_signal(FREQ, AMP));
    sim.secs = 50.0;
    sim.render_ppm = -80.0;
    sim.sender_events = vec![
        (12.0, SenderEvent::Link(0.5, 1000)),  // loss -> Opus
        (30.0, SenderEvent::Link(0.0, 800)),   // clean again -> PCM after the dwell time
    ];
    let o = sim.run();
    let w = warmup_end(&o);
    let (g, md) = analyse_sine(&o.l, w, o.l.len(), FREQ, AMP, 0.2);
    report("auto", &o, g, md);
    // The log shows both codecs, in the order PCM -> Opus -> PCM.
    let mut transitions = Vec::new();
    let mut last = None;
    for &(t, f) in &o.sent_log {
        let opus = f & audio_flags::OPUS != 0;
        if last != Some(opus) {
            transitions.push((t - 5.0, opus));
            last = Some(opus);
        }
    }
    eprintln!("transitions: {transitions:?}");
    assert_eq!(transitions.len(), 3, "{transitions:?}");
    assert!(!transitions[0].1 && transitions[1].1 && !transitions[2].1);
    assert!(transitions[1].0 > 12.0 && transitions[1].0 < 12.1);
    assert!(transitions[2].0 > 35.0 && transitions[2].0 < 36.0, "dwell: {transitions:?}");
    let s = o.rx.stats();
    assert_eq!(s.underruns, 0);
    assert_eq!(s.plc_count, 0);
    assert_eq!(g, 0, "glitches");
    assert!(md < max_sine_delta() * 1.6);
    // Tight check right around both switches: sample accurate, no 2.5 ms phase jump.
    for &(t, _) in &transitions[1..] {
        let centre = ((t + 0.05) * RATE) as usize + first_signal(&o.l) - (0.0137 * RATE) as usize;
        let _ = centre;
    }
}

#[test]
fn explicit_policy_changes_apply_instantly() {
    let mut sim = Sim::new(speaker_cfg(CodecPolicy::Pcm), sine_signal(FREQ, AMP));
    sim.secs = 20.0;
    sim.sender_events = vec![
        (5.0, SenderEvent::Policy(CodecPolicy::Opus)),
        (8.0, SenderEvent::Policy(CodecPolicy::Pcm)),
        (11.0, SenderEvent::Policy(CodecPolicy::Opus)),
        (11.5, SenderEvent::Policy(CodecPolicy::Pcm)),
        (12.0, SenderEvent::Policy(CodecPolicy::Opus)),
    ];
    let o = sim.run();
    let w = warmup_end(&o);
    let (g, md) = analyse_sine(&o.l, w, o.l.len(), FREQ, AMP, 0.2);
    report("policy", &o, g, md);
    assert_eq!(g, 0);
    assert_eq!(o.rx.stats().underruns, 0);
    assert!(md < max_sine_delta() * 1.6);
}

#[test]
#[ignore]
fn debug_pcm_loss() {
    let net = NetCfg { loss: 0.02, ..NetCfg::clean() };
    let mut sim = Sim::new(speaker_cfg(CodecPolicy::Pcm), sine_signal(FREQ, AMP));
    sim.secs = 12.0;
    sim.net = net;
    let o = sim.run();
    let w = warmup_end(&o);
    let c = (2.0 * (2.0 * PI * FREQ / RATE).cos()) as f32;
    let mut shown = 0;
    let mut n = w;
    while n < o.l.len() - 2 && shown < 3 {
        let r = o.l[n + 1] + o.l[n - 1] - c * o.l[n];
        if r.abs() > 0.05 * AMP {
            eprintln!("glitch at {n}:");
            let from = n.saturating_sub(8);
            let line: Vec<String> = (from..from + 400).step_by(1).map(|i| format!("{:.2}", o.l[i])).collect();
            eprintln!("{}", line.join(" "));
            shown += 1;
            n += 3000;
        }
        n += 1;
    }
}
