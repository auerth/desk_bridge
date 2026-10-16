//! Pure helpers of the audio backend (no COM, no OS calls) so they can be unit-tested anywhere.

use deskbridge_platform::audio::RATE;

/// Converts a WASAPI QPC position (100 ns units, as returned by `IAudioCaptureClient::GetBuffer`)
/// to microseconds on the same monotonic (QPC) clock.
pub fn qpc_100ns_to_us(pos_100ns: u64) -> u64 {
    pos_100ns / 10
}

/// Converts raw `QueryPerformanceCounter` ticks to microseconds (`freq` in Hz).
pub fn qpc_ticks_to_us(ticks: i64, freq: i64) -> u64 {
    if freq <= 0 || ticks < 0 {
        return 0;
    }
    // i128 so that long uptimes with a high-frequency counter cannot overflow.
    (i128::from(ticks) * 1_000_000 / i128::from(freq)) as u64
}

/// Duration of `frames` sample frames at 48 kHz, in microseconds.
pub fn frames_to_us(frames: u64) -> u64 {
    frames * 1_000_000 / u64::from(RATE)
}

/// Averages interleaved `channels`-channel audio down to mono. A trailing partial frame is dropped.
pub fn downmix_to_mono(input: &[f32], channels: usize) -> Vec<f32> {
    match channels {
        0 => Vec::new(),
        1 => input.to_vec(),
        n => {
            let inv = 1.0 / n as f32;
            input
                .chunks_exact(n)
                .map(|f| f.iter().sum::<f32>() * inv)
                .collect()
        }
    }
}

/// Peak absolute sample value (0.0 for an empty slice).
pub fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0_f32, |m, s| m.max(s.abs()))
}

/// Generates zero-filled frames at a steady cadence while the device delivers no packets.
///
/// WASAPI loopback only produces data while something is playing, but downstream silence
/// detection / keep-alive logic needs a regular frame stream. The capture thread feeds every real
/// packet into [`on_data`](Self::on_data) and calls [`poll`](Self::poll) after every wake-up.
#[derive(Debug, Clone)]
pub struct SilenceFiller {
    threshold_us: u64,
    chunk_frames: usize,
    /// Timestamp at which the next synthetic chunk would start.
    next_ts_us: u64,
    /// Wall-clock time (same clock as `now_us`) when real data was last seen.
    last_data_us: u64,
}

impl SilenceFiller {
    /// Real data silent for `threshold_us` triggers synthetic chunks of `chunk_frames` frames.
    pub fn new(start_us: u64, threshold_us: u64, chunk_frames: usize) -> Self {
        Self {
            threshold_us,
            chunk_frames,
            next_ts_us: start_us,
            last_data_us: start_us,
        }
    }

    fn chunk_us(&self) -> u64 {
        frames_to_us(self.chunk_frames as u64)
    }

    /// Records a real packet starting at `ts_us` with `frames` frames, seen at wall time `now_us`.
    pub fn on_data(&mut self, ts_us: u64, frames: usize, now_us: u64) {
        self.next_ts_us = ts_us + frames_to_us(frames as u64);
        self.last_data_us = now_us;
    }

    /// Returns the next synthetic chunk `(ts_us, frames)` if one is due. Call repeatedly until it
    /// returns `None`.
    pub fn poll(&mut self, now_us: u64) -> Option<(u64, usize)> {
        if now_us.saturating_sub(self.last_data_us) < self.threshold_us {
            return None;
        }
        let chunk = self.chunk_us();
        // After a long gap (system sleep, debugger) do not burst; resynchronise to "now".
        if now_us.saturating_sub(self.next_ts_us) > 10 * chunk {
            self.next_ts_us = now_us.saturating_sub(chunk);
        }
        if self.next_ts_us + chunk <= now_us {
            let ts = self.next_ts_us;
            self.next_ts_us += chunk;
            Some((ts, self.chunk_frames))
        } else {
            None
        }
    }
}

/// Heuristic: does this endpoint look like a virtual / software device (not a real headset)?
///
/// `desc` is the device description, `enumerator` the PnP enumerator name (`HDAUDIO`, `USB`,
/// `BTHENUM`, `ROOT`, ...). Root-enumerated devices are software devices by definition.
pub fn looks_virtual(name: &str, desc: Option<&str>, enumerator: Option<&str>) -> bool {
    const PHRASES: [&str; 6] = [
        "desk bridge",
        "deskbridge",
        "vb-audio",
        "steam streaming",
        "remote audio",
        "virtual",
    ];
    const WORDS: [&str; 3] = ["cable", "voicemeeter", "vac"];

    let hay = match desc {
        Some(d) => format!("{name} {d}").to_lowercase(),
        None => name.to_lowercase(),
    };
    if PHRASES.iter().any(|p| hay.contains(p)) {
        return true;
    }
    if hay
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| WORDS.contains(&w))
    {
        return true;
    }
    enumerator.is_some_and(|e| e.eq_ignore_ascii_case("ROOT"))
}

/// What a device state change means for a consumer tracking *usable* endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateTransition {
    BecameActive,
    BecameInactive,
    NoChange,
}

/// `DEVICE_STATE_ACTIVE`.
pub const DEVICE_STATE_ACTIVE_MASK: u32 = 0x1;

/// Maps a `OnDeviceStateChanged` new state (ACTIVE=1, DISABLED=2, NOTPRESENT=4, UNPLUGGED=8) to a
/// transition relative to what we knew before. Powering off a wireless headset yields
/// `UNPLUGGED`/`NOTPRESENT`, powering it on `ACTIVE`.
pub fn classify_state(was_active: bool, new_state: u32) -> StateTransition {
    let is_active = new_state & DEVICE_STATE_ACTIVE_MASK != 0;
    match (was_active, is_active) {
        (false, true) => StateTransition::BecameActive,
        (true, false) => StateTransition::BecameInactive,
        _ => StateTransition::NoChange,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qpc_conversions() {
        assert_eq!(qpc_100ns_to_us(10_000_000), 1_000_000);
        assert_eq!(qpc_100ns_to_us(0), 0);
        assert_eq!(qpc_ticks_to_us(10_000_000, 10_000_000), 1_000_000);
        assert_eq!(qpc_ticks_to_us(5, 0), 0);
        // 30 days of uptime at 10 MHz must not overflow.
        let ticks = 30 * 86_400 * 10_000_000_i64;
        assert_eq!(qpc_ticks_to_us(ticks, 10_000_000), 30 * 86_400 * 1_000_000);
    }

    #[test]
    fn frame_durations() {
        assert_eq!(frames_to_us(480), 10_000);
        assert_eq!(frames_to_us(240), 5_000);
    }

    #[test]
    fn downmix() {
        assert_eq!(downmix_to_mono(&[1.0, 0.0, 0.5, 0.5], 2), vec![0.5, 0.5]);
        assert_eq!(downmix_to_mono(&[0.25, 0.75], 1), vec![0.25, 0.75]);
        // Partial trailing frame is dropped.
        assert_eq!(downmix_to_mono(&[1.0, 1.0, 1.0], 2), vec![1.0]);
        assert!(downmix_to_mono(&[1.0], 0).is_empty());
        let m = downmix_to_mono(&[0.3, 0.3, 0.3, 0.0, 0.0, 0.9], 3);
        assert!((m[0] - 0.3).abs() < 1e-6 && (m[1] - 0.3).abs() < 1e-6);
    }

    #[test]
    fn peak_level() {
        assert_eq!(peak(&[]), 0.0);
        assert_eq!(peak(&[0.1, -0.7, 0.3]), 0.7);
    }

    #[test]
    fn silence_cadence() {
        let mut f = SilenceFiller::new(1_000_000, 20_000, 480);
        // Not yet 20 ms without data.
        assert_eq!(f.poll(1_010_000), None);
        assert_eq!(f.poll(1_019_999), None);
        // After 20 ms, two chunks are due to catch up (0-10 ms, 10-20 ms), then nothing.
        assert_eq!(f.poll(1_020_000), Some((1_000_000, 480)));
        assert_eq!(f.poll(1_020_000), Some((1_010_000, 480)));
        assert_eq!(f.poll(1_020_000), None);
        // Cadence continues every 10 ms.
        assert_eq!(f.poll(1_030_000), Some((1_020_000, 480)));
        assert_eq!(f.poll(1_030_000), None);
    }

    #[test]
    fn silence_stops_when_data_arrives_and_resumes() {
        let mut f = SilenceFiller::new(0, 20_000, 480);
        f.on_data(0, 480, 10_000);
        f.on_data(10_000, 480, 20_000);
        assert_eq!(f.poll(25_000), None);
        // Data stops at 20 ms; next synthetic chunk follows the end of the last real packet.
        assert_eq!(f.poll(40_000), Some((20_000, 480)));
        assert_eq!(f.poll(40_000), Some((30_000, 480)));
        assert_eq!(f.poll(40_000), None);
    }

    #[test]
    fn silence_resyncs_after_long_gap() {
        let mut f = SilenceFiller::new(0, 20_000, 480);
        // 10 seconds without polling must not produce a 1000-chunk burst.
        let mut n = 0;
        while f.poll(10_000_000).is_some() {
            n += 1;
            assert!(n < 5, "burst after long gap");
        }
        assert_eq!(n, 1);
    }

    #[test]
    fn virtual_name_heuristics() {
        assert!(looks_virtual("Desk Bridge Lautsprecher", None, None));
        assert!(looks_virtual(
            "CABLE Output (VB-Audio Virtual Cable)",
            None,
            None
        ));
        assert!(looks_virtual("Line 1 (Virtual Audio Cable)", None, None));
        assert!(looks_virtual(
            "Voicemeeter Input (VB-Audio Voicemeeter VAIO)",
            None,
            None
        ));
        assert!(looks_virtual("Speakers", Some("Whatever"), Some("ROOT")));
        assert!(!looks_virtual(
            "Speakers (Realtek(R) Audio)",
            Some("Speakers"),
            Some("HDAUDIO")
        ));
        assert!(!looks_virtual(
            "Game (Arctis Nova 7 Wireless)",
            Some("Game"),
            Some("USB")
        ));
        assert!(!looks_virtual(
            "Headset Earphone (Jabra Evolve2)",
            None,
            Some("BTHENUM")
        ));
        assert!(!looks_virtual("Cabletron Gaming Headset", None, None));
    }

    #[test]
    fn state_transitions() {
        assert_eq!(classify_state(false, 1), StateTransition::BecameActive);
        assert_eq!(classify_state(true, 8), StateTransition::BecameInactive); // unplugged
        assert_eq!(classify_state(true, 4), StateTransition::BecameInactive); // not present
        assert_eq!(classify_state(true, 2), StateTransition::BecameInactive); // disabled
        assert_eq!(classify_state(true, 1), StateTransition::NoChange);
        assert_eq!(classify_state(false, 8), StateTransition::NoChange);
    }
}
