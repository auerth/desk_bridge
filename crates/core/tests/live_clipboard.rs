//! Manual end-to-end check with the real Windows clipboard on the receiving side: the PC (mock)
//! copies files, the laptop engine offers them as virtual files on the real clipboard and a real
//! Explorer shell paste pulls them over the QUIC link. Overwrites the clipboard, so `#[ignore]`.
//!
//! `cargo test -p deskbridge-core --test live_clipboard -- --ignored --nocapture`
#![cfg(windows)]

use deskbridge_core::config::Config;
use deskbridge_core::engine::{self, EngineHandle, EngineParams};
use deskbridge_core::ipc::{ConnState, UiRequest};
use deskbridge_core::paths::AppPaths;
use deskbridge_core::platform::{MockHandles, Platform};
use deskbridge_platform_windows::clipboard::WinClipboard;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

async fn start(
    name: &str,
    is_pc: bool,
    real_clipboard: bool,
) -> (EngineHandle, MockHandles, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let (mut platform, m) = Platform::mock(1920, 1080);
    if real_clipboard {
        platform.clipboard = Arc::new(WinClipboard::new());
    }
    let mut config = Config::default();
    config.device.name = name.into();
    config.device.input_source = is_pc;
    config.device.headset_sink = is_pc;
    config.device.stream_source = !is_pc;
    let h = engine::start(EngineParams {
        paths: AppPaths::under(dir.path()),
        config,
        platform,
        bind: "127.0.0.1:0".parse().unwrap(),
        enable_discovery: false,
        identity: None,
    })
    .await
    .unwrap();
    (h, m, dir)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "overwrites the real clipboard"]
async fn explorer_paste_of_files_from_the_peer() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("deskbridge_core=debug,deskbridge_platform_windows=debug")
        .try_init();
    let (pc, pcm, _d1) = start("PC", true, false).await;
    let (lp, _lpm, _d2) = start("Laptop", false, true).await;
    let code = lp.request(UiRequest::StartPairing).await.unwrap()["code"]
        .as_str()
        .unwrap()
        .to_string();
    let lp_addr = lp.local_addr();
    pc.request(UiRequest::PairWith {
        device_id: None,
        address: Some(lp_addr.to_string()),
        code,
    })
    .await
    .unwrap();
    for _ in 0..100 {
        if !pc.state().await.peers.is_empty() && !lp.state().await.peers.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    pc.request(UiRequest::AddManualHost {
        host: lp_addr.to_string(),
    })
    .await
    .unwrap();
    lp.request(UiRequest::AddManualHost {
        host: pc.local_addr().to_string(),
    })
    .await
    .unwrap();
    for _ in 0..200 {
        if pc.state().await.connection.state == ConnState::Connected
            && lp.state().await.connection.state == ConnState::Connected
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let src = tempfile::tempdir().unwrap();
    let big: Vec<u8> = (0..5_000_000u32).map(|i| (i % 253) as u8).collect();
    std::fs::write(src.path().join("big.bin"), &big).unwrap();
    std::fs::write(src.path().join("klein.txt"), "hallo welt").unwrap();
    pcm.clipboard.user_copy_files(vec![
        src.path().join("big.bin"),
        src.path().join("klein.txt"),
    ]);
    tokio::time::sleep(Duration::from_secs(2)).await;

    let out = tempfile::tempdir().unwrap();
    let script = format!(
        "Add-Type -AssemblyName System.Windows.Forms; [void][Windows.Forms.Clipboard]::ContainsText(); $sh = New-Object -ComObject Shell.Application; $ns = $sh.Namespace('{}'); $ns.Self.InvokeVerb('paste'); Start-Sleep -Seconds 10",
        out.path().display()
    );
    let o = tokio::task::spawn_blocking(move || {
        Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-STA", "-Command", &script])
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    println!("powershell: {}", String::from_utf8_lossy(&o.stderr));
    let got_small = std::fs::read_to_string(out.path().join("klein.txt")).unwrap_or_default();
    let got_big = std::fs::read(out.path().join("big.bin")).unwrap_or_default();
    pc.stop();
    lp.stop();
    assert_eq!(got_small, "hallo welt", "klein.txt");
    assert!(got_big == big, "big.bin: got {} bytes", got_big.len());
}
