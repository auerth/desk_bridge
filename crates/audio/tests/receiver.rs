//! Robustness tests of `AudioReceiver` against hostile / odd input.

use bytes::Bytes;
use deskbridge_audio::{AudioReceiver, ManualClock, ReceiverConfig};
use deskbridge_proto::media::AudioPacket;
use std::sync::Arc;

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
}

#[test]
fn random_packets_never_panic_and_output_stays_bounded() {
    for seed in [42, 7, 99, 12345] {
        fuzz(seed);
    }
}

fn fuzz(seed: u64) {
    let clock = Arc::new(ManualClock::new());
    let rx = AudioReceiver::with_clock(ReceiverConfig::default(), clock.clone()).unwrap();
    let src = rx.source();
    let mut rng = Lcg(seed);
    let mut out = vec![0f32; 480 * 2];
    let mut t = 1_000_000u64;
    for i in 0..20_000u32 {
        let len = (rng.next() % 1400) as usize;
        let payload: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        let pkt = AudioPacket {
            stream_id: 1,
            flags: (rng.next() % 8) as u8,
            seq: if i % 3 == 0 {
                rng.next() as u16
            } else {
                (i / 3) as u16
            },
            ts_us: rng.next() % 10_000_000,
            payload: Bytes::from(payload),
        };
        clock.set_us(t);
        rx.on_packet(pkt);
        if i % 5 == 0 {
            t += 10_000;
            clock.set_us(t);
            // odd sizes too
            let n = 2 * (1 + (rng.next() % 1500) as usize);
            let mut buf = vec![0f32; n];
            src.fill(&mut buf);
            let bad = buf
                .iter()
                .copied()
                .find(|v| !(v.is_finite() && v.abs() <= 8.0));
            assert!(bad.is_none(), "bad sample {bad:?} at i={i}");
        }
        if i == 10_000 {
            rx.reset();
        }
    }
    src.fill(&mut out);
    let s = rx.stats();
    assert!(s.packets > 0);
    assert!(s.resample_ratio > 0.99 && s.resample_ratio < 1.01);
}

#[test]
fn empty_and_tiny_fill_calls_are_safe() {
    let rx =
        AudioReceiver::with_clock(ReceiverConfig::default(), Arc::new(ManualClock::new())).unwrap();
    let src = rx.source();
    src.fill(&mut []);
    let mut one = [1.0f32];
    src.fill(&mut one);
    assert_eq!(one[0], 0.0);
    let mut two = [1.0f32, 1.0];
    src.fill(&mut two);
    assert_eq!(two, [0.0, 0.0]);
    assert_eq!(rx.peak_level(), 0.0);
}

#[test]
fn defaults_match_the_spec() {
    let wired = AudioReceiver::new(ReceiverConfig::default()).unwrap();
    assert_eq!(wired.stats().target_ms, 10.0);
    let wifi = AudioReceiver::new(ReceiverConfig {
        wired: false,
        ..ReceiverConfig::default()
    })
    .unwrap();
    assert_eq!(wifi.stats().target_ms, 20.0);
    let capped = AudioReceiver::new(ReceiverConfig {
        wired: false,
        max_ms: 15.0,
        ..Default::default()
    })
    .unwrap();
    assert_eq!(capped.stats().target_ms, 15.0);
}
