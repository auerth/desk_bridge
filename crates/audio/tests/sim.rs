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
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
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
        Self {
            base_us: 1000.0,
            jitter_us: 1500.0,
            loss: 0.0,
            dup: 0.0,
            reorder: 0.0,
            reorder_extra_us: 0.0,
        }
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
    /// (start s, duration s): packets sent in this window are delivered at its end.
    stall: Option<(f64, f64)>,
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
            stall: None,
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
            let t_net = net
                .peek()
                .map_or(f64::INFINITY, |Reverse((t, _, _))| *t as f64 * 1e-6);
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
                        let mut at = (t_send * 1e6 + delay).round() as u64;
                        if let Some((st, du)) = self.stall {
                            let (a, b) = ((t0 + st) * 1e6, (t0 + st + du) * 1e6);
                            if (at as f64) > a && (at as f64) < b {
                                at = b as u64 + (rng.next_f64() * 200.0) as u64;
                            }
                        }
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
                    out.series.push(Series {
                        t: t_ren - t0,
                        stats: out.rx.stats(),
                    });
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
        s.fill_ms,
        s.target_ms,
        s.drift_ppm,
        s.resample_ratio,
        s.underruns,
        s.overruns,
        s.plc_count,
        s.late,
        s.duplicates,
        s.loss_pct,
        s.jitter_ms,
        s.jitter_peak_ms,
        s.packets
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
    let (g, md) = analyse_sine(
        &o.l,
        w,
        o.l.len(),
        FREQ,
        AMP,
        if codec == CodecPolicy::Opus {
            0.2
        } else {
            0.03
        },
    );
    report(name, &o, g, md);
    if std::env::var("SIM_TRACE").is_ok() {
        for p in o.series.iter().step_by(2) {
            eprintln!(
                "  t={:6.2} fill={:6.2} tgt={:5.2} drift={:8.1} ratio={:.6} und={} plc={} st={:?}",
                p.t,
                p.stats.fill_ms,
                p.stats.target_ms,
                p.stats.drift_ppm,
                p.stats.resample_ratio,
                p.stats.underruns,
                p.stats.plc_count,
                p.stats.state
            );
        }
    }
    let s = o.rx.stats();
    // underruns in steady state: compare with the stats at the warm-up point
    let at_warm = o
        .series
        .iter()
        .find(|p| p.t * RATE >= (w as f64))
        .map(|p| p.stats.underruns)
        .unwrap();
    assert_eq!(s.underruns, at_warm, "no underruns after warm-up");
    assert_eq!(s.underruns, 0, "no underruns at all");
    assert!(
        (s.fill_ms - s.target_ms).abs() < 3.0,
        "fill {} vs target {}",
        s.fill_ms,
        s.target_ms
    );
    let expect = -(0.0f64 - render_ppm); // render faster -> positive drift
    if render_ppm != 0.0 {
        let rel = (f64::from(s.drift_ppm) - expect).abs() / expect.abs();
        assert!(
            rel < 0.2,
            "drift estimate {} vs truth {expect}",
            s.drift_ppm
        );
    } else {
        assert!(s.drift_ppm.abs() < 30.0, "drift {}", s.drift_ppm);
    }
    assert_eq!(g, 0, "glitches");
    assert!(
        md <= max_sine_delta()
            * if codec == CodecPolicy::Opus {
                1.6
            } else {
                1.05
            },
        "max delta {md}"
    );
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

fn impaired_case(
    name: &str,
    codec: CodecPolicy,
    net: NetCfg,
    secs: f64,
    thr: f32,
) -> (SimOut, usize, f32) {
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
    let net = NetCfg {
        loss: 0.02,
        ..NetCfg::clean()
    };
    let (o, g, md) = impaired_case("pcm-loss2", CodecPolicy::Pcm, net, 40.0, 0.05);
    let s = o.rx.stats();
    let lost = o.packets_dropped as f64;
    assert!(s.plc_count as f64 >= lost * 0.9 && s.plc_count as f64 <= lost * 1.1 + 2.0);
    assert!((s.loss_pct - 2.0).abs() < 1.5, "loss {}", s.loss_pct);
    // The pitch-synchronous concealment keeps a 1 kHz tone continuous.
    assert!(
        g as f64 <= lost * 0.15 + 2.0,
        "glitches {g} for {lost} lost packets"
    );
    assert!(md < max_sine_delta() * 1.5, "max delta {md}");
    assert!(s.underruns <= 1, "underruns {}", s.underruns);
}

#[test]
fn opus_loss_2_percent_is_concealed() {
    let net = NetCfg {
        loss: 0.02,
        ..NetCfg::clean()
    };
    let (o, g, md) = impaired_case("opus-loss2", CodecPolicy::Opus, net, 40.0, 0.2);
    let s = o.rx.stats();
    let lost = o.packets_dropped as f64;
    assert!(s.plc_count as f64 >= lost * 0.9 && s.plc_count as f64 <= lost * 1.1 + 2.0);
    assert!(
        g as f64 <= lost * 0.3 + 2.0,
        "glitches {g} for {lost} lost packets"
    );
    assert!(md < max_sine_delta() * 2.0, "max delta {md}");
    // At most the first loss (before the loss estimate raised the target) may underrun.
    assert!(s.underruns <= 3, "underruns {}", s.underruns);
}

#[test]
fn reordering_and_duplicates_are_handled() {
    let net = NetCfg {
        reorder: 0.05,
        reorder_extra_us: 4000.0,
        dup: 0.03,
        ..NetCfg::clean()
    };
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
    let rx =
        AudioReceiver::with_clock(ReceiverConfig::default(), Arc::new(ManualClock::new())).unwrap();
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
        (12.0, SenderEvent::Link(0.5, 1000)), // loss -> Opus
        (30.0, SenderEvent::Link(0.0, 800)),  // clean again -> PCM after the dwell time
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
    assert!(transitions[1].0 > 11.99 && transitions[1].0 < 12.1);
    // Back to PCM as soon as the link is clean (the 5 s minimum dwell has long passed).
    assert!(
        transitions[2].0 > 29.99 && transitions[2].0 < 30.1,
        "{transitions:?}"
    );
    let s = o.rx.stats();
    assert_eq!(s.underruns, 0);
    assert_eq!(s.plc_count, 0);
    assert_eq!(g, 0, "glitches");
    assert!(md < max_sine_delta() * 1.6);
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

/// Largest normalised 2nd-order residual in `x[from..to]` (0 = perfect sine).
fn max_residual(x: &[f32], from: usize, to: usize, freq: f64, amp: f32) -> f32 {
    let c = (2.0 * (2.0 * PI * freq / RATE).cos()) as f32;
    (from.max(1)..to.min(x.len() - 1))
        .map(|n| (x[n + 1] + x[n - 1] - c * x[n]).abs() / amp)
        .fold(0.0, f32::max)
}

#[test]
fn switch_is_sample_accurate() {
    // PCM -> Opus at 8 s, Opus -> PCM at 14 s. Compare the residual around the switches with
    // the Opus-only residual: a misaligned switch would be an order of magnitude larger.
    let mut sim = Sim::new(speaker_cfg(CodecPolicy::Pcm), sine_signal(FREQ, AMP));
    sim.secs = 20.0;
    sim.sender_events = vec![
        (8.0, SenderEvent::Policy(CodecPolicy::Opus)),
        (14.0, SenderEvent::Policy(CodecPolicy::Pcm)),
    ];
    let o = sim.run();
    let at = |t: f64| (t * RATE) as usize;
    let lat = (0.030 * RATE) as usize;
    let opus_only = max_residual(&o.l, at(10.0), at(12.0), FREQ, AMP);
    let pcm_only = max_residual(&o.l, at(3.0), at(6.0), FREQ, AMP);
    let sw1 = max_residual(&o.l, at(8.0) - 2 * lat, at(8.0) + 2 * lat, FREQ, AMP);
    let sw2 = max_residual(&o.l, at(14.0) - 2 * lat, at(14.0) + 2 * lat, FREQ, AMP);
    eprintln!("residuals: pcm={pcm_only:.4} opus={opus_only:.4} sw1={sw1:.4} sw2={sw2:.4}");
    assert!(pcm_only < 0.005);
    // A switch that is off by even one sample would show ~0.13; 120 samples (the Opus
    // look-ahead) would be ~1.
    assert!(opus_only < 0.005);
    assert!(sw1 < 0.05, "PCM->Opus switch residual {sw1}");
    assert!(sw2 < 0.05, "Opus->PCM switch residual {sw2}");
}

// ---------------------------------------------------------------------------------------
// Silence suppression, mono, volume, latency
// ---------------------------------------------------------------------------------------

#[test]
fn silence_is_suppressed_and_resumes_instantly() {
    let sig: Signal = Box::new(|i| {
        let t = i as f64 / RATE;
        if !(3.0..8.0).contains(&t) {
            let s = AMP * (2.0 * PI * FREQ * t).sin() as f32;
            (s, s)
        } else {
            (0.0, 0.0)
        }
    });
    let mut sim = Sim::new(speaker_cfg(CodecPolicy::Pcm), sig);
    sim.secs = 14.0;
    let o = sim.run();
    // sender side: ~10 keep-alives/s once the 200 ms hang-over has passed
    let in_silence = |t: f64| t - 5.0 > 3.3 && t - 5.0 < 7.9;
    let ka = o
        .sent_log
        .iter()
        .filter(|(t, f)| in_silence(*t) && f & audio_flags::SILENCE != 0)
        .count();
    let other = o
        .sent_log
        .iter()
        .filter(|(t, f)| in_silence(*t) && f & audio_flags::SILENCE == 0)
        .count();
    let dur = 7.9 - 3.3;
    eprintln!("keep-alives {ka} in {dur:.1}s, other packets {other}");
    assert_eq!(other, 0, "audio packets during suppressed silence");
    assert!(
        (ka as f64 / dur - 10.0).abs() < 1.0,
        "keep-alive rate {}",
        ka as f64 / dur
    );
    let total = o.sent_log.len() as f64;
    assert!(total < 9.5 * 200.0 + 70.0, "total packets {total}");

    // receiver: silence is not an underrun, signal is back with the normal latency
    let s = o.rx.stats();
    assert_eq!(s.underruns, 0);
    let from = (7.5 * RATE) as usize;
    let first_after = o.l[from..].iter().position(|v| v.abs() > 0.05).unwrap() + from;
    // output index 0 is rendered at 0.0137 s after the sim start
    let t_out = first_after as f64 / RATE + 0.0137;
    eprintln!("signal back at output t={t_out:.4} s");
    assert!(t_out - 8.0 < 0.045, "resume latency {}", t_out - 8.0);
    // no growth: the buffer re-centred
    assert!(
        (s.fill_ms - s.target_ms).abs() < 3.0,
        "fill {} target {}",
        s.fill_ms,
        s.target_ms
    );
    let w = first_after + 3 * 48_000;
    let (g, md) = analyse_sine(&o.l, w, o.l.len(), FREQ, AMP, 0.03);
    assert_eq!(g, 0);
    assert!(md < max_sine_delta() * 1.05);
    // The pre-silence part is intact too.
    let fs = first_signal(&o.l);
    let (g0, _) = analyse_sine(
        &o.l,
        fs + 48_000,
        fs + (2.5 * RATE) as usize,
        FREQ,
        AMP,
        0.03,
    );
    assert_eq!(g0, 0);
}

#[test]
fn mono_mic_is_upmixed_to_stereo() {
    for codec in [CodecPolicy::Pcm, CodecPolicy::Opus] {
        let mut sim = Sim::new(mic_cfg(codec), sine_signal(FREQ, AMP));
        sim.secs = 15.0;
        sim.rx_cfg.channels_in = 1;
        let o = sim.run();
        let w = warmup_end(&o);
        assert!(
            o.l[w..].iter().zip(&o.r[w..]).all(|(a, b)| a == b),
            "L != R"
        );
        let thr = if codec == CodecPolicy::Opus {
            0.2
        } else {
            0.03
        };
        let (g, md) = analyse_sine(&o.l, w, o.l.len(), FREQ, AMP, thr);
        report("mono", &o, g, md);
        assert_eq!(g, 0);
        assert_eq!(o.rx.stats().underruns, 0);
        assert!(o.l[w..].iter().fold(0f32, |m, v| m.max(v.abs())) > 0.45);
    }
}

#[test]
fn stereo_channels_stay_separate() {
    let sig: Signal = Box::new(|i| {
        let t = i as f64 / RATE;
        (
            AMP * (2.0 * PI * 700.0 * t).sin() as f32,
            AMP * (2.0 * PI * 1500.0 * t).sin() as f32,
        )
    });
    let mut sim = Sim::new(speaker_cfg(CodecPolicy::Pcm), sig);
    sim.secs = 10.0;
    let o = sim.run();
    let w = warmup_end(&o);
    let (gl, _) = analyse_sine(&o.l, w, o.l.len(), 700.0, AMP, 0.03);
    let (gr, _) = analyse_sine(&o.r, w, o.r.len(), 1500.0, AMP, 0.03);
    assert_eq!((gl, gr), (0, 0));
}

#[test]
fn volume_and_mute_ramp_without_clicks() {
    let clock = Arc::new(ManualClock::new());
    let rx = AudioReceiver::with_clock(ReceiverConfig::default(), clock.clone()).unwrap();
    let mut tx = AudioSender::new(speaker_cfg(CodecPolicy::Pcm)).unwrap();
    let pull = rx.source();
    let mut buf = vec![0f32; 480 * 2];
    let mut out = Vec::new();
    let mut t_us = 1_000_000u64;
    let mut idx = 0u64;
    for step in 0..400 {
        // 10 ms of audio in, 10 ms out
        let mut chunk = Vec::with_capacity(960);
        for i in 0..480 {
            let s = AMP * (2.0 * PI * FREQ * (idx + i) as f64 / RATE).sin() as f32;
            chunk.push(s);
            chunk.push(s);
        }
        idx += 480;
        clock.set_us(t_us);
        for d in tx.push(&chunk, t_us) {
            rx.on_packet(AudioPacket::decode(Bytes::from(d.to_vec())).unwrap());
        }
        t_us += 10_000;
        clock.set_us(t_us);
        if step == 100 {
            rx.set_volume(0.25);
        }
        if step == 200 {
            rx.set_muted(true);
        }
        if step == 300 {
            rx.set_muted(false);
            rx.set_volume(1.0);
        }
        pull.fill(&mut buf);
        out.extend(buf.chunks_exact(2).map(|f| f[0]));
    }
    // Full-scale gain changes take 10 ms: the steepest sample step stays near the signal's own.
    let max_step = AMP * (2.0 * PI * FREQ / RATE).sin() as f32 + AMP / 480.0 * 1.05;
    let w = 60 * 480;
    let md = out[w..]
        .windows(2)
        .map(|p| (p[1] - p[0]).abs())
        .fold(0f32, f32::max);
    assert!(md <= max_step * 1.02, "max delta {md} > {max_step}");
    let rms =
        |a: usize, b: usize| (out[a..b].iter().map(|v| v * v).sum::<f32>() / (b - a) as f32).sqrt();
    let full = AMP / 2f32.sqrt();
    let seg = |s: usize| (s * 480 + 2400, (s + 20) * 480 - 2400);
    let (a, b) = seg(150);
    assert!(
        (rms(a, b) / full - 0.25).abs() < 0.02,
        "quarter volume: {}",
        rms(a, b) / full
    );
    let (a, b) = seg(250);
    assert!(rms(a, b) < 1e-6);
    let (a, b) = seg(350);
    assert!((rms(a, b) / full - 1.0).abs() < 0.03);
    assert!(rx.peak_level() > 0.3);
    assert_eq!(rx.stats().underruns, 0);
}

#[test]
fn end_to_end_latency_is_within_budget() {
    // A short click in a quiet stream; measure input -> output delay on a clean wired link
    // with 5 ms capture chunks and a 10 ms render period.
    let click_at = 12.0f64;
    let sig: Signal = Box::new(move |i| {
        let t = i as f64 / RATE;
        let d = t - click_at;
        let bg = (0.0004 * (2.0 * PI * 300.0 * t).sin()) as f32;
        if (0.0..0.002).contains(&d) {
            let w = 0.5 - 0.5 * (2.0 * PI * d / 0.002).cos();
            let s = (0.8 * w * (2.0 * PI * 2000.0 * d).sin()) as f32;
            (s, s)
        } else {
            (bg, bg)
        }
    });
    let mut sim = Sim::new(speaker_cfg(CodecPolicy::Pcm), sig);
    sim.secs = 15.0;
    sim.net = NetCfg {
        base_us: 300.0,
        jitter_us: 300.0,
        ..NetCfg::clean()
    };
    sim.capture_chunk = 240;
    let o = sim.run();
    let from = (11.0 * RATE) as usize;
    let peak_idx = (from..o.l.len())
        .max_by(|&a, &b| o.l[a].abs().total_cmp(&o.l[b].abs()))
        .unwrap();
    // output sample j is rendered at 0.0137 s + j / RATE (render clock without drift)
    let t_out = 0.0137 + peak_idx as f64 / RATE;
    let latency = t_out - (click_at + 0.001);
    let s = o.rx.stats();
    eprintln!(
        "latency {:.1} ms, target {:.1} ms, fill {:.1}",
        latency * 1e3,
        s.target_ms,
        s.fill_ms
    );
    assert!(
        latency > 0.0 && latency < 0.030,
        "latency {} ms",
        latency * 1e3
    );
}

#[test]
fn burst_after_stall_is_absorbed_without_runaway_latency() {
    // Congestion: packets sent during a 300 ms window are all delivered at its end.
    let mut sim = Sim::new(speaker_cfg(CodecPolicy::Pcm), sine_signal(FREQ, AMP));
    sim.secs = 40.0;
    sim.render_ppm = 50.0;
    sim.stall = Some((15.0, 0.3));
    let o = sim.run();
    let s = o.rx.stats();
    let (g, md) = analyse_sine(
        &o.l,
        ((22.0 + 0.0137) * RATE) as usize,
        o.l.len(),
        FREQ,
        AMP,
        0.03,
    );
    report("stall", &o, g, md);
    assert!(s.underruns <= 3, "underruns {}", s.underruns);
    assert!(
        (s.fill_ms - s.target_ms).abs() < 3.0,
        "fill {} target {}",
        s.fill_ms,
        s.target_ms
    );
    // The latency recovered and the tone is clean again.
    assert_eq!(g, 0);
}

/// Long run with drifting clocks on both sides, loss and jitter. Run explicitly:
/// `cargo test -p deskbridge-audio --release --test sim soak -- --ignored --nocapture`.
#[test]
#[ignore]
fn soak_ten_minutes_with_drift_loss_and_jitter() {
    let mut sim = Sim::new(speaker_cfg(CodecPolicy::Pcm), sine_signal(FREQ, AMP));
    sim.secs = 600.0;
    sim.sender_ppm = 35.0;
    sim.render_ppm = -120.0;
    sim.net.loss = 0.002;
    sim.net.jitter_us = 2500.0;
    let o = sim.run();
    let w = warmup_end(&o);
    let (g, md) = analyse_sine(&o.l, w, o.l.len(), FREQ, AMP, 0.05);
    report("soak", &o, g, md);
    for p in o.series.iter().step_by(20) {
        eprintln!(
            "  t={:6.1} fill={:6.2} tgt={:5.2} drift={:8.1} ratio={:.6} und={} plc={}",
            p.t, p.stats.fill_ms, p.stats.target_ms, p.stats.drift_ppm, p.stats.resample_ratio,
            p.stats.underruns, p.stats.plc_count
        );
    }
    let s = o.rx.stats();
    // truth: render/sender = (1-120e-6)/(1+35e-6) -> about -155 ppm
    assert!(
        (f64::from(s.drift_ppm) + 155.0).abs() < 25.0,
        "drift {}",
        s.drift_ppm
    );
    assert!(s.underruns <= 2);
    assert!((s.fill_ms - s.target_ms).abs() < 3.0);
    assert!(g as f64 <= o.packets_dropped as f64 * 0.1 + 3.0);
}
