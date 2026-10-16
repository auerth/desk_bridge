//! Shared screen layout.
//!
//! Every screen of both devices is a rectangle in one *layout space*. Sizes are normalised to
//! 96 dpi (`px * 96 / dpi`) so that a 4K laptop at 150 % and a 1440p monitor at 100 % line up in a
//! natural way. When the cursor crosses an edge, its *relative position* (0…1) along the shared
//! segment is mapped onto the neighbouring screen, so differing resolutions feel right.

use deskbridge_platform::screen::ScreenInfo;
use deskbridge_proto::pb::{self, Edge, Placement};
use uuid::Uuid;

/// A cursor that reaches an edge this close (layout units, about one inch) to the neighbour's
/// span still switches and is mapped to the nearest point, so nobody has to hunt for the exact
/// spot along the edge.
pub const SNAP: f64 = 96.0;

/// Two edges closer than this (layout units) count as touching.
pub const TOUCH_EPS: f64 = 2.0;

#[derive(Debug, Clone, PartialEq)]
pub struct LScreen {
    pub device: Uuid,
    pub screen: u32,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// Native pixel size (for mapping back to pixels).
    pub px_w: u32,
    pub px_h: u32,
}

impl LScreen {
    #[must_use]
    pub fn right(&self) -> f64 {
        self.x + self.w
    }

    #[must_use]
    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }
}

/// Neighbour found when crossing an edge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hit {
    pub device: Uuid,
    pub screen: u32,
    /// Position along the edge of the *target* screen (0…1).
    pub rel: f64,
    /// Edge of the target screen the cursor enters through.
    pub entry_edge: Edge,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Layout {
    pub screens: Vec<LScreen>,
    pub revision: u64,
}

/// Opposite edge.
#[must_use]
pub fn opposite(e: Edge) -> Edge {
    match e {
        Edge::Left => Edge::Right,
        Edge::Right => Edge::Left,
        Edge::Top => Edge::Bottom,
        Edge::Bottom => Edge::Top,
        Edge::Unspecified => Edge::Unspecified,
    }
}

/// Size of a screen in layout units.
#[must_use]
pub fn layout_size(s: &ScreenInfo) -> (f64, f64) {
    let dpi = f64::from(s.dpi.max(48));
    (f64::from(s.w) * 96.0 / dpi, f64::from(s.h) * 96.0 / dpi)
}

impl Layout {
    /// Builds a layout from the devices' screens and the placement list. Screens without a
    /// placement are skipped.
    #[must_use]
    pub fn from_parts(
        devices: &[(Uuid, Vec<ScreenInfo>)],
        placements: &[Placement],
        revision: u64,
    ) -> Self {
        let mut screens = Vec::new();
        for (dev, list) in devices {
            for s in list {
                let Some(p) = placements
                    .iter()
                    .find(|p| p.device_id == dev.to_string() && p.screen_id == s.id)
                else {
                    continue;
                };
                let (w, h) = layout_size(s);
                screens.push(LScreen {
                    device: *dev,
                    screen: s.id,
                    x: f64::from(p.x),
                    y: f64::from(p.y),
                    w,
                    h,
                    px_w: s.w,
                    px_h: s.h,
                });
            }
        }
        Self { screens, revision }
    }

    /// Default arrangement: the device's own screens keep their native arrangement; the remote
    /// device's primary screen sits to the `side` of the local virtual desktop, vertically
    /// centred on the local primary screen.
    #[must_use]
    pub fn default_placements(
        local: &(Uuid, Vec<ScreenInfo>),
        remote: &(Uuid, Vec<ScreenInfo>),
        side: Edge,
    ) -> Vec<Placement> {
        let mut out = Vec::new();
        let mut min_x = f64::MAX;
        let mut min_y = f64::MAX;
        let mut max_x = f64::MIN;
        let mut max_y = f64::MIN;
        let mut prim = None;
        for s in &local.1 {
            let (w, h) = layout_size(s);
            let (x, y) = (
                f64::from(s.x) * 96.0 / f64::from(s.dpi.max(48)),
                f64::from(s.y) * 96.0 / f64::from(s.dpi.max(48)),
            );
            out.push(Placement {
                device_id: local.0.to_string(),
                screen_id: s.id,
                x: x.round() as i32,
                y: y.round() as i32,
            });
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x + w);
            max_y = max_y.max(y + h);
            if s.primary || prim.is_none() {
                prim = Some((x, y, w, h));
            }
        }
        let (px, py, pw, ph) = prim.unwrap_or((0.0, 0.0, 1920.0, 1080.0));
        for (i, s) in remote.1.iter().enumerate() {
            let (w, h) = layout_size(s);
            // Only the primary (first) remote screen gets docked; others stack after it.
            let (x, y) = match side {
                Edge::Left => (min_x - w, py + (ph - h) / 2.0),
                Edge::Top => (px + (pw - w) / 2.0, min_y - h),
                Edge::Bottom => (px + (pw - w) / 2.0, max_y),
                _ => (max_x, py + (ph - h) / 2.0),
            };
            let shift = i as f64 * 64.0;
            out.push(Placement {
                device_id: remote.0.to_string(),
                screen_id: s.id,
                x: (x + shift).round() as i32,
                y: (y + shift).round() as i32,
            });
        }
        out
    }

    #[must_use]
    pub fn find(&self, device: Uuid, screen: u32) -> Option<&LScreen> {
        self.screens
            .iter()
            .find(|s| s.device == device && s.screen == screen)
    }

    /// Looks for a screen touching the given edge of `from` at relative position `rel` (0…1 along
    /// the edge). Returns the neighbour and the mapped position on its entry edge.
    #[must_use]
    pub fn neighbor(&self, from: (Uuid, u32), edge: Edge, rel: f64) -> Option<Hit> {
        let src = self.find(from.0, from.1)?;
        let rel = rel.clamp(0.0, 1.0);
        // Point in layout space on the source edge.
        let (px, py) = match edge {
            Edge::Left => (src.x, src.y + rel * src.h),
            Edge::Right => (src.right(), src.y + rel * src.h),
            Edge::Top => (src.x + rel * src.w, src.y),
            Edge::Bottom => (src.x + rel * src.w, src.bottom()),
            Edge::Unspecified => return None,
        };
        let mut best: Option<(f64, Hit)> = None;
        for t in &self.screens {
            if t.device == src.device && t.screen == src.screen {
                continue;
            }
            let (touch, lo, hi, start, len, entry) = match edge {
                Edge::Right => (
                    (t.x - src.right()).abs() <= TOUCH_EPS,
                    t.y,
                    t.bottom(),
                    t.y,
                    t.h,
                    Edge::Left,
                ),
                Edge::Left => (
                    (t.right() - src.x).abs() <= TOUCH_EPS,
                    t.y,
                    t.bottom(),
                    t.y,
                    t.h,
                    Edge::Right,
                ),
                Edge::Bottom => (
                    (t.y - src.bottom()).abs() <= TOUCH_EPS,
                    t.x,
                    t.right(),
                    t.x,
                    t.w,
                    Edge::Top,
                ),
                Edge::Top => (
                    (t.bottom() - src.y).abs() <= TOUCH_EPS,
                    t.x,
                    t.right(),
                    t.x,
                    t.w,
                    Edge::Bottom,
                ),
                Edge::Unspecified => return None,
            };
            if !touch {
                continue;
            }
            let along = match edge {
                Edge::Left | Edge::Right => py,
                _ => px,
            };
            let dist = if along < lo {
                lo - along
            } else if along > hi {
                along - hi
            } else {
                0.0
            };
            if dist <= SNAP {
                let rel_t = ((along - start) / len).clamp(0.0, 1.0);
                let hit = Hit {
                    device: t.device,
                    screen: t.screen,
                    rel: rel_t,
                    entry_edge: entry,
                };
                if dist == 0.0 {
                    return Some(hit);
                }
                if best.as_ref().is_none_or(|(d, _)| dist < *d) {
                    best = Some((dist, hit));
                }
            }
        }
        best.map(|(_, h)| h)
    }

    /// Wire representation for `LayoutUpdate`.
    #[must_use]
    pub fn to_placements(&self) -> Vec<Placement> {
        self.screens
            .iter()
            .map(|s| Placement {
                device_id: s.device.to_string(),
                screen_id: s.screen,
                x: s.x.round() as i32,
                y: s.y.round() as i32,
            })
            .collect()
    }

    #[must_use]
    pub fn to_update(&self) -> pb::LayoutUpdate {
        pb::LayoutUpdate {
            placements: self.to_placements(),
            revision: self.revision,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deskbridge_platform::mock::screen;

    fn ids() -> (Uuid, Uuid) {
        (Uuid::from_u128(1), Uuid::from_u128(2))
    }

    fn setup(laptop_dpi: u32, laptop_w: u32, laptop_h: u32, side: Edge) -> (Layout, Uuid, Uuid) {
        let (pc, lp) = ids();
        let mut pc_screen = screen(0, 0, 0, 5120, 1440);
        pc_screen.primary = true;
        let mut lp_screen = screen(0, 0, 0, laptop_w, laptop_h);
        lp_screen.dpi = laptop_dpi;
        let local = (pc, vec![pc_screen]);
        let remote = (lp, vec![lp_screen]);
        let placements = Layout::default_placements(&local, &remote, side);
        (Layout::from_parts(&[local, remote], &placements, 1), pc, lp)
    }

    #[test]
    fn right_edge_maps_to_laptop_left_edge() {
        let (l, pc, lp) = setup(96, 1920, 1080, Edge::Right);
        let hit = l.neighbor((pc, 0), Edge::Right, 0.5).unwrap();
        assert_eq!(hit.device, lp);
        assert_eq!(hit.entry_edge, Edge::Left);
        assert!((hit.rel - 0.5).abs() < 0.01, "centred");
    }

    #[test]
    fn rel_position_follows_different_heights() {
        let (l, pc, lp) = setup(96, 1920, 1080, Edge::Right);
        // Laptop (1080 high) is centred next to the 1440 high PC screen: top of laptop = 180.
        // PC y = 180 is the laptop's rel 0; y = 1260 is rel 1.
        let top = l.neighbor((pc, 0), Edge::Right, 180.0 / 1440.0).unwrap();
        assert!(top.rel < 0.01);
        let bottom = l.neighbor((pc, 0), Edge::Right, 1260.0 / 1440.0).unwrap();
        assert!(bottom.rel > 0.99);
        // Above the laptop: nothing is there, the cursor is blocked.
        assert!(l.neighbor((pc, 0), Edge::Right, 0.0).is_none());
        // close to the span the cursor snaps to its nearest point
        let snapped = l.neighbor((pc, 0), Edge::Right, 0.1).unwrap();
        assert!(snapped.rel < 0.01);
        let _ = lp;
    }

    #[test]
    fn dpi_normalisation() {
        // 2880x1800 @ 192 dpi is 1440x900 layout units.
        let (l, _, lp) = setup(192, 2880, 1800, Edge::Right);
        let s = l.find(lp, 0).unwrap();
        assert!((s.w - 1440.0).abs() < 0.01);
        assert!((s.h - 900.0).abs() < 0.01);
    }

    #[test]
    fn return_direction_works() {
        let (l, pc, lp) = setup(96, 1920, 1080, Edge::Right);
        let back = l.neighbor((lp, 0), Edge::Left, 0.5).unwrap();
        assert_eq!(back.device, pc);
        assert_eq!(back.entry_edge, Edge::Right);
        assert!((back.rel - 0.5).abs() < 0.01);
    }

    #[test]
    fn other_sides() {
        let (l, pc, lp) = setup(96, 1920, 1080, Edge::Left);
        assert_eq!(l.neighbor((pc, 0), Edge::Left, 0.5).unwrap().device, lp);
        assert!(l.neighbor((pc, 0), Edge::Right, 0.5).is_none());
        let (l, pc, lp) = setup(96, 1920, 1080, Edge::Bottom);
        assert_eq!(l.neighbor((pc, 0), Edge::Bottom, 0.5).unwrap().device, lp);
    }
}
