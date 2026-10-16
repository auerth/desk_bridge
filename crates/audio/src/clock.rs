//! Time source abstraction so the receiver can be driven by a simulated clock in tests.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Monotonic microsecond clock.
pub trait Clock: Send + Sync {
    fn now_us(&self) -> u64;
}

/// Real-time clock based on [`Instant`].
#[derive(Debug)]
pub struct SystemClock {
    start: Instant,
}

impl SystemClock {
    #[must_use]
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now_us(&self) -> u64 {
        self.start.elapsed().as_micros() as u64
    }
}

/// Manually driven clock (tests, simulations).
#[derive(Debug, Default)]
pub struct ManualClock {
    us: AtomicU64,
}

impl ManualClock {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_us(&self, us: u64) {
        self.us.store(us, Ordering::Relaxed);
    }

    pub fn advance_us(&self, us: u64) {
        self.us.fetch_add(us, Ordering::Relaxed);
    }
}

impl Clock for ManualClock {
    fn now_us(&self) -> u64 {
        self.us.load(Ordering::Relaxed)
    }
}
