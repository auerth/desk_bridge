//! Secure-desktop capture of the helper (ADR 0007).
//!
//! The helper (SYSTEM) is the only process that may duplicate the secure desktop, but encoder and
//! network live in the agent. This module runs the duplication and publishes the pictures in
//! named GPU-shared textures (`deskbridge_platform_windows::video::secure`); the agent only gets
//! small control messages over the helper pipe (`SecureStart` / `SecurePoll` / `SecureStop`).
//!
//! Threading: the pipe handler talks to [`Shared`]; one *controller* thread repeatedly starts a
//! *session* thread. A session attaches to the input desktop, duplicates the output and pumps
//! pictures until the input desktop changes or the duplication is lost. A new desktop gets a new
//! thread because `SetThreadDesktop` fails once a thread owns D3D/DXGI objects. The input
//! injection thread is untouched: both simply follow the same input desktop.

use crate::desktop::{DesktopKind, ThreadDesktop, classify, probe_input_desktop};
use crate::protocol::{ErrorCode, Response};
use deskbridge_platform_windows::video::{Pointer, ProducerInfo, Pump, SecureProducer};
use parking_lot::{Condvar, Mutex};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Serves the three capture requests (mockable; the real one is [`SecureCaptureHost`]).
pub trait SecureCapture: Send + Sync {
    /// Starts (or restarts) the capture of the output at the desktop rectangle `(x, y, w, h)`.
    fn start(&self, rect: (i32, i32, u32, u32)) -> Response;
    /// Waits up to `wait` for something newer than `generation`.
    fn poll(&self, generation: u32, wait: Duration) -> Response;
    /// Ends the capture and drops the textures.
    fn stop(&self);
}

/// Longest wait a poll may ask for (it occupies a blocking thread).
pub const MAX_POLL_WAIT: Duration = Duration::from_millis(250);
/// How long `start` waits for the first session to come up.
const START_TIMEOUT: Duration = Duration::from_secs(4);
/// No poll for this long: the agent is gone or wedged, stop capturing.
const POLL_WATCHDOG: Duration = Duration::from_secs(10);
/// Wait inside `AcquireNextFrame` per pump.
const PUMP_TIMEOUT_MS: u32 = 20;
/// Minimum time between two input-desktop probes of a session.
const PROBE_INTERVAL: Duration = Duration::from_millis(40);
/// Sessions that fail faster than this count towards giving up.
const QUICK_FAILURE: Duration = Duration::from_secs(2);
const MAX_QUICK_FAILURES: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Started {
    luid: i64,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Latest {
    name: String,
    w: u32,
    h: u32,
}

#[derive(Default)]
struct State {
    started: Option<Started>,
    latest: Option<Latest>,
    pointer: Pointer,
    /// Bumped by every new picture, pointer change and new session.
    generation: u32,
    /// Value of `generation` at the last new picture.
    image_generation: u32,
    /// The input desktop is the user's: nothing to show (the agent captures it itself).
    user_desktop: bool,
    /// The capture gave up.
    failure: Option<String>,
    last_poll: Option<Instant>,
}

/// State shared by the pipe handler, the controller and the session threads.
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    stop: AtomicBool,
}

impl Shared {
    fn new() -> Self {
        Self {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            stop: AtomicBool::new(false),
        }
    }

    fn reset(&self) {
        *self.state.lock() = State::default();
        self.stop.store(false, Ordering::SeqCst);
    }

    /// A session came up on a new duplication: the old textures are gone.
    fn begin_session(&self, info: ProducerInfo) {
        let mut st = self.state.lock();
        st.started = Some(Started {
            luid: info.luid,
            x: info.x,
            y: info.y,
            w: info.w,
            h: info.h,
        });
        st.latest = None;
        st.pointer = Pointer::default();
        st.user_desktop = false;
        st.generation = st.generation.wrapping_add(1);
        self.changed.notify_all();
    }

    fn publish(&self, size: (u32, u32), image: Option<String>, pointer: Option<Pointer>) {
        let mut st = self.state.lock();
        st.generation = st.generation.wrapping_add(1);
        if let Some(name) = image {
            st.image_generation = st.generation;
            st.latest = Some(Latest {
                name,
                w: size.0,
                h: size.1,
            });
        }
        if let Some(p) = pointer {
            st.pointer = p;
        }
        self.changed.notify_all();
    }

    fn set_user_desktop(&self, on: bool) {
        let mut st = self.state.lock();
        st.user_desktop = on;
        self.changed.notify_all();
    }

    fn fail(&self, why: String) {
        let mut st = self.state.lock();
        st.failure = Some(why);
        self.changed.notify_all();
    }

    fn touch(&self) {
        self.state.lock().last_poll = Some(Instant::now());
    }

    fn polled_recently(&self) -> bool {
        self.state
            .lock()
            .last_poll
            .is_none_or(|t| t.elapsed() < POLL_WATCHDOG)
    }

    /// Waits until the first session is up (or the capture cannot start).
    fn wait_started(&self, timeout: Duration) -> Response {
        let deadline = Instant::now() + timeout;
        let mut st = self.state.lock();
        loop {
            if let Some(s) = st.started {
                return Response::SecureStarted {
                    luid: s.luid,
                    x: s.x,
                    y: s.y,
                    w: s.w,
                    h: s.h,
                };
            }
            if let Some(why) = &st.failure {
                return Response::error(ErrorCode::Failed, why.clone());
            }
            if st.user_desktop {
                return Response::error(
                    ErrorCode::Failed,
                    "the input desktop is the user's desktop",
                );
            }
            if self.changed.wait_until(&mut st, deadline).timed_out() {
                return Response::error(ErrorCode::Failed, "secure capture did not start in time");
            }
        }
    }

    /// The answer to one poll.
    fn poll(&self, generation: u32, wait: Duration) -> Response {
        let deadline = Instant::now() + wait.min(MAX_POLL_WAIT);
        let mut st = self.state.lock();
        st.last_poll = Some(Instant::now());
        loop {
            if let Some(why) = &st.failure {
                return Response::error(ErrorCode::Failed, why.clone());
            }
            if st.user_desktop {
                return Response::error(
                    ErrorCode::Failed,
                    "the input desktop is the user's desktop",
                );
            }
            if st.generation != generation
                && let Some(l) = &st.latest
            {
                return Response::SecureFrame {
                    generation: st.generation,
                    name: l.name.clone(),
                    w: l.w,
                    h: l.h,
                    pointer_x: st.pointer.x,
                    pointer_y: st.pointer.y,
                    pointer_visible: st.pointer.visible,
                    // wrapping "newer than": the polled generation is behind the last picture
                    image: st.image_generation.wrapping_sub(generation) as i32 > 0,
                };
            }
            if self.changed.wait_until(&mut st, deadline).timed_out() {
                return Response::SecureNoFrame;
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Real implementation
// ---------------------------------------------------------------------------------------------

/// How a session thread ended.
enum SessionEnd {
    Stopped,
    /// The input desktop changed or the duplication was lost: start over.
    Restart,
    /// The input desktop is the user's.
    UserDesktop,
    Failed(String),
}

/// The helper's secure-desktop capture.
pub struct SecureCaptureHost {
    /// DACL of the shared textures: SYSTEM and the interactive user.
    sddl: String,
    shared: Arc<Shared>,
    controller: Mutex<Option<JoinHandle<()>>>,
}

impl SecureCaptureHost {
    #[must_use]
    pub fn new(sddl: String) -> Self {
        Self {
            sddl,
            shared: Arc::new(Shared::new()),
            controller: Mutex::new(None),
        }
    }

    fn stop_controller(&self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        self.shared.changed.notify_all();
        let handle = self.controller.lock().take();
        if let Some(h) = handle {
            let _ = h.join();
        }
        self.shared.reset();
    }
}

impl Drop for SecureCaptureHost {
    fn drop(&mut self) {
        self.stop_controller();
    }
}

impl SecureCapture for SecureCaptureHost {
    fn start(&self, rect: (i32, i32, u32, u32)) -> Response {
        // A second start (the agent re-connected) replaces the first capture.
        self.stop_controller();
        if probe_input_desktop().kind == DesktopKind::Normal {
            return Response::error(ErrorCode::Failed, "the input desktop is the user's desktop");
        }
        self.shared.touch();
        let shared = self.shared.clone();
        let sddl = self.sddl.clone();
        let spawned = std::thread::Builder::new()
            .name("dbh-capture-ctl".into())
            .spawn(move || controller(&shared, rect, &sddl));
        match spawned {
            Ok(h) => *self.controller.lock() = Some(h),
            Err(e) => return Response::error(ErrorCode::Failed, format!("spawn: {e}")),
        }
        let answer = self.shared.wait_started(START_TIMEOUT);
        if matches!(answer, Response::Error { .. }) {
            self.stop_controller();
        }
        answer
    }

    fn poll(&self, generation: u32, wait: Duration) -> Response {
        if self.controller.lock().is_none() {
            return Response::error(ErrorCode::Failed, "secure capture is not running");
        }
        self.shared.poll(generation, wait)
    }

    fn stop(&self) {
        self.stop_controller();
    }
}

/// Runs sessions until stopped: a new one for every desktop switch, with a brake on repeated
/// quick failures.
fn controller(shared: &Arc<Shared>, rect: (i32, i32, u32, u32), sddl: &str) {
    let pid = std::process::id();
    let mut epoch = 0u32;
    let mut quick_failures = 0u32;
    while !shared.stop.load(Ordering::SeqCst) {
        epoch = epoch.wrapping_add(1);
        let prefix = format!("DeskBridgeSecure-{pid}-{epoch}");
        let began = Instant::now();
        let sh = shared.clone();
        let sddl = sddl.to_string();
        let joined = std::thread::Builder::new()
            .name("dbh-capture".into())
            .spawn(move || session(&sh, rect, &sddl, &prefix))
            .map_err(|e| e.to_string())
            .and_then(|h| h.join().map_err(|_| "capture thread panicked".to_string()));
        let end = match joined {
            // Closed here, after the thread is gone: the desktop is no longer in use.
            Ok((end, desktop)) => {
                drop(desktop);
                end
            }
            Err(e) => SessionEnd::Failed(e),
        };
        match end {
            SessionEnd::Stopped => return,
            SessionEnd::Restart => {
                quick_failures = 0;
                std::thread::sleep(Duration::from_millis(50));
            }
            SessionEnd::UserDesktop => {
                quick_failures = 0;
                shared.set_user_desktop(true);
                // Nothing to show until a secure desktop is up again.
                while !shared.stop.load(Ordering::SeqCst)
                    && probe_input_desktop().kind == DesktopKind::Normal
                {
                    std::thread::sleep(Duration::from_millis(100));
                }
                shared.set_user_desktop(false);
            }
            SessionEnd::Failed(why) => {
                tracing::warn!("secure capture session failed: {why}");
                quick_failures = if began.elapsed() < QUICK_FAILURE {
                    quick_failures + 1
                } else {
                    1
                };
                if quick_failures >= MAX_QUICK_FAILURES {
                    shared.fail(why);
                    return;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

/// One session on a fresh thread attached to the current input desktop.
fn session(
    shared: &Shared,
    rect: (i32, i32, u32, u32),
    sddl: &str,
    prefix: &str,
) -> (SessionEnd, ThreadDesktop) {
    let mut desktop = ThreadDesktop::default();
    if let Err(code) = desktop.follow() {
        let why = format!("cannot attach to the input desktop (error {code})");
        return (SessionEnd::Failed(why), desktop);
    }
    let attached = desktop.current_name().unwrap_or_default().to_string();
    if classify(Some(&attached), false) == DesktopKind::Normal {
        return (SessionEnd::UserDesktop, desktop);
    }
    let sd = match crate::pipe_security::SecurityDescriptor::from_sddl(sddl) {
        Ok(sd) => sd,
        Err(e) => return (SessionEnd::Failed(e.to_string()), desktop),
    };
    let sa = sd.attributes();
    let mut producer = match SecureProducer::open(rect, prefix, Some(&sa)) {
        Ok(p) => p,
        Err(e) => return (SessionEnd::Failed(e.to_string()), desktop),
    };
    let info = producer.info();
    tracing::info!(desktop = %attached, w = info.w, h = info.h, "secure capture session started");
    shared.begin_session(info);

    let mut last_probe = Instant::now();
    let end = loop {
        if shared.stop.load(Ordering::SeqCst) {
            break SessionEnd::Stopped;
        }
        if !shared.polled_recently() {
            tracing::warn!("no poll for {POLL_WATCHDOG:?}: the agent is gone, capture stops");
            shared.stop.store(true, Ordering::SeqCst);
            break SessionEnd::Stopped;
        }
        if last_probe.elapsed() >= PROBE_INTERVAL {
            last_probe = Instant::now();
            // `None` = the switch is in progress; the next probe decides.
            if probe_input_desktop()
                .name
                .is_some_and(|n| !n.eq_ignore_ascii_case(&attached))
            {
                break SessionEnd::Restart;
            }
        }
        match producer.pump(PUMP_TIMEOUT_MS) {
            Ok(Pump::Idle) => {}
            Ok(Pump::Update { image, pointer }) => shared.publish((info.w, info.h), image, pointer),
            Ok(Pump::Lost) => {
                tracing::debug!("secure capture: access lost, re-opening");
                break SessionEnd::Restart;
            }
            Err(e) => break SessionEnd::Failed(e.to_string()),
        }
    };
    (end, desktop)
}

#[cfg(test)]
mod tests {
    use super::*;

    const POINTER: Pointer = Pointer {
        x: 5,
        y: 6,
        visible: true,
    };

    fn frame(r: Response) -> (u32, String, bool, (i32, i32, bool)) {
        match r {
            Response::SecureFrame {
                generation,
                name,
                image,
                pointer_x,
                pointer_y,
                pointer_visible,
                ..
            } => (
                generation,
                name,
                image,
                (pointer_x, pointer_y, pointer_visible),
            ),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn nothing_new_answers_no_frame_after_the_wait() {
        let s = Shared::new();
        let t = Instant::now();
        assert_eq!(
            s.poll(0, Duration::from_millis(30)),
            Response::SecureNoFrame
        );
        assert!(t.elapsed() >= Duration::from_millis(25));
    }

    #[test]
    fn a_picture_is_delivered_once_and_the_pointer_follows_without_the_image_flag() {
        let s = Shared::new();
        s.publish((100, 50), Some("a-1".into()), Some(POINTER));
        let (g1, name, image, ptr) = frame(s.poll(0, Duration::ZERO));
        assert_eq!(
            (g1, name.as_str(), image, ptr),
            (1, "a-1", true, (5, 6, true))
        );
        // seen: nothing newer
        assert_eq!(s.poll(g1, Duration::ZERO), Response::SecureNoFrame);
        // pointer only: new generation, same texture, no new image
        s.publish(
            (100, 50),
            None,
            Some(Pointer {
                x: 9,
                y: 9,
                visible: false,
            }),
        );
        let (g2, name, image, ptr) = frame(s.poll(g1, Duration::ZERO));
        assert_eq!(
            (g2, name.as_str(), image, ptr),
            (2, "a-1", false, (9, 9, false))
        );
        // an agent that missed both still learns about the picture
        assert!(frame(s.poll(0, Duration::ZERO)).2);
    }

    #[test]
    fn a_waiting_poll_wakes_up_when_a_picture_is_published() {
        let s = Arc::new(Shared::new());
        let s2 = s.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            s2.publish((10, 10), Some("b-0".into()), None);
        });
        let started = Instant::now();
        let (_, name, image, _) = frame(s.poll(0, Duration::from_millis(200)));
        assert_eq!((name.as_str(), image), ("b-0", true));
        assert!(started.elapsed() < Duration::from_millis(190));
        t.join().unwrap();
    }

    #[test]
    fn a_new_session_drops_the_old_texture_name() {
        let s = Shared::new();
        s.publish((10, 10), Some("old-0".into()), None);
        s.begin_session(ProducerInfo {
            luid: 7,
            x: 0,
            y: 0,
            w: 10,
            h: 10,
        });
        // generation moved on but there is no picture of the new session yet
        assert_eq!(s.poll(1, Duration::ZERO), Response::SecureNoFrame);
        s.publish((10, 10), Some("new-0".into()), None);
        assert_eq!(frame(s.poll(1, Duration::ZERO)).1, "new-0");
    }

    #[test]
    fn user_desktop_and_failure_are_errors_for_the_poller() {
        let s = Shared::new();
        s.publish((10, 10), Some("a-0".into()), None);
        s.set_user_desktop(true);
        assert!(matches!(
            s.poll(0, Duration::ZERO),
            Response::Error {
                code: ErrorCode::Failed,
                ..
            }
        ));
        s.set_user_desktop(false);
        assert!(matches!(
            s.poll(0, Duration::ZERO),
            Response::SecureFrame { .. }
        ));
        s.fail("gone".into());
        assert!(matches!(
            s.poll(0, Duration::ZERO),
            Response::Error { text, .. } if text == "gone"
        ));
    }

    #[test]
    fn start_reports_the_first_session_or_the_reason_it_cannot() {
        let s = Shared::new();
        s.begin_session(ProducerInfo {
            luid: (1i64 << 32) | 2,
            x: -10,
            y: 20,
            w: 640,
            h: 480,
        });
        assert_eq!(
            s.wait_started(Duration::ZERO),
            Response::SecureStarted {
                luid: (1i64 << 32) | 2,
                x: -10,
                y: 20,
                w: 640,
                h: 480
            }
        );
        let s = Shared::new();
        assert!(matches!(
            s.wait_started(Duration::from_millis(10)),
            Response::Error { .. }
        ));
        s.fail("no gpu".into());
        assert!(matches!(
            s.wait_started(Duration::from_secs(1)),
            Response::Error { text, .. } if text == "no gpu"
        ));
    }

    #[test]
    fn poll_wait_is_capped() {
        let s = Shared::new();
        let t = Instant::now();
        assert_eq!(s.poll(0, Duration::from_secs(60)), Response::SecureNoFrame);
        assert!(t.elapsed() < MAX_POLL_WAIT + Duration::from_millis(200));
    }

    #[test]
    fn host_poll_without_start_is_an_error() {
        let h = SecureCaptureHost::new("D:P(A;;GA;;;SY)".into());
        assert!(matches!(
            h.poll(0, Duration::ZERO),
            Response::Error {
                code: ErrorCode::Failed,
                ..
            }
        ));
        h.stop();
    }
}
