//! Exponential reconnect backoff (0.5 s … 10 s).

use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Backoff {
    current: Duration,
    min: Duration,
    max: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new(Duration::from_millis(500), Duration::from_secs(10))
    }
}

impl Backoff {
    #[must_use]
    pub fn new(min: Duration, max: Duration) -> Self {
        Self { current: min, min, max }
    }

    /// Returns the delay to wait now and advances the sequence.
    pub fn next_delay(&mut self) -> Duration {
        let d = self.current;
        self.current = (self.current.mul_f32(1.7)).min(self.max);
        d
    }

    pub fn reset(&mut self) {
        self.current = self.min;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grows_and_caps() {
        let mut b = Backoff::default();
        let first = b.next_delay();
        assert_eq!(first, Duration::from_millis(500));
        let mut last = first;
        for _ in 0..20 {
            let d = b.next_delay();
            assert!(d >= last);
            assert!(d <= Duration::from_secs(10));
            last = d;
        }
        assert_eq!(last, Duration::from_secs(10));
        b.reset();
        assert_eq!(b.next_delay(), Duration::from_millis(500));
    }
}
