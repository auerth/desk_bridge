//! Agent supervisor and helper keeper.
//!
//! The decisions are pure functions ([`decide_agent`], [`decide_helper`], [`Backoff`]) so they are
//! unit-tested; [`Supervisor`] feeds them with real observations once per second.

use crate::control::{HelperStatus, SharedHelperStatus};
use crate::launch::{self, Launched};
use crate::session;
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};
use tokio::sync::Notify;

/// Name of the agent's single-instance mutex (kept in sync with `core::brand`, see tests).
pub const AGENT_MUTEX: &str = "Global\\softwelop-deskbridge-agent";
pub const AGENT_EXE: &str = "deskbridge-agent.exe";

/// How long we leave the scheduled task time to start the agent after a logon.
pub const INITIAL_GRACE: Duration = Duration::from_secs(20);
pub const BACKOFF_MIN: Duration = Duration::from_secs(1);
pub const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// A process that ran at least this long counts as healthy: the backoff resets.
pub const STABLE_RUN: Duration = Duration::from_secs(60);
const TICK: Duration = Duration::from_secs(1);

/// Exponential restart delay: 1 s, 1 s, 2 s, 4 s ... capped at 30 s.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Backoff {
    failures: u32,
}

impl Backoff {
    /// Delay before the next start.
    #[must_use]
    pub fn delay(&self) -> Duration {
        let shift = self.failures.saturating_sub(1).min(8);
        (BACKOFF_MIN * (1u32 << shift)).min(BACKOFF_MAX)
    }

    /// A process we supervise ended after running `ran_for`.
    pub fn on_exit(&mut self, ran_for: Duration) {
        if ran_for >= STABLE_RUN {
            self.failures = 0;
        } else {
            self.failures = self.failures.saturating_add(1);
        }
    }

    pub fn reset(&mut self) {
        self.failures = 0;
    }
}

/// Observations the agent decision is based on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentView {
    /// A console session with a logged-on user exists.
    pub session_ready: bool,
    /// An agent instance runs in that session (mutex held or process found).
    pub alive: bool,
    /// The last agent exited with code 0 (user quit it): do not resurrect it.
    pub suppressed: bool,
    /// The agent was running at some point in this session (so a missing agent is a crash, not a
    /// slow scheduled task).
    pub seen_alive: bool,
    /// How long no agent has been running.
    pub down_for: Duration,
    /// Time since we last started one (`None` = never in this session).
    pub since_last_start: Option<Duration>,
    pub backoff: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentDecision {
    /// Nobody logged on at the console.
    NoSession,
    Running,
    /// The agent was stopped on purpose.
    Suppressed,
    /// Give the scheduled task / the backoff time.
    Wait,
    Start,
}

/// Should the supervisor start the agent now?
#[must_use]
pub fn decide_agent(v: &AgentView) -> AgentDecision {
    if !v.session_ready {
        return AgentDecision::NoSession;
    }
    if v.alive {
        return AgentDecision::Running;
    }
    if v.suppressed {
        return AgentDecision::Suppressed;
    }
    // After a crash the backoff alone gates the restart; after a logon the scheduled task gets
    // INITIAL_GRACE to start the agent first (never two instances).
    let needed = if v.seen_alive { v.backoff } else { INITIAL_GRACE };
    if v.down_for < needed {
        return AgentDecision::Wait;
    }
    if v.since_last_start.is_some_and(|s| s < v.backoff) {
        return AgentDecision::Wait;
    }
    AgentDecision::Start
}

/// What to do about the secure-desktop helper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelperDecision {
    Idle,
    Wait,
    Start,
    /// Running for a session/user that no longer is the console user.
    Stop,
}

/// `desired` = a console user exists; `key_matches` = the running helper belongs to the current
/// (session, user); `since_exit` = time since the previous helper ended.
#[must_use]
pub fn decide_helper(
    desired: bool,
    running: bool,
    key_matches: bool,
    since_exit: Option<Duration>,
    backoff: Duration,
) -> HelperDecision {
    match (desired, running) {
        (false, false) => HelperDecision::Idle,
        (false, true) => HelperDecision::Stop,
        (true, true) if !key_matches => HelperDecision::Stop,
        (true, true) => HelperDecision::Idle,
        (true, false) => {
            if since_exit.is_some_and(|s| s < backoff) {
                HelperDecision::Wait
            } else {
                HelperDecision::Start
            }
        }
    }
}

/// Is `code` the exit code of an intentional shutdown?
#[must_use]
pub fn is_clean_exit(code: u32) -> bool {
    code == 0
}

/// State shared with the control pipe.
pub struct Shared {
    pub helper: SharedHelperStatus,
    /// SID of the console user (pipe ACL); updated by the supervisor.
    pub user_sid: Mutex<Option<String>>,
    /// Wakes the control pipe so it rebuilds its pending instance with the new ACL.
    pub sid_changed: Arc<Notify>,
}

impl Shared {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            helper: Arc::new(Mutex::new(HelperStatus::default())),
            user_sid: Mutex::new(None),
            sid_changed: Arc::new(Notify::new()),
        })
    }

    fn set_user_sid(&self, sid: Option<String>) {
        let mut g = self.user_sid.lock();
        if *g != sid {
            *g = sid;
            drop(g);
            self.sid_changed.notify_one();
        }
    }
}

/// Events delivered to the supervisor thread.
#[derive(Debug, Clone, Copy)]
pub enum Event {
    Stop,
    /// `SERVICE_CONTROL_SESSIONCHANGE` (event type, session id).
    SessionChange(u32, u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionKey {
    session: u32,
    user: String,
}

struct Tracked {
    process: crate::winutil::OwnedHandle,
    pid: u32,
    since: Instant,
}

/// Supervisor options.
pub struct Options {
    pub exe_dir: PathBuf,
    pub manage_agent: bool,
    pub manage_helper: bool,
}

pub struct Supervisor {
    opts: Options,
    shared: Arc<Shared>,
    key: Option<SessionKey>,
    // agent
    agent: Option<Tracked>,
    agent_backoff: Backoff,
    agent_down_since: Instant,
    agent_last_start: Option<Instant>,
    agent_seen_alive: bool,
    agent_suppressed: bool,
    // helper
    helper: Option<(Tracked, SessionKey)>,
    helper_backoff: Backoff,
    helper_last_exit: Option<Instant>,
}

impl Supervisor {
    #[must_use]
    pub fn new(opts: Options, shared: Arc<Shared>) -> Self {
        Self {
            opts,
            shared,
            key: None,
            agent: None,
            agent_backoff: Backoff::default(),
            agent_down_since: Instant::now(),
            agent_last_start: None,
            agent_seen_alive: false,
            agent_suppressed: false,
            helper: None,
            helper_backoff: Backoff::default(),
            helper_last_exit: None,
        }
    }

    /// Runs until `Event::Stop` (or the sender is dropped), then stops the helper. The agent is
    /// left running on purpose: stopping the service must not drop the user's mouse and keyboard.
    pub fn run(mut self, rx: &Receiver<Event>) {
        tracing::info!("supervisor started");
        loop {
            self.tick(Instant::now());
            match rx.recv_timeout(TICK) {
                Ok(Event::Stop) | Err(RecvTimeoutError::Disconnected) => break,
                Ok(Event::SessionChange(ev, sid)) => {
                    tracing::info!(session = sid, "session change: {}", session::describe_event(ev));
                    if !session::wakes_supervisor(ev) {
                        // lock/unlock: nothing to re-evaluate, wait for the regular tick
                        continue;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
        self.stop_helper("service stopping");
        self.publish_helper();
        tracing::info!("supervisor stopped");
    }

    fn observe_session(&mut self, now: Instant) {
        let current = session::active_console_session().and_then(|s| {
            session::session_user(s).map(|user| SessionKey { session: s, user })
        });
        if current == self.key {
            return;
        }
        tracing::info!(from = ?self.key, to = ?current, "console session changed");
        self.key = current;
        // New session / user: forget everything about the old one.
        self.agent = None;
        self.agent_seen_alive = false;
        self.agent_suppressed = false;
        self.agent_down_since = now;
        self.agent_last_start = None;
        self.agent_backoff.reset();
        let sid = self.key.as_ref().and_then(|k| launch::session_user_sid(k.session));
        self.shared.set_user_sid(sid);
    }

    pub fn tick(&mut self, now: Instant) {
        self.observe_session(now);
        if self.opts.manage_helper {
            self.tick_helper(now);
        }
        if self.opts.manage_agent {
            self.tick_agent(now);
        }
    }

    // ---- agent --------------------------------------------------------------------------

    fn tick_agent(&mut self, now: Instant) {
        let Some(key) = self.key.clone() else { return };
        if let Some(t) = &self.agent {
            if let Some(code) = launch::exit_code(&t.process) {
                let ran_for = now.duration_since(t.since);
                if is_clean_exit(code) {
                    tracing::info!(pid = t.pid, ?ran_for, "agent exited normally (code 0), not restarting");
                    self.agent_suppressed = true;
                } else {
                    // Watchdog: a crashed agent may have left injected keys pressed. The
                    // restarted agent releases them at startup (`release_stuck_keys`).
                    tracing::warn!(
                        pid = t.pid,
                        exit_code = format!("{code:#010x}"),
                        ?ran_for,
                        "agent crashed, will restart"
                    );
                    self.agent_backoff.on_exit(ran_for);
                }
                self.agent = None;
                self.agent_down_since = now;
            }
        }
        if self.agent.is_none() {
            // Adopt an instance the scheduled task started, to learn when it dies and how.
            if let Some(pid) = launch::find_process_in_session(AGENT_EXE, key.session) {
                if let Some(process) = launch::open_for_wait(pid) {
                    tracing::info!(pid, "adopted running agent");
                    self.agent = Some(Tracked { process, pid, since: now });
                }
            }
        }
        let alive = self.agent.is_some() || launch::mutex_exists(AGENT_MUTEX);
        if alive {
            if !self.agent_seen_alive {
                tracing::info!("agent is running");
            }
            self.agent_seen_alive = true;
            self.agent_suppressed = false;
            self.agent_down_since = now;
            return;
        }
        let view = AgentView {
            session_ready: true,
            alive,
            suppressed: self.agent_suppressed,
            seen_alive: self.agent_seen_alive,
            down_for: now.duration_since(self.agent_down_since),
            since_last_start: self.agent_last_start.map(|t| now.duration_since(t)),
            backoff: self.agent_backoff.delay(),
        };
        if decide_agent(&view) != AgentDecision::Start {
            return;
        }
        self.agent_last_start = Some(now);
        let exe = self.opts.exe_dir.join(AGENT_EXE);
        match launch::launch_agent(key.session, &exe) {
            Ok((Launched { process, pid }, elevated)) => {
                tracing::info!(pid, elevated, session = key.session, "agent started");
                if !elevated {
                    tracing::warn!("agent token is not elevated (standard user?): UIPI limits apply");
                }
                self.agent = Some(Tracked { process, pid, since: now });
            }
            Err(e) => {
                tracing::error!("cannot start the agent ({}): {e}", exe.display());
                self.agent_backoff.on_exit(Duration::ZERO);
            }
        }
    }

    // ---- helper -------------------------------------------------------------------------

    fn tick_helper(&mut self, now: Instant) {
        if let Some((t, _)) = &self.helper {
            if let Some(code) = launch::exit_code(&t.process) {
                let ran_for = now.duration_since(t.since);
                tracing::warn!(pid = t.pid, exit_code = format!("{code:#010x}"), ?ran_for, "helper exited");
                self.helper_backoff.on_exit(ran_for);
                self.helper_last_exit = Some(now);
                self.helper = None;
            }
        }
        let key_matches = match (&self.helper, &self.key) {
            (Some((_, hk)), Some(k)) => hk == k,
            _ => false,
        };
        let action = decide_helper(
            self.key.is_some(),
            self.helper.is_some(),
            key_matches,
            self.helper_last_exit.map(|t| now.duration_since(t)),
            self.helper_backoff.delay(),
        );
        match action {
            HelperDecision::Idle | HelperDecision::Wait => {}
            HelperDecision::Stop => self.stop_helper("console session changed"),
            HelperDecision::Start => self.start_helper(now),
        }
        self.publish_helper();
    }

    fn start_helper(&mut self, now: Instant) {
        let Some(key) = self.key.clone() else { return };
        let exe = match std::env::current_exe() {
            Ok(e) => e,
            Err(e) => {
                tracing::error!("cannot locate own executable: {e}");
                return;
            }
        };
        let mut args = vec!["--helper".to_string(), "--parent-pid".into(), std::process::id().to_string()];
        if let Some(sid) = self.shared.user_sid.lock().clone() {
            args.push("--user-sid".into());
            args.push(sid);
        }
        self.helper_last_exit = Some(now); // spacing for the next attempt if this one fails
        match launch::launch_helper(key.session, &exe, &args) {
            Ok(Launched { process, pid }) => {
                tracing::info!(pid, session = key.session, "secure-desktop helper started");
                self.helper = Some((Tracked { process, pid, since: now }, key));
            }
            Err(e) => {
                tracing::error!("cannot start the secure-desktop helper: {e}");
                self.helper_backoff.on_exit(Duration::ZERO);
            }
        }
    }

    fn stop_helper(&mut self, why: &str) {
        if let Some((t, _)) = self.helper.take() {
            tracing::info!(pid = t.pid, "stopping helper: {why}");
            launch::terminate(&t.process);
            self.helper_backoff.reset();
            self.helper_last_exit = None;
        }
    }

    fn publish_helper(&self) {
        let status = match &self.helper {
            Some((t, k)) => HelperStatus { running: true, pid: t.pid, session: k.session },
            None => HelperStatus::default(),
        };
        *self.shared.helper.lock() = status;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: Duration = Duration::from_secs(1);

    fn view() -> AgentView {
        AgentView {
            session_ready: true,
            alive: false,
            suppressed: false,
            seen_alive: false,
            down_for: Duration::ZERO,
            since_last_start: None,
            backoff: BACKOFF_MIN,
        }
    }

    #[test]
    fn backoff_sequence_and_reset() {
        let mut b = Backoff::default();
        let mut seen = vec![b.delay().as_secs()];
        for _ in 0..8 {
            b.on_exit(Duration::from_secs(2));
            seen.push(b.delay().as_secs());
        }
        assert_eq!(seen, vec![1, 1, 2, 4, 8, 16, 30, 30, 30]);
        // a healthy run resets
        b.on_exit(STABLE_RUN);
        assert_eq!(b.delay(), BACKOFF_MIN);
        b.on_exit(S);
        b.on_exit(S);
        b.reset();
        assert_eq!(b.delay(), BACKOFF_MIN);
    }

    #[test]
    fn no_session_means_no_start() {
        let v = AgentView { session_ready: false, down_for: Duration::from_secs(999), ..view() };
        assert_eq!(decide_agent(&v), AgentDecision::NoSession);
    }

    #[test]
    fn never_starts_a_second_instance() {
        let v = AgentView { alive: true, down_for: Duration::from_secs(999), ..view() };
        assert_eq!(decide_agent(&v), AgentDecision::Running);
    }

    #[test]
    fn after_logon_the_scheduled_task_gets_a_grace_period() {
        let mut v = view();
        v.down_for = INITIAL_GRACE - S;
        assert_eq!(decide_agent(&v), AgentDecision::Wait);
        v.down_for = INITIAL_GRACE;
        assert_eq!(decide_agent(&v), AgentDecision::Start);
    }

    #[test]
    fn after_a_crash_only_the_backoff_applies() {
        let mut v = AgentView { seen_alive: true, backoff: Duration::from_secs(4), ..view() };
        v.down_for = Duration::from_secs(3);
        assert_eq!(decide_agent(&v), AgentDecision::Wait);
        v.down_for = Duration::from_secs(4);
        assert_eq!(decide_agent(&v), AgentDecision::Start);
    }

    #[test]
    fn a_fresh_start_is_not_repeated_within_the_backoff() {
        let v = AgentView {
            seen_alive: true,
            backoff: Duration::from_secs(8),
            down_for: Duration::from_secs(60),
            since_last_start: Some(Duration::from_secs(3)),
            ..view()
        };
        assert_eq!(decide_agent(&v), AgentDecision::Wait);
        let v = AgentView { since_last_start: Some(Duration::from_secs(8)), ..v };
        assert_eq!(decide_agent(&v), AgentDecision::Start);
    }

    #[test]
    fn a_deliberately_stopped_agent_stays_stopped() {
        let v = AgentView { suppressed: true, seen_alive: true, down_for: Duration::from_secs(999), ..view() };
        assert_eq!(decide_agent(&v), AgentDecision::Suppressed);
        assert!(is_clean_exit(0));
        assert!(!is_clean_exit(1));
        assert!(!is_clean_exit(101));
        assert!(!is_clean_exit(0xC000_0005));
    }

    #[test]
    fn helper_decisions() {
        let b = BACKOFF_MIN;
        assert_eq!(decide_helper(false, false, false, None, b), HelperDecision::Idle);
        assert_eq!(decide_helper(false, true, true, None, b), HelperDecision::Stop);
        assert_eq!(decide_helper(true, true, true, None, b), HelperDecision::Idle);
        assert_eq!(decide_helper(true, true, false, None, b), HelperDecision::Stop);
        assert_eq!(decide_helper(true, false, false, None, b), HelperDecision::Start);
        assert_eq!(decide_helper(true, false, false, Some(Duration::ZERO), b), HelperDecision::Wait);
        assert_eq!(decide_helper(true, false, false, Some(b), b), HelperDecision::Start);
    }

    #[test]
    fn mutex_name_matches_core_brand() {
        let core = include_str!("../../core/src/brand.rs");
        let line = core
            .lines()
            .find(|l| l.trim_start().starts_with("pub const AGENT_INSTANCE_ID:"))
            .expect("AGENT_INSTANCE_ID in core brand.rs");
        let escaped = AGENT_MUTEX.replace('\\', "\\\\");
        assert!(line.contains(&format!("\"{escaped}\"")), "drifted from core: {line}");
    }
}
