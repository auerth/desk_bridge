//! In-process round trips over *test-named* pipes: real server, real overlapped client, mock
//! injector. Needs neither SYSTEM nor the SCM and never touches the real helper/service pipes.
#![cfg(windows)]

use deskbridge_platform::input::Button;
use deskbridge_platform::mock::{InjectedOp, MockInjector};
use deskbridge_service::client::PipeClient;
use deskbridge_service::control::{
    ControlHandler, HelperStatus, SasError, SasSender, SharedHelperStatus,
};
use deskbridge_service::helper::{DesktopFollower, HelperHandler, run_worker};
use deskbridge_service::pipe_security::PipeAccess;
use deskbridge_service::pipe_server::{AccessFn, Handler, serve};
use deskbridge_service::protocol::{ErrorCode, PROTOCOL_VERSION, Request, Response};
use deskbridge_service::winutil::current_user_sid;
use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Notify, watch};

const T: Duration = Duration::from_secs(5);

fn test_pipe_name(tag: &str) -> String {
    static N: AtomicU32 = AtomicU32::new(0);
    format!(
        r"\\.\pipe\deskbridge-test-{}-{}-{tag}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    )
}

/// SYSTEM + the current user; the integrity label is off because the test process need not be
/// elevated.
fn test_access() -> AccessFn {
    let sid = current_user_sid();
    Arc::new(move || PipeAccess {
        user_sid: sid.clone(),
        require_high_integrity: false,
    })
}

struct Server {
    name: String,
    stop: watch::Sender<bool>,
}

impl Server {
    fn start(tag: &str, handler: Arc<dyn Handler>) -> Self {
        let name = test_pipe_name(tag);
        let (stop, rx) = watch::channel(false);
        tokio::spawn(serve(
            name.clone(),
            test_access(),
            handler,
            rx,
            Arc::new(Notify::new()),
        ));
        Self { name, stop }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}

async fn blocking<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> R {
    tokio::task::spawn_blocking(f).await.unwrap()
}

/// Connects, retrying while the server task has not created its first instance yet.
fn connect(name: &str) -> PipeClient {
    let deadline = Instant::now() + T;
    loop {
        match PipeClient::connect(name, Duration::from_secs(1)) {
            Ok(c) => return c,
            Err(e) if Instant::now() < deadline => {
                let _ = e;
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => panic!("cannot connect to {name}: {e}"),
        }
    }
}

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + T;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

struct NoFollow;
impl DesktopFollower for NoFollow {
    fn follow(&mut self) -> bool {
        false
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn helper_pipe_injects_commands_and_releases_on_disconnect() {
    let (tx, rx) = std::sync::mpsc::channel();
    let inj = Arc::new(MockInjector::default());
    let worker = {
        let inj = inj.clone();
        std::thread::spawn(move || run_worker(&rx, inj.as_ref(), &mut NoFollow))
    };
    let server = Server::start("helper", Arc::new(HelperHandler::new(tx)));
    let name = server.name.clone();
    let inj2 = inj.clone();

    blocking(move || {
        let mut c = connect(&name);
        match c.call(&Request::Ping, T).unwrap() {
            Response::Pong { version, pid } => {
                assert_eq!(version, PROTOCOL_VERSION);
                assert_eq!(pid, std::process::id());
            }
            other => panic!("{other:?}"),
        }
        // Fire-and-forget commands, then a Ping as ordering barrier on the connection.
        c.send(
            &Request::Key {
                hid: 0xE0,
                down: true,
            },
            T,
        )
        .unwrap(); // left ctrl
        c.send(
            &Request::Key {
                hid: 0x04,
                down: true,
            },
            T,
        )
        .unwrap();
        c.send(&Request::MouseRel { dx: 5, dy: -7 }, T).unwrap();
        c.send(&Request::MouseAbs { x: 100, y: 200 }, T).unwrap();
        c.send(
            &Request::Button {
                button: Button::Left,
                down: true,
            },
            T,
        )
        .unwrap();
        c.send(&Request::Wheel { dx: 0, dy: -120 }, T).unwrap();
        assert!(matches!(
            c.call(&Request::Ping, T).unwrap(),
            Response::Pong { .. }
        ));
        wait_until("injected ops", || inj2.ops().len() >= 6);
        assert_eq!(
            inj2.ops(),
            vec![
                InjectedOp::Key {
                    hid: 0xE0,
                    down: true
                },
                InjectedOp::Key {
                    hid: 0x04,
                    down: true
                },
                InjectedOp::MouseRel { dx: 5, dy: -7 },
                InjectedOp::MouseAbs { x: 100, y: 200 },
                InjectedOp::Button {
                    button: Button::Left,
                    down: true
                },
                InjectedOp::Wheel { dx: 0, dy: -120 },
            ]
        );
        assert_eq!(inj2.pressed_keys(), vec![0x04, 0xE0]);

        // Control commands are not served here.
        assert!(matches!(
            c.call(&Request::SendSas, T).unwrap(),
            Response::Error {
                code: ErrorCode::BadRequest,
                ..
            }
        ));

        // The "agent crash": the connection just goes away while keys are held. The helper must
        // lift them on its own.
        drop(c);
        wait_until("release on disconnect", || inj2.pressed_keys().is_empty());
    })
    .await;

    drop(server);
    // The server task owns the handler (and with it the channel sender); give it a moment to
    // drop, then the worker ends.
    tokio::time::sleep(Duration::from_millis(200)).await;
    drop(inj);
    let _ = worker;
}

struct CountingSas(Mutex<u32>);
impl SasSender for CountingSas {
    fn send(&self) -> Result<(), SasError> {
        *self.0.lock() += 1;
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn control_pipe_ping_sas_and_helper_status() {
    let sas = Arc::new(CountingSas(Mutex::new(0)));
    let helper: SharedHelperStatus = Arc::new(Mutex::new(HelperStatus {
        running: true,
        pid: 1234,
        session: 2,
    }));
    let server = Server::start(
        "control",
        Arc::new(ControlHandler {
            sas: sas.clone(),
            helper,
        }),
    );
    let name = server.name.clone();
    let sas2 = sas.clone();
    blocking(move || {
        let mut c = connect(&name);
        assert!(matches!(
            c.call(&Request::Ping, T).unwrap(),
            Response::Pong { .. }
        ));
        assert_eq!(c.call(&Request::SendSas, T).unwrap(), Response::Ok);
        assert_eq!(*sas2.0.lock(), 1);
        assert_eq!(
            c.call(&Request::HelperStatus, T).unwrap(),
            Response::HelperStatus {
                running: true,
                pid: 1234,
                session: 2
            }
        );
        // Injection is refused on the control pipe (the call may not expect an answer, so use
        // Ping afterwards to prove the connection is still healthy).
        c.send(&Request::ReleaseAll, T).unwrap();
        assert!(matches!(
            c.call(&Request::Ping, T).unwrap(),
            Response::Pong { .. }
        ));
    })
    .await;
    drop(server);
}

/// A handler that never answers, to exercise the client timeout.
struct Mute;
impl Handler for Mute {
    fn handle(&self, _req: Request) -> Option<Response> {
        None
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_times_out_on_a_silent_server_and_reports_missing_pipes() {
    let server = Server::start("mute", Arc::new(Mute));
    let name = server.name.clone();
    blocking(move || {
        let mut c = connect(&name);
        let t0 = Instant::now();
        let err = c
            .call(&Request::Ping, Duration::from_millis(200))
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut, "{err}");
        assert!(t0.elapsed() < Duration::from_secs(3));
        // The connection stays usable for sends after a timed-out read.
        c.send(&Request::ReleaseAll, T).unwrap();

        let missing = test_pipe_name("does-not-exist");
        let err = PipeClient::connect(&missing, Duration::from_millis(100))
            .err()
            .expect("must fail");
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound, "{err}");
        assert!(!deskbridge_service::client::ping(
            &missing,
            Duration::from_millis(100)
        ));
    })
    .await;
    drop(server);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_drops_clients_that_send_garbage_and_keeps_serving() {
    let sas = Arc::new(CountingSas(Mutex::new(0)));
    let helper: SharedHelperStatus = Arc::new(Mutex::new(HelperStatus::default()));
    let server = Server::start("garbage", Arc::new(ControlHandler { sas, helper }));
    let name = server.name.clone();
    blocking(move || {
        use std::io::{Read, Write};
        let open_raw = || {
            let deadline = Instant::now() + T;
            loop {
                match std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&name)
                {
                    Ok(f) => return f,
                    Err(e) if Instant::now() < deadline => {
                        let _ = e;
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(e) => panic!("cannot open {name}: {e}"),
                }
            }
        };
        // 1. absurd length prefix: the server closes the connection.
        let mut raw = open_raw();
        raw.write_all(&[0xFF, 0xFF, 1, 2, 3]).unwrap();
        let mut buf = [0u8; 8];
        assert_eq!(raw.read(&mut buf).unwrap_or(0), 0);
        drop(raw);

        // 2. well-framed but unknown tag: BadRequest answer, connection stays usable.
        let mut raw = open_raw();
        raw.write_all(&[1, 0, 0x7F]).unwrap();
        let mut buf = [0u8; 64];
        let n = raw.read(&mut buf).unwrap();
        let mut fb = deskbridge_service::protocol::FrameBuffer::default();
        fb.push(&buf[..n]);
        let payload = fb.next_frame().unwrap().expect("one frame");
        assert!(matches!(
            Response::decode(&payload).unwrap(),
            Response::Error {
                code: ErrorCode::BadRequest,
                ..
            }
        ));
        raw.write_all(&Request::Ping.encode()).unwrap();
        let n = raw.read(&mut buf).unwrap();
        fb.push(&buf[..n]);
        let payload = fb.next_frame().unwrap().expect("pong frame");
        assert!(matches!(
            Response::decode(&payload).unwrap(),
            Response::Pong { .. }
        ));
        drop(raw);

        // 3. the server still serves regular clients.
        assert!(deskbridge_service::client::ping(&name, T));
    })
    .await;
    drop(server);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_recreates_the_pending_instance() {
    let access_calls = Arc::new(AtomicU32::new(0));
    let name = test_pipe_name("refresh");
    let (stop, rx) = watch::channel(false);
    let refresh = Arc::new(Notify::new());
    let access: AccessFn = {
        let calls = access_calls.clone();
        let sid = current_user_sid();
        Arc::new(move || {
            calls.fetch_add(1, Ordering::SeqCst);
            PipeAccess {
                user_sid: sid.clone(),
                require_high_integrity: false,
            }
        })
    };
    tokio::spawn(serve(
        name.clone(),
        access,
        Arc::new(Mute),
        rx,
        refresh.clone(),
    ));
    let n2 = name.clone();
    blocking(move || drop(connect(&n2))).await;
    let before = access_calls.load(Ordering::SeqCst);
    refresh.notify_one();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        access_calls.load(Ordering::SeqCst) > before,
        "ACL was not re-evaluated"
    );
    // still reachable afterwards
    let n3 = name.clone();
    blocking(move || drop(connect(&n3))).await;
    let _ = stop.send(true);
}
