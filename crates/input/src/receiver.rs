//! Target side: injects incoming input and detects the return edge.
//!
//! Injection only happens between `Enter` and `Leave`. `Leave`, `ReleaseAll`, a stream error and
//! a disconnect all call `release_all()`, so keys can never get stuck.

use crate::layout::Layout;
use deskbridge_platform::hid;
use deskbridge_platform::input::{Button, InputInjector};
use deskbridge_platform::screen::{Point, ScreenInfo, ScreenProvider};
use deskbridge_proto::pb::{self, Edge, input_event::Ev};
use std::sync::Arc;
use uuid::Uuid;

pub struct Receiver {
    own: Uuid,
    source: Uuid,
    injector: Arc<dyn InputInjector>,
    screens: Arc<dyn ScreenProvider>,
    layout: Layout,
    active: Option<Active>,
}

struct Active {
    /// Edge hit already reported (re-armed when the cursor moves away from the edge).
    edge_reported: bool,
}

impl Receiver {
    #[must_use]
    pub fn new(
        own: Uuid,
        source: Uuid,
        injector: Arc<dyn InputInjector>,
        screens: Arc<dyn ScreenProvider>,
    ) -> Self {
        Self {
            own,
            source,
            injector,
            screens,
            layout: Layout::default(),
            active: None,
        }
    }

    pub fn set_layout(&mut self, layout: Layout) {
        self.layout = layout;
    }

    #[must_use]
    pub fn is_active(&self) -> bool {
        self.active.is_some()
    }

    /// Handles one event from the source. Returns events to send back (e.g. `EdgeHit`).
    pub fn on_event(&mut self, ev: &pb::InputEvent) -> Vec<pb::InputEvent> {
        let Some(e) = &ev.ev else { return vec![] };
        match e {
            Ev::Enter(en) => {
                self.enter(en);
                vec![]
            }
            Ev::Leave(_) | Ev::ReleaseAll(_) => {
                self.injector.release_all();
                if matches!(e, Ev::Leave(_)) {
                    self.active = None;
                }
                vec![]
            }
            _ if self.active.is_none() => vec![], // never inject outside Enter..Leave
            Ev::Key(k) => {
                if let Ok(code) = u16::try_from(k.hid) {
                    self.injector.key(code, k.down);
                }
                vec![]
            }
            Ev::Btn(b) => {
                if let Some(btn) = Button::from_wire(b.button) {
                    self.injector.button(btn, b.down);
                }
                vec![]
            }
            Ev::Wheel(w) => {
                self.injector.wheel(w.dx, w.dy);
                vec![]
            }
            Ev::Rel(r) => {
                self.injector.mouse_rel(r.dx, r.dy);
                self.check_return_edge(r.dx, r.dy, ev.ts_us)
            }
            Ev::Abs(a) => {
                if let Some(s) = self
                    .screens
                    .screens()
                    .into_iter()
                    .find(|s| s.id == a.screen_id)
                {
                    let x =
                        s.x + (a.x.clamp(0.0, 1.0) * (s.w.saturating_sub(1)) as f32).round() as i32;
                    let y =
                        s.y + (a.y.clamp(0.0, 1.0) * (s.h.saturating_sub(1)) as f32).round() as i32;
                    self.injector.mouse_abs_px(x, y);
                }
                vec![]
            }
            Ev::Toggles(t) => {
                self.sync_toggles(t);
                vec![]
            }
            Ev::EdgeHit(_) => vec![],
        }
    }

    /// Connection lost: release everything.
    pub fn on_disconnect(&mut self) {
        self.injector.release_all();
        self.active = None;
    }

    fn enter(&mut self, en: &pb::Enter) {
        // Fresh start: nothing of an older session may remain pressed.
        self.injector.release_all();
        let screens = self.screens.screens();
        let target = screens
            .iter()
            .find(|s| s.id == en.screen_id)
            .or_else(|| screens.iter().find(|s| s.primary))
            .or(screens.first());
        if let Some(s) = target {
            let rel = f64::from(en.rel_pos.clamp(0.0, 1.0));
            let at = |len: u32| (rel * f64::from(len.saturating_sub(1))).round() as i32;
            let p = match Edge::try_from(en.edge).unwrap_or(Edge::Unspecified) {
                Edge::Left => Point {
                    x: s.x + 2,
                    y: s.y + at(s.h),
                },
                Edge::Right => Point {
                    x: s.right() - 3,
                    y: s.y + at(s.h),
                },
                Edge::Top => Point {
                    x: s.x + at(s.w),
                    y: s.y + 2,
                },
                Edge::Bottom => Point {
                    x: s.x + at(s.w),
                    y: s.bottom() - 3,
                },
                Edge::Unspecified => Point {
                    x: s.x + (s.w / 2) as i32,
                    y: s.y + (s.h / 2) as i32,
                },
            };
            self.injector.mouse_abs_px(p.x, p.y);
        }
        if let Some(t) = &en.toggles {
            self.sync_toggles(t);
        }
        self.active = Some(Active {
            edge_reported: false,
        });
    }

    fn sync_toggles(&self, want: &pb::ToggleState) {
        let (caps, num, scroll) = self.injector.toggles();
        if caps != want.caps {
            self.pulse(hid::CAPS_LOCK);
        }
        if num != want.num {
            self.pulse(hid::NUM_LOCK);
        }
        if scroll != want.scroll {
            self.pulse(hid::SCROLL_LOCK);
        }
    }

    fn pulse(&self, key: u16) {
        self.injector.key(key, true);
        self.injector.key(key, false);
    }

    fn check_return_edge(&mut self, dx: i32, dy: i32, ts_us: u64) -> Vec<pb::InputEvent> {
        let p = self.screens.cursor_pos();
        let screens = self.screens.screens();
        let Some(s) = screens.iter().find(|s| s.contains(p)) else {
            return vec![];
        };
        let Some(active) = self.active.as_mut() else {
            return vec![];
        };
        let (edge, pushing) = edge_and_push(s, p, dx, dy);
        let Some(edge) = edge.filter(|_| pushing) else {
            if edge.is_none() {
                active.edge_reported = false;
            }
            return vec![];
        };
        if active.edge_reported {
            return vec![];
        }
        let rel = match edge {
            Edge::Left | Edge::Right => f64::from(p.y - s.y) / f64::from((s.h as i32 - 1).max(1)),
            _ => f64::from(p.x - s.x) / f64::from((s.w as i32 - 1).max(1)),
        };
        let Some(hit) = self.layout.neighbor((self.own, s.id), edge, rel) else {
            return vec![];
        };
        if hit.device != self.source {
            return vec![];
        }
        active.edge_reported = true;
        vec![pb::InputEvent {
            ts_us,
            ev: Some(Ev::EdgeHit(pb::EdgeHit {
                screen_id: s.id,
                edge: edge as i32,
                rel_pos: rel as f32,
            })),
        }]
    }
}

/// Which edge the cursor stands on, and whether the last motion pushes outwards.
fn edge_and_push(s: &ScreenInfo, p: Point, dx: i32, dy: i32) -> (Option<Edge>, bool) {
    if p.x <= s.x {
        return (Some(Edge::Left), dx < 0);
    }
    if p.x >= s.right() - 1 {
        return (Some(Edge::Right), dx > 0);
    }
    if p.y <= s.y {
        return (Some(Edge::Top), dy < 0);
    }
    if p.y >= s.bottom() - 1 {
        return (Some(Edge::Bottom), dy > 0);
    }
    (None, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use deskbridge_platform::mock::{InjectedOp, MockInjector, MockScreens, screen};

    fn setup() -> (Receiver, Arc<MockInjector>, Arc<MockScreens>) {
        let (pc, lp) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let mut pcs = screen(0, 0, 0, 2560, 1440);
        pcs.primary = true;
        let lps = screen(0, 0, 0, 1920, 1080);
        let placements = Layout::default_placements(
            &(pc, vec![pcs.clone()]),
            &(lp, vec![lps.clone()]),
            Edge::Right,
        );
        let layout =
            Layout::from_parts(&[(pc, vec![pcs]), (lp, vec![lps.clone()])], &placements, 1);
        let inj = Arc::new(MockInjector::default());
        let scr = Arc::new(MockScreens::with(vec![lps]));
        let mut r = Receiver::new(lp, pc, inj.clone(), scr.clone());
        r.set_layout(layout);
        (r, inj, scr)
    }

    fn ev(e: Ev) -> pb::InputEvent {
        pb::InputEvent {
            ts_us: 0,
            ev: Some(e),
        }
    }

    fn enter_left(r: &mut Receiver) {
        r.on_event(&ev(Ev::Enter(pb::Enter {
            screen_id: 0,
            rel_pos: 0.5,
            edge: Edge::Left as i32,
            toggles: Some(pb::ToggleState::default()),
        })));
    }

    #[test]
    fn no_injection_before_enter() {
        let (mut r, inj, _) = setup();
        r.on_event(&ev(Ev::Key(pb::Key { hid: 4, down: true })));
        assert!(inj.ops().is_empty());
    }

    #[test]
    fn enter_places_cursor_and_injects() {
        let (mut r, inj, _) = setup();
        enter_left(&mut r);
        assert_eq!(
            inj.ops().last(),
            Some(&InjectedOp::MouseAbs { x: 2, y: 540 })
        );
        r.on_event(&ev(Ev::Key(pb::Key { hid: 4, down: true })));
        assert_eq!(inj.pressed_keys(), vec![4]);
    }

    #[test]
    fn leave_releases_all() {
        let (mut r, inj, _) = setup();
        enter_left(&mut r);
        r.on_event(&ev(Ev::Key(pb::Key {
            hid: hid::L_SHIFT.into(),
            down: true,
        })));
        r.on_event(&ev(Ev::Btn(pb::MouseButton {
            button: 1,
            down: true,
        })));
        r.on_event(&ev(Ev::Leave(pb::Leave {})));
        assert!(inj.pressed_keys().is_empty());
        assert!(!r.is_active());
        // after leave nothing is injected
        inj.clear();
        r.on_event(&ev(Ev::Key(pb::Key { hid: 4, down: true })));
        assert!(inj.ops().is_empty());
    }

    #[test]
    fn disconnect_releases_all() {
        let (mut r, inj, _) = setup();
        enter_left(&mut r);
        r.on_event(&ev(Ev::Key(pb::Key {
            hid: hid::L_CTRL.into(),
            down: true,
        })));
        r.on_disconnect();
        assert!(inj.pressed_keys().is_empty());
    }

    #[test]
    fn return_edge_reports_once() {
        let (mut r, _, scr) = setup();
        enter_left(&mut r);
        scr.set_cursor(Point { x: 0, y: 540 });
        let out = r.on_event(&ev(Ev::Rel(pb::MouseRel { dx: -5, dy: 0 })));
        assert_eq!(out.len(), 1);
        let Some(Ev::EdgeHit(h)) = &out[0].ev else {
            panic!()
        };
        assert_eq!(h.edge, Edge::Left as i32);
        // second push does not re-report
        assert!(
            r.on_event(&ev(Ev::Rel(pb::MouseRel { dx: -5, dy: 0 })))
                .is_empty()
        );
        // move away, re-armed
        scr.set_cursor(Point { x: 50, y: 540 });
        r.on_event(&ev(Ev::Rel(pb::MouseRel { dx: 5, dy: 0 })));
        scr.set_cursor(Point { x: 0, y: 540 });
        assert_eq!(
            r.on_event(&ev(Ev::Rel(pb::MouseRel { dx: -5, dy: 0 })))
                .len(),
            1
        );
    }

    #[test]
    fn moving_along_edge_does_not_return() {
        let (mut r, _, scr) = setup();
        enter_left(&mut r);
        scr.set_cursor(Point { x: 0, y: 540 });
        assert!(
            r.on_event(&ev(Ev::Rel(pb::MouseRel { dx: 0, dy: 5 })))
                .is_empty()
        );
        assert!(
            r.on_event(&ev(Ev::Rel(pb::MouseRel { dx: 3, dy: 0 })))
                .is_empty()
        );
    }

    #[test]
    fn only_returns_towards_the_source_side() {
        let (mut r, _, scr) = setup();
        enter_left(&mut r);
        // right edge has no neighbour in the layout
        scr.set_cursor(Point { x: 1919, y: 540 });
        assert!(
            r.on_event(&ev(Ev::Rel(pb::MouseRel { dx: 5, dy: 0 })))
                .is_empty()
        );
    }

    #[test]
    fn toggles_are_synchronised() {
        let (mut r, inj, _) = setup();
        inj.set_toggles(false, false, false);
        r.on_event(&ev(Ev::Enter(pb::Enter {
            screen_id: 0,
            rel_pos: 0.5,
            edge: Edge::Left as i32,
            toggles: Some(pb::ToggleState {
                caps: true,
                num: false,
                scroll: false,
            }),
        })));
        let ops = inj.ops();
        assert!(ops.contains(&InjectedOp::Key {
            hid: hid::CAPS_LOCK,
            down: true
        }));
        assert!(ops.contains(&InjectedOp::Key {
            hid: hid::CAPS_LOCK,
            down: false
        }));
    }

    #[test]
    fn absolute_maps_to_pixels() {
        let (mut r, inj, _) = setup();
        r.on_event(&ev(Ev::Enter(pb::Enter {
            screen_id: 0,
            rel_pos: 0.5,
            edge: Edge::Unspecified as i32,
            toggles: None,
        })));
        r.on_event(&ev(Ev::Abs(pb::MouseAbs {
            x: 1.0,
            y: 0.0,
            screen_id: 0,
        })));
        assert_eq!(
            inj.ops().last(),
            Some(&InjectedOp::MouseAbs { x: 1919, y: 0 })
        );
    }
}
