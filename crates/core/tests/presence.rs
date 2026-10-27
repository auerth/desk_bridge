//! "Keep Teams online": input is registered only when the setting is on, Teams runs and the
//! machine has been idle for a while.

use deskbridge_core::config::Config;
use deskbridge_core::engine::{self, EngineHandle, EngineParams};
use deskbridge_core::paths::AppPaths;
use deskbridge_core::platform::{MockHandles, Platform};
use std::sync::atomic::Ordering;
use std::time::Duration;

async fn start(keep: bool, idle_secs: u64) -> (EngineHandle, MockHandles, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let (platform, m) = Platform::mock(1920, 1080);
    // Arranged before the engine starts: its first presence check runs right after start.
    m.presence.teams.store(true, Ordering::Relaxed);
    m.presence.idle_secs.store(idle_secs, Ordering::Relaxed);
    let mut config = Config::default();
    config.device.name = "Laptop".into();
    config.general.keep_teams_online = keep;
    let h = engine::start(EngineParams {
        paths: AppPaths::under(dir.path()),
        config,
        platform,
        bind: "127.0.0.1:0".parse().unwrap(),
        enable_discovery: false,
        identity: None,
        logon_screen: false,
    })
    .await
    .unwrap();
    (h, m, dir)
}

async fn wait_teams_seen(h: &EngineHandle) {
    for _ in 0..100 {
        if h.state().await.teams_running {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the engine never noticed the Teams process");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_machine_with_teams_gets_input() {
    let (h, m, _dir) = start(true, 200).await;
    wait_teams_seen(&h).await;
    assert_eq!(m.presence.nudges.load(Ordering::Relaxed), 1);
    h.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn switch_off_means_no_input() {
    let (h, m, _dir) = start(false, 200).await;
    wait_teams_seen(&h).await;
    assert_eq!(m.presence.nudges.load(Ordering::Relaxed), 0);
    h.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn active_user_is_left_alone() {
    let (h, m, _dir) = start(true, 5).await;
    wait_teams_seen(&h).await;
    assert_eq!(m.presence.nudges.load(Ordering::Relaxed), 0);
    h.stop();
}
