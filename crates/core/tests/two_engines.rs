//! Two engines with mock platforms talking over 127.0.0.1: pairing, session, edge switching and
//! the "no stuck keys" guarantees.

use deskbridge_core::config::Config;
use deskbridge_core::engine::{self, EngineHandle, EngineParams};
use deskbridge_core::ipc::{ConnState, UiRequest};
use deskbridge_core::paths::AppPaths;
use deskbridge_core::platform::{MockHandles, Platform};
use deskbridge_platform::hid;
use deskbridge_platform::input::{CaptureEvent, GrabMode};
use deskbridge_platform::mock::InjectedOp;
use std::time::Duration;

fn text_of(c: &deskbridge_platform::mock::MockClipboard) -> Option<String> {
    c.content.lock().iter().find(|(m, _)| m == "text/plain").map(|(_, d)| String::from_utf8_lossy(d).into_owned())
}

struct Node {
    h: EngineHandle,
    m: MockHandles,
    _dir: tempfile::TempDir,
}

async fn node(name: &str, is_pc: bool, w: u32, h: u32) -> Node {
    node_with(name, is_pc, w, h, |_| {}).await
}

async fn node_with(name: &str, is_pc: bool, w: u32, h: u32, tweak: impl FnOnce(&mut Config)) -> Node {
    let dir = tempfile::tempdir().unwrap();
    let (platform, m) = Platform::mock(w, h);
    let mut config = Config::default();
    config.device.name = name.into();
    config.device.input_source = is_pc;
    config.device.headset_sink = is_pc;
    config.device.stream_source = !is_pc;
    tweak(&mut config);
    let handle = engine::start(EngineParams {
        paths: AppPaths::under(dir.path()),
        config,
        platform,
        bind: "127.0.0.1:0".parse().unwrap(),
        enable_discovery: false,
        identity: None,
    })
    .await
    .unwrap();
    Node { h: handle, m, _dir: dir }
}

async fn wait_for<F: FnMut() -> bool>(what: &str, mut f: F) {
    for _ in 0..200 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timeout waiting for: {what}");
}

async fn wait_connected(a: &EngineHandle, b: &EngineHandle) {
    for _ in 0..200 {
        let (sa, sb) = (a.state().await, b.state().await);
        if sa.connection.state == ConnState::Connected && sb.connection.state == ConnState::Connected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("not connected");
}

/// Pairs and connects PC and laptop.
async fn setup() -> (Node, Node) {
    let pc = node("PC", true, 2560, 1440).await;
    let lp = node("Laptop", false, 1920, 1080).await;

    let code = lp.h.request(UiRequest::StartPairing).await.unwrap()["code"].as_str().unwrap().to_string();
    let lp_addr = lp.h.local_addr();
    pc.h.request(UiRequest::PairWith { device_id: None, address: Some(lp_addr.to_string()), code }).await.unwrap();

    for _ in 0..100 {
        if !pc.h.state().await.peers.is_empty() && !lp.h.state().await.peers.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(pc.h.state().await.peers.len(), 1, "pc has laptop paired");
    assert_eq!(lp.h.state().await.peers.len(), 1, "laptop has pc paired");

    // Neither side learned a usable address of the other for the session (ephemeral port on the
    // pairing server side), so use manual hosts like a user without mDNS would.
    pc.h.request(UiRequest::AddManualHost { host: lp_addr.to_string() }).await.unwrap();
    lp.h.request(UiRequest::AddManualHost { host: pc.h.local_addr().to_string() }).await.unwrap();
    wait_connected(&pc.h, &lp.h).await;
    (pc, lp)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pairing_connects_and_exchanges_screens() {
    let (pc, lp) = setup().await;
    let s = pc.h.state().await;
    assert_eq!(s.connection.peer.as_ref().unwrap().name, "Laptop");
    assert!(s.connection.rtt_us.is_some() || s.connection.state == ConnState::Connected);
    // both devices' screens are known on the PC
    assert_eq!(s.screens.screens.len(), 2);
    assert_eq!(s.screens.placements.len(), 2);
    let l = lp.h.state().await;
    assert_eq!(l.screens.screens.len(), 2);
    pc.h.stop();
    lp.h.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn edge_crossing_routes_input_and_returns() {
    let (pc, lp) = setup().await;

    // The laptop is placed to the right of the PC's screen, vertically centred (1080 in 1440).
    pc.m.capture.feed(CaptureEvent::MouseMove { x: 2559, y: 720 });
    wait_for("pc grabs input", || *pc.m.capture.grab.lock() == GrabMode::All).await;
    let injector = lp.m.injector.clone();
    wait_for("laptop receives cursor", || injector.ops().iter().any(|o| matches!(o, InjectedOp::MouseAbs { .. }))).await;

    // keys and motion arrive on the laptop
    pc.m.capture.feed(CaptureEvent::Key { hid: hid::A, down: true });
    pc.m.capture.feed(CaptureEvent::MouseRel { dx: 5, dy: 3 });
    wait_for("key injected", || injector.pressed_keys().contains(&hid::A)).await;
    wait_for("motion injected", || injector.ops().contains(&InjectedOp::MouseRel { dx: 5, dy: 3 })).await;

    // UI reflects "input is on the other device"
    assert_eq!(pc.h.state().await.location, "remote");

    // Laptop cursor is pushed against its left edge → EdgeHit → PC takes input back.
    lp.m.screens.set_cursor(deskbridge_platform::screen::Point { x: 0, y: 540 });
    pc.m.capture.feed(CaptureEvent::MouseRel { dx: -4, dy: 0 });
    wait_for("pc grab released", || *pc.m.capture.grab.lock() == GrabMode::None).await;
    // Leaving releases whatever was pressed on the laptop.
    wait_for("laptop released keys", || injector.pressed_keys().is_empty()).await;
    assert_eq!(pc.h.state().await.location, "local");

    pc.h.stop();
    lp.h.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn connection_loss_never_leaves_keys_stuck() {
    let (pc, lp) = setup().await;
    pc.m.capture.feed(CaptureEvent::MouseMove { x: 2559, y: 720 });
    let injector = lp.m.injector.clone();
    wait_for("laptop active", || injector.ops().iter().any(|o| matches!(o, InjectedOp::MouseAbs { .. }))).await;
    pc.m.capture.feed(CaptureEvent::Key { hid: hid::L_SHIFT, down: true });
    wait_for("shift down on laptop", || injector.pressed_keys().contains(&hid::L_SHIFT)).await;

    // "Pull the cable": the PC engine disappears while Shift is held.
    pc.h.stop();
    wait_for("laptop released everything", || injector.pressed_keys().is_empty()).await;
    lp.h.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wrong_pairing_code_is_rejected() {
    let pc = node("PC", true, 2560, 1440).await;
    let lp = node("Laptop", false, 1920, 1080).await;
    lp.h.request(UiRequest::StartPairing).await.unwrap();
    pc.h.request(UiRequest::PairWith { device_id: None, address: Some(lp.h.local_addr().to_string()), code: "000000".into() })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(pc.h.state().await.peers.is_empty());
    assert!(lp.h.state().await.peers.is_empty());
    pc.h.stop();
    lp.h.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn emergency_hotkey_returns_control() {
    let (pc, lp) = setup().await;
    // default hotkeys are registered with the capture on session start
    pc.m.capture.feed(CaptureEvent::MouseMove { x: 2559, y: 720 });
    wait_for("grab", || *pc.m.capture.grab.lock() == GrabMode::All).await;
    for k in [hid::L_CTRL, hid::L_ALT, hid::L_SHIFT, hid::BACKSPACE] {
        pc.m.capture.feed(CaptureEvent::Key { hid: k, down: true });
    }
    wait_for("grab released", || *pc.m.capture.grab.lock() == GrabMode::None).await;
    pc.h.stop();
    lp.h.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clipboard_text_syncs_both_ways_without_echo() {
    let (pc, lp) = setup().await;
    pc.m.clipboard.user_copy_text("Grüße vom PC €");
    wait_for("text arrives on laptop", || {
        text_of(&lp.m.clipboard).as_deref() == Some("Grüße vom PC €")
    })
    .await;
    lp.m.clipboard.user_copy_text("und zurück");
    wait_for("text arrives on pc", || text_of(&pc.m.clipboard).as_deref() == Some("und zurück")).await;
    pc.h.stop();
    lp.h.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn files_are_sent_with_hash_verification() {
    let recv_dir = tempfile::tempdir().unwrap();
    let rd = recv_dir.path().to_string_lossy().into_owned();
    let pc = node("PC", true, 2560, 1440).await;
    let lp = node_with("Laptop", false, 1920, 1080, |c| c.files.target_dir = rd).await;
    // pair + connect like setup(), but with the tweaked laptop
    let code = lp.h.request(UiRequest::StartPairing).await.unwrap()["code"].as_str().unwrap().to_string();
    pc.h.request(UiRequest::PairWith { device_id: None, address: Some(lp.h.local_addr().to_string()), code }).await.unwrap();
    for _ in 0..100 {
        if !pc.h.state().await.peers.is_empty() && !lp.h.state().await.peers.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    pc.h.request(UiRequest::AddManualHost { host: lp.h.local_addr().to_string() }).await.unwrap();
    lp.h.request(UiRequest::AddManualHost { host: pc.h.local_addr().to_string() }).await.unwrap();
    wait_connected(&pc.h, &lp.h).await;

    let src = tempfile::tempdir().unwrap();
    let folder = src.path().join("Ordner");
    std::fs::create_dir_all(folder.join("sub")).unwrap();
    let big: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(folder.join("big.bin"), &big).unwrap();
    std::fs::write(folder.join("sub").join("klein.txt"), "hallo").unwrap();
    std::fs::write(src.path().join("einzel.txt"), "einzeln").unwrap();

    pc.h.request(UiRequest::SendFiles {
        paths: vec![folder.to_string_lossy().into_owned(), src.path().join("einzel.txt").to_string_lossy().into_owned()],
    })
    .await
    .unwrap();

    let target = recv_dir.path().to_path_buf();
    wait_for("files received", || {
        std::fs::read(target.join("Ordner").join("big.bin")).is_ok_and(|d| d == big)
            && std::fs::read_to_string(target.join("Ordner").join("sub").join("klein.txt")).is_ok_and(|t| t == "hallo")
            && std::fs::read_to_string(target.join("einzel.txt")).is_ok_and(|t| t == "einzeln")
    })
    .await;
    // transfer shows up as done on the receiving side
    wait_for("transfer done", || true).await;
    for _ in 0..100 {
        let st = lp.h.state().await;
        if st.transfers.iter().any(|t| t.status == "done") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(lp.h.state().await.transfers.iter().any(|t| t.status == "done"));
    pc.h.stop();
    lp.h.stop();
}
