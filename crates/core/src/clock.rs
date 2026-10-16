//! Monotonic microsecond clock and peer clock offset estimation (Ping/Pong).

use std::sync::OnceLock;
use std::time::Instant;

fn epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

/// Microseconds on this process' monotonic clock (arbitrary origin).
#[must_use]
pub fn now_us() -> u64 {
    epoch().elapsed().as_micros() as u64
}

/// Estimates RTT and the offset between the peer's clock and ours using the sample with the
/// lowest RTT of a sliding window (NTP-style minimum filter).
#[derive(Debug, Default)]
pub struct ClockSync {
    samples: Vec<Sample>,
    last_rtt_us: Option<u64>,
}

#[derive(Debug, Clone, Copy)]
struct Sample {
    rtt_us: u64,
    /// peer_time - our_time (µs)
    offset_us: i64,
}

const WINDOW: usize = 16;

impl ClockSync {
    /// `t0`: our send time, `t1`: peer's time when it answered, `t2`: our receive time.
    pub fn on_pong(&mut self, t0: u64, t1: u64, t2: u64) {
        if t2 < t0 {
            return;
        }
        let rtt = t2 - t0;
        let offset = t1 as i64 - (t0 as i64 + (rtt / 2) as i64);
        self.last_rtt_us = Some(rtt);
        self.samples.push(Sample { rtt_us: rtt, offset_us: offset });
        if self.samples.len() > WINDOW {
            self.samples.remove(0);
        }
    }

    /// Latest round-trip time.
    #[must_use]
    pub fn rtt_us(&self) -> Option<u64> {
        self.last_rtt_us
    }

    /// peer_clock - our_clock, from the lowest-RTT recent sample.
    #[must_use]
    pub fn offset_us(&self) -> Option<i64> {
        self.samples.iter().min_by_key(|s| s.rtt_us).map(|s| s.offset_us)
    }

    /// Converts a peer timestamp to our clock.
    #[must_use]
    pub fn to_local(&self, peer_us: u64) -> Option<u64> {
        let off = self.offset_us()?;
        Some((peer_us as i64 - off).max(0) as u64)
    }

    pub fn reset(&mut self) {
        self.samples.clear();
        self.last_rtt_us = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symmetric_path_gives_exact_offset() {
        let mut c = ClockSync::default();
        // Peer clock is 1_000_000 µs ahead; one-way delay 200 µs.
        c.on_pong(10_000, 10_000 + 200 + 1_000_000, 10_400);
        assert_eq!(c.rtt_us(), Some(400));
        assert_eq!(c.offset_us(), Some(1_000_000));
        assert_eq!(c.to_local(2_000_000), Some(1_000_000));
    }

    #[test]
    fn lowest_rtt_sample_wins() {
        let mut c = ClockSync::default();
        // noisy sample: asymmetric delay 5 ms → wrong offset
        c.on_pong(0, 5_000, 10_000);
        // good sample
        c.on_pong(20_000, 20_100, 20_200);
        assert_eq!(c.offset_us(), Some(0));
    }

    #[test]
    fn nonsense_is_ignored() {
        let mut c = ClockSync::default();
        c.on_pong(100, 0, 50);
        assert!(c.rtt_us().is_none());
    }
}
