//! Frame pacing helpers (plan 8.7): keep-alive frames when nothing changes and throttling of
//! on-demand IDR requests. Pure logic driven by an explicit millisecond clock.

/// Keep-alive interval when the picture does not change.
pub const KEEPALIVE_MS: u64 = 500;
/// Minimum spacing between two forced IDR frames (an IDR at 5120x1440 can be ~1 MB).
pub const IDR_MIN_INTERVAL_MS: u64 = 250;

/// Throttles keyframe requests: at most one IDR per `min_interval_ms`. A request that arrives too
/// early is remembered and granted by [`IdrThrottle::poll`] as soon as the interval has elapsed,
/// so bursts of `KeyframeRequest` collapse into one IDR but are never lost.
#[derive(Debug, Clone)]
pub struct IdrThrottle {
    min_interval_ms: u64,
    last_idr_ms: Option<u64>,
    pending: bool,
}

impl IdrThrottle {
    #[must_use]
    pub fn new(min_interval_ms: u64) -> Self {
        Self {
            min_interval_ms,
            last_idr_ms: None,
            pending: false,
        }
    }

    fn may_send(&self, now_ms: u64) -> bool {
        self.last_idr_ms
            .is_none_or(|t| now_ms.saturating_sub(t) >= self.min_interval_ms)
    }

    /// A receiver asked for a keyframe. Returns true if an IDR should be produced right now;
    /// otherwise the request is kept pending.
    pub fn request(&mut self, now_ms: u64) -> bool {
        if self.may_send(now_ms) {
            self.last_idr_ms = Some(now_ms);
            self.pending = false;
            true
        } else {
            self.pending = true;
            false
        }
    }

    /// Call once per encoder loop iteration: returns true if a deferred request is now due.
    pub fn poll(&mut self, now_ms: u64) -> bool {
        if self.pending && self.may_send(now_ms) {
            self.pending = false;
            self.last_idr_ms = Some(now_ms);
            true
        } else {
            false
        }
    }

    /// Records an IDR that was produced for another reason (stream start, scene reset).
    pub fn note_idr(&mut self, now_ms: u64) {
        self.last_idr_ms = Some(now_ms);
        self.pending = false;
    }

    #[must_use]
    pub fn has_pending(&self) -> bool {
        self.pending
    }
}

impl Default for IdrThrottle {
    fn default() -> Self {
        Self::new(IDR_MIN_INTERVAL_MS)
    }
}

/// Sender-side pacing state: tells the engine when a keep-alive packet is due and throttles IDRs.
#[derive(Debug, Clone)]
pub struct KeepAlive {
    timeout_ms: u64,
    last_sent_ms: Option<u64>,
    /// IDR throttling lives here so the sender loop has one pacing object.
    pub idr: IdrThrottle,
}

impl KeepAlive {
    #[must_use]
    pub fn new(timeout_ms: u64) -> Self {
        Self {
            timeout_ms,
            last_sent_ms: None,
            idr: IdrThrottle::default(),
        }
    }

    /// Any frame (real or keep-alive) went out.
    pub fn note_sent(&mut self, now_ms: u64) {
        self.last_sent_ms = Some(now_ms);
    }

    /// True if no frame was sent for `timeout_ms`. The caller then sends a keep-alive packet and
    /// calls [`KeepAlive::note_sent`]. Before the first frame nothing is due.
    #[must_use]
    pub fn keepalive_due(&self, now_ms: u64) -> bool {
        self.last_sent_ms
            .is_some_and(|t| now_ms.saturating_sub(t) >= self.timeout_ms)
    }

    /// Milliseconds until the next keep-alive is due (for a timer), `None` before the first frame.
    #[must_use]
    pub fn next_due_in_ms(&self, now_ms: u64) -> Option<u64> {
        self.last_sent_ms
            .map(|t| (t + self.timeout_ms).saturating_sub(now_ms))
    }
}

impl Default for KeepAlive {
    fn default() -> Self {
        Self::new(KEEPALIVE_MS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keepalive_fires_after_500ms_of_silence() {
        let mut k = KeepAlive::default();
        assert!(
            !k.keepalive_due(10_000),
            "nothing due before the first frame"
        );
        k.note_sent(1_000);
        assert!(!k.keepalive_due(1_499));
        assert!(k.keepalive_due(1_500));
        assert_eq!(k.next_due_in_ms(1_200), Some(300));
        assert_eq!(k.next_due_in_ms(2_000), Some(0));
        k.note_sent(1_500); // the keep-alive itself
        assert!(!k.keepalive_due(1_700));
        assert!(k.keepalive_due(2_000));
    }

    #[test]
    fn real_frames_postpone_keepalive() {
        let mut k = KeepAlive::default();
        for t in (0..2_000).step_by(100) {
            k.note_sent(t);
            assert!(!k.keepalive_due(t + 100));
        }
    }

    #[test]
    fn idr_is_throttled_to_one_per_250ms() {
        let mut t = IdrThrottle::default();
        assert!(t.request(1_000));
        assert!(!t.request(1_100));
        assert!(!t.request(1_200));
        assert!(t.has_pending());
        assert!(!t.poll(1_249));
        assert!(
            t.poll(1_250),
            "deferred request is granted when the interval is over"
        );
        assert!(!t.has_pending());
        assert!(!t.poll(1_300));
        // a request after a long pause is immediate again
        assert!(t.request(5_000));
    }

    #[test]
    fn burst_collapses_into_single_idr() {
        let mut t = IdrThrottle::default();
        let mut granted = 0;
        for ms in 0..200 {
            if t.request(10_000 + ms) {
                granted += 1;
            }
        }
        assert_eq!(granted, 1);
        assert!(t.poll(10_250));
        assert!(!t.poll(10_251));
    }

    #[test]
    fn note_idr_resets_pending() {
        let mut t = IdrThrottle::default();
        assert!(t.request(0));
        assert!(!t.request(10));
        t.note_idr(20);
        assert!(!t.has_pending());
        assert!(!t.request(200));
        assert!(t.poll(270));
    }
}
