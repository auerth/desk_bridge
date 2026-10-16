//! Input glue: router (source side), receiver (target side), layout sync.

use super::{Engine, screen_from_pb};
use crate::config::PlacementConfig;
use crate::ipc::UiEvent;
use deskbridge_input::hotkeys::{self, HotkeyAction};
use deskbridge_input::layout::Layout;
use deskbridge_input::receiver::Receiver;
use deskbridge_input::router::{Action, Location};
use deskbridge_platform::input::{CaptureEvent, GrabMode};
use deskbridge_proto::pb::{self, Edge, input_event::Ev};
use std::time::Instant;
use tokio::sync::mpsc;

impl Engine {
    /// Called when a session came up: start capture (source), create the receiver, sync layout.
    pub(crate) fn start_roles(&mut self) {
        let Some(s) = &self.session else { return };
        let peer_id = s.link.peer.id;
        self.receiver = Some(Receiver::new(
            self.own_id(),
            peer_id,
            self.platform.injector.clone(),
            self.platform.screens.clone(),
        ));
        self.router.set_config(super::router_config(&self.cfg));
        self.router.set_screens(self.local_screens.clone());
        self.rebuild_layout(true);
        if self.cfg.device.input_source && self.cfg.input.enabled {
            self.start_capture();
        }
    }

    pub(crate) fn stop_roles(&mut self) {
        let actions = self.router.on_disconnect();
        self.exec_actions(actions, false);
        self.stop_capture();
        if let Some(r) = self.receiver.as_mut() {
            r.on_disconnect();
        }
        self.receiver = None;
        self.platform.injector.release_all();
    }

    pub(crate) fn start_capture(&mut self) {
        if self.capture_running {
            return;
        }
        let (tx, mut rx) = mpsc::unbounded_channel::<CaptureEvent>();
        let (hk, conflicts) = hotkeys::build(&self.cfg.hotkeys);
        for (a, b) in conflicts {
            tracing::warn!(?a, ?b, "hotkey conflict, second binding ignored");
        }
        self.platform.capture.set_hotkeys(hk);
        if let Err(e) = self.platform.capture.start(tx) {
            tracing::error!("input capture failed to start: {e}");
            self.warn_once(
                "capture_failed",
                &format!("Eingabe-Erfassung konnte nicht gestartet werden: {e}"),
            );
            return;
        }
        let m = self.msg_tx.clone();
        tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                if m.send(super::Msg::Capture(ev)).is_err() {
                    break;
                }
            }
        });
        self.capture_running = true;
    }

    pub(crate) fn stop_capture(&mut self) {
        if self.capture_running {
            self.platform.capture.set_grab(GrabMode::None);
            self.platform.capture.stop();
            self.capture_running = false;
        }
    }

    pub(crate) fn on_capture(&mut self, ev: CaptureEvent) {
        let actions = self.router.on_capture(ev, Instant::now());
        if !actions.is_empty() {
            self.exec_actions(actions, true);
        }
    }

    /// Executes router actions. `connected`: whether the peer link may be used.
    pub(crate) fn exec_actions(&mut self, actions: Vec<Action>, connected: bool) {
        for a in actions {
            match a {
                Action::Send(ev) => {
                    if connected {
                        if let Some(s) = &self.session {
                            s.link.send_input(ev);
                        }
                    } else if let (Ev::Leave(_), Some(s)) = (&ev, &self.session) {
                        s.link.send_input(ev);
                    }
                }
                Action::SetGrab(mode) => self.platform.capture.set_grab(mode),
                Action::MoveCursor(p) => self.platform.capture.set_cursor_pos(p.x, p.y),
                Action::Osd(loc) => {
                    if matches!(loc, Location::Local) {
                        self.claim_clipboard_files();
                    }
                    let peer_name = self
                        .session
                        .as_ref()
                        .map(|s| s.link.peer.name.clone())
                        .unwrap_or_default();
                    let (icon, text) = match loc {
                        Location::Remote => ("laptop".to_string(), peer_name),
                        Location::Local => ("monitor".to_string(), self.cfg.device.name.clone()),
                    };
                    self.emit(UiEvent::Osd { icon, text });
                    self.dirty = true;
                }
                Action::Hotkey(h) => self.on_hotkey(h),
            }
        }
        self.dirty = true;
    }

    pub(crate) fn on_edge_recheck(&mut self) {
        self.edge_recheck_pending = false;
        let Some(s) = &self.session else { return };
        let link = s.link.clone();
        if let Some(r) = self.receiver.as_mut() {
            for reply in r.recheck_return_edge() {
                link.send_input_event(reply);
            }
        }
    }

    /// The user is on this device now: files copied on the other one may take over the clipboard.
    fn claim_clipboard_files(&self) {
        if let Some(c) = &self.clip {
            c.job(super::clip::ClipJob::Claim);
        }
    }

    fn on_hotkey(&mut self, h: HotkeyAction) {
        match h {
            HotkeyAction::LockCursor => {
                self.cursor_locked = self.router.is_locked();
                let text = if self.cursor_locked {
                    "Cursor gesperrt"
                } else {
                    "Cursor frei"
                };
                self.emit(UiEvent::Osd {
                    icon: "lock".into(),
                    text: text.into(),
                });
            }
            HotkeyAction::Emergency => {
                self.cursor_locked = false;
                self.emit(UiEvent::Osd {
                    icon: "alert".into(),
                    text: "Notfall: alles lokal".into(),
                });
                self.on_emergency();
            }
            HotkeyAction::JumpOther => {}
            other => self.on_feature_hotkey(other),
        }
    }

    /// Mirrors state that the UI shows and keeps lock-key state fresh for `Enter`.
    pub(crate) fn tick_input(&mut self) {
        if self.cfg.device.input_source && self.session.is_some() && !self.router.is_remote() {
            let (caps, num, scroll) = self.platform.injector.toggles();
            self.router
                .set_toggles(pb::ToggleState { caps, num, scroll });
        }
    }

    pub(crate) fn on_peer_input(&mut self, session_id: u64, ev: pb::InputEvent) {
        let Some(s) = &self.session else { return };
        if s.link.session_id != session_id {
            return;
        }
        let link = s.link.clone();
        self.record_input_latency(&ev);
        match &ev.ev {
            Some(Ev::EdgeHit(h)) => {
                let actions = self.router.on_edge_hit(h);
                self.exec_actions(actions, true);
            }
            Some(_) => {
                let was_active = self.receiver.as_ref().is_some_and(Receiver::is_active);
                if let Some(r) = self.receiver.as_mut() {
                    let replies = r.on_event(&ev);
                    let moved = matches!(ev.ev, Some(Ev::Rel(_)));
                    let no_reply = replies.is_empty();
                    for reply in replies {
                        link.send_input_event(reply);
                    }
                    if moved && no_reply && !self.edge_recheck_pending {
                        // The cursor position lags the injected motion: look again in a moment.
                        self.edge_recheck_pending = true;
                        let m = self.msg_tx.clone();
                        tokio::spawn(async move {
                            tokio::time::sleep(std::time::Duration::from_millis(8)).await;
                            let _ = m.send(super::Msg::EdgeRecheck);
                        });
                    }
                    let now_active = r.is_active();
                    if now_active != was_active {
                        let name = self.cfg.device.name.clone();
                        if now_active {
                            self.claim_clipboard_files();
                            self.emit(UiEvent::Osd {
                                icon: "laptop".into(),
                                text: name,
                            });
                        }
                        self.dirty = true;
                    }
                }
            }
            None => {}
        }
    }

    /// One-way latency of input events, using the Ping/Pong clock offset (plan section 3).
    fn record_input_latency(&mut self, ev: &pb::InputEvent) {
        if !matches!(
            ev.ev,
            Some(Ev::Key(_) | Ev::Rel(_) | Ev::Btn(_) | Ev::Abs(_) | Ev::Wheel(_))
        ) {
            return;
        }
        let Some(local) = self.clock.to_local(ev.ts_us) else {
            return;
        };
        let lat = crate::clock::now_us()
            .saturating_sub(local)
            .min(u64::from(u32::MAX)) as u32;
        self.input_latency.push_back(lat);
        while self.input_latency.len() > 2000 {
            self.input_latency.pop_front();
        }
    }

    pub(crate) fn input_latency_percentiles(&self) -> (Option<u32>, Option<u32>, Option<u32>) {
        if self.input_latency.is_empty() {
            return (None, None, None);
        }
        let mut v: Vec<u32> = self.input_latency.iter().copied().collect();
        v.sort_unstable();
        let at = |q: f64| v[(((v.len() - 1) as f64) * q).round() as usize];
        (Some(at(0.5)), Some(at(0.95)), v.last().copied())
    }

    pub(crate) fn on_emergency(&mut self) {
        let actions = self.router.force_local();
        self.exec_actions(actions, true);
        self.platform.injector.release_all();
        self.on_feature_emergency();
    }

    // ---------------------------------------------------------------------------------------
    // layout
    // ---------------------------------------------------------------------------------------

    fn default_side(&self) -> Edge {
        match self.cfg.layout.default_side.as_str() {
            "left" => Edge::Left,
            "top" => Edge::Top,
            "bottom" => Edge::Bottom,
            _ => Edge::Right,
        }
    }

    /// Recomputes the layout from screens + placements. The input source owns the placements.
    pub(crate) fn rebuild_layout(&mut self, send: bool) {
        let Some(s) = &self.session else {
            self.layout = Layout::default();
            self.router.set_layout(Layout::default());
            return;
        };
        let own = self.own_id();
        let peer = s.link.peer.id;
        let devices = [
            (own, self.local_screens.clone()),
            (peer, s.remote_screens.clone()),
        ];
        let mut changed = false;
        let placements: Vec<pb::Placement> = if self.cfg.device.input_source {
            let defaults =
                Layout::default_placements(&devices[0], &devices[1], self.default_side());
            // keep known placements, add missing ones
            for d in defaults {
                let known = self
                    .cfg
                    .layout
                    .placement
                    .iter()
                    .any(|p| p.device_id == d.device_id && p.screen_id == d.screen_id);
                if !known {
                    self.cfg.layout.placement.push(PlacementConfig {
                        device_id: d.device_id,
                        screen_id: d.screen_id,
                        x: d.x,
                        y: d.y,
                    });
                    changed = true;
                }
            }
            if changed {
                self.cfg.layout.revision += 1;
                self.persist_config();
            }
            self.cfg
                .layout
                .placement
                .iter()
                .map(|p| pb::Placement {
                    device_id: p.device_id.clone(),
                    screen_id: p.screen_id,
                    x: p.x,
                    y: p.y,
                })
                .collect()
        } else {
            self.peer_placements.clone()
        };
        let revision = if self.cfg.device.input_source {
            self.cfg.layout.revision
        } else {
            self.peer_layout_revision
        };
        self.layout = Layout::from_parts(&devices, &placements, revision);
        self.router.set_layout(self.layout.clone());
        if let Some(r) = self.receiver.as_mut() {
            r.set_layout(self.layout.clone());
        }
        if (send || changed)
            && self.cfg.device.input_source
            && let Some(s) = &self.session
        {
            s.link
                .send_control(pb::control::Msg::Layout(pb::LayoutUpdate {
                    placements,
                    revision,
                }));
        }
        self.dirty = true;
    }

    pub(crate) fn on_layout_update(&mut self, l: pb::LayoutUpdate) {
        if self.cfg.device.input_source {
            return; // we own the layout
        }
        self.peer_placements = l.placements;
        self.peer_layout_revision = l.revision;
        self.rebuild_layout(false);
    }

    pub(crate) fn set_layout_from_ui(&mut self, placements: Vec<crate::ipc::PlacementDto>) {
        if !self.cfg.device.input_source {
            return;
        }
        self.cfg.layout.placement = placements
            .into_iter()
            .map(|p| PlacementConfig {
                device_id: p.device_id,
                screen_id: p.screen_id,
                x: p.x,
                y: p.y,
            })
            .collect();
        self.cfg.layout.revision += 1;
        self.persist_config();
        self.rebuild_layout(true);
    }

    /// Converts the remote hello screens (kept for tests).
    #[allow(dead_code)]
    pub(crate) fn remote_screens_from_hello(
        h: &pb::Hello,
    ) -> Vec<deskbridge_platform::screen::ScreenInfo> {
        h.screens.iter().map(screen_from_pb).collect()
    }
}
