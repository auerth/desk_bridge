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
    c.content
        .lock()
        .iter()
        .find(|(m, _)| m == "text/plain")
        .map(|(_, d)| String::from_utf8_lossy(d).into_owned())
}

struct Node {
    h: EngineHandle,
    m: MockHandles,
    _dir: tempfile::TempDir,
}

async fn node(name: &str, is_pc: bool, w: u32, h: u32) -> Node {
    node_with(name, is_pc, w, h, |_| {}).await
}

async fn node_with(
    name: &str,
    is_pc: bool,
    w: u32,
    h: u32,
    tweak: impl FnOnce(&mut Config),
) -> Node {
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
    Node {
        h: handle,
        m,
        _dir: dir,
    }
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
        if sa.connection.state == ConnState::Connected
            && sb.connection.state == ConnState::Connected
        {
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
    connect_nodes(pc, lp).await
}

async fn connect_nodes(pc: Node, lp: Node) -> (Node, Node) {
    let code = lp.h.request(UiRequest::StartPairing).await.unwrap()["code"]
        .as_str()
        .unwrap()
        .to_string();
    let lp_addr = lp.h.local_addr();
    pc.h.request(UiRequest::PairWith {
        device_id: None,
        address: Some(lp_addr.to_string()),
        code,
    })
    .await
    .unwrap();

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
    pc.h.request(UiRequest::AddManualHost {
        host: lp_addr.to_string(),
    })
    .await
    .unwrap();
    lp.h.request(UiRequest::AddManualHost {
        host: pc.h.local_addr().to_string(),
    })
    .await
    .unwrap();
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
    pc.m.capture
        .feed(CaptureEvent::MouseMove { x: 2559, y: 720 });
    wait_for("pc grabs input", || {
        *pc.m.capture.grab.lock() == GrabMode::All
    })
    .await;
    let injector = lp.m.injector.clone();
    wait_for("laptop receives cursor", || {
        injector
            .ops()
            .iter()
            .any(|o| matches!(o, InjectedOp::MouseAbs { .. }))
    })
    .await;

    // keys and motion arrive on the laptop
    pc.m.capture.feed(CaptureEvent::Key {
        hid: hid::A,
        down: true,
    });
    pc.m.capture.feed(CaptureEvent::MouseRel { dx: 5, dy: 3 });
    wait_for("key injected", || injector.pressed_keys().contains(&hid::A)).await;
    wait_for("motion injected", || {
        injector
            .ops()
            .contains(&InjectedOp::MouseRel { dx: 5, dy: 3 })
    })
    .await;

    // UI reflects "input is on the other device"
    assert_eq!(pc.h.state().await.location, "remote");

    // Laptop cursor is pushed against its left edge → EdgeHit → PC takes input back.
    lp.m.screens
        .set_cursor(deskbridge_platform::screen::Point { x: 0, y: 540 });
    pc.m.capture.feed(CaptureEvent::MouseRel { dx: -4, dy: 0 });
    wait_for("pc grab released", || {
        *pc.m.capture.grab.lock() == GrabMode::None
    })
    .await;
    // Leaving releases whatever was pressed on the laptop.
    wait_for("laptop released keys", || {
        injector.pressed_keys().is_empty()
    })
    .await;
    assert_eq!(pc.h.state().await.location, "local");

    pc.h.stop();
    lp.h.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn connection_loss_never_leaves_keys_stuck() {
    let (pc, lp) = setup().await;
    pc.m.capture
        .feed(CaptureEvent::MouseMove { x: 2559, y: 720 });
    let injector = lp.m.injector.clone();
    wait_for("laptop active", || {
        injector
            .ops()
            .iter()
            .any(|o| matches!(o, InjectedOp::MouseAbs { .. }))
    })
    .await;
    pc.m.capture.feed(CaptureEvent::Key {
        hid: hid::L_SHIFT,
        down: true,
    });
    wait_for("shift down on laptop", || {
        injector.pressed_keys().contains(&hid::L_SHIFT)
    })
    .await;

    // "Pull the cable": the PC engine disappears while Shift is held.
    pc.h.stop();
    wait_for("laptop released everything", || {
        injector.pressed_keys().is_empty()
    })
    .await;
    lp.h.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wrong_pairing_code_is_rejected() {
    let pc = node("PC", true, 2560, 1440).await;
    let lp = node("Laptop", false, 1920, 1080).await;
    lp.h.request(UiRequest::StartPairing).await.unwrap();
    pc.h.request(UiRequest::PairWith {
        device_id: None,
        address: Some(lp.h.local_addr().to_string()),
        code: "000000".into(),
    })
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
    pc.m.capture
        .feed(CaptureEvent::MouseMove { x: 2559, y: 720 });
    wait_for("grab", || *pc.m.capture.grab.lock() == GrabMode::All).await;
    for k in [hid::L_CTRL, hid::L_ALT, hid::L_SHIFT, hid::BACKSPACE] {
        pc.m.capture.feed(CaptureEvent::Key { hid: k, down: true });
    }
    wait_for("grab released", || {
        *pc.m.capture.grab.lock() == GrabMode::None
    })
    .await;
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
    wait_for("text arrives on pc", || {
        text_of(&pc.m.clipboard).as_deref() == Some("und zurück")
    })
    .await;
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
    let code = lp.h.request(UiRequest::StartPairing).await.unwrap()["code"]
        .as_str()
        .unwrap()
        .to_string();
    pc.h.request(UiRequest::PairWith {
        device_id: None,
        address: Some(lp.h.local_addr().to_string()),
        code,
    })
    .await
    .unwrap();
    for _ in 0..100 {
        if !pc.h.state().await.peers.is_empty() && !lp.h.state().await.peers.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    pc.h.request(UiRequest::AddManualHost {
        host: lp.h.local_addr().to_string(),
    })
    .await
    .unwrap();
    lp.h.request(UiRequest::AddManualHost {
        host: pc.h.local_addr().to_string(),
    })
    .await
    .unwrap();
    wait_connected(&pc.h, &lp.h).await;

    let src = tempfile::tempdir().unwrap();
    let folder = src.path().join("Ordner");
    std::fs::create_dir_all(folder.join("sub")).unwrap();
    let big: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(folder.join("big.bin"), &big).unwrap();
    std::fs::write(folder.join("sub").join("klein.txt"), "hallo").unwrap();
    std::fs::write(src.path().join("einzel.txt"), "einzeln").unwrap();

    pc.h.request(UiRequest::SendFiles {
        paths: vec![
            folder.to_string_lossy().into_owned(),
            src.path().join("einzel.txt").to_string_lossy().into_owned(),
        ],
    })
    .await
    .unwrap();

    let target = recv_dir.path().to_path_buf();
    wait_for("files received", || {
        std::fs::read(target.join("Ordner").join("big.bin")).is_ok_and(|d| d == big)
            && std::fs::read_to_string(target.join("Ordner").join("sub").join("klein.txt"))
                .is_ok_and(|t| t == "hallo")
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
    assert!(
        lp.h.state()
            .await
            .transfers
            .iter()
            .any(|t| t.status == "done")
    );
    pc.h.stop();
    lp.h.stop();
}

fn sine_frame(n0: usize, frames: usize, channels: usize) -> Vec<f32> {
    let mut v = Vec::with_capacity(frames * channels);
    for i in 0..frames {
        let x = 0.4 * (2.0 * std::f32::consts::PI * 440.0 * (n0 + i) as f32 / 48_000.0).sin();
        for _ in 0..channels {
            v.push(x);
        }
    }
    v
}

fn rms(v: &[f32]) -> f32 {
    (v.iter().map(|x| x * x).sum::<f32>() / v.len().max(1) as f32).sqrt()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn laptop_audio_reaches_the_headset_and_pauses_when_it_disappears() {
    let (pc, lp) = setup().await;
    // PC → select a specific output device (like a wireless headset endpoint)
    let mut cfg: deskbridge_core::config::Config =
        serde_json::from_value(pc.h.request(UiRequest::GetConfig).await.unwrap()).unwrap();
    cfg.audio.laptop_to_pc.output_device = "out2".into();
    pc.h.request(UiRequest::SetConfig {
        config: Box::new(cfg),
    })
    .await
    .unwrap();
    wait_for("playback on out2", || {
        pc.m.audio_playback.active_device() == Some(Some("out2".to_string()))
    })
    .await;
    wait_for("laptop captures loopback", || {
        lp.m.audio_capture.active(true)
    })
    .await;

    // 1.5 s of audio at real-time pace
    let mut collected: Vec<f32> = Vec::new();
    let mut n = 0usize;
    for step in 0..150 {
        lp.m.audio_capture
            .push(true, sine_frame(n, 480, 2), 2, step as u64 * 10_000);
        n += 480;
        tokio::time::sleep(Duration::from_millis(10)).await;
        if let Some(out) = pc.m.audio_playback.pull(480) {
            collected.extend(out);
        }
    }
    let tail = &collected[collected.len() / 2..];
    assert!(
        rms(tail) > 0.1,
        "signal arrives on the headset (rms {})",
        rms(tail)
    );
    let st = pc.h.state().await;
    assert_eq!(
        st.features.laptop_audio.base.state,
        deskbridge_core::ipc::Live::Live
    );
    assert!(!st.features.laptop_audio.codec.is_empty());

    // headset powered off → paused, laptop stops capturing; powered on → resumes
    pc.m.audio_devices.remove("out2");
    wait_for("playback paused", || {
        pc.m.audio_playback.active_device().is_none()
    })
    .await;
    wait_for("laptop stops capture", || !lp.m.audio_capture.active(true)).await;
    assert!(pc.h.state().await.features.laptop_audio.device_missing);
    pc.m.audio_devices
        .add(deskbridge_platform::mock::audio_device(
            "out2",
            "Headset",
            deskbridge_platform::audio::DeviceKind::Render,
            false,
        ));
    wait_for("playback resumes", || {
        pc.m.audio_playback.active_device() == Some(Some("out2".to_string()))
    })
    .await;
    wait_for("laptop captures again", || lp.m.audio_capture.active(true)).await;
    pc.h.stop();
    lp.h.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn microphone_flows_only_while_the_laptop_uses_it() {
    let pc = node("PC", true, 2560, 1440).await;
    let lp = node("Laptop", false, 1920, 1080).await;
    lp.m.virtual_audio
        .available
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let (pc, lp) = connect_nodes(pc, lp).await;

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !pc.m.audio_capture.active(false),
        "mic must stay closed while no laptop app uses it"
    );

    lp.m.virtual_audio.set_in_use(true);
    wait_for("mic capture starts on pc", || {
        pc.m.audio_capture.active(false)
    })
    .await;
    let mut n = 0usize;
    for step in 0..120 {
        pc.m.audio_capture
            .push(false, sine_frame(n, 480, 1), 1, step as u64 * 10_000);
        n += 480;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    wait_for("virtual mic received audio", || {
        let w = lp.m.virtual_audio.mic_written.lock();
        w.len() > 480 * 20 && rms(&w[w.len() - 480 * 20..]) > 0.05
    })
    .await;
    assert!(
        pc.h.state()
            .await
            .features
            .mic
            .base
            .detail
            .unwrap_or_default()
            .contains("Laptop nutzt dein Mikro")
    );

    lp.m.virtual_audio.set_in_use(false);
    wait_for("mic capture stops again", || {
        !pc.m.audio_capture.active(false)
    })
    .await;
    pc.h.stop();
    lp.h.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mic_is_unavailable_without_a_driver() {
    let (pc, lp) = setup().await;
    let st = pc.h.state().await;
    assert_eq!(
        st.features.mic.base.state,
        deskbridge_core::ipc::Live::Unavailable
    );
    pc.h.stop();
    lp.h.stop();
}

async fn wait_event<F: FnMut(&deskbridge_core::ipc::UiEvent) -> bool>(
    rx: &mut tokio::sync::broadcast::Receiver<deskbridge_core::ipc::Push>,
    what: &str,
    mut f: F,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(deskbridge_core::ipc::Push::Event { event })) if f(&event) => return,
            Ok(Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
            _ => panic!("timeout waiting for event: {what}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn partner_update_is_offered_accepted_transferred_and_verified() {
    use deskbridge_core::ipc::UiEvent;
    let (pc, lp) = setup().await;
    let src = tempfile::tempdir().unwrap();
    let installer = src.path().join("DeskBridge-Setup-9.9.9.exe");
    let payload: Vec<u8> = (0..1_500_000u32).map(|i| (i * 7 % 256) as u8).collect();
    std::fs::write(&installer, &payload).unwrap();

    let mut lrx = lp.h.subscribe();
    pc.h.request(UiRequest::OfferPartnerUpdate {
        path: installer.to_string_lossy().into_owned(),
        version: "9.9.9".into(),
    })
    .await
    .unwrap();
    wait_event(
        &mut lrx,
        "offer reaches laptop",
        |e| matches!(e, UiEvent::PartnerUpdateOffered { version, .. } if version == "9.9.9"),
    )
    .await;
    lp.h.request(UiRequest::AnswerPartnerUpdate { accept: true })
        .await
        .unwrap();
    let mut got = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while got.is_none() {
        match tokio::time::timeout_at(deadline, lrx.recv()).await {
            Ok(Ok(deskbridge_core::ipc::Push::Event {
                event: UiEvent::PartnerUpdateReady { path, version },
            })) => got = Some((path, version)),
            Ok(_) => {}
            Err(_) => panic!("update never arrived"),
        }
    }
    let (path, version) = got.unwrap();
    assert_eq!(version, "9.9.9");
    assert_eq!(std::fs::read(path).unwrap(), payload);
    pc.h.stop();
    lp.h.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn focus_mode_streams_frames_and_restores_everything() {
    use std::sync::atomic::Ordering::SeqCst;
    let (pc, lp) = setup().await;
    let r =
        pc.h.request(UiRequest::SetFocus {
            mode: "full".into(),
        })
        .await;
    assert!(r.is_ok(), "focus start accepted: {r:?}");
    wait_for("viewer opens", || {
        pc.m.video.stats.presenter_open.load(SeqCst)
    })
    .await;
    wait_for("laptop captures", || {
        lp.m.video.stats.capturer_running.load(SeqCst)
    })
    .await;
    wait_for("frames presented on the pc", || {
        pc.m.video.stats.frames_presented.load(SeqCst) > 10
    })
    .await;
    assert!(lp.m.video.stats.keyframes_encoded.load(SeqCst) >= 1);
    assert!(
        lp.m.display.world().virtual_enabled,
        "virtual display is on while streaming"
    );
    let st = pc.h.state().await;
    assert_eq!(st.features.focus.phase, "streaming");
    assert_eq!(
        st.features.focus.base.state,
        deskbridge_core::ipc::Live::Live
    );
    // the pointer is mapped onto the stream while it runs
    pc.m.capture
        .feed(CaptureEvent::MouseMove { x: 100, y: 100 });
    wait_for("pc keyboard grabbed", || {
        *pc.m.capture.grab.lock() == GrabMode::KeyboardOnly
    })
    .await;

    pc.h.request(UiRequest::SetFocus { mode: "off".into() })
        .await
        .unwrap();
    wait_for("viewer closes", || {
        !pc.m.video.stats.presenter_open.load(SeqCst)
    })
    .await;
    wait_for("capture stops", || {
        !lp.m.video.stats.capturer_running.load(SeqCst)
    })
    .await;
    wait_for("virtual display off again", || {
        !lp.m.display.world().virtual_enabled
    })
    .await;
    assert!(
        !lp.m.display.ever_blackout(),
        "never left the laptop without a display"
    );
    wait_for("keyboard released", || {
        *pc.m.capture.grab.lock() == GrabMode::None
    })
    .await;
    pc.h.stop();
    lp.h.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn focus_mode_is_rolled_back_when_the_connection_drops() {
    use std::sync::atomic::Ordering::SeqCst;
    let (pc, lp) = setup().await;
    pc.h.request(UiRequest::SetFocus {
        mode: "split".into(),
    })
    .await
    .unwrap();
    wait_for("streaming", || {
        lp.m.video.stats.capturer_running.load(SeqCst)
            && pc.m.video.stats.frames_presented.load(SeqCst) > 3
    })
    .await;
    assert!(lp.m.display.world().virtual_enabled);
    pc.h.stop(); // cable pulled
    wait_for("laptop restores its display", || {
        !lp.m.display.world().virtual_enabled
    })
    .await;
    wait_for("capture stopped", || {
        !lp.m.video.stats.capturer_running.load(SeqCst)
    })
    .await;
    lp.h.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn focus_start_failure_on_the_laptop_leaves_the_pc_clean() {
    use deskbridge_display::mock::{Failure, Step};
    use std::sync::atomic::Ordering::SeqCst;
    let (pc, lp) = setup().await;
    lp.m.display.fail(Step::EnableVirtual, Failure::always());
    pc.h.request(UiRequest::SetFocus {
        mode: "full".into(),
    })
    .await
    .unwrap();
    for _ in 0..100 {
        if pc.h.state().await.features.focus.phase == "off"
            && lp.m.display.count(Step::EnableVirtual) > 0
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(pc.h.state().await.features.focus.phase, "off");
    assert!(!pc.m.video.stats.presenter_open.load(SeqCst));
    assert!(!lp.m.display.world().virtual_enabled);
    assert!(!lp.m.display.ever_blackout());
    pc.h.stop();
    lp.h.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn laptop_speakers_are_muted_while_streaming_and_restored_afterwards() {
    let (pc, lp) = setup().await;
    wait_for("laptop captures loopback", || {
        lp.m.audio_capture.active(true)
    })
    .await;
    wait_for("laptop speakers muted", || {
        lp.m.local_output.is_muted("out1")
    })
    .await;

    // Headset gone → the stream pauses → the laptop gets its speakers back.
    let mut cfg: deskbridge_core::config::Config =
        serde_json::from_value(pc.h.request(UiRequest::GetConfig).await.unwrap()).unwrap();
    cfg.audio.laptop_to_pc.enabled = false;
    pc.h.request(UiRequest::SetConfig {
        config: Box::new(cfg),
    })
    .await
    .unwrap();
    wait_for("laptop speakers restored", || {
        !lp.m.local_output.is_muted("out1")
    })
    .await;
    pc.h.stop();
    lp.h.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn laptop_speakers_are_restored_when_the_connection_drops() {
    let (pc, lp) = setup().await;
    wait_for("laptop speakers muted", || {
        lp.m.local_output.is_muted("out1")
    })
    .await;
    pc.h.stop(); // cable pulled
    wait_for("laptop speakers restored", || {
        !lp.m.local_output.is_muted("out1")
    })
    .await;
    lp.h.stop();
}

/// Explorer pasting virtual files: the laptop reads the PC's copied files through the provider,
/// including a partial read followed by a backward seek (a reopen) and a second pass.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clipboard_files_are_readable_through_the_virtual_provider() {
    let pc = node("PC", true, 2560, 1440).await;
    let lp = node_with("Laptop", false, 1920, 1080, |c| {
        c.clipboard.files_on_arrival = false;
    })
    .await;
    let (pc, lp) = connect_nodes(pc, lp).await;
    let src = tempfile::tempdir().unwrap();
    let big: Vec<u8> = (0..2_000_000u32).map(|i| (i % 253) as u8).collect();
    let small = b"hallo welt".to_vec();
    std::fs::write(src.path().join("big.bin"), &big).unwrap();
    std::fs::write(src.path().join("klein.txt"), &small).unwrap();
    pc.m.clipboard.user_copy_files(vec![
        src.path().join("big.bin"),
        src.path().join("klein.txt"),
    ]);
    wait_for("laptop offers virtual files", || {
        lp.m.clipboard.file_provider.lock().is_some()
    })
    .await;
    let offered = lp.m.clipboard.offered_files.lock().clone();
    assert_eq!(offered.len(), 2);
    let provider = lp.m.clipboard.file_provider.lock().clone().unwrap();
    let idx = |n: &str| offered.iter().position(|o| o.rel_path == n).unwrap();
    let (ib, is) = (idx("big.bin"), idx("klein.txt"));
    // A plain OS thread without Tokio context, like Explorer's clipboard thread.
    let (got_big, got_small, got_big_again) = tokio::task::spawn_blocking(move || {
        std::thread::spawn(move || {
            use std::io::Read;
            let read_all = |i: usize| {
                let mut v = Vec::new();
                provider.open(i).unwrap().read_to_end(&mut v).unwrap();
                v
            };
            // Probe: read a little, drop (as Explorer does for type detection) ...
            let mut head = [0u8; 16];
            provider.open(ib).unwrap().read_exact(&mut head).unwrap();
            // ... then the real copies.
            (read_all(ib), read_all(is), read_all(ib))
        })
        .join()
        .unwrap()
    })
    .await
    .unwrap();
    assert!(got_big == big, "big file differs ({} bytes)", got_big.len());
    assert_eq!(got_small, small);
    assert!(got_big_again == big);
    pc.h.stop();
    lp.h.stop();
}

/// Copying files on the PC must not touch the laptop's clipboard until the cursor arrives there.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn copied_files_take_over_the_clipboard_only_on_arrival() {
    let (pc, lp) = setup().await;
    let src = tempfile::tempdir().unwrap();
    std::fs::write(src.path().join("a.txt"), "hallo").unwrap();
    pc.m.clipboard
        .user_copy_files(vec![src.path().join("a.txt")]);
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(
        lp.m.clipboard.file_provider.lock().is_none(),
        "the laptop clipboard must stay untouched until the cursor arrives"
    );
    assert!(
        lp.h.state().await.transfers.is_empty(),
        "no transfer entry before anything is pasted"
    );
    // cursor pushes over the right edge of the PC -> arrives on the laptop
    pc.m.capture
        .feed(CaptureEvent::MouseMove { x: 2559, y: 720 });
    wait_for("files offered after arrival", || {
        lp.m.clipboard.file_provider.lock().is_some()
    })
    .await;
    pc.h.stop();
    lp.h.stop();
}
