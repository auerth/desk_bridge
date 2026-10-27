//! The secure-desktop helper (`deskbridge-service.exe --helper`).
//!
//! A SYSTEM process in the console session whose startup desktop is `winsta0\Winlogon`. It serves
//! `\\.\pipe\deskbridge-helper` and injects the agent's commands with `SendInput`, after making
//! sure its injector thread is attached to the *current input desktop* (UAC consent, lock screen,
//! logon screen).
//!
//! Threading: all pipe I/O runs on tokio; every command is forwarded over a channel to one plain
//! thread ([`run_worker`]) that owns the injector and the desktop attachment, because
//! `SetThreadDesktop` is per thread and `SendInput` must come from the attached thread.
//!
//! The secure-desktop picture (`SecureStart` / `SecurePoll` / `SecureStop`, protocol 2) is served
//! by [`SecureCapture`] on its own threads and never passes through the injection worker.

use crate::desktop::ThreadDesktop;
use crate::pipe_server::Handler;
use crate::protocol::{ErrorCode, PROTOCOL_VERSION, Request, Response};
use crate::secure_capture::SecureCapture;
use deskbridge_platform::input::InputInjector;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

/// What the worker thread processes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmd {
    Input(Request),
    /// Sent when the agent disconnects (or crashes): release everything.
    ReleaseAll,
}

/// Pipe-side handler: validates and forwards, never blocks on injection.
pub struct HelperHandler {
    tx: parking_lot::Mutex<Sender<Cmd>>,
    /// `None`: the capture requests are answered with `Unsupported`.
    capture: Option<Arc<dyn SecureCapture>>,
}

impl HelperHandler {
    /// Input only (no secure-desktop picture).
    #[must_use]
    pub fn new(tx: Sender<Cmd>) -> Self {
        Self {
            tx: parking_lot::Mutex::new(tx),
            capture: None,
        }
    }

    /// Input plus the secure-desktop picture.
    #[must_use]
    pub fn with_capture(tx: Sender<Cmd>, capture: Arc<dyn SecureCapture>) -> Self {
        Self {
            capture: Some(capture),
            ..Self::new(tx)
        }
    }

    fn capture_request(&self, req: Request) -> Response {
        let Some(capture) = &self.capture else {
            return Response::error(ErrorCode::Unsupported, "secure capture not available");
        };
        match req {
            Request::SecureStart { x, y, w, h } => capture.start((x, y, w, h)),
            Request::SecurePoll {
                generation,
                wait_ms,
            } => capture.poll(generation, Duration::from_millis(u64::from(wait_ms))),
            _ => {
                capture.stop();
                Response::Ok
            }
        }
    }
}

impl Handler for HelperHandler {
    fn handle(&self, req: Request) -> Option<Response> {
        match req {
            Request::Ping => Some(Response::Pong {
                version: PROTOCOL_VERSION,
                pid: std::process::id(),
            }),
            Request::SendSas
            | Request::HelperStatus
            | Request::CableMic { .. }
            | Request::MakeNetworkPrivate => Some(Response::error(
                ErrorCode::BadRequest,
                "not served by the helper pipe",
            )),
            Request::ShowRealDisplays => Some(show_real_displays()),
            capture @ (Request::SecureStart { .. }
            | Request::SecurePoll { .. }
            | Request::SecureStop) => Some(self.capture_request(capture)),
            injection => {
                // A closed channel means the worker died; the helper process is restarted by the
                // service, nothing sensible to answer for fire-and-forget commands.
                let _ = self.tx.lock().send(Cmd::Input(injection));
                None
            }
        }
    }

    fn on_disconnect(&self) {
        let _ = self.tx.lock().send(Cmd::ReleaseAll);
    }

    fn on_capture_disconnect(&self) {
        // The agent ended or crashed: nobody polls any more.
        if let Some(c) = &self.capture {
            c.stop();
        }
    }
}

/// Follows the input desktop for the worker (mockable).
pub trait DesktopFollower {
    /// Attach to the current input desktop; `true` if the attachment changed.
    fn follow(&mut self) -> bool;
}

/// Real implementation on top of [`ThreadDesktop`].
pub struct SystemDesktopFollower {
    desktop: ThreadDesktop,
    last_error: Option<u32>,
}

impl SystemDesktopFollower {
    #[must_use]
    pub fn new() -> Self {
        Self {
            desktop: ThreadDesktop::default(),
            last_error: None,
        }
    }
}

impl Default for SystemDesktopFollower {
    fn default() -> Self {
        Self::new()
    }
}

impl DesktopFollower for SystemDesktopFollower {
    fn follow(&mut self) -> bool {
        match self.desktop.follow() {
            Ok(changed) => {
                self.last_error = None;
                if changed {
                    tracing::info!(desktop = ?self.desktop.current_name(), "input desktop changed");
                }
                changed
            }
            Err(code) => {
                // Log each distinct error once; the injector still tries on the old desktop.
                if self.last_error != Some(code) {
                    tracing::warn!("cannot attach to the input desktop (error {code})");
                    self.last_error = Some(code);
                }
                false
            }
        }
    }
}

/// Minimum time between two input-desktop probes (mouse motion arrives at up to 1 kHz).
const FOLLOW_INTERVAL: Duration = Duration::from_millis(40);

/// Applies one request to an injector.
pub fn apply(inj: &dyn InputInjector, req: &Request) {
    match *req {
        Request::Key { hid, down } => inj.key(hid, down),
        Request::MouseRel { dx, dy } => inj.mouse_rel(dx, dy),
        Request::MouseAbs { x, y } => inj.mouse_abs_px(x, y),
        Request::Button { button, down } => inj.button(button, down),
        Request::Wheel { dx, dy } => inj.wheel(dx, dy),
        Request::ReleaseAll => inj.release_all(),
        Request::Ping
        | Request::SendSas
        | Request::HelperStatus
        | Request::CableMic { .. }
        | Request::MakeNetworkPrivate
        | Request::ShowRealDisplays
        | Request::SecureStart { .. }
        | Request::SecurePoll { .. }
        | Request::SecureStop => {}
    }
}

/// `ShowRealDisplays`: on a fresh thread attached to the current input desktop (lock screen,
/// UAC, logon), where `SetDisplayConfig` is allowed for this SYSTEM process in the console
/// session, unlike for the agent on the user desktop.
#[cfg(windows)]
fn show_real_displays() -> Response {
    let r = std::thread::Builder::new()
        .name("helper-displays".into())
        .spawn(|| {
            let mut desk = ThreadDesktop::default();
            if let Err(code) = desk.follow() {
                tracing::warn!(
                    "cannot attach to the input desktop for the display switch (error {code})"
                );
            }
            deskbridge_platform_windows::display::activate_all_displays()
        })
        .map_err(|e| e.to_string())
        .and_then(|h| h.join().map_err(|_| "display thread panicked".to_string()));
    match r {
        Ok(Ok(())) => {
            tracing::info!("real displays activated on the input desktop");
            Response::Ok
        }
        Ok(Err(e)) => {
            tracing::warn!("activating the real displays failed: {e}");
            Response::error(ErrorCode::Failed, e.to_string())
        }
        Err(e) => Response::error(ErrorCode::Failed, e),
    }
}

#[cfg(not(windows))]
fn show_real_displays() -> Response {
    Response::error(ErrorCode::Unsupported, "Windows only")
}

/// Worker loop: runs until the sender side is dropped.
pub fn run_worker(rx: &Receiver<Cmd>, inj: &dyn InputInjector, follower: &mut dyn DesktopFollower) {
    let mut last_follow: Option<Instant> = None;
    // Attach once up front so the first command does not pay for it.
    follower.follow();
    loop {
        let cmd = match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(c) => c,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        if last_follow.is_none_or(|t| t.elapsed() >= FOLLOW_INTERVAL) {
            last_follow = Some(Instant::now());
            if follower.follow() {
                // Keys held on the previous desktop must not stay logically pressed.
                inj.release_all();
            }
        }
        match cmd {
            Cmd::Input(req) => apply(inj, &req),
            Cmd::ReleaseAll => inj.release_all(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deskbridge_platform::input::Button;
    use deskbridge_platform::mock::{InjectedOp, MockInjector};
    use std::sync::Arc;
    use std::sync::mpsc::channel;

    struct FakeFollower {
        changes: Vec<bool>,
        calls: usize,
    }
    impl DesktopFollower for FakeFollower {
        fn follow(&mut self) -> bool {
            let r = self.changes.get(self.calls).copied().unwrap_or(false);
            self.calls += 1;
            r
        }
    }

    #[test]
    fn handler_forwards_injection_and_answers_control() {
        let (tx, rx) = channel();
        let h = HelperHandler::new(tx);
        assert!(matches!(
            h.handle(Request::Ping),
            Some(Response::Pong { .. })
        ));
        assert!(matches!(
            h.handle(Request::SendSas),
            Some(Response::Error {
                code: ErrorCode::BadRequest,
                ..
            })
        ));
        assert_eq!(h.handle(Request::Key { hid: 4, down: true }), None);
        h.on_disconnect();
        assert_eq!(
            rx.try_recv().unwrap(),
            Cmd::Input(Request::Key { hid: 4, down: true })
        );
        assert_eq!(rx.try_recv().unwrap(), Cmd::ReleaseAll);
        assert!(rx.try_recv().is_err());
    }

    #[derive(Default)]
    struct FakeCapture {
        calls: parking_lot::Mutex<Vec<String>>,
    }
    impl SecureCapture for FakeCapture {
        fn start(&self, rect: (i32, i32, u32, u32)) -> Response {
            self.calls.lock().push(format!("start {rect:?}"));
            Response::SecureStarted {
                luid: 9,
                x: rect.0,
                y: rect.1,
                w: rect.2,
                h: rect.3,
            }
        }
        fn poll(&self, generation: u32, wait: Duration) -> Response {
            self.calls
                .lock()
                .push(format!("poll {generation} {}ms", wait.as_millis()));
            Response::SecureNoFrame
        }
        fn stop(&self) {
            self.calls.lock().push("stop".into());
        }
    }

    #[test]
    fn capture_requests_are_served_without_touching_the_injection_worker() {
        let (tx, rx) = channel();
        let fake = Arc::new(FakeCapture::default());
        let h = HelperHandler::with_capture(tx, fake.clone());
        assert!(matches!(
            h.handle(Request::SecureStart {
                x: 1,
                y: 2,
                w: 3,
                h: 4
            }),
            Some(Response::SecureStarted { luid: 9, .. })
        ));
        assert_eq!(
            h.handle(Request::SecurePoll {
                generation: 7,
                wait_ms: 30
            }),
            Some(Response::SecureNoFrame)
        );
        assert_eq!(h.handle(Request::SecureStop), Some(Response::Ok));
        h.on_capture_disconnect();
        assert_eq!(
            *fake.calls.lock(),
            ["start (1, 2, 3, 4)", "poll 7 30ms", "stop", "stop"]
        );
        // nothing reached the injector, and a capture connection does not release keys
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn capture_requests_without_capture_are_unsupported() {
        let (tx, _rx) = channel();
        let h = HelperHandler::new(tx);
        for req in [
            Request::SecureStart {
                x: 0,
                y: 0,
                w: 1,
                h: 1,
            },
            Request::SecurePoll {
                generation: 0,
                wait_ms: 0,
            },
            Request::SecureStop,
        ] {
            assert!(matches!(
                h.handle(req),
                Some(Response::Error {
                    code: ErrorCode::Unsupported,
                    ..
                })
            ));
        }
    }

    #[test]
    fn worker_applies_commands_and_releases_on_disconnect() {
        let (tx, rx) = channel();
        let inj = Arc::new(MockInjector::default());
        tx.send(Cmd::Input(Request::Key {
            hid: 0xE0,
            down: true,
        }))
        .unwrap();
        tx.send(Cmd::Input(Request::MouseRel { dx: 3, dy: -4 }))
            .unwrap();
        tx.send(Cmd::Input(Request::MouseAbs { x: 10, y: 20 }))
            .unwrap();
        tx.send(Cmd::Input(Request::Button {
            button: Button::Right,
            down: true,
        }))
        .unwrap();
        tx.send(Cmd::Input(Request::Wheel { dx: 0, dy: 120 }))
            .unwrap();
        tx.send(Cmd::ReleaseAll).unwrap();
        drop(tx);
        run_worker(
            &rx,
            inj.as_ref(),
            &mut FakeFollower {
                changes: vec![],
                calls: 0,
            },
        );
        let ops = inj.ops();
        assert_eq!(
            ops[0],
            InjectedOp::Key {
                hid: 0xE0,
                down: true
            }
        );
        assert_eq!(ops[1], InjectedOp::MouseRel { dx: 3, dy: -4 });
        assert_eq!(ops[2], InjectedOp::MouseAbs { x: 10, y: 20 });
        assert_eq!(
            ops[3],
            InjectedOp::Button {
                button: Button::Right,
                down: true
            }
        );
        assert_eq!(ops[4], InjectedOp::Wheel { dx: 0, dy: 120 });
        assert!(
            inj.pressed_keys().is_empty(),
            "release_all must lift the key"
        );
    }

    #[test]
    fn worker_releases_everything_when_the_desktop_changes() {
        let (tx, rx) = channel();
        let inj = Arc::new(MockInjector::default());
        tx.send(Cmd::Input(Request::Key { hid: 4, down: true }))
            .unwrap();
        drop(tx);
        // first call (priming) = no change, second (before the first command) = changed
        run_worker(
            &rx,
            inj.as_ref(),
            &mut FakeFollower {
                changes: vec![false, true],
                calls: 0,
            },
        );
        assert_eq!(*inj.release_all_calls.lock(), 1);
        // the key pressed after the switch is still down
        assert_eq!(inj.pressed_keys(), vec![4]);
    }
}
