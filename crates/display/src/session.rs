//! Crash-safe orchestration of the display side of focus mode (plan 6.4, 8.7, 14, 16).
//!
//! State machine: `Off -> Preparing -> Streaming -> Stopping -> Off`. Every step runs under a
//! timeout and has a rollback. Before anything is changed the display topology is written to disk
//! together with a marker file; the marker is only deleted after the topology was restored and
//! the virtual display disabled. [`FocusDisplaySession::recover_on_startup`] finishes the job
//! after a crash.
//!
//! Rollback order is always "restore topology, then disable the virtual device": the restored
//! configuration puts the real monitors back first, so removing the virtual monitor can never
//! leave the machine without an active display.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use deskbridge_platform::display::{DisplayManager, Mode, Topology};
use deskbridge_platform::{PlatformError, Result};
use serde::{Deserialize, Serialize};

use crate::util::{atomic_write, run_with_timeout, unix_ms};

pub type ScreenId = u32;

/// Lifecycle of the display part of focus mode (plan 6.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FocusState {
    Off,
    Preparing,
    Streaming,
    /// Also the state after a failed rollback: the marker is still on disk and
    /// [`FocusDisplaySession::end`] may be called again.
    Stopping,
}

#[derive(Debug, Clone, Default)]
pub struct FocusOptions {
    /// Switch the internal panel off so that all windows move to the virtual display.
    pub disable_internal: bool,
    /// Preferred render GPU (DXGI adapter name, substring ok).
    pub render_adapter_hint: Option<String>,
}

/// Timeouts per step plus the retry policy for rollback steps.
#[derive(Debug, Clone)]
pub struct StepTimeouts {
    pub save: Duration,
    pub enable: Duration,
    pub switch: Duration,
    pub restore: Duration,
    pub disable: Duration,
    /// Attempts for restore / disable during rollback (>= 1).
    pub rollback_attempts: u32,
    pub retry_delay: Duration,
}

impl Default for StepTimeouts {
    fn default() -> Self {
        Self {
            save: Duration::from_secs(10),
            enable: Duration::from_secs(20),
            switch: Duration::from_secs(15),
            restore: Duration::from_secs(20),
            disable: Duration::from_secs(15),
            rollback_attempts: 3,
            retry_delay: Duration::from_millis(500),
        }
    }
}

impl StepTimeouts {
    /// Short timeouts and no retry delay, for tests.
    #[must_use]
    pub fn fast() -> Self {
        let s = Duration::from_secs(2);
        Self {
            save: s,
            enable: s,
            switch: s,
            restore: s,
            disable: s,
            rollback_attempts: 3,
            retry_delay: Duration::ZERO,
        }
    }
}

/// Content of the marker file next to `topology.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FocusMarker {
    /// Always `"focus"`.
    pub state: String,
    pub pid: u32,
    /// Unix milliseconds.
    pub started: u64,
    /// The topology was already restored; only the virtual device still has to be disabled.
    #[serde(default)]
    pub topology_restored: bool,
}

/// Decides whether the process that wrote a marker still runs.
pub trait ProcessProbe: Send + Sync {
    /// `started_unix_ms`: when the marker was written; a process created after that is not the
    /// writer (pid reuse).
    fn is_alive(&self, pid: u32, started_unix_ms: u64) -> bool;
}

/// Asks the operating system.
pub struct OsProcessProbe;

impl ProcessProbe for OsProcessProbe {
    fn is_alive(&self, pid: u32, started_unix_ms: u64) -> bool {
        os_is_alive(pid, started_unix_ms)
    }
}

#[cfg(windows)]
fn os_is_alive(pid: u32, started_ms: u64) -> bool {
    use windows::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, FILETIME};
    use windows::Win32::System::Threading::{
        GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    const STILL_ACTIVE: u32 = 259;
    // 100 ns intervals between 1601-01-01 and 1970-01-01, in milliseconds.
    const EPOCH_DIFF_MS: u64 = 11_644_473_600_000;
    if pid == 0 {
        return false;
    }
    // SAFETY: plain Win32 calls on a handle we open and close here; out pointers are valid locals.
    unsafe {
        let h = match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(h) => h,
            // Exists but we may not look at it: assume it is alive (never recover under a live owner).
            Err(e) => return e.code() == ERROR_ACCESS_DENIED.to_hresult(),
        };
        let mut code = 0u32;
        let alive = GetExitCodeProcess(h, &mut code).is_ok() && code == STILL_ACTIVE;
        let mut same = true;
        if alive {
            let (mut c, mut x, mut k, mut u) = (
                FILETIME::default(),
                FILETIME::default(),
                FILETIME::default(),
                FILETIME::default(),
            );
            if GetProcessTimes(h, &mut c, &mut x, &mut k, &mut u).is_ok() {
                let ft = (u64::from(c.dwHighDateTime) << 32) | u64::from(c.dwLowDateTime);
                let created_ms = (ft / 10_000).saturating_sub(EPOCH_DIFF_MS);
                // The marker is written after the process started; allow clock slack.
                same = created_ms <= started_ms.saturating_add(10_000);
            }
        }
        let _ = CloseHandle(h);
        alive && same
    }
}

#[cfg(not(windows))]
fn os_is_alive(pid: u32, _started_ms: u64) -> bool {
    pid != 0 && Path::new(&format!("/proc/{pid}")).exists()
}

/// Marker file next to the topology file: `topology.json` -> `topology.focus-marker.json`.
#[must_use]
pub fn marker_path(topology_path: &Path) -> PathBuf {
    let stem = topology_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "topology".into());
    topology_path.with_file_name(format!("{stem}.focus-marker.json"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryOutcome {
    /// No marker: the last focus session ended cleanly (or never ran).
    NothingToRecover,
    /// The process that owns the marker is still running; nothing was touched.
    OwnerAlive { pid: u32 },
    /// A dead session was cleaned up: topology restored, virtual device disabled, marker removed.
    Recovered,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn read_json<T: for<'de> Deserialize<'de>>(p: &Path) -> Option<T> {
    let bytes = std::fs::read(p).ok()?;
    match serde_json::from_slice(&bytes) {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!(path = %p.display(), error = %e, "unreadable focus state file");
            None
        }
    }
}

/// Runs `f` on a worker thread with a timeout.
fn timed<T: Send + 'static>(
    dm: &Arc<dyn DisplayManager>,
    label: &str,
    timeout: Duration,
    f: impl FnOnce(&dyn DisplayManager) -> Result<T> + Send + 'static,
) -> Result<T> {
    let dm = Arc::clone(dm);
    run_with_timeout(label, timeout, move || f(dm.as_ref()))
}

/// Like [`timed`] but retries up to `attempts` times.
fn timed_retry<T: Send + 'static>(
    dm: &Arc<dyn DisplayManager>,
    label: &str,
    t: &StepTimeouts,
    timeout: Duration,
    f: impl Fn(&dyn DisplayManager) -> Result<T> + Send + Sync + Clone + 'static,
) -> Result<T> {
    let attempts = t.rollback_attempts.max(1);
    let mut last = None;
    for n in 1..=attempts {
        match timed(dm, label, timeout, f.clone()) {
            Ok(v) => return Ok(v),
            Err(e) => {
                tracing::warn!(step = label, attempt = n, of = attempts, error = %e, "display step failed");
                last = Some(e);
                if n < attempts && !t.retry_delay.is_zero() {
                    std::thread::sleep(t.retry_delay);
                }
            }
        }
    }
    Err(last.unwrap_or_else(|| PlatformError::other(format!("{label}: no attempt made"))))
}

/// Files of one session.
#[derive(Debug, Clone)]
struct Files {
    topology: PathBuf,
    marker: PathBuf,
}

impl Files {
    fn new(topology: &Path) -> Self {
        Self {
            topology: topology.to_path_buf(),
            marker: marker_path(topology),
        }
    }

    fn write_marker(&self, m: &FocusMarker) -> std::io::Result<()> {
        let json = serde_json::to_vec_pretty(m).map_err(std::io::Error::other)?;
        atomic_write(&self.marker, &json)
    }

    fn remove_all(&self) {
        for p in [&self.marker, &self.topology] {
            if let Err(e) = std::fs::remove_file(p)
                && e.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(path = %p.display(), error = %e, "cannot delete focus state file");
            }
        }
    }
}

/// The shared teardown: restore the topology (unless the marker says it is done), disable the
/// virtual device, delete the files. On error the marker stays so that the next attempt (or the
/// next start) continues where this one stopped.
fn teardown(
    dm: &Arc<dyn DisplayManager>,
    files: &Files,
    t: &StepTimeouts,
    topology: Option<Topology>,
    mut marker: FocusMarker,
) -> Result<()> {
    if !marker.topology_restored {
        let top = topology.unwrap_or_else(|| {
            // Without a saved topology the platform falls back to "extend all connected displays".
            tracing::warn!(
                "no saved topology available; asking the platform for a best-effort restore"
            );
            Topology::default()
        });
        timed_retry(dm, "restore-topology", t, t.restore, move |d| d.restore_topology(&top)).map_err(
            |e| {
                tracing::error!(error = %e, "topology restore failed; keeping the focus marker for the next attempt");
                e
            },
        )?;
        marker.topology_restored = true;
        if let Err(e) = files.write_marker(&marker) {
            tracing::warn!(error = %e, "cannot update focus marker");
        }
    }
    timed_retry(dm, "disable-virtual", t, t.disable, |d| d.disable_virtual()).map_err(|e| {
        tracing::error!(error = %e, "disabling the virtual display failed; keeping the focus marker");
        e
    })?;
    files.remove_all();
    Ok(())
}

struct Active {
    topology: Topology,
    marker: FocusMarker,
}

/// Display part of focus mode over any [`DisplayManager`].
pub struct FocusDisplaySession {
    dm: Arc<dyn DisplayManager>,
    files: Files,
    timeouts: StepTimeouts,
    probe: Arc<dyn ProcessProbe>,
    /// Serialises begin/end/recover.
    op: Mutex<()>,
    state: Mutex<FocusState>,
    active: Mutex<Option<Active>>,
}

impl FocusDisplaySession {
    #[must_use]
    pub fn new(dm: Arc<dyn DisplayManager>, topology_path: impl Into<PathBuf>) -> Self {
        Self::with_config(
            dm,
            topology_path,
            StepTimeouts::default(),
            Arc::new(OsProcessProbe),
        )
    }

    #[must_use]
    pub fn with_config(
        dm: Arc<dyn DisplayManager>,
        topology_path: impl Into<PathBuf>,
        timeouts: StepTimeouts,
        probe: Arc<dyn ProcessProbe>,
    ) -> Self {
        Self {
            dm,
            files: Files::new(&topology_path.into()),
            timeouts,
            probe,
            op: Mutex::new(()),
            state: Mutex::new(FocusState::Off),
            active: Mutex::new(None),
        }
    }

    #[must_use]
    pub fn state(&self) -> FocusState {
        *lock(&self.state)
    }

    fn set_state(&self, s: FocusState) {
        *lock(&self.state) = s;
    }

    /// Prepares the display for focus mode: saves the topology to disk, enables the virtual
    /// display with `mode` and makes it the (only) display. On any failure everything is rolled
    /// back before the error is returned.
    pub fn begin(&self, mode: Mode, opts: &FocusOptions) -> Result<ScreenId> {
        let _op = lock(&self.op);
        let cur = self.state();
        if cur != FocusState::Off {
            return Err(PlatformError::other(format!(
                "focus display session already active ({cur:?})"
            )));
        }
        self.set_state(FocusState::Preparing);
        match self.begin_locked(mode, opts) {
            Ok(id) => {
                self.set_state(FocusState::Streaming);
                tracing::info!(screen = id, ?mode, "focus display ready");
                Ok(id)
            }
            Err((e, rolled_back)) => {
                self.set_state(if rolled_back {
                    FocusState::Off
                } else {
                    FocusState::Stopping
                });
                Err(e)
            }
        }
    }

    /// Error side: `(error, rolled_back)`.
    fn begin_locked(
        &self,
        mode: Mode,
        opts: &FocusOptions,
    ) -> std::result::Result<ScreenId, (PlatformError, bool)> {
        let t = &self.timeouts;
        // 0. A marker from an earlier session must be dealt with first: overwriting it would
        //    replace the original topology by whatever the machine looks like now.
        if self.files.marker.exists() {
            tracing::warn!("found an unfinished focus session; recovering it first");
            match self.recover_locked() {
                Ok(RecoveryOutcome::OwnerAlive { pid }) => {
                    return Err((
                        PlatformError::other(format!(
                            "another process (pid {pid}) owns the focus display state"
                        )),
                        true,
                    ));
                }
                Ok(_) => {}
                Err(e) => return Err((e, true)),
            }
        }

        // 1. Save the topology, in memory and on disk, before touching anything.
        let topology = timed(&self.dm, "save-topology", t.save, |d| d.save_topology())
            .map_err(|e| (e, true))?;
        let marker = FocusMarker {
            state: "focus".into(),
            pid: std::process::id(),
            started: unix_ms(),
            topology_restored: false,
        };
        let persisted = serde_json::to_vec_pretty(&topology)
            .map_err(std::io::Error::other)
            .and_then(|json| atomic_write(&self.files.topology, &json))
            .and_then(|()| self.files.write_marker(&marker));
        if let Err(e) = persisted {
            self.files.remove_all();
            return Err((
                PlatformError::other(format!("cannot persist topology for crash recovery: {e}")),
                true,
            ));
        }
        *lock(&self.active) = Some(Active {
            topology: topology.clone(),
            marker: marker.clone(),
        });

        // 2./3. Enable the virtual display and switch to it; any failure rolls back.
        let hint = opts.render_adapter_hint.clone();
        let steps = || -> Result<ScreenId> {
            let id = timed(&self.dm, "enable-virtual", t.enable, move |d| {
                d.enable_virtual(mode, hint.as_deref())
            })?;
            let internal = opts.disable_internal;
            timed(&self.dm, "use-as-only-display", t.switch, move |d| {
                d.use_as_only_display(id, internal)
            })?;
            Ok(id)
        };
        match steps() {
            Ok(id) => Ok(id),
            Err(e) => {
                tracing::error!(error = %e, "focus display preparation failed; rolling back");
                match teardown(&self.dm, &self.files, t, Some(topology), marker) {
                    Ok(()) => {
                        *lock(&self.active) = None;
                        Err((e, true))
                    }
                    Err(re) => Err((
                        PlatformError::other(format!(
                            "{e}; rollback also failed: {re} (will retry at next start)"
                        )),
                        false,
                    )),
                }
            }
        }
    }

    /// Ends focus mode: restores the saved topology, disables the virtual display, deletes the
    /// marker. Idempotent; after a failure it can be called again.
    pub fn end(&self) -> Result<()> {
        let _op = lock(&self.op);
        let cur = self.state();
        if cur == FocusState::Off {
            return Ok(());
        }
        self.set_state(FocusState::Stopping);
        let active = lock(&self.active).take();
        let (topology, marker) = match active {
            Some(a) => (Some(a.topology), a.marker),
            None => (
                read_json::<Topology>(&self.files.topology),
                read_json::<FocusMarker>(&self.files.marker).unwrap_or_else(|| FocusMarker {
                    state: "focus".into(),
                    pid: std::process::id(),
                    started: unix_ms(),
                    topology_restored: false,
                }),
            ),
        };
        // A restore that already succeeded in an earlier attempt must not run again.
        let marker = read_json::<FocusMarker>(&self.files.marker).unwrap_or(marker);
        match teardown(
            &self.dm,
            &self.files,
            &self.timeouts,
            topology.clone(),
            marker.clone(),
        ) {
            Ok(()) => {
                self.set_state(FocusState::Off);
                tracing::info!("focus display ended");
                Ok(())
            }
            Err(e) => {
                if let Some(topology) = topology {
                    *lock(&self.active) = Some(Active { topology, marker });
                }
                Err(e)
            }
        }
    }

    /// Cleans up after a crashed session (see [`recover_on_startup`]) using this session's
    /// configuration. Refuses to run while a session of this object is active.
    pub fn recover(&self) -> Result<RecoveryOutcome> {
        let _op = lock(&self.op);
        if self.state() != FocusState::Off {
            return Ok(RecoveryOutcome::OwnerAlive {
                pid: std::process::id(),
            });
        }
        self.recover_locked()
    }

    fn recover_locked(&self) -> Result<RecoveryOutcome> {
        recover_impl(
            &self.dm,
            &self.files,
            &self.timeouts,
            self.probe.as_ref(),
            std::process::id(),
        )
    }

    /// Startup recovery: if `topology_path` has a marker written by a process that is no longer
    /// running, restore the saved topology and disable the virtual device. Idempotent.
    pub fn recover_on_startup(
        topology_path: &Path,
        dm: &Arc<dyn DisplayManager>,
    ) -> Result<RecoveryOutcome> {
        recover_on_startup(topology_path, dm)
    }
}

impl Drop for FocusDisplaySession {
    fn drop(&mut self) {
        if self.state() != FocusState::Off {
            tracing::warn!("focus display session dropped while active; ending it");
            if let Err(e) = self.end() {
                tracing::error!(error = %e, "ending the focus display session on drop failed");
            }
        }
    }
}

fn recover_impl(
    dm: &Arc<dyn DisplayManager>,
    files: &Files,
    t: &StepTimeouts,
    probe: &dyn ProcessProbe,
    own_pid: u32,
) -> Result<RecoveryOutcome> {
    if !files.marker.exists() {
        return Ok(RecoveryOutcome::NothingToRecover);
    }
    let marker = read_json::<FocusMarker>(&files.marker);
    if let Some(m) = &marker {
        if m.state != "focus" {
            tracing::warn!(state = %m.state, "ignoring focus marker with unknown state");
            files.remove_all();
            return Ok(RecoveryOutcome::NothingToRecover);
        }
        if m.pid != 0 && probe.is_alive(m.pid, m.started) {
            tracing::info!(
                pid = m.pid,
                own = (m.pid == own_pid),
                "focus marker owner is still running"
            );
            return Ok(RecoveryOutcome::OwnerAlive { pid: m.pid });
        }
    }
    tracing::warn!(marker = ?marker, "recovering the display state of a dead focus session");
    let marker = marker.unwrap_or(FocusMarker {
        state: "focus".into(),
        pid: 0,
        started: 0,
        topology_restored: false,
    });
    let topology = read_json::<Topology>(&files.topology);
    teardown(dm, files, t, topology, marker)?;
    Ok(RecoveryOutcome::Recovered)
}

/// Startup recovery with the real process probe and default timeouts.
pub fn recover_on_startup(
    topology_path: &Path,
    dm: &Arc<dyn DisplayManager>,
) -> Result<RecoveryOutcome> {
    recover_on_startup_with(topology_path, dm, &StepTimeouts::default(), &OsProcessProbe)
}

/// [`recover_on_startup`] with explicit timeouts and probe (tests).
pub fn recover_on_startup_with(
    topology_path: &Path,
    dm: &Arc<dyn DisplayManager>,
    timeouts: &StepTimeouts,
    probe: &dyn ProcessProbe,
) -> Result<RecoveryOutcome> {
    recover_impl(
        dm,
        &Files::new(topology_path),
        timeouts,
        probe,
        std::process::id(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::{Call, Failure, MockDisplayManager, Step, World};

    const MODE: Mode = Mode {
        width: 5120,
        height: 1440,
        hz: 120,
    };

    struct FakeProbe(bool);
    impl ProcessProbe for FakeProbe {
        fn is_alive(&self, _pid: u32, _started: u64) -> bool {
            self.0
        }
    }

    struct Rig {
        mock: Arc<MockDisplayManager>,
        dir: tempfile::TempDir,
        path: PathBuf,
    }

    impl Rig {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("topology.json");
            Self {
                mock: Arc::new(MockDisplayManager::new()),
                dir,
                path,
            }
        }

        fn dm(&self) -> Arc<dyn DisplayManager> {
            self.mock.clone()
        }

        fn session_with(&self, t: StepTimeouts, owner_alive: bool) -> FocusDisplaySession {
            FocusDisplaySession::with_config(
                self.dm(),
                &self.path,
                t,
                Arc::new(FakeProbe(owner_alive)),
            )
        }

        fn session(&self) -> FocusDisplaySession {
            self.session_with(StepTimeouts::fast(), false)
        }

        fn files_left(&self) -> usize {
            std::fs::read_dir(self.dir.path()).unwrap().count()
        }
    }

    fn opts(disable_internal: bool) -> FocusOptions {
        FocusOptions {
            disable_internal,
            render_adapter_hint: Some("NVIDIA".into()),
        }
    }

    #[test]
    fn happy_path_with_and_without_internal_panel() {
        for disable_internal in [false, true] {
            let rig = Rig::new();
            let s = rig.session();
            let id = s.begin(MODE, &opts(disable_internal)).unwrap();
            assert_eq!(id, rig.mock.virtual_screen_id());
            assert_eq!(s.state(), FocusState::Streaming);
            let w = rig.mock.world();
            assert!(w.virtual_enabled && w.virtual_in_use);
            assert_eq!(w.internal_active, !disable_internal);
            assert!(rig.path.exists() && marker_path(&rig.path).exists());
            // Order and arguments of the calls.
            let calls = rig.mock.calls();
            assert_eq!(calls[0], Call::SaveTopology);
            assert_eq!(
                calls[1],
                Call::EnableVirtual {
                    mode: MODE,
                    hint: Some("NVIDIA".into())
                }
            );
            assert_eq!(
                calls[2],
                Call::UseAsOnlyDisplay {
                    screen_id: id,
                    disable_internal
                }
            );

            s.end().unwrap();
            assert_eq!(s.state(), FocusState::Off);
            assert_eq!(rig.mock.world(), World::default());
            assert_eq!(rig.files_left(), 0, "marker and topology deleted");
            assert!(!rig.mock.ever_blackout());
            // restore happens before the device is disabled
            let order: Vec<Step> = rig.mock.calls().iter().map(Call::step).collect();
            let r = order
                .iter()
                .position(|s| *s == Step::RestoreTopology)
                .unwrap();
            let d = order
                .iter()
                .position(|s| *s == Step::DisableVirtual)
                .unwrap();
            assert!(r < d);
        }
    }

    #[test]
    fn marker_content() {
        let rig = Rig::new();
        let s = rig.session();
        s.begin(MODE, &opts(true)).unwrap();
        let m: FocusMarker =
            serde_json::from_slice(&std::fs::read(marker_path(&rig.path)).unwrap()).unwrap();
        assert_eq!(m.state, "focus");
        assert_eq!(m.pid, std::process::id());
        assert!(m.started > 1_700_000_000_000);
        assert!(!m.topology_restored);
        // the stored topology is the one from before the switch
        let t: Topology = serde_json::from_slice(&std::fs::read(&rig.path).unwrap()).unwrap();
        let w: World = serde_json::from_str(&t.data).unwrap();
        assert_eq!(w, World::default());
        s.end().unwrap();
    }

    #[test]
    fn failure_at_every_begin_step_leaves_the_topology_restored() {
        let cases: Vec<(Step, Failure)> = vec![
            (Step::SaveTopology, Failure::always()),
            (Step::EnableVirtual, Failure::always()),
            (Step::EnableVirtual, Failure::always().with_effect()),
            (Step::UseAsOnlyDisplay, Failure::always()),
            (Step::UseAsOnlyDisplay, Failure::always().with_effect()),
        ];
        for disable_internal in [false, true] {
            for (step, f) in &cases {
                let rig = Rig::new();
                rig.mock.fail(*step, *f);
                let s = rig.session();
                let err = s.begin(MODE, &opts(disable_internal)).unwrap_err();
                assert!(err.to_string().contains("injected"), "{step:?}: {err}");
                assert_eq!(s.state(), FocusState::Off, "{step:?}");
                assert_eq!(
                    rig.mock.world(),
                    World::default(),
                    "{step:?} internal={disable_internal} effect={}",
                    f.after_effect
                );
                assert!(!rig.mock.ever_blackout(), "{step:?}");
                assert_eq!(rig.files_left(), 0, "{step:?}: files must be cleaned up");
                // and the session is usable again
                rig.mock.clear_failures();
                s.begin(MODE, &opts(disable_internal)).unwrap();
                s.end().unwrap();
                assert_eq!(rig.mock.world(), World::default());
            }
        }
    }

    #[test]
    fn unwritable_topology_path_aborts_before_changing_anything() {
        let rig = Rig::new();
        // A directory where the topology file should go makes the write fail.
        std::fs::create_dir(&rig.path).unwrap();
        let s = rig.session();
        let err = s.begin(MODE, &opts(true)).unwrap_err();
        assert!(err.to_string().contains("crash recovery"), "{err}");
        assert_eq!(rig.mock.count(Step::EnableVirtual), 0);
        assert_eq!(rig.mock.world(), World::default());
        assert_eq!(s.state(), FocusState::Off);
    }

    #[test]
    fn rollback_retries_and_can_be_continued_with_end() {
        let rig = Rig::new();
        // restore fails more often than the retry budget -> rollback fails, marker stays
        rig.mock
            .fail(Step::UseAsOnlyDisplay, Failure::always().with_effect());
        rig.mock.fail(Step::RestoreTopology, Failure::times(3));
        let s = rig.session();
        let err = s.begin(MODE, &opts(true)).unwrap_err();
        assert!(err.to_string().contains("rollback also failed"), "{err}");
        assert_eq!(s.state(), FocusState::Stopping);
        assert!(
            marker_path(&rig.path).exists(),
            "marker must survive a failed rollback"
        );
        assert_eq!(rig.mock.count(Step::RestoreTopology), 3);
        assert_eq!(
            rig.mock.count(Step::DisableVirtual),
            0,
            "never disable the device before the topology is back"
        );
        assert!(!rig.mock.ever_blackout());
        // a new begin is refused while stopping
        assert!(s.begin(MODE, &opts(true)).is_err());
        // failure cleared -> end() finishes the rollback
        rig.mock.clear_failures();
        s.end().unwrap();
        assert_eq!(s.state(), FocusState::Off);
        assert_eq!(rig.mock.world(), World::default());
        assert_eq!(rig.files_left(), 0);
    }

    #[test]
    fn transient_rollback_failures_are_retried() {
        let rig = Rig::new();
        let s = rig.session();
        s.begin(MODE, &opts(true)).unwrap();
        rig.mock.fail(Step::RestoreTopology, Failure::times(2));
        s.end().unwrap();
        assert_eq!(rig.mock.count(Step::RestoreTopology), 3);
        assert_eq!(rig.mock.world(), World::default());

        let s = rig.session();
        s.begin(MODE, &opts(true)).unwrap();
        rig.mock.fail(Step::DisableVirtual, Failure::times(1));
        s.end().unwrap();
        assert_eq!(rig.mock.world(), World::default());
    }

    #[test]
    fn failed_disable_keeps_marker_and_does_not_restore_twice() {
        let rig = Rig::new();
        let s = rig.session();
        s.begin(MODE, &opts(true)).unwrap();
        rig.mock.fail(Step::DisableVirtual, Failure::always());
        assert!(s.end().is_err());
        assert_eq!(s.state(), FocusState::Stopping);
        let m: FocusMarker =
            serde_json::from_slice(&std::fs::read(marker_path(&rig.path)).unwrap()).unwrap();
        assert!(m.topology_restored);
        let restores = rig.mock.count(Step::RestoreTopology);
        rig.mock.clear_failures();
        s.end().unwrap();
        assert_eq!(
            rig.mock.count(Step::RestoreTopology),
            restores,
            "restore must not be repeated"
        );
        assert_eq!(rig.mock.world(), World::default());
        assert_eq!(rig.files_left(), 0);
    }

    #[test]
    fn double_begin_and_double_end() {
        let rig = Rig::new();
        let s = rig.session();
        assert!(s.end().is_ok(), "end without begin is a no-op");
        assert_eq!(rig.mock.calls().len(), 0);
        s.begin(MODE, &opts(true)).unwrap();
        let calls = rig.mock.calls().len();
        let err = s.begin(MODE, &opts(true)).unwrap_err();
        assert!(err.to_string().contains("already active"));
        assert_eq!(
            rig.mock.calls().len(),
            calls,
            "second begin touches nothing"
        );
        assert_eq!(s.state(), FocusState::Streaming);
        s.end().unwrap();
        let calls = rig.mock.calls().len();
        s.end().unwrap();
        assert_eq!(rig.mock.calls().len(), calls, "second end touches nothing");
        assert_eq!(rig.mock.world(), World::default());
    }

    #[test]
    fn crash_after_begin_is_recovered_at_next_start() {
        let rig = Rig::new();
        let s = rig.session();
        s.begin(MODE, &opts(true)).unwrap();
        // simulate kill -9: no end(), no Drop
        std::mem::forget(s);
        assert!(rig.mock.world().virtual_in_use && !rig.mock.world().internal_active);
        let t = StepTimeouts::fast();

        // owner still alive (e.g. second instance started): nothing happens
        let out = recover_on_startup_with(&rig.path, &rig.dm(), &t, &FakeProbe(true)).unwrap();
        assert!(matches!(out, RecoveryOutcome::OwnerAlive { .. }));
        assert!(rig.mock.world().virtual_in_use);

        // owner dead: topology restored, device disabled, files deleted
        let out = recover_on_startup_with(&rig.path, &rig.dm(), &t, &FakeProbe(false)).unwrap();
        assert_eq!(out, RecoveryOutcome::Recovered);
        assert_eq!(rig.mock.world(), World::default());
        assert!(!rig.mock.ever_blackout());
        assert_eq!(rig.files_left(), 0);

        // idempotent
        let calls = rig.mock.calls().len();
        let out = recover_on_startup_with(&rig.path, &rig.dm(), &t, &FakeProbe(false)).unwrap();
        assert_eq!(out, RecoveryOutcome::NothingToRecover);
        assert_eq!(rig.mock.calls().len(), calls);
    }

    #[test]
    fn crash_during_rollback_is_resumed() {
        let rig = Rig::new();
        let s = rig.session();
        s.begin(MODE, &opts(true)).unwrap();
        rig.mock.fail(Step::DisableVirtual, Failure::always());
        assert!(s.end().is_err());
        std::mem::forget(s); // process dies after the restore but before the disable
        rig.mock.clear_failures();
        let restores = rig.mock.count(Step::RestoreTopology);
        let out = recover_on_startup_with(
            &rig.path,
            &rig.dm(),
            &StepTimeouts::fast(),
            &FakeProbe(false),
        )
        .unwrap();
        assert_eq!(out, RecoveryOutcome::Recovered);
        assert_eq!(
            rig.mock.count(Step::RestoreTopology),
            restores,
            "topology was already restored"
        );
        assert_eq!(rig.mock.world(), World::default());
    }

    #[test]
    fn recovery_without_topology_file_still_disables_the_device() {
        let rig = Rig::new();
        let s = rig.session();
        s.begin(MODE, &opts(true)).unwrap();
        std::mem::forget(s);
        std::fs::remove_file(&rig.path).unwrap();
        let out = recover_on_startup_with(
            &rig.path,
            &rig.dm(),
            &StepTimeouts::fast(),
            &FakeProbe(false),
        )
        .unwrap();
        assert_eq!(out, RecoveryOutcome::Recovered);
        // an empty topology was passed to the platform (its fallback chain restores a display)
        assert!(
            rig.mock
                .calls()
                .iter()
                .any(|c| matches!(c, Call::RestoreTopology(t) if t.data.is_empty()))
        );
        assert!(!rig.mock.world().virtual_enabled);
    }

    #[test]
    fn corrupt_marker_is_treated_as_dead_owner() {
        let rig = Rig::new();
        std::fs::write(marker_path(&rig.path), b"{ not json").unwrap();
        let out = recover_on_startup_with(
            &rig.path,
            &rig.dm(),
            &StepTimeouts::fast(),
            &FakeProbe(true),
        )
        .unwrap();
        assert_eq!(out, RecoveryOutcome::Recovered);
        assert_eq!(rig.files_left(), 0);
    }

    #[test]
    fn begin_recovers_an_old_session_first_and_respects_a_live_owner() {
        // dead owner: begin() cleans up and proceeds
        let rig = Rig::new();
        let crashed = rig.session();
        crashed.begin(MODE, &opts(true)).unwrap();
        std::mem::forget(crashed);
        let s = rig.session_with(StepTimeouts::fast(), false);
        s.begin(MODE, &opts(false)).unwrap();
        let w = rig.mock.world();
        assert!(w.internal_active && w.virtual_in_use);
        // the topology saved by the new session is the restored one, not the crashed state
        let t: Topology = serde_json::from_slice(&std::fs::read(&rig.path).unwrap()).unwrap();
        assert_eq!(
            serde_json::from_str::<World>(&t.data).unwrap(),
            World::default()
        );
        s.end().unwrap();
        assert_eq!(rig.mock.world(), World::default());

        // live owner: begin() refuses and changes nothing
        let rig = Rig::new();
        let other = rig.session();
        other.begin(MODE, &opts(true)).unwrap();
        std::mem::forget(other);
        let s = rig.session_with(StepTimeouts::fast(), true);
        let before = rig.mock.world();
        assert!(s.begin(MODE, &opts(true)).is_err());
        assert_eq!(rig.mock.world(), before);
        assert_eq!(s.state(), FocusState::Off);
    }

    #[test]
    fn step_timeout_triggers_rollback() {
        let rig = Rig::new();
        rig.mock
            .delay(Step::EnableVirtual, Duration::from_millis(300));
        let t = StepTimeouts {
            enable: Duration::from_millis(40),
            ..StepTimeouts::fast()
        };
        let s = rig.session_with(t, false);
        let err = s.begin(MODE, &opts(true)).unwrap_err();
        assert!(err.to_string().contains("timed out"), "{err}");
        assert_eq!(s.state(), FocusState::Off);
        assert!(rig.mock.world().internal_active);
        assert!(!rig.mock.ever_blackout());
        assert_eq!(rig.files_left(), 0);
    }

    #[test]
    fn drop_ends_an_active_session() {
        let rig = Rig::new();
        {
            let s = rig.session();
            s.begin(MODE, &opts(true)).unwrap();
        }
        assert_eq!(rig.mock.world(), World::default());
        assert_eq!(rig.files_left(), 0);
    }

    #[test]
    fn twenty_cycles_leave_the_topology_unchanged() {
        let rig = Rig::new();
        let s = rig.session();
        for i in 0..20 {
            s.begin(MODE, &opts(i % 2 == 0)).unwrap();
            s.end().unwrap();
            assert_eq!(rig.mock.world(), World::default(), "cycle {i}");
        }
        assert!(!rig.mock.ever_blackout());
    }

    #[test]
    fn os_probe_knows_this_process() {
        let probe = OsProcessProbe;
        assert!(probe.is_alive(std::process::id(), unix_ms()));
        assert!(!probe.is_alive(0, 0));
        // a very high pid that does not exist
        assert!(!probe.is_alive(0x7FFF_FFF0, 0));
    }

    #[test]
    fn marker_path_naming() {
        assert_eq!(
            marker_path(Path::new("C:/x/topology.json")),
            Path::new("C:/x/topology.focus-marker.json")
        );
    }
}
