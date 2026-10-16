//! Mode planning for focus mode (plan 8.7 "Split-Modus", 8.8): given the PC target monitor's
//! current mode and the requested layout, compute the virtual display mode of the laptop, the
//! region the viewer window occupies on the PC monitor, and the mode list for the driver config.
//!
//! Pure logic, no OS access.

use deskbridge_platform::display::Mode;
use serde::{Deserialize, Serialize};

/// Smallest/largest dimensions we are willing to plan (sanity bounds, not driver limits).
const MIN_DIM: u32 = 160;
const MAX_DIM: u32 = 16_384;
const MAX_HZ: u32 = 1_000;
/// Mode that always exists as a safe fallback in the driver configuration.
pub const SAFE_MODE: Mode = Mode {
    width: 1920,
    height: 1080,
    hz: 60,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Side {
    Left,
    Right,
}

/// Share of the PC monitor's width that the laptop occupies in split mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SplitShare {
    /// 50 %
    Half,
    /// 67 % (2/3)
    TwoThirds,
    /// 33 % (1/3)
    OneThird,
}

impl SplitShare {
    pub const ALL: [SplitShare; 3] = [Self::Half, Self::TwoThirds, Self::OneThird];

    /// Accepts 50, 67/66 and 33/34 (the UI labels).
    #[must_use]
    pub fn from_percent(p: u32) -> Option<Self> {
        match p {
            50 => Some(Self::Half),
            66 | 67 => Some(Self::TwoThirds),
            33 | 34 => Some(Self::OneThird),
            _ => None,
        }
    }

    #[must_use]
    pub fn percent(self) -> u32 {
        match self {
            Self::Half => 50,
            Self::TwoThirds => 67,
            Self::OneThird => 33,
        }
    }

    const fn fraction(self) -> (u64, u64) {
        match self {
            Self::Half => (1, 2),
            Self::TwoThirds => (2, 3),
            Self::OneThird => (1, 3),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FocusLayout {
    /// The laptop fills the whole PC monitor.
    Full,
    /// The laptop occupies `share` of the PC monitor on `side`; the PC desktop keeps the rest.
    Split { side: Side, share: SplitShare },
}

/// Rectangle in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    #[must_use]
    pub fn right(&self) -> i32 {
        self.x + self.w as i32
    }

    /// The same rectangle translated by the monitor origin.
    #[must_use]
    pub fn offset(&self, dx: i32, dy: i32) -> Rect {
        Rect {
            x: self.x + dx,
            y: self.y + dy,
            ..*self
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PlanOptions {
    /// Round split widths to even numbers (3414 + 1706 instead of 3413 + 1707). Needed when the
    /// driver or the video encoder rejects odd widths (ADR 0004: untested on hardware).
    pub even_widths: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    #[error("invalid monitor mode {0}x{1}@{2}")]
    InvalidMode(u32, u32, u32),
    #[error("split width {0} is too small")]
    TooNarrow(u32),
}

/// Result of [`plan_focus`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FocusPlan {
    pub layout: FocusLayout,
    /// The PC monitor's mode the plan was made for.
    pub pc_mode: Mode,
    /// Mode of the virtual display on the laptop.
    pub virtual_mode: Mode,
    /// Where the viewer window sits, relative to the PC monitor's top-left corner.
    pub viewer_region: Rect,
    /// The part of the monitor left to the PC desktop (`None` in full mode).
    pub pc_region: Option<Rect>,
    /// All modes to write to the driver configuration (superset of `virtual_mode`).
    pub driver_modes: Vec<Mode>,
}

impl FocusPlan {
    /// Viewer rectangle in virtual-desktop coordinates for a monitor at `(mx, my)`.
    #[must_use]
    pub fn viewer_rect_on(&self, mx: i32, my: i32) -> Rect {
        self.viewer_region.offset(mx, my)
    }
}

/// `round(a / b)` for positive integers, halves rounded up.
const fn round_div(a: u64, b: u64) -> u64 {
    (2 * a + b) / (2 * b)
}

/// Width of the laptop part for `share` of `pc_width`.
///
/// 5120 px: 50 % = 2560, 67 % = 3413 (3414 even), 33 % = 1707 (1706 even). The complement of
/// the 67 % part is the 33 % part in both variants (they sum to the monitor width).
#[must_use]
pub fn split_width(pc_width: u32, share: SplitShare, even: bool) -> u32 {
    let (n, d) = share.fraction();
    let w = u64::from(pc_width);
    let v = if even {
        2 * round_div(w * n, 2 * d)
    } else {
        round_div(w * n, d)
    };
    v as u32
}

fn validate(m: Mode) -> Result<(), PlanError> {
    let ok = (MIN_DIM..=MAX_DIM).contains(&m.width)
        && (MIN_DIM..=MAX_DIM).contains(&m.height)
        && (1..=MAX_HZ).contains(&m.hz);
    if ok {
        Ok(())
    } else {
        Err(PlanError::InvalidMode(m.width, m.height, m.hz))
    }
}

/// Computes the plan for `layout` on a PC monitor currently running `pc_mode`.
pub fn plan_focus(
    pc_mode: Mode,
    layout: FocusLayout,
    opts: PlanOptions,
) -> Result<FocusPlan, PlanError> {
    validate(pc_mode)?;
    let h = pc_mode.height;
    let (virtual_w, region, pc_region) = match layout {
        FocusLayout::Full => (
            pc_mode.width,
            Rect {
                x: 0,
                y: 0,
                w: pc_mode.width,
                h,
            },
            None,
        ),
        FocusLayout::Split { side, share } => {
            let w = split_width(pc_mode.width, share, opts.even_widths);
            if w < MIN_DIM || w >= pc_mode.width {
                return Err(PlanError::TooNarrow(w));
            }
            let rest = pc_mode.width - w;
            let (vx, px) = match side {
                Side::Left => (0, w as i32),
                Side::Right => (rest as i32, 0),
            };
            (
                w,
                Rect { x: vx, y: 0, w, h },
                Some(Rect {
                    x: px,
                    y: 0,
                    w: rest,
                    h,
                }),
            )
        }
    };
    Ok(FocusPlan {
        layout,
        pc_mode,
        virtual_mode: Mode {
            width: virtual_w,
            height: h,
            hz: pc_mode.hz,
        },
        viewer_region: region,
        pc_region,
        driver_modes: driver_modes(pc_mode, opts),
    })
}

/// Modes to publish in the driver config for a PC monitor mode: the full-size modes at 120/60
/// (and the monitor's own rate), every split variant, plus the 1920x1080@60 safe mode.
///
/// Both the odd and the even split widths are included regardless of `opts`, so switching
/// `even_widths` later needs no driver reload. The list stays well below the ~100 mode limit
/// reported for the driver (ADR 0004).
#[must_use]
pub fn driver_modes(pc_mode: Mode, _opts: PlanOptions) -> Vec<Mode> {
    let (w, h, hz) = (pc_mode.width, pc_mode.height, pc_mode.hz);
    let mut out: Vec<Mode> = Vec::new();
    let mut push = |width: u32, rates: &[u32]| {
        for &r in rates {
            let m = Mode {
                width,
                height: h,
                hz: r,
            };
            if r > 0 && !out.contains(&m) {
                out.push(m);
            }
        }
    };
    // Full size: the monitor's rate first so it is the driver's preferred/first mode.
    push(w, &[hz, 120, 60]);
    for share in SplitShare::ALL {
        for even in [false, true] {
            let sw = split_width(w, share, even);
            if sw < MIN_DIM || sw >= w {
                continue;
            }
            if share == SplitShare::Half {
                push(sw, &[hz, 60]);
            } else {
                push(sw, &[hz]);
            }
        }
    }
    if !out.contains(&SAFE_MODE) {
        out.push(SAFE_MODE);
    }
    out
}

/// Parses `5120x1440@120` (also `5120×1440@120`, the rate may be omitted -> 60).
pub fn parse_mode(s: &str) -> Option<Mode> {
    let s = s.trim().replace('×', "x");
    let (dims, hz) = match s.split_once('@') {
        Some((d, r)) => (d, r.trim().parse::<u32>().ok()?),
        None => (s.as_str(), 60),
    };
    let (w, h) = dims.split_once(['x', 'X'])?;
    Some(Mode {
        width: w.trim().parse().ok()?,
        height: h.trim().parse().ok()?,
        hz,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const UW: Mode = Mode {
        width: 5120,
        height: 1440,
        hz: 120,
    };

    fn m(w: u32, h: u32, hz: u32) -> Mode {
        Mode {
            width: w,
            height: h,
            hz,
        }
    }

    #[test]
    fn full_layout_keeps_the_monitor_mode() {
        let p = plan_focus(UW, FocusLayout::Full, PlanOptions::default()).unwrap();
        assert_eq!(p.virtual_mode, UW);
        assert_eq!(
            p.viewer_region,
            Rect {
                x: 0,
                y: 0,
                w: 5120,
                h: 1440
            }
        );
        assert!(p.pc_region.is_none());
    }

    #[test]
    fn split_widths_match_the_adr() {
        assert_eq!(split_width(5120, SplitShare::Half, false), 2560);
        assert_eq!(split_width(5120, SplitShare::TwoThirds, false), 3413);
        assert_eq!(split_width(5120, SplitShare::OneThird, false), 1707);
        assert_eq!(split_width(5120, SplitShare::TwoThirds, true), 3414);
        assert_eq!(split_width(5120, SplitShare::OneThird, true), 1706);
        assert_eq!(split_width(5120, SplitShare::Half, true), 2560);
        // the two parts always add up to the monitor width
        for even in [false, true] {
            assert_eq!(
                split_width(5120, SplitShare::TwoThirds, even)
                    + split_width(5120, SplitShare::OneThird, even),
                5120,
                "even={even}"
            );
        }
    }

    #[test]
    fn split_left_and_right_regions() {
        let l = plan_focus(
            UW,
            FocusLayout::Split {
                side: Side::Left,
                share: SplitShare::Half,
            },
            PlanOptions::default(),
        )
        .unwrap();
        assert_eq!(l.virtual_mode, m(2560, 1440, 120));
        assert_eq!((l.viewer_region.x, l.viewer_region.w), (0, 2560));
        assert_eq!(l.pc_region.unwrap().x, 2560);

        let r = plan_focus(
            UW,
            FocusLayout::Split {
                side: Side::Right,
                share: SplitShare::TwoThirds,
            },
            PlanOptions::default(),
        )
        .unwrap();
        assert_eq!(r.virtual_mode, m(3413, 1440, 120));
        assert_eq!((r.viewer_region.x, r.viewer_region.w), (1707, 3413));
        let pc = r.pc_region.unwrap();
        assert_eq!((pc.x, pc.w), (0, 1707));
        // regions tile the monitor without gap or overlap
        assert_eq!(pc.right(), r.viewer_region.x);
        assert_eq!(r.viewer_region.right(), 5120);
        // absolute position on a monitor at x=-5120
        assert_eq!(r.viewer_rect_on(-5120, 0).x, -5120 + 1707);
    }

    #[test]
    fn even_widths_option() {
        let p = plan_focus(
            UW,
            FocusLayout::Split {
                side: Side::Left,
                share: SplitShare::TwoThirds,
            },
            PlanOptions { even_widths: true },
        )
        .unwrap();
        assert_eq!(p.virtual_mode.width, 3414);
        assert_eq!(p.pc_region.unwrap().w, 1706);
        let one = plan_focus(
            UW,
            FocusLayout::Split {
                side: Side::Right,
                share: SplitShare::OneThird,
            },
            PlanOptions { even_widths: true },
        )
        .unwrap();
        assert_eq!(one.virtual_mode.width, 1706);
        assert_eq!(one.viewer_region.x, 5120 - 1706);
    }

    #[test]
    fn driver_modes_cover_all_variants() {
        let modes = driver_modes(UW, PlanOptions::default());
        for want in [
            m(5120, 1440, 120),
            m(5120, 1440, 60),
            m(2560, 1440, 120),
            m(2560, 1440, 60),
            m(3413, 1440, 120),
            m(3414, 1440, 120),
            m(1707, 1440, 120),
            m(1706, 1440, 120),
            SAFE_MODE,
        ] {
            assert!(modes.contains(&want), "missing {want:?}");
        }
        // no duplicates, short list, monitor mode first
        let mut dedup = modes.clone();
        dedup.dedup();
        assert_eq!(dedup.len(), modes.len());
        assert!(modes.len() < 20);
        assert_eq!(modes[0], UW);
        // every plan's virtual mode is published
        for share in SplitShare::ALL {
            for side in [Side::Left, Side::Right] {
                for even in [false, true] {
                    let p = plan_focus(
                        UW,
                        FocusLayout::Split { side, share },
                        PlanOptions { even_widths: even },
                    )
                    .unwrap();
                    assert!(p.driver_modes.contains(&p.virtual_mode));
                }
            }
        }
    }

    #[test]
    fn other_monitor_rates_are_included() {
        let modes = driver_modes(m(3440, 1440, 144), PlanOptions::default());
        assert!(modes.contains(&m(3440, 1440, 144)));
        assert!(modes.contains(&m(3440, 1440, 120)));
        assert!(modes.contains(&m(3440, 1440, 60)));
        assert!(modes.contains(&m(1720, 1440, 144)));
    }

    #[test]
    fn rejects_bad_input() {
        assert!(plan_focus(m(0, 1440, 120), FocusLayout::Full, PlanOptions::default()).is_err());
        assert!(plan_focus(m(5120, 1440, 0), FocusLayout::Full, PlanOptions::default()).is_err());
        let tiny = plan_focus(
            m(320, 240, 60),
            FocusLayout::Split {
                side: Side::Left,
                share: SplitShare::OneThird,
            },
            PlanOptions::default(),
        );
        assert!(matches!(tiny, Err(PlanError::TooNarrow(_))));
    }

    #[test]
    fn mode_and_percent_parsing() {
        assert_eq!(parse_mode("5120x1440@120"), Some(UW));
        assert_eq!(parse_mode("5120×1440@120"), Some(UW));
        assert_eq!(parse_mode("1920x1080"), Some(m(1920, 1080, 60)));
        assert_eq!(parse_mode("nonsense"), None);
        assert_eq!(SplitShare::from_percent(67), Some(SplitShare::TwoThirds));
        assert_eq!(SplitShare::from_percent(50).unwrap().percent(), 50);
        assert_eq!(SplitShare::from_percent(10), None);
    }
}
