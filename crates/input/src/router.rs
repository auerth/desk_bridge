//! Source side input routing: local ⇄ remote state machine.
//!
//! The router is a pure function of (state, event, time): it never touches the OS. The caller
//! (`core`) executes the returned [`Action`]s.
//!
//! Safety rule (plan 6.4): every exit from `Remote` emits `Leave`, and the *target* releases all
//! keys and buttons it injected; the source lifts the grab.

use crate::hotkeys::HotkeyAction;
use crate::layout::Layout;
use deskbridge_platform::hid;
use deskbridge_platform::input::{CaptureEvent, GrabMode};
use deskbridge_platform::screen::{Point, ScreenInfo};
use deskbridge_proto::pb::{self, Edge, input_event::Ev};
use std::collections::BTreeSet;
use std::time::{Duration, Instant};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct RouterConfig {
    /// Cursor has to push against the edge this long before the switch happens (0–500 ms).
    pub switch_delay: Duration,
    /// Modifier mask (`CTRL|ALT|…`) that must be held to switch; 0 = none required.
    pub require_modifiers: u8,
    /// No switching within this many pixels of a screen corner.
    pub corner_margin_px: i32,
    /// Require two touches of the edge within 500 ms.
    pub double_bump: bool,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            switch_delay: Duration::ZERO,
            require_modifiers: 0,
            corner_margin_px: 8,
            double_bump: false,
        }
    }
}

/// Region of the PC desktop that shows the remote screen (focus mode).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AbsRegion {
    pub device: Uuid,
    pub screen: u32,
    /// Position and size in PC virtual-desktop pixels.
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl AbsRegion {
    fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum State {
    Local,
    /// Cursor is on the remote device (relative mode).
    Remote {
        device: Uuid,
        screen: u32,
    },
}

/// What the caller has to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Send this event to the peer on the input stream.
    Send(Ev),
    SetGrab(GrabMode),
    /// Place the local cursor (after returning from the remote device).
    MoveCursor(Point),
    /// Show an OSD pill for the new location.
    Osd(Location),
    Hotkey(HotkeyAction),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Location {
    Local,
    Remote,
}

pub struct Router {
    own: Uuid,
    cfg: RouterConfig,
    layout: Layout,
    screens: Vec<ScreenInfo>,
    state: State,
    locked: bool,
    toggles: pb::ToggleState,
    held: BTreeSet<u16>,
    mods: u8,
    edge_since: Option<(Edge, Instant)>,
    bump: Option<(Edge, Instant, bool)>,
    region: Option<AbsRegion>,
    region_inside: bool,
    /// Last absolute cursor position seen (the cursor stays put while it is pushed against an edge).
    last_pos: Option<Point>,
}

fn mod_mask_of(held: &BTreeSet<u16>) -> u8 {
    held.iter().fold(0u8, |m, k| m | hid::modifier_bit(*k))
}

impl Router {
    #[must_use]
    pub fn new(own: Uuid, cfg: RouterConfig) -> Self {
        Self {
            own,
            cfg,
            layout: Layout::default(),
            screens: Vec::new(),
            state: State::Local,
            locked: false,
            toggles: pb::ToggleState::default(),
            held: BTreeSet::new(),
            mods: 0,
            edge_since: None,
            bump: None,
            region: None,
            region_inside: false,
            last_pos: None,
        }
    }

    pub fn set_config(&mut self, cfg: RouterConfig) {
        self.cfg = cfg;
    }

    pub fn set_layout(&mut self, layout: Layout) {
        self.layout = layout;
    }

    pub fn set_screens(&mut self, screens: Vec<ScreenInfo>) {
        self.screens = screens;
    }

    pub fn set_toggles(&mut self, t: pb::ToggleState) {
        self.toggles = t;
    }

    #[must_use]
    pub fn state(&self) -> State {
        self.state
    }

    #[must_use]
    pub fn is_remote(&self) -> bool {
        matches!(self.state, State::Remote { .. }) || self.region_inside
    }

    #[must_use]
    pub fn is_locked(&self) -> bool {
        self.locked
    }

    /// Enables absolute pointer mapping for focus mode (split or full). `None` disables it.
    pub fn set_region(&mut self, region: Option<AbsRegion>) -> Vec<Action> {
        let mut out = Vec::new();
        if region.is_none() && self.region_inside {
            self.region_inside = false;
            out.push(Action::Send(Ev::Leave(pb::Leave {})));
            out.push(Action::SetGrab(GrabMode::None));
        }
        self.region = region;
        out
    }

    fn local_edge(&self, p: Point) -> Option<(u32, Edge, f64, i32)> {
        let s = self.screens.iter().find(|s| s.contains(p))?;
        let candidates = [
            (Edge::Left, p.x <= s.x),
            (Edge::Right, p.x >= s.right() - 1),
            (Edge::Top, p.y <= s.y),
            (Edge::Bottom, p.y >= s.bottom() - 1),
        ];
        for (edge, hit) in candidates {
            if !hit {
                continue;
            }
            // Is there another *local* screen right behind this edge? Then Windows handles it.
            let beyond = match edge {
                Edge::Left => Point { x: s.x - 1, y: p.y },
                Edge::Right => Point {
                    x: s.right(),
                    y: p.y,
                },
                Edge::Top => Point { x: p.x, y: s.y - 1 },
                _ => Point {
                    x: p.x,
                    y: s.bottom(),
                },
            };
            if self
                .screens
                .iter()
                .any(|o| o.id != s.id && o.contains(beyond))
            {
                continue;
            }
            let (pos, len) = match edge {
                Edge::Left | Edge::Right => (p.y - s.y, s.h as i32),
                _ => (p.x - s.x, s.w as i32),
            };
            let along_margin = pos.min(len - 1 - pos);
            let rel = f64::from(pos) / f64::from((len - 1).max(1));
            return Some((s.id, edge, rel, along_margin));
        }
        None
    }

    /// Feeds one captured event.
    pub fn on_capture(&mut self, ev: CaptureEvent, now: Instant) -> Vec<Action> {
        match ev {
            CaptureEvent::Hotkey(id) => return self.on_hotkey(id),
            CaptureEvent::Key { hid, down } => {
                if down {
                    self.held.insert(hid);
                } else {
                    self.held.remove(&hid);
                }
                self.mods = mod_mask_of(&self.held);
            }
            _ => {}
        }
        // The low-level hook reports where the cursor *would* go, before Windows clamps it to
        // the desktop: a fast push to the left arrives as x = -6, which lies on no screen and
        // found no edge until a slow sideways wiggle happened to report x = 0. Clamp like
        // Windows does.
        let ev = match ev {
            CaptureEvent::MouseMove { x, y } if self.region.is_none() => {
                let p = self.clamp_to_screens(Point { x, y });
                CaptureEvent::MouseMove { x: p.x, y: p.y }
            }
            other => other,
        };
        if let CaptureEvent::MouseMove { x, y } = ev {
            self.last_pos = Some(Point { x, y });
        }
        if self.region.is_some() {
            return self.on_capture_region(ev);
        }
        match self.state {
            State::Local => {
                // A cursor that is clamped at a screen edge produces no further absolute
                // positions while the user keeps pushing, only raw deltas. Treat a push against
                // the edge as "the cursor is still there" so switching never needs a wiggle.
                let ev = match (ev, self.last_pos) {
                    (CaptureEvent::MouseRel { dx, dy }, Some(p))
                        if self.pushes_outward(p, dx, dy) =>
                    {
                        CaptureEvent::MouseMove { x: p.x, y: p.y }
                    }
                    _ => ev,
                };
                self.on_capture_local(ev, now)
            }
            State::Remote { .. } => self.on_capture_remote(ev),
        }
    }

    /// `p` itself if it lies on a local screen, else the nearest point of the nearest screen.
    fn clamp_to_screens(&self, p: Point) -> Point {
        if self.screens.is_empty() || self.screens.iter().any(|s| s.contains(p)) {
            return p;
        }
        self.screens
            .iter()
            .map(|s| {
                let c = Point {
                    x: p.x.clamp(s.x, s.right() - 1),
                    y: p.y.clamp(s.y, s.bottom() - 1),
                };
                let d = i64::from(c.x - p.x).pow(2) + i64::from(c.y - p.y).pow(2);
                (d, c)
            })
            .min_by_key(|(d, _)| *d)
            .map_or(p, |(_, c)| c)
    }

    /// Is `p` on an outer screen edge and does the motion (dx, dy) push against it?
    fn pushes_outward(&self, p: Point, dx: i32, dy: i32) -> bool {
        match self.local_edge(p) {
            Some((_, Edge::Right, ..)) => dx > 0,
            Some((_, Edge::Left, ..)) => dx < 0,
            Some((_, Edge::Bottom, ..)) => dy > 0,
            Some((_, Edge::Top, ..)) => dy < 0,
            _ => false,
        }
    }

    fn on_capture_local(&mut self, ev: CaptureEvent, now: Instant) -> Vec<Action> {
        let CaptureEvent::MouseMove { x, y } = ev else {
            return vec![];
        };
        if self.locked {
            return vec![];
        }
        let Some((screen, edge, rel, corner)) = self.local_edge(Point { x, y }) else {
            self.edge_since = None;
            if let Some(b) = &mut self.bump {
                b.2 = false; // left the edge: next touch counts
            }
            return vec![];
        };
        if corner < self.cfg.corner_margin_px {
            self.edge_since = None;
            return vec![];
        }
        let Some(hit) = self.layout.neighbor((self.own, screen), edge, rel) else {
            self.edge_since = None;
            return vec![];
        };
        if hit.device == self.own {
            return vec![];
        }
        if self.cfg.require_modifiers != 0
            && (self.mods & self.cfg.require_modifiers) != self.cfg.require_modifiers
        {
            self.edge_since = None;
            return vec![];
        }
        // dwell time
        let since = match self.edge_since {
            Some((e, t)) if e == edge => t,
            _ => {
                self.edge_since = Some((edge, now));
                now
            }
        };
        if now.duration_since(since) < self.cfg.switch_delay {
            return vec![];
        }
        // double bump
        if self.cfg.double_bump {
            match &mut self.bump {
                Some((e, t, armed))
                    if *e == edge && now.duration_since(*t) < Duration::from_millis(500) =>
                {
                    if !*armed {
                        // second touch within the window
                        self.bump = None;
                    } else {
                        return vec![];
                    }
                }
                _ => {
                    self.bump = Some((edge, now, true));
                    return vec![];
                }
            }
        }
        self.edge_since = None;
        self.enter_remote(hit.device, hit.screen, hit.rel as f32, hit.entry_edge)
    }

    fn enter_remote(&mut self, device: Uuid, screen: u32, rel: f32, edge: Edge) -> Vec<Action> {
        self.state = State::Remote { device, screen };
        let mut out = vec![
            Action::SetGrab(GrabMode::All),
            Action::Send(Ev::Enter(pb::Enter {
                screen_id: screen,
                rel_pos: rel,
                edge: edge as i32,
                toggles: Some(self.toggles),
            })),
        ];
        // Modifiers that are physically held keep working on the target.
        for k in self.held.iter().copied().filter(|k| hid::is_modifier(*k)) {
            out.push(Action::Send(Ev::Key(pb::Key {
                hid: u32::from(k),
                down: true,
            })));
        }
        out.push(Action::Osd(Location::Remote));
        out
    }

    fn leave_remote(&mut self, cursor: Option<Point>) -> Vec<Action> {
        self.state = State::Local;
        self.edge_since = None;
        let mut out = vec![
            Action::Send(Ev::Leave(pb::Leave {})),
            Action::SetGrab(GrabMode::None),
        ];
        if let Some(c) = cursor {
            out.push(Action::MoveCursor(c));
        }
        out.push(Action::Osd(Location::Local));
        out
    }

    fn on_capture_remote(&mut self, ev: CaptureEvent) -> Vec<Action> {
        match ev {
            CaptureEvent::Key { hid, down } => {
                vec![Action::Send(Ev::Key(pb::Key {
                    hid: u32::from(hid),
                    down,
                }))]
            }
            CaptureEvent::MouseRel { dx, dy } => {
                vec![Action::Send(Ev::Rel(pb::MouseRel { dx, dy }))]
            }
            CaptureEvent::Button { button, down } => {
                vec![Action::Send(Ev::Btn(pb::MouseButton {
                    button: button.to_wire(),
                    down,
                }))]
            }
            CaptureEvent::Wheel { dx, dy } => vec![Action::Send(Ev::Wheel(pb::Wheel { dx, dy }))],
            CaptureEvent::MouseMove { .. } | CaptureEvent::Hotkey(_) => vec![],
        }
    }

    fn on_capture_region(&mut self, ev: CaptureEvent) -> Vec<Action> {
        let region = self.region.expect("checked by caller");
        match ev {
            CaptureEvent::MouseMove { x, y } => {
                let inside = region.contains(x, y);
                let mut out = Vec::new();
                if inside && !self.region_inside {
                    self.region_inside = true;
                    out.push(Action::SetGrab(GrabMode::KeyboardOnly));
                    out.push(Action::Send(Ev::Enter(pb::Enter {
                        screen_id: region.screen,
                        rel_pos: 0.5,
                        edge: Edge::Unspecified as i32,
                        toggles: Some(self.toggles),
                    })));
                    out.push(Action::Osd(Location::Remote));
                } else if !inside && self.region_inside {
                    self.region_inside = false;
                    out.push(Action::Send(Ev::Leave(pb::Leave {})));
                    out.push(Action::SetGrab(GrabMode::None));
                    out.push(Action::Osd(Location::Local));
                }
                if inside {
                    out.push(Action::Send(Ev::Abs(pb::MouseAbs {
                        x: ((x - region.x) as f32 + 0.5) / region.w as f32,
                        y: ((y - region.y) as f32 + 0.5) / region.h as f32,
                        screen_id: region.screen,
                    })));
                }
                out
            }
            _ if !self.region_inside => vec![],
            CaptureEvent::Key { hid, down } => {
                vec![Action::Send(Ev::Key(pb::Key {
                    hid: u32::from(hid),
                    down,
                }))]
            }
            CaptureEvent::Button { button, down } => {
                vec![Action::Send(Ev::Btn(pb::MouseButton {
                    button: button.to_wire(),
                    down,
                }))]
            }
            CaptureEvent::Wheel { dx, dy } => vec![Action::Send(Ev::Wheel(pb::Wheel { dx, dy }))],
            CaptureEvent::MouseRel { .. } | CaptureEvent::Hotkey(_) => vec![],
        }
    }

    fn on_hotkey(&mut self, id: u32) -> Vec<Action> {
        let Some(action) = HotkeyAction::from_id(id) else {
            return vec![];
        };
        match action {
            HotkeyAction::LockCursor => {
                self.locked = !self.locked;
                vec![Action::Hotkey(action)]
            }
            HotkeyAction::Emergency => {
                self.locked = false;
                let mut out = if self.is_remote() {
                    self.force_local()
                } else {
                    vec![]
                };
                out.push(Action::Hotkey(action));
                out
            }
            HotkeyAction::JumpOther => {
                if self.is_remote() {
                    let mut out = self.force_local();
                    out.push(Action::Hotkey(action));
                    out
                } else if let Some(t) = self.first_remote_screen() {
                    let mut out = self.enter_remote(t.0, t.1, 0.5, Edge::Unspecified);
                    out.push(Action::Hotkey(action));
                    out
                } else {
                    vec![]
                }
            }
            other => vec![Action::Hotkey(other)],
        }
    }

    fn first_remote_screen(&self) -> Option<(Uuid, u32)> {
        self.layout
            .screens
            .iter()
            .find(|s| s.device != self.own)
            .map(|s| (s.device, s.screen))
    }

    /// Unconditionally returns to local control (emergency, connection loss).
    pub fn force_local(&mut self) -> Vec<Action> {
        let was_region = self.region_inside;
        let was_remote = matches!(self.state, State::Remote { .. });
        self.region_inside = false;
        if !was_region && !was_remote {
            return vec![];
        }
        self.state = State::Local;
        self.edge_since = None;
        vec![
            Action::Send(Ev::Leave(pb::Leave {})),
            Action::SetGrab(GrabMode::None),
            Action::Osd(Location::Local),
        ]
    }

    /// The peer reports that its cursor pushed through an edge towards us.
    pub fn on_edge_hit(&mut self, hit: &pb::EdgeHit) -> Vec<Action> {
        let State::Remote { device, .. } = self.state else {
            return vec![];
        };
        if self.locked {
            return vec![];
        }
        let edge = Edge::try_from(hit.edge).unwrap_or(Edge::Unspecified);
        let Some(back) =
            self.layout
                .neighbor((device, hit.screen_id), edge, f64::from(hit.rel_pos))
        else {
            return self.leave_remote(None);
        };
        if back.device != self.own {
            return self.leave_remote(None);
        }
        let cursor = self.screens.iter().find(|s| s.id == back.screen).map(|s| {
            let along = |len: u32| (back.rel * f64::from(len.saturating_sub(1))).round() as i32;
            match back.entry_edge {
                Edge::Left => Point {
                    x: s.x + 2,
                    y: s.y + along(s.h),
                },
                Edge::Right => Point {
                    x: s.right() - 3,
                    y: s.y + along(s.h),
                },
                Edge::Top => Point {
                    x: s.x + along(s.w),
                    y: s.y + 2,
                },
                _ => Point {
                    x: s.x + along(s.w),
                    y: s.bottom() - 3,
                },
            }
        });
        self.leave_remote(cursor)
    }

    /// Connection lost or peer gone: never leave the user without a local mouse and keyboard.
    pub fn on_disconnect(&mut self) -> Vec<Action> {
        self.force_local()
    }

    #[must_use]
    pub fn held_keys(&self) -> Vec<u16> {
        self.held.iter().copied().collect()
    }
}

/// Convenience: modifier masks re-exported for config code.
pub mod mods {
    pub use deskbridge_platform::hotkey::{ALT, CTRL, GUI, SHIFT};
}

#[cfg(test)]
mod tests {
    use super::*;
    use deskbridge_platform::hotkey::CTRL;
    use deskbridge_platform::mock::screen;
    use deskbridge_proto::pb::Placement;

    fn pc() -> Uuid {
        Uuid::from_u128(1)
    }
    fn lp() -> Uuid {
        Uuid::from_u128(2)
    }

    fn router(cfg: RouterConfig) -> Router {
        let mut pc_screen = screen(0, 0, 0, 2560, 1440);
        pc_screen.primary = true;
        let lp_screen = screen(0, 0, 0, 1920, 1080);
        let placements = Layout::default_placements(
            &(pc(), vec![pc_screen.clone()]),
            &(lp(), vec![lp_screen.clone()]),
            Edge::Right,
        );
        let layout = Layout::from_parts(
            &[(pc(), vec![pc_screen.clone()]), (lp(), vec![lp_screen])],
            &placements,
            1,
        );
        let mut r = Router::new(pc(), cfg);
        r.set_layout(layout);
        r.set_screens(vec![pc_screen]);
        r
    }

    fn has_enter(a: &[Action]) -> bool {
        a.iter().any(|x| matches!(x, Action::Send(Ev::Enter(_))))
    }

    #[test]
    fn crossing_right_edge_enters_remote() {
        let mut r = router(RouterConfig::default());
        let t = Instant::now();
        // Not at the edge yet.
        assert!(
            r.on_capture(CaptureEvent::MouseMove { x: 2000, y: 720 }, t)
                .is_empty()
        );
        // Corner is locked.
        assert!(
            r.on_capture(CaptureEvent::MouseMove { x: 2559, y: 2 }, t)
                .is_empty()
        );
        let a = r.on_capture(CaptureEvent::MouseMove { x: 2559, y: 720 }, t);
        assert!(has_enter(&a), "{a:?}");
        assert!(a.contains(&Action::SetGrab(GrabMode::All)));
        assert!(matches!(r.state(), State::Remote { .. }));
    }

    #[test]
    fn pushing_against_the_edge_switches_without_a_new_position() {
        let mut r = router(RouterConfig::default());
        let t = Instant::now();
        // The cursor arrives at the right edge (a bit above the laptop's span is irrelevant here)
        // and gets clamped: from now on only raw deltas arrive.
        assert!(
            r.on_capture(CaptureEvent::MouseMove { x: 2558, y: 720 }, t)
                .is_empty()
        );
        // pushing sideways inside the screen does nothing
        assert!(
            r.on_capture(CaptureEvent::MouseRel { dx: 5, dy: 0 }, t)
                .is_empty()
        );
        r.on_capture(CaptureEvent::MouseMove { x: 2559, y: 720 }, t);
        // already switched by the move itself in the default config; use a delayed config instead
        let mut r = router(RouterConfig {
            switch_delay: Duration::from_millis(50),
            ..Default::default()
        });
        r.on_capture(CaptureEvent::MouseMove { x: 2559, y: 720 }, t);
        assert!(matches!(r.state(), State::Local));
        let a = r.on_capture(
            CaptureEvent::MouseRel { dx: 12, dy: 0 },
            t + Duration::from_millis(80),
        );
        assert!(has_enter(&a), "{a:?}");
    }

    #[test]
    fn overshooting_hook_position_still_switches() {
        // A fast flick: the hook reports a point beyond the screen, never the edge pixel itself.
        let mut r = router(RouterConfig::default());
        let a = r.on_capture(CaptureEvent::MouseMove { x: 2571, y: 720 }, Instant::now());
        assert!(has_enter(&a), "{a:?}");
        // Same with a dwell time: the follow-up raw deltas keep pushing against the edge.
        let mut r = router(RouterConfig {
            switch_delay: Duration::from_millis(50),
            ..Default::default()
        });
        let t = Instant::now();
        assert!(
            r.on_capture(CaptureEvent::MouseMove { x: 2590, y: 720 }, t)
                .is_empty()
        );
        let a = r.on_capture(
            CaptureEvent::MouseRel { dx: 9, dy: 0 },
            t + Duration::from_millis(60),
        );
        assert!(has_enter(&a), "{a:?}");
    }

    #[test]
    fn remote_forwards_events_and_locks_out_local() {
        let mut r = router(RouterConfig::default());
        r.on_capture(CaptureEvent::MouseMove { x: 2559, y: 720 }, Instant::now());
        let a = r.on_capture(
            CaptureEvent::Key {
                hid: hid::A,
                down: true,
            },
            Instant::now(),
        );
        assert_eq!(
            a,
            vec![Action::Send(Ev::Key(pb::Key {
                hid: u32::from(hid::A),
                down: true
            }))]
        );
        let a = r.on_capture(CaptureEvent::MouseRel { dx: 3, dy: -2 }, Instant::now());
        assert_eq!(
            a,
            vec![Action::Send(Ev::Rel(pb::MouseRel { dx: 3, dy: -2 }))]
        );
    }

    #[test]
    fn edge_hit_returns_and_places_cursor() {
        let mut r = router(RouterConfig::default());
        r.on_capture(CaptureEvent::MouseMove { x: 2559, y: 720 }, Instant::now());
        let a = r.on_edge_hit(&pb::EdgeHit {
            screen_id: 0,
            edge: Edge::Left as i32,
            rel_pos: 0.5,
        });
        assert!(a.contains(&Action::Send(Ev::Leave(pb::Leave {}))));
        assert!(a.contains(&Action::SetGrab(GrabMode::None)));
        let cursor = a
            .iter()
            .find_map(|x| {
                if let Action::MoveCursor(p) = x {
                    Some(*p)
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(cursor.x, 2560 - 3);
        assert!((cursor.y - 720).abs() <= 2);
        assert_eq!(r.state(), State::Local);
    }

    #[test]
    fn disconnect_always_returns_local() {
        let mut r = router(RouterConfig::default());
        r.on_capture(CaptureEvent::MouseMove { x: 2559, y: 720 }, Instant::now());
        let a = r.on_disconnect();
        assert!(a.contains(&Action::SetGrab(GrabMode::None)));
        assert!(r.on_disconnect().is_empty());
    }

    #[test]
    fn lock_prevents_switching() {
        let mut r = router(RouterConfig::default());
        r.on_capture(
            CaptureEvent::Hotkey(HotkeyAction::LockCursor.id()),
            Instant::now(),
        );
        assert!(r.is_locked());
        assert!(
            r.on_capture(CaptureEvent::MouseMove { x: 2559, y: 720 }, Instant::now())
                .is_empty()
        );
    }

    #[test]
    fn delay_requires_dwell() {
        let mut r = router(RouterConfig {
            switch_delay: Duration::from_millis(200),
            ..Default::default()
        });
        let t = Instant::now();
        assert!(
            r.on_capture(CaptureEvent::MouseMove { x: 2559, y: 720 }, t)
                .is_empty()
        );
        assert!(
            r.on_capture(
                CaptureEvent::MouseMove { x: 2559, y: 720 },
                t + Duration::from_millis(100)
            )
            .is_empty()
        );
        let a = r.on_capture(
            CaptureEvent::MouseMove { x: 2559, y: 720 },
            t + Duration::from_millis(250),
        );
        assert!(has_enter(&a));
    }

    #[test]
    fn modifier_requirement() {
        let mut r = router(RouterConfig {
            require_modifiers: CTRL,
            ..Default::default()
        });
        assert!(
            r.on_capture(CaptureEvent::MouseMove { x: 2559, y: 720 }, Instant::now())
                .is_empty()
        );
        r.on_capture(
            CaptureEvent::Key {
                hid: hid::L_CTRL,
                down: true,
            },
            Instant::now(),
        );
        assert!(has_enter(&r.on_capture(
            CaptureEvent::MouseMove { x: 2559, y: 720 },
            Instant::now()
        )));
    }

    #[test]
    fn held_modifiers_are_replayed_on_enter() {
        let mut r = router(RouterConfig::default());
        r.on_capture(
            CaptureEvent::Key {
                hid: hid::L_SHIFT,
                down: true,
            },
            Instant::now(),
        );
        let a = r.on_capture(CaptureEvent::MouseMove { x: 2559, y: 720 }, Instant::now());
        assert!(a.contains(&Action::Send(Ev::Key(pb::Key {
            hid: u32::from(hid::L_SHIFT),
            down: true
        }))));
    }

    #[test]
    fn emergency_hotkey_goes_local() {
        let mut r = router(RouterConfig::default());
        r.on_capture(CaptureEvent::MouseMove { x: 2559, y: 720 }, Instant::now());
        let a = r.on_capture(
            CaptureEvent::Hotkey(HotkeyAction::Emergency.id()),
            Instant::now(),
        );
        assert!(a.contains(&Action::SetGrab(GrabMode::None)));
        assert!(a.contains(&Action::Hotkey(HotkeyAction::Emergency)));
        assert_eq!(r.state(), State::Local);
    }

    #[test]
    fn jump_hotkey_toggles() {
        let mut r = router(RouterConfig::default());
        let a = r.on_capture(
            CaptureEvent::Hotkey(HotkeyAction::JumpOther.id()),
            Instant::now(),
        );
        assert!(has_enter(&a));
        let a = r.on_capture(
            CaptureEvent::Hotkey(HotkeyAction::JumpOther.id()),
            Instant::now(),
        );
        assert!(a.contains(&Action::Send(Ev::Leave(pb::Leave {}))));
    }

    #[test]
    fn region_maps_absolute_and_switches_keyboard() {
        let mut r = router(RouterConfig::default());
        let _ = r.set_region(Some(AbsRegion {
            device: lp(),
            screen: 0,
            x: 2560,
            y: 0,
            w: 2560,
            h: 1440,
        }));
        // outside → nothing
        assert!(
            r.on_capture(CaptureEvent::MouseMove { x: 100, y: 100 }, Instant::now())
                .is_empty()
        );
        // inside → enter + grab + abs
        let a = r.on_capture(
            CaptureEvent::MouseMove {
                x: 2560 + 1280,
                y: 720,
            },
            Instant::now(),
        );
        assert!(a.contains(&Action::SetGrab(GrabMode::KeyboardOnly)));
        assert!(has_enter(&a));
        let abs = a
            .iter()
            .find_map(|x| {
                if let Action::Send(Ev::Abs(m)) = x {
                    Some(*m)
                } else {
                    None
                }
            })
            .unwrap();
        assert!((abs.x - 0.5).abs() < 0.01 && (abs.y - 0.5).abs() < 0.01);
        // keys flow only while inside
        assert!(
            !r.on_capture(
                CaptureEvent::Key {
                    hid: hid::A,
                    down: true
                },
                Instant::now()
            )
            .is_empty()
        );
        let a = r.on_capture(CaptureEvent::MouseMove { x: 10, y: 10 }, Instant::now());
        assert!(a.contains(&Action::Send(Ev::Leave(pb::Leave {}))));
        assert!(
            r.on_capture(
                CaptureEvent::Key {
                    hid: hid::A,
                    down: true
                },
                Instant::now()
            )
            .is_empty()
        );
    }

    #[allow(dead_code)]
    fn _unused(_: Placement) {}
}
