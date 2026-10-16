//! Client for the agent's UI pipe (`\\.\pipe\deskbridge-ui`, newline-delimited JSON).
//!
//! * Requests are `{"id": n, "req": {...}}`, replies `{"id": n, "ok": true, "data": ...}`.
//! * Pushes (`{"push": {"type": "state" | "event", ...}}`) are forwarded to the frontend as the
//!   Tauri events `agent://state` and `agent://event`; pipe up/down is announced as `agent://connection`.
//! * The task reconnects every second. If the pipe does not exist it tries to start
//!   `deskbridge-agent.exe` from the UI's own directory (a few times, with backoff).
//!
//! Payloads are passed through as raw `serde_json::Value`; the TypeScript types in `app/src/api/types.ts`
//! mirror `crates/core/src/ipc.rs`.

use crate::{brand, tray, windows};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::ClientOptions;
use tokio::sync::{mpsc, oneshot};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
const RECONNECT_DELAY: Duration = Duration::from_secs(1);
/// `ERROR_PIPE_BUSY`: all pipe instances are in use, retry immediately.
const ERROR_PIPE_BUSY: i32 = 231;
/// Give up starting the agent after this many attempts (the user can start it manually).
const MAX_AGENT_STARTS: u32 = 4;

type Pending = Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>;

/// Values from the agent's config that the Rust side needs (tray language, focus toggle).
#[derive(Debug, Clone)]
pub struct Prefs {
    pub language: String,
    pub focus_mode: String,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            language: "auto".into(),
            focus_mode: "full".into(),
        }
    }
}

pub struct IpcClient {
    next_id: AtomicU64,
    pending: Pending,
    writer: Mutex<Option<mpsc::UnboundedSender<String>>>,
    connected: AtomicBool,
    latest_state: Mutex<Option<Value>>,
    prefs: Mutex<Prefs>,
}

impl IpcClient {
    pub fn new() -> Self {
        Self {
            next_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            writer: Mutex::new(None),
            connected: AtomicBool::new(false),
            latest_state: Mutex::new(None),
            prefs: Mutex::new(Prefs::default()),
        }
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    pub fn latest_state(&self) -> Option<Value> {
        self.latest_state.lock().ok().and_then(|g| g.clone())
    }

    pub fn prefs(&self) -> Prefs {
        self.prefs.lock().map(|p| p.clone()).unwrap_or_default()
    }

    /// Sends one request and waits for its reply (or the timeout).
    pub async fn request(&self, req: Value) -> Result<Value, String> {
        if !self.is_connected() {
            return Err("agent_offline".into());
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().map_err(|_| "poisoned")?.insert(id, tx);

        let line = json!({ "id": id, "req": req }).to_string();
        let sent = self
            .writer
            .lock()
            .map_err(|_| "poisoned")?
            .as_ref()
            .map(|w| w.send(line).is_ok())
            .unwrap_or(false);
        if !sent {
            self.pending.lock().map_err(|_| "poisoned")?.remove(&id);
            return Err("agent_offline".into());
        }

        match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("agent_offline".into()),
            Err(_) => {
                if let Ok(mut p) = self.pending.lock() {
                    p.remove(&id);
                }
                Err("timeout".into())
            }
        }
    }

    /// Remembers values of a config object (reply of `get_config` / `set_config`).
    pub fn remember_config(&self, config: &Value) {
        if let Ok(mut p) = self.prefs.lock() {
            if let Some(l) = config.pointer("/general/language").and_then(Value::as_str) {
                p.language = l.to_owned();
            }
            if let Some(m) = config.pointer("/focus/mode").and_then(Value::as_str) {
                p.focus_mode = m.to_owned();
            }
        }
    }

    fn fail_all_pending(&self) {
        if let Ok(mut p) = self.pending.lock() {
            for (_, tx) in p.drain() {
                let _ = tx.send(Err("agent_offline".into()));
            }
        }
    }
}

/// Starts the reconnect loop on the Tauri async runtime.
pub fn spawn(app: AppHandle, client: Arc<IpcClient>) {
    tauri::async_runtime::spawn(async move {
        run(app, client).await;
    });
}

/// Rate-limits starting the agent process.
struct AgentLauncher {
    attempts: u32,
    last: Option<Instant>,
}

impl AgentLauncher {
    fn backoff(&self) -> Duration {
        Duration::from_secs((15u64 << self.attempts.saturating_sub(1).min(4)).min(240))
    }

    /// Returns `Some(started)` when an attempt was made, `None` when waiting or given up.
    async fn maybe_start(&mut self) -> Option<bool> {
        if self.attempts >= MAX_AGENT_STARTS {
            return None;
        }
        if let Some(last) = self.last {
            if last.elapsed() < self.backoff() {
                return None;
            }
        }
        self.attempts += 1;
        self.last = Some(Instant::now());
        Some(start_agent().await)
    }

    fn reset(&mut self) {
        self.attempts = 0;
        self.last = None;
    }

    fn recently_started(&self) -> bool {
        self.last
            .is_some_and(|l| l.elapsed() < Duration::from_secs(20))
    }
}

/// Starts the agent: first through the scheduled task (that is how the elevated agent is meant to run),
/// then, if its pipe does not show up within ~2 s, by spawning the executable next to the UI.
async fn start_agent() -> bool {
    let via_task = tauri::async_runtime::spawn_blocking(run_agent_task)
        .await
        .unwrap_or(false);
    if via_task {
        for _ in 0..10 {
            tokio::time::sleep(Duration::from_millis(200)).await;
            if pipe_exists() {
                tracing::info!("agent started via scheduled task");
                return true;
            }
        }
        tracing::warn!(
            "scheduled task ran but the agent pipe did not appear; falling back to a direct start"
        );
    }
    spawn_agent_exe()
}

fn pipe_exists() -> bool {
    std::fs::metadata(brand::UI_PIPE_NAME).is_ok()
}

/// `schtasks /run /tn "DeskBridge Agent"`; true when the task exists and was triggered.
fn run_agent_task() -> bool {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    match std::process::Command::new("schtasks")
        .args(["/run", "/tn", brand::AGENT_TASK_NAME])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    {
        Ok(out) => out.status.success(),
        Err(e) => {
            tracing::debug!("schtasks not available: {e}");
            false
        }
    }
}

fn spawn_agent_exe() -> bool {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const DETACHED_PROCESS: u32 = 0x0000_0008;

    let Some(exe) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(brand::AGENT_EXE)))
    else {
        return false;
    };
    if !exe.exists() {
        tracing::warn!("agent executable not found at {}", exe.display());
        return false;
    }
    match std::process::Command::new(&exe)
        .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
        .spawn()
    {
        Ok(_) => {
            tracing::info!("started agent {}", exe.display());
            true
        }
        Err(e) => {
            tracing::warn!("could not start agent {}: {e}", exe.display());
            false
        }
    }
}

async fn run(app: AppHandle, client: Arc<IpcClient>) {
    let mut launcher = AgentLauncher {
        attempts: 0,
        last: None,
    };
    let mut last_offline_reason: Option<String> = None;

    loop {
        match ClientOptions::new().open(brand::UI_PIPE_NAME) {
            Ok(pipe) => {
                launcher.reset();
                last_offline_reason = None;
                tracing::info!("connected to agent pipe");
                session(&app, &client, pipe).await;
                tracing::info!("agent pipe closed");
            }
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
            Err(e) => {
                let started = launcher.maybe_start().await;
                let reason = if e.kind() == std::io::ErrorKind::NotFound {
                    "not_running".to_owned()
                } else {
                    e.to_string()
                };
                let starting = launcher.recently_started();
                let key = format!("{reason}/{starting}");
                if last_offline_reason.as_deref() != Some(key.as_str()) {
                    last_offline_reason = Some(key);
                    let _ = app.emit(
                        "agent://connection",
                        json!({ "connected": false, "reason": reason, "starting": starting, "start_failed": started == Some(false) }),
                    );
                    tray::refresh(&app, &client);
                }
            }
        }
        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

/// Handles one connected pipe until it closes.
async fn session(
    app: &AppHandle,
    client: &Arc<IpcClient>,
    pipe: tokio::net::windows::named_pipe::NamedPipeClient,
) {
    let (read_half, mut write_half) = tokio::io::split(pipe);
    let mut lines = BufReader::new(read_half).lines();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    if let Ok(mut w) = client.writer.lock() {
        *w = Some(tx);
    }
    client.connected.store(true, Ordering::Relaxed);
    let _ = app.emit("agent://connection", json!({ "connected": true }));

    // Initial snapshots (the agent also pushes on change; these make the first paint instant).
    {
        let app = app.clone();
        let client = client.clone();
        tauri::async_runtime::spawn(async move {
            if let Ok(cfg) = client.request(json!({ "type": "get_config" })).await {
                client.remember_config(&cfg);
                tray::refresh(&app, &client);
            }
            if let Ok(state) = client.request(json!({ "type": "get_state" })).await {
                handle_state(&app, &client, state);
            }
        });
    }
    tray::refresh(app, client);

    loop {
        tokio::select! {
            line = lines.next_line() => match line {
                Ok(Some(line)) => handle_line(app, client, &line),
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!("pipe read error: {e}");
                    break;
                }
            },
            out = rx.recv() => match out {
                Some(mut text) => {
                    text.push('\n');
                    if let Err(e) = write_half.write_all(text.as_bytes()).await {
                        tracing::warn!("pipe write error: {e}");
                        break;
                    }
                    let _ = write_half.flush().await;
                }
                None => break,
            },
        }
    }

    client.connected.store(false, Ordering::Relaxed);
    if let Ok(mut w) = client.writer.lock() {
        *w = None;
    }
    client.fail_all_pending();
    let _ = app.emit(
        "agent://connection",
        json!({ "connected": false, "reason": "disconnected" }),
    );
    tray::refresh(app, client);
}

fn handle_line(app: &AppHandle, client: &Arc<IpcClient>, line: &str) {
    if line.trim().is_empty() {
        return;
    }
    let msg: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("invalid json from agent: {e}");
            return;
        }
    };

    if let Some(push) = msg.get("push") {
        match push.get("type").and_then(Value::as_str) {
            Some("state") => {
                if let Some(state) = push.get("state") {
                    handle_state(app, client, state.clone());
                }
            }
            Some("event") => {
                if let Some(event) = push.get("event") {
                    handle_event(app, event);
                }
            }
            other => tracing::debug!("unknown push type {other:?}"),
        }
        return;
    }

    if let Some(id) = msg.get("id").and_then(Value::as_u64) {
        let ok = msg.get("ok").and_then(Value::as_bool).unwrap_or(false);
        let result = if ok {
            Ok(msg.get("data").cloned().unwrap_or(Value::Null))
        } else {
            Err(msg
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
                .to_owned())
        };
        let tx = client.pending.lock().ok().and_then(|mut p| p.remove(&id));
        if let Some(tx) = tx {
            let _ = tx.send(result);
        }
    }
}

fn handle_state(app: &AppHandle, client: &Arc<IpcClient>, state: Value) {
    if let Ok(mut s) = client.latest_state.lock() {
        *s = Some(state.clone());
    }
    let _ = app.emit("agent://state", &state);
    tray::refresh(app, client);
}

fn handle_event(app: &AppHandle, event: &Value) {
    let _ = app.emit("agent://event", event);
    if event.get("kind").and_then(Value::as_str) == Some("osd") {
        let icon = event.get("icon").and_then(Value::as_str).unwrap_or("");
        let text = event.get("text").and_then(Value::as_str).unwrap_or("");
        windows::show_osd(app, icon, text);
    }
}

/// Sends a fire-and-forget request from Rust code (tray actions). Errors are only logged.
pub fn send_detached(app: &AppHandle, req: Value) {
    let client = app.state::<Arc<IpcClient>>().inner().clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = client.request(req.clone()).await {
            let kind = req
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_owned();
            tracing::warn!("request {kind} failed: {e}");
        }
    });
}
