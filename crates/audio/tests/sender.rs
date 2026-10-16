//! Unit-level tests of `AudioSender` packetisation.

use bytes::Bytes;
use deskbridge_audio::opus::packet_frames;
use deskbridge_audio::{AudioSender, CodecPolicy, OpusApp, SenderConfig, pcm_decode};
use deskbridge_proto::media::{AudioPacket, STREAM_AUDIO_MIC, STREAM_AUDIO_SPEAKER, audio_flags};

fn cfg(channels: u16, codec: CodecPolicy) -> SenderConfig {
    SenderConfig {
        stream_id: if channels == 1 {
            STREAM_AUDIO_MIC
        } else {
            STREAM_AUDIO_SPEAKER
        },
        channels,
        codec,
        opus_bitrate_bps: 128_000,
        opus_app: OpusApp::LowDelay,
    }
}

fn sine(start: usize, frames: usize, ch: usize, amp: f32) -> Vec<f32> {
    let mut v = Vec::with_capacity(frames * ch);
    for i in 0..frames {
        let s = amp * (2.0 * std::f32::consts::PI * 440.0 * (start + i) as f32 / 48_000.0).sin();
        for _ in 0..ch {
            v.push(s);
        }
    }
    v
}

fn parse(d: &[Bytes]) -> Vec<AudioPacket> {
    d.iter()
        .map(|b| AudioPacket::decode(b.clone()).unwrap())
        .collect()
}

/// Pushes `total` frames in chunks of `chunk` frames and returns all packets.
fn feed(
    tx: &mut AudioSender,
    ch: usize,
    total: usize,
    chunk: usize,
    base_ts: u64,
) -> Vec<AudioPacket> {
    let mut out = Vec::new();
    let mut done = 0;
    while done < total {
        let n = chunk.min(total - done);
        let ts = base_ts + (done as u64) * 1_000_000 / 48_000;
        out.extend(parse(&tx.push(&sine(done, n, ch, 0.5), ts)));
        done += n;
    }
    out
}

#[test]
fn pcm_reframes_arbitrary_chunks_into_5ms_packets() {
    let mut tx = AudioSender::new(cfg(2, CodecPolicy::Pcm)).unwrap();
    let pkts = feed(&mut tx, 2, 48_000, 441, 1_000_000);
    assert_eq!(pkts.len(), 48_000 / 240);
    let mut samples = Vec::new();
    for (i, p) in pkts.iter().enumerate() {
        assert_eq!(p.stream_id, STREAM_AUDIO_SPEAKER);
        assert_eq!(p.flags, 0);
        assert_eq!(p.seq, i as u16);
        assert_eq!(p.ts_us, 1_000_000 + i as u64 * 5_000);
        assert_eq!(p.payload.len(), 240 * 2 * 2);
        pcm_decode(&p.payload, &mut samples);
    }
    let want = sine(0, pkts.len() * 240, 2, 0.5);
    let err = samples
        .iter()
        .zip(&want)
        .map(|(a, b)| (a - b).abs())
        .fold(0f32, f32::max);
    assert!(err <= 1.0 / 32768.0 + 1e-6, "pcm error {err}");
}

#[test]
fn opus_packets_are_10ms_and_flagged() {
    let mut tx = AudioSender::new(cfg(2, CodecPolicy::Opus)).unwrap();
    let pkts = feed(&mut tx, 2, 48_000, 480, 0);
    assert_eq!(pkts.len(), 100);
    assert!(tx.current_codec_is_opus());
    for (i, p) in pkts.iter().enumerate() {
        assert_eq!(p.flags, audio_flags::OPUS);
        assert_eq!(p.ts_us, i as u64 * 10_000);
        assert_eq!(packet_frames(&p.payload), Some(480));
        assert!(
            p.payload.len() < 1200,
            "fits a datagram: {}",
            p.payload.len()
        );
    }
    // bitrate changes take effect
    let before: usize = pkts[50..].iter().map(|p| p.payload.len()).sum();
    tx.set_opus_bitrate(32_000);
    let more = feed(&mut tx, 2, 48_000, 480, 1_000_000);
    let after: usize = more[50..].iter().map(|p| p.payload.len()).sum();
    assert!(after * 2 < before, "{after} vs {before}");
}

#[test]
fn mono_flag_and_sizes() {
    let mut tx = AudioSender::new(cfg(1, CodecPolicy::Pcm)).unwrap();
    let pkts = feed(&mut tx, 1, 4_800, 480, 0);
    assert_eq!(pkts.len(), 20);
    assert!(
        pkts.iter()
            .all(|p| p.flags == audio_flags::MONO && p.payload.len() == 480)
    );
    assert!(pkts.iter().all(|p| p.stream_id == STREAM_AUDIO_MIC));
    let mut tx = AudioSender::new(cfg(1, CodecPolicy::Opus)).unwrap();
    let pkts = feed(&mut tx, 1, 4_800, 480, 0);
    assert!(
        pkts.iter()
            .all(|p| p.flags == audio_flags::MONO | audio_flags::OPUS)
    );
}

#[test]
fn sequence_numbers_wrap() {
    let mut tx = AudioSender::new(cfg(2, CodecPolicy::Pcm)).unwrap();
    tx.set_start_seq(65_530);
    let pkts = feed(&mut tx, 2, 240 * 12, 240, 0);
    let seqs: Vec<u16> = pkts.iter().map(|p| p.seq).collect();
    assert_eq!(seqs[..8], [65530, 65531, 65532, 65533, 65534, 65535, 0, 1]);
}

#[test]
fn silence_is_reduced_to_keepalives_and_resumes_instantly() {
    let mut tx = AudioSender::new(cfg(2, CodecPolicy::Pcm)).unwrap();
    let silent = |tx: &mut AudioSender, secs: f64, ts: u64| {
        let n = (secs * 48_000.0) as usize;
        let mut out = Vec::new();
        for k in 0..n / 480 {
            out.extend(parse(&tx.push(&vec![0.0; 960], ts + k as u64 * 10_000)));
        }
        out
    };
    let pkts = silent(&mut tx, 5.0, 0);
    let audio = pkts.iter().filter(|p| !p.is_silence()).count();
    let ka = pkts.iter().filter(|p| p.is_silence()).count();
    // 200 ms hang-over = 40 packets of 5 ms, then ~10 keep-alives per second
    assert_eq!(audio, 39, "audio packets before suppression");
    assert!((47..=50).contains(&ka), "keep-alives {ka}");
    assert!(
        pkts.iter()
            .filter(|p| p.is_silence())
            .all(|p| p.payload.is_empty())
    );
    // sequence numbers stay contiguous across keep-alives
    assert!(
        pkts.windows(2)
            .all(|w| w[1].seq == w[0].seq.wrapping_add(1))
    );
    // 1 s of signal after silence: the very first packet carries audio
    let resume = parse(&tx.push(&sine(0, 480, 2, 0.3), 5_000_000));
    assert_eq!(resume.len(), 2);
    assert!(
        resume
            .iter()
            .all(|p| !p.is_silence() && p.payload.len() == 960)
    );
    let mut s = Vec::new();
    pcm_decode(&resume[0].payload, &mut s);
    assert!(s.iter().any(|v| v.abs() > 0.1));
    // very quiet noise below -80 dBFS counts as silence
    let mut tx = AudioSender::new(cfg(2, CodecPolicy::Pcm)).unwrap();
    let mut n = 0;
    for k in 0..200 {
        n += tx.push(&vec![2e-5; 960], k * 10_000).len();
    }
    assert!(n < 100, "{n}");
}

#[test]
fn timestamps_follow_capture_gaps() {
    let mut tx = AudioSender::new(cfg(2, CodecPolicy::Pcm)).unwrap();
    let a = parse(&tx.push(&sine(0, 480, 2, 0.5), 10_000));
    assert_eq!(a[1].ts_us, 15_000);
    // capture paused for ~1 s (e.g. loopback without playback)
    let b = parse(&tx.push(&sine(480, 480, 2, 0.5), 1_020_000));
    assert_eq!(b[0].ts_us, 1_020_000);
    assert_eq!(b[1].ts_us, 1_025_000);
}

#[test]
fn auto_policy_follows_link_with_hysteresis() {
    let mut tx = AudioSender::new(cfg(2, CodecPolicy::Auto)).unwrap();
    let mut ts = 0u64;
    let mut run = |tx: &mut AudioSender, secs: usize| {
        for _ in 0..secs * 100 {
            tx.push(&sine(0, 480, 2, 0.4), ts);
            ts += 10_000;
        }
    };
    run(&mut tx, 1);
    assert!(!tx.current_codec_is_opus(), "wired + clean: PCM");
    tx.set_link(0.05, 1_000);
    run(&mut tx, 1);
    assert!(!tx.current_codec_is_opus(), "0.05 % loss is still fine");
    tx.set_link(0.2, 1_000);
    run(&mut tx, 1);
    assert!(tx.current_codec_is_opus(), "0.2 % loss -> Opus");
    tx.set_link(0.0, 1_000);
    run(&mut tx, 2);
    assert!(
        tx.current_codec_is_opus(),
        "stays on Opus for the minimum dwell time"
    );
    run(&mut tx, 4);
    assert!(!tx.current_codec_is_opus(), "clean again -> PCM");
    tx.set_link(0.0, 20_000);
    run(&mut tx, 1);
    assert!(tx.current_codec_is_opus(), "high RTT (Wi-Fi) -> Opus");
    tx.set_codec_policy(CodecPolicy::Pcm);
    run(&mut tx, 1);
    assert!(!tx.current_codec_is_opus());
}

#[test]
fn peak_level_is_since_last_call() {
    let mut tx = AudioSender::new(cfg(2, CodecPolicy::Pcm)).unwrap();
    tx.push(&sine(0, 480, 2, 0.25), 0);
    let p = tx.peak_level();
    assert!((p - 0.25).abs() < 0.01, "{p}");
    assert_eq!(tx.peak_level(), 0.0);
}

#[test]
fn empty_and_partial_input_is_harmless() {
    let mut tx = AudioSender::new(cfg(2, CodecPolicy::Pcm)).unwrap();
    assert!(tx.push(&[], 0).is_empty());
    assert!(tx.push(&[0.5, 0.5, 0.5], 0).is_empty()); // 1 frame + 1 stray sample
    let out = tx.push(&sine(1, 239, 2, 0.5), 20);
    assert_eq!(out.len(), 1);
}
