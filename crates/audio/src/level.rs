//! Level metering and digital-silence detection.

use deskbridge_platform::audio::RATE;

/// Silence threshold in dBFS (below this a block counts as digital silence).
pub const SILENCE_THRESHOLD_DBFS: f32 = -80.0;

/// Converts dBFS to a linear amplitude.
#[must_use]
pub fn db_to_linear(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// Peak meter with exponential decay plus a "max since last read" tracker.
#[derive(Debug, Clone)]
pub struct LevelMeter {
    decayed: f32,
    held: f32,
    /// Per-sample-frame decay factor of the displayed peak.
    decay: f32,
}

impl LevelMeter {
    /// `decay_ms`: time in which the displayed peak falls by ~63 % (1/e).
    #[must_use]
    pub fn new(decay_ms: f32) -> Self {
        let frames = (decay_ms.max(1.0) * 0.001 * RATE as f32).max(1.0);
        Self { decayed: 0.0, held: 0.0, decay: (-1.0 / frames).exp() }
    }

    /// Feeds a block of interleaved samples; returns the block peak.
    pub fn process(&mut self, samples: &[f32]) -> f32 {
        self.process_frames(samples, samples.len())
    }

    /// Like [`process`](Self::process), `frames` is the time length used for the decay.
    pub fn process_frames(&mut self, samples: &[f32], frames: usize) -> f32 {
        let peak = peak_of(samples);
        self.decayed = (self.decayed * self.decay.powi(frames.min(i32::MAX as usize) as i32)).max(peak);
        self.held = self.held.max(peak);
        peak
    }

    /// Peak with decay applied (for a UI meter).
    #[must_use]
    pub fn peak(&self) -> f32 {
        self.decayed
    }

    /// Maximum since the previous call (resets the tracker).
    pub fn take_peak(&mut self) -> f32 {
        std::mem::take(&mut self.held)
    }
}

impl Default for LevelMeter {
    fn default() -> Self {
        Self::new(300.0)
    }
}

/// Maximum absolute sample value.
#[must_use]
pub fn peak_of(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0f32, |m, &s| m.max(s.abs()))
}

/// Detects digital silence. A stream counts as silent after `hangover_frames` consecutive
/// frames below the threshold; it resumes instantly on the first louder block.
#[derive(Debug, Clone)]
pub struct SilenceDetector {
    threshold: f32,
    hangover_frames: u64,
    silent_frames: u64,
}

impl SilenceDetector {
    /// `threshold_dbfs`: e.g. [`SILENCE_THRESHOLD_DBFS`]; `hangover_ms`: e.g. 200.
    #[must_use]
    pub fn new(threshold_dbfs: f32, hangover_ms: u32) -> Self {
        Self {
            threshold: db_to_linear(threshold_dbfs),
            hangover_frames: u64::from(hangover_ms) * u64::from(RATE) / 1000,
            silent_frames: 0,
        }
    }

    /// Stateless check of a block against the threshold.
    #[must_use]
    pub fn block_is_silent(&self, samples: &[f32]) -> bool {
        peak_of(samples) <= self.threshold
    }

    /// Feeds one block (`frames` per channel). Returns `true` while the stream is in
    /// suppressed silence (silent for at least the hangover time including this block).
    pub fn update(&mut self, samples: &[f32], frames: usize) -> bool {
        if self.block_is_silent(samples) {
            self.silent_frames = self.silent_frames.saturating_add(frames as u64);
        } else {
            self.silent_frames = 0;
        }
        self.is_suppressed()
    }

    #[must_use]
    pub fn is_suppressed(&self) -> bool {
        self.silent_frames >= self.hangover_frames
    }

    pub fn reset(&mut self) {
        self.silent_frames = 0;
    }
}

impl Default for SilenceDetector {
    fn default() -> Self {
        Self::new(SILENCE_THRESHOLD_DBFS, 200)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_conversion() {
        assert!((db_to_linear(-20.0) - 0.1).abs() < 1e-6);
        assert!((db_to_linear(-80.0) - 1e-4).abs() < 1e-9);
    }

    #[test]
    fn meter_peak_and_take() {
        let mut m = LevelMeter::new(100.0);
        m.process(&[0.1, -0.5, 0.2]);
        assert!((m.peak() - 0.5).abs() < 1e-6);
        assert!((m.take_peak() - 0.5).abs() < 1e-6);
        assert_eq!(m.take_peak(), 0.0);
        let zeros = vec![0.0; 4800];
        m.process_frames(&zeros, 4800);
        assert!(m.peak() < 0.5 * 0.4, "decays: {}", m.peak());
        assert!(m.peak() > 0.0);
    }

    #[test]
    fn silence_hangover_and_instant_resume() {
        let mut d = SilenceDetector::default();
        let z = vec![0.0f32; 480 * 2];
        let mut n = 0;
        while !d.update(&z, 480) {
            n += 1;
            assert!(n < 100);
        }
        // 200 ms = 9600 frames = 20 blocks of 480.
        assert_eq!(n + 1, 20);
        let mut loud = z.clone();
        loud[10] = 0.01;
        assert!(!d.update(&loud, 480));
        assert!(!d.is_suppressed());
        // -90 dBFS noise is still silence
        let quiet = vec![3e-5f32; 960];
        assert!(d.block_is_silent(&quiet));
    }
}
