//! Adaptive bitrate controller (plan 8.7): AIMD with hysteresis, pure logic.
//!
//! Feed it one [`AbrSample`] per measurement interval (about every 500 ms) together with a
//! monotonic millisecond clock; it returns the bitrate to configure on the encoder.
//!
//! * Congestion (loss, queueing delay seen as RTT inflation, late frames) -> multiplicative
//!   decrease, at most once per `decrease_cooldown_ms`.
//! * Several consecutive clean intervals -> additive increase (slow probing). After a decrease
//!   the number of clean intervals required grows (hysteresis against oscillation) and decays again.
//! * Encoder / decoder too slow for the frame budget: lowering the bitrate barely helps, so this
//!   is reported as an fps suggestion instead (and the bitrate is not driven further down).

/// Link class decides the allowed range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkClass {
    Wired,
    Wifi,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AbrConfig {
    pub min_kbps: u32,
    pub max_kbps: u32,
    pub start_kbps: u32,
    /// Multiplicative decrease factor on mild congestion.
    pub decrease_mild: f64,
    /// Multiplicative decrease factor on heavy congestion (loss above `heavy_loss_pct`).
    pub decrease_heavy: f64,
    /// Additive increase per step, as a fraction of `max_kbps`.
    pub increase_fraction: f64,
    pub loss_threshold_pct: f32,
    pub heavy_loss_pct: f32,
    /// RTT counts as congested above `base_rtt * rtt_factor + rtt_slack_ms`.
    pub rtt_factor: f32,
    pub rtt_slack_ms: f32,
    pub decrease_cooldown_ms: u64,
    pub increase_interval_ms: u64,
    /// Clean intervals required before the first increase.
    pub good_needed_base: u32,
    /// Upper bound of the hysteresis counter.
    pub good_needed_max: u32,
}

impl AbrConfig {
    #[must_use]
    pub fn for_link(link: LinkClass) -> Self {
        let (min_kbps, max_kbps, start_kbps) = match link {
            LinkClass::Wired => (40_000, 300_000, 120_000),
            LinkClass::Wifi => (8_000, 80_000, 40_000),
        };
        Self {
            min_kbps,
            max_kbps,
            start_kbps,
            decrease_mild: 0.85,
            decrease_heavy: 0.65,
            increase_fraction: 0.04,
            loss_threshold_pct: 0.5,
            heavy_loss_pct: 3.0,
            rtt_factor: 1.5,
            rtt_slack_ms: 4.0,
            decrease_cooldown_ms: 1_000,
            increase_interval_ms: 1_000,
            good_needed_base: 3,
            good_needed_max: 12,
        }
    }
}

/// One measurement interval.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AbrSample {
    /// Frames lost or dropped late by the receiver in the interval, percent.
    pub loss_pct: f32,
    /// Smoothed round-trip time in milliseconds (0 = unknown).
    pub rtt_ms: f32,
    /// Average encoder time per frame (sender), milliseconds.
    pub encode_ms: f32,
    /// Average decoder time per frame (receiver), milliseconds.
    pub decode_ms: f32,
    /// Frames per second currently being produced.
    pub fps: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbrReason {
    None,
    Loss,
    HeavyLoss,
    RttInflation,
    Increase,
    /// Encode or decode time exceeds the frame budget; bitrate unchanged, fps suggested.
    ComputeBound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbrDecision {
    pub bitrate_kbps: u32,
    /// True if `bitrate_kbps` differs from the previous decision (caller should reconfigure).
    pub changed: bool,
    /// Set when the pipeline cannot keep up with the frame budget (suggest e.g. 120 -> 60).
    pub suggest_fps: Option<u32>,
    pub reason: AbrReason,
}

#[derive(Debug, Clone)]
pub struct BitrateController {
    cfg: AbrConfig,
    kbps: f64,
    base_rtt_ms: Option<f32>,
    last_decrease_ms: Option<u64>,
    last_increase_ms: u64,
    good_streak: u32,
    good_needed: u32,
}

impl BitrateController {
    #[must_use]
    pub fn new(cfg: AbrConfig) -> Self {
        let kbps = f64::from(cfg.start_kbps.clamp(cfg.min_kbps, cfg.max_kbps));
        let good_needed = cfg.good_needed_base;
        Self {
            cfg,
            kbps,
            base_rtt_ms: None,
            last_decrease_ms: None,
            last_increase_ms: 0,
            good_streak: 0,
            good_needed,
        }
    }

    #[must_use]
    pub fn for_link(link: LinkClass) -> Self {
        Self::new(AbrConfig::for_link(link))
    }

    #[must_use]
    pub fn bitrate_kbps(&self) -> u32 {
        self.kbps.round() as u32
    }

    #[must_use]
    pub fn config(&self) -> &AbrConfig {
        &self.cfg
    }

    /// Lowest RTT seen so far, used as the queue-free baseline.
    #[must_use]
    pub fn base_rtt_ms(&self) -> Option<f32> {
        self.base_rtt_ms
    }

    /// Processes one interval. `now_ms` is any monotonic millisecond clock.
    pub fn update(&mut self, now_ms: u64, s: &AbrSample) -> AbrDecision {
        let before = self.bitrate_kbps();

        if s.rtt_ms > 0.0 {
            self.base_rtt_ms = Some(self.base_rtt_ms.map_or(s.rtt_ms, |b| b.min(s.rtt_ms)));
        }

        let budget_ms = if s.fps > 0 {
            1000.0 / s.fps as f32
        } else {
            f32::INFINITY
        };
        let compute_bound = s.encode_ms > budget_ms * 0.95 || s.decode_ms > budget_ms * 0.95;

        let rtt_congested = match (self.base_rtt_ms, s.rtt_ms > 0.0) {
            (Some(base), true) => s.rtt_ms > base * self.cfg.rtt_factor + self.cfg.rtt_slack_ms,
            _ => false,
        };
        let heavy = s.loss_pct >= self.cfg.heavy_loss_pct;
        let lossy = s.loss_pct >= self.cfg.loss_threshold_pct;
        let congested = heavy || lossy || rtt_congested;

        let mut reason = AbrReason::None;

        if congested {
            self.good_streak = 0;
            let cooled = self
                .last_decrease_ms
                .is_none_or(|t| now_ms.saturating_sub(t) >= self.cfg.decrease_cooldown_ms);
            if cooled {
                let factor = if heavy {
                    self.cfg.decrease_heavy
                } else {
                    self.cfg.decrease_mild
                };
                self.kbps = (self.kbps * factor).max(f64::from(self.cfg.min_kbps));
                self.last_decrease_ms = Some(now_ms);
                // Hysteresis: be more careful before probing upwards again.
                self.good_needed = (self.good_needed + 2).min(self.cfg.good_needed_max);
                reason = if heavy {
                    AbrReason::HeavyLoss
                } else if lossy {
                    AbrReason::Loss
                } else {
                    AbrReason::RttInflation
                };
            }
        } else {
            self.good_streak += 1;
            let ready = self.good_streak >= self.good_needed
                && now_ms.saturating_sub(self.last_increase_ms) >= self.cfg.increase_interval_ms
                && !compute_bound;
            if ready {
                let step = f64::from(self.cfg.max_kbps) * self.cfg.increase_fraction;
                self.kbps = (self.kbps + step).min(f64::from(self.cfg.max_kbps));
                self.last_increase_ms = now_ms;
                if self.kbps.round() as u32 != before {
                    reason = AbrReason::Increase;
                }
                // A long clean run relaxes the hysteresis again.
                if self.good_streak >= self.good_needed * 2 {
                    self.good_needed = self
                        .good_needed
                        .saturating_sub(1)
                        .max(self.cfg.good_needed_base);
                }
            }
        }

        let suggest_fps = if compute_bound && s.fps > 30 {
            if reason == AbrReason::None {
                reason = AbrReason::ComputeBound;
            }
            Some(next_lower_fps(s.fps))
        } else {
            None
        };

        let bitrate_kbps = self.bitrate_kbps();
        AbrDecision {
            bitrate_kbps,
            changed: bitrate_kbps != before,
            suggest_fps,
            reason,
        }
    }

    /// Sets the bitrate explicitly (e.g. after a negotiated start value or a `BitrateHint`).
    pub fn set_bitrate_kbps(&mut self, kbps: u32) {
        self.kbps = f64::from(kbps.clamp(self.cfg.min_kbps, self.cfg.max_kbps));
    }
}

fn next_lower_fps(fps: u32) -> u32 {
    match fps {
        0..=30 => fps,
        31..=60 => 30,
        61..=90 => 60,
        _ => 60,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean(fps: u32) -> AbrSample {
        AbrSample {
            loss_pct: 0.0,
            rtt_ms: 1.0,
            encode_ms: 3.0,
            decode_ms: 3.0,
            fps,
        }
    }

    #[test]
    fn starts_at_configured_value_and_clamps() {
        assert_eq!(
            BitrateController::for_link(LinkClass::Wired).bitrate_kbps(),
            120_000
        );
        assert_eq!(
            BitrateController::for_link(LinkClass::Wifi).bitrate_kbps(),
            40_000
        );
        let mut cfg = AbrConfig::for_link(LinkClass::Wired);
        cfg.start_kbps = 1;
        assert_eq!(
            BitrateController::new(cfg.clone()).bitrate_kbps(),
            cfg.min_kbps
        );
        cfg.start_kbps = 9_999_999;
        assert_eq!(
            BitrateController::new(cfg.clone()).bitrate_kbps(),
            cfg.max_kbps
        );
    }

    #[test]
    fn loss_decreases_multiplicatively_with_cooldown() {
        let mut c = BitrateController::for_link(LinkClass::Wired);
        let lossy = AbrSample {
            loss_pct: 1.0,
            ..clean(120)
        };
        let d = c.update(1_000, &lossy);
        assert!(d.changed);
        assert_eq!(d.reason, AbrReason::Loss);
        assert_eq!(d.bitrate_kbps, 102_000); // 120_000 * 0.85
        // within the cooldown: no second cut
        let d = c.update(1_500, &lossy);
        assert!(!d.changed);
        // after the cooldown: cut again
        let d = c.update(2_100, &lossy);
        assert!(d.changed);
        assert_eq!(d.bitrate_kbps, 86_700);
    }

    #[test]
    fn heavy_loss_cuts_harder() {
        let mut c = BitrateController::for_link(LinkClass::Wired);
        let d = c.update(
            1_000,
            &AbrSample {
                loss_pct: 10.0,
                ..clean(120)
            },
        );
        assert_eq!(d.reason, AbrReason::HeavyLoss);
        assert_eq!(d.bitrate_kbps, 78_000);
    }

    #[test]
    fn never_below_min_or_above_max() {
        let mut c = BitrateController::for_link(LinkClass::Wired);
        let mut t = 0;
        for _ in 0..100 {
            t += 1_100;
            c.update(
                t,
                &AbrSample {
                    loss_pct: 20.0,
                    ..clean(120)
                },
            );
        }
        assert_eq!(c.bitrate_kbps(), 40_000);
        for _ in 0..2_000 {
            t += 1_100;
            c.update(t, &clean(120));
        }
        assert_eq!(c.bitrate_kbps(), 300_000);
    }

    #[test]
    fn increases_only_after_good_streak() {
        let mut c = BitrateController::for_link(LinkClass::Wired);
        // start below max so there is room
        c.set_bitrate_kbps(100_000);
        let mut t = 0;
        // 2 clean intervals: not yet (needs 3)
        for _ in 0..2 {
            t += 1_000;
            let d = c.update(t, &clean(120));
            assert!(!d.changed);
        }
        t += 1_000;
        let d = c.update(t, &clean(120));
        assert!(d.changed);
        assert_eq!(d.reason, AbrReason::Increase);
        assert_eq!(d.bitrate_kbps, 112_000); // + 4 % of 300_000
    }

    #[test]
    fn hysteresis_requires_longer_streak_after_a_cut() {
        let mut c = BitrateController::for_link(LinkClass::Wired);
        c.update(
            1_000,
            &AbrSample {
                loss_pct: 2.0,
                ..clean(120)
            },
        );
        let after_cut = c.bitrate_kbps();
        // good_needed is now 5: four clean seconds are not enough
        let mut t = 1_000;
        for _ in 0..4 {
            t += 1_000;
            c.update(t, &clean(120));
        }
        assert_eq!(c.bitrate_kbps(), after_cut);
        t += 1_000;
        c.update(t, &clean(120));
        assert!(c.bitrate_kbps() > after_cut);
    }

    #[test]
    fn rtt_inflation_is_congestion() {
        let mut c = BitrateController::for_link(LinkClass::Wired);
        // establish baseline 1 ms
        c.update(500, &clean(120));
        assert_eq!(c.base_rtt_ms(), Some(1.0));
        // 4 ms is within 1*1.5 + 4 = 5.5
        let d = c.update(
            1_000,
            &AbrSample {
                rtt_ms: 4.0,
                ..clean(120)
            },
        );
        assert!(!d.changed);
        let d = c.update(
            1_600,
            &AbrSample {
                rtt_ms: 12.0,
                ..clean(120)
            },
        );
        assert!(d.changed);
        assert_eq!(d.reason, AbrReason::RttInflation);
    }

    #[test]
    fn unknown_rtt_is_ignored() {
        let mut c = BitrateController::for_link(LinkClass::Wired);
        let d = c.update(
            1_000,
            &AbrSample {
                rtt_ms: 0.0,
                ..clean(120)
            },
        );
        assert!(!d.changed);
        assert_eq!(c.base_rtt_ms(), None);
    }

    #[test]
    fn compute_bound_suggests_fps_and_blocks_increase() {
        let mut c = BitrateController::for_link(LinkClass::Wired);
        c.set_bitrate_kbps(100_000);
        // encode 9 ms > 8.33 ms budget at 120 fps
        let slow = AbrSample {
            encode_ms: 9.0,
            ..clean(120)
        };
        let mut t = 0;
        let mut suggested = None;
        for _ in 0..10 {
            t += 1_000;
            let d = c.update(t, &slow);
            assert!(!d.changed, "no bitrate change while compute bound");
            suggested = d.suggest_fps.or(suggested);
        }
        assert_eq!(suggested, Some(60));
        // at 60 fps the budget is 16.6 ms: fine
        let d = c.update(
            t + 1_000,
            &AbrSample {
                encode_ms: 9.0,
                ..clean(60)
            },
        );
        assert_eq!(d.suggest_fps, None);
    }

    #[test]
    fn decode_time_counts_too() {
        let mut c = BitrateController::for_link(LinkClass::Wired);
        let d = c.update(
            1_000,
            &AbrSample {
                decode_ms: 12.0,
                ..clean(120)
            },
        );
        assert_eq!(d.suggest_fps, Some(60));
        assert_eq!(d.reason, AbrReason::ComputeBound);
    }

    #[test]
    fn wifi_has_lower_caps() {
        let mut c = BitrateController::for_link(LinkClass::Wifi);
        let mut t = 0;
        for _ in 0..3_000 {
            t += 1_100;
            c.update(t, &clean(60));
        }
        assert_eq!(c.bitrate_kbps(), 80_000);
        for _ in 0..100 {
            t += 1_100;
            c.update(
                t,
                &AbrSample {
                    loss_pct: 30.0,
                    ..clean(60)
                },
            );
        }
        assert_eq!(c.bitrate_kbps(), 8_000);
    }

    #[test]
    fn synthetic_trace_recovers_and_does_not_oscillate_wildly() {
        // 0-20 s clean, 20-25 s lossy, 25-120 s clean. Count the direction changes.
        let mut c = BitrateController::for_link(LinkClass::Wired);
        let mut t = 0;
        let mut series = Vec::new();
        while t < 120_000 {
            t += 500;
            let lossy = (20_000..25_000).contains(&t);
            let s = if lossy {
                AbrSample {
                    loss_pct: 2.0,
                    ..clean(120)
                }
            } else {
                clean(120)
            };
            c.update(t, &s);
            series.push((t, c.bitrate_kbps()));
        }
        let at = |ms: u64| series.iter().find(|(t, _)| *t == ms).unwrap().1;
        assert!(at(19_500) > at(25_000), "loss reduced the rate");
        assert!(at(120_000) > at(25_000), "rate recovered afterwards");
        // After the loss window only increases (no sawtooth without congestion).
        let post: Vec<u32> = series
            .iter()
            .filter(|(t, _)| *t > 25_000)
            .map(|(_, k)| *k)
            .collect();
        assert!(post.windows(2).all(|w| w[1] >= w[0]));
        // During the 5 s window the number of cuts is bounded by the cooldown (1/s).
        let cuts = series
            .windows(2)
            .filter(|w| w[0].0 >= 20_000 && w[1].0 <= 25_000 && w[1].1 < w[0].1)
            .count();
        assert!(cuts <= 5, "cuts={cuts}");
    }
}
