//! Transfer bookkeeping for the UI: running transfers, speed, and the persisted history.

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How many finished transfers are kept in `transfers.json`.
pub const HISTORY_LIMIT: usize = 50;
const HISTORY_FILE: &str = "transfers.json";
/// Minimum interval between speed samples.
const SPEED_WINDOW: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Send,
    Receive,
}

impl Direction {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Send => "send",
            Self::Receive => "receive",
        }
    }
}

/// Snapshot of one transfer; same shape as `core::ipc::TransferDto` plus speed/error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransferInfo {
    pub id: u64,
    /// "send" | "receive"
    pub direction: String,
    pub name: String,
    pub files: u32,
    pub total_bytes: u64,
    pub done_bytes: u64,
    /// "running" | "done" | "failed" | "cancelled"
    pub status: String,
    /// Target folder (receive) or source folder (send), for "Im Ordner zeigen".
    pub path: Option<String>,
    pub started_unix_ms: u64,
    /// Smoothed throughput in bytes per second (0 unless running).
    #[serde(default)]
    pub speed_bps: u64,
    #[serde(default)]
    pub error: Option<String>,
}

struct Runtime {
    cancelled: Arc<AtomicBool>,
    last_at: Instant,
    last_bytes: u64,
    ema_bps: f64,
}

#[derive(Default)]
struct State {
    /// Oldest first.
    items: Vec<TransferInfo>,
    runtime: std::collections::HashMap<u64, Runtime>,
}

struct Inner {
    state: Mutex<State>,
    file: Option<PathBuf>,
    counter: AtomicU64,
}

/// Cheap to clone; all clones share the same state.
#[derive(Clone)]
pub struct TransferManager {
    inner: Arc<Inner>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

impl TransferManager {
    /// `state_dir`: folder for `transfers.json`; `None` keeps the history in memory only.
    /// Transfers that were still "running" when the file was written are marked as failed.
    #[must_use]
    pub fn new(state_dir: Option<PathBuf>) -> Self {
        let file = state_dir.map(|d| d.join(HISTORY_FILE));
        let mut state = State::default();
        if let Some(f) = &file
            && let Ok(bytes) = std::fs::read(f)
        {
            match serde_json::from_slice::<Vec<TransferInfo>>(&bytes) {
                Ok(mut items) => {
                    for it in &mut items {
                        if it.status == "running" {
                            it.status = "failed".into();
                            it.error = Some("Unterbrochen".into());
                        }
                        it.speed_bps = 0;
                    }
                    state.items = items;
                }
                Err(e) => tracing::warn!("transfers.json unlesbar: {e}"),
            }
        }
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(state),
                file,
                counter: AtomicU64::new(0),
            }),
        }
    }

    /// Allocates a transfer id that is unique across restarts (time based).
    #[must_use]
    pub fn next_id(&self) -> u64 {
        let c = self.inner.counter.fetch_add(1, Ordering::Relaxed) & 0xFFFF;
        (now_ms() << 16) | c
    }

    /// Registers a new running transfer. `id` comes from the peer for incoming transfers.
    pub fn begin(
        &self,
        id: u64,
        direction: Direction,
        name: impl Into<String>,
        files: u32,
        total_bytes: u64,
        path: Option<String>,
    ) -> TransferHandle {
        let cancelled = Arc::new(AtomicBool::new(false));
        {
            let mut st = self.inner.state.lock();
            st.items.retain(|i| i.id != id);
            st.items.push(TransferInfo {
                id,
                direction: direction.as_str().into(),
                name: name.into(),
                files,
                total_bytes,
                done_bytes: 0,
                status: "running".into(),
                path,
                started_unix_ms: now_ms(),
                speed_bps: 0,
                error: None,
            });
            st.runtime.insert(
                id,
                Runtime {
                    cancelled: cancelled.clone(),
                    last_at: Instant::now(),
                    last_bytes: 0,
                    ema_bps: 0.0,
                },
            );
            Self::trim(&mut st);
        }
        self.persist();
        TransferHandle {
            mgr: self.clone(),
            id,
            cancelled,
        }
    }

    /// Requests cancellation; the transfer task polls [`TransferHandle::is_cancelled`].
    pub fn cancel(&self, id: u64) -> bool {
        let st = self.inner.state.lock();
        if let Some(rt) = st.runtime.get(&id) {
            rt.cancelled.store(true, Ordering::SeqCst);
            true
        } else {
            false
        }
    }

    /// All transfers, running ones first, then newest first.
    #[must_use]
    pub fn snapshot(&self) -> Vec<TransferInfo> {
        let st = self.inner.state.lock();
        let mut v: Vec<TransferInfo> = st
            .items
            .iter()
            .map(|i| {
                let mut i = i.clone();
                i.speed_bps = if i.status == "running" {
                    st.runtime.get(&i.id).map_or(0, |r| r.ema_bps as u64)
                } else {
                    0
                };
                i
            })
            .collect();
        v.reverse();
        v.sort_by_key(|i| i.status != "running"); // stable: keeps newest-first within groups
        v
    }

    /// Removes finished transfers from the history.
    pub fn clear_finished(&self) {
        self.inner
            .state
            .lock()
            .items
            .retain(|i| i.status == "running");
        self.persist();
    }

    fn trim(st: &mut State) {
        // Never drop running transfers; drop the oldest finished ones beyond the limit.
        let finished = st.items.iter().filter(|i| i.status != "running").count();
        let mut excess = finished.saturating_sub(HISTORY_LIMIT);
        st.items.retain(|i| {
            if excess > 0 && i.status != "running" {
                excess -= 1;
                false
            } else {
                true
            }
        });
    }

    fn persist(&self) {
        let Some(file) = &self.inner.file else { return };
        let json = {
            let st = self.inner.state.lock();
            match serde_json::to_vec_pretty(&st.items) {
                Ok(j) => j,
                Err(e) => {
                    tracing::warn!("transfers.json: {e}");
                    return;
                }
            }
        };
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = file.with_extension("json.tmp");
        if let Err(e) = std::fs::write(&tmp, json).and_then(|()| std::fs::rename(&tmp, file)) {
            tracing::warn!("transfers.json konnte nicht geschrieben werden: {e}");
        }
    }

    fn with_item<R>(
        &self,
        id: u64,
        f: impl FnOnce(&mut TransferInfo, Option<&mut Runtime>) -> R,
    ) -> Option<R> {
        let mut st = self.inner.state.lock();
        let State { items, runtime } = &mut *st;
        let item = items.iter_mut().find(|i| i.id == id)?;
        Some(f(item, runtime.get_mut(&id)))
    }
}

/// Handle of one running transfer, used by the transfer tasks to report progress.
#[derive(Clone)]
pub struct TransferHandle {
    mgr: TransferManager,
    id: u64,
    cancelled: Arc<AtomicBool>,
}

impl TransferHandle {
    #[must_use]
    pub fn id(&self) -> u64 {
        self.id
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    /// Adds `n` transferred bytes (callable from several parallel streams).
    pub fn add_bytes(&self, n: u64) {
        self.mgr.with_item(self.id, |item, rt| {
            item.done_bytes += n;
            if let Some(rt) = rt {
                let dt = rt.last_at.elapsed();
                if dt >= SPEED_WINDOW {
                    let inst = (item.done_bytes - rt.last_bytes) as f64 / dt.as_secs_f64();
                    rt.ema_bps = if rt.ema_bps == 0.0 {
                        inst
                    } else {
                        0.6 * rt.ema_bps + 0.4 * inst
                    };
                    rt.last_at = Instant::now();
                    rt.last_bytes = item.done_bytes;
                }
            }
        });
    }

    /// Marks the transfer successful.
    pub fn finish(&self) {
        self.end("done", None);
    }

    /// Marks the transfer failed with a user-visible reason.
    pub fn fail(&self, reason: impl Into<String>) {
        self.end("failed", Some(reason.into()));
    }

    /// Marks the transfer cancelled.
    pub fn cancelled(&self) {
        self.end("cancelled", None);
    }

    fn end(&self, status: &str, error: Option<String>) {
        self.mgr.with_item(self.id, |item, _| {
            if item.status != "running" {
                return;
            }
            item.status = status.into();
            item.error = error;
            item.speed_bps = 0;
            if status == "done" {
                item.done_bytes = item.total_bytes.max(item.done_bytes);
            }
        });
        {
            let mut st = self.mgr.inner.state.lock();
            st.runtime.remove(&self.id);
            TransferManager::trim(&mut st);
        }
        self.mgr.persist();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_and_snapshot_order() {
        let m = TransferManager::new(None);
        let a = m.begin(1, Direction::Send, "a", 1, 100, None);
        let b = m.begin(2, Direction::Receive, "b", 2, 200, Some(r"C:\x".into()));
        a.add_bytes(40);
        b.add_bytes(200);
        b.finish();
        let s = m.snapshot();
        assert_eq!(s[0].id, 1);
        assert_eq!(s[0].status, "running");
        assert_eq!(s[0].done_bytes, 40);
        assert_eq!(s[1].id, 2);
        assert_eq!(s[1].status, "done");
        assert_eq!(s[1].direction, "receive");
        a.fail("Verbindung getrennt");
        let s = m.snapshot();
        assert_eq!(s[0].id, 2, "newest finished transfer comes first");
        let a_info = s.iter().find(|i| i.id == 1).unwrap();
        assert_eq!(a_info.status, "failed");
        assert_eq!(a_info.error.as_deref(), Some("Verbindung getrennt"));
    }

    #[test]
    fn speed_is_measured() {
        let m = TransferManager::new(None);
        let h = m.begin(1, Direction::Receive, "x", 1, 10_000_000, None);
        std::thread::sleep(Duration::from_millis(300));
        h.add_bytes(3_000_000);
        let s = m.snapshot();
        assert!(s[0].speed_bps > 1_000_000, "speed {}", s[0].speed_bps);
        h.finish();
        assert_eq!(m.snapshot()[0].speed_bps, 0);
    }

    #[test]
    fn history_is_limited_and_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let m = TransferManager::new(Some(dir.path().to_path_buf()));
        for i in 0..60u64 {
            let h = m.begin(i, Direction::Send, format!("t{i}"), 1, 10, None);
            h.add_bytes(10);
            h.finish();
        }
        let running = m.begin(1000, Direction::Receive, "run", 1, 10, None);
        assert_eq!(m.snapshot().len(), HISTORY_LIMIT + 1);

        // A fresh manager sees the history; the transfer that was running is marked failed.
        let m2 = TransferManager::new(Some(dir.path().to_path_buf()));
        let s = m2.snapshot();
        assert_eq!(s.len(), HISTORY_LIMIT + 1);
        let r = s.iter().find(|i| i.id == 1000).unwrap();
        assert_eq!(r.status, "failed");
        assert!(
            s.iter().all(|i| i.id >= 10 || i.id == 1000),
            "oldest were dropped"
        );
        assert!(dir.path().join("transfers.json").exists());
        running.cancelled();
        assert_eq!(
            m.snapshot().iter().find(|i| i.id == 1000).unwrap().status,
            "cancelled"
        );
    }

    #[test]
    fn cancel_flag_is_visible_to_the_handle() {
        let m = TransferManager::new(None);
        let h = m.begin(m.next_id(), Direction::Send, "x", 1, 1, None);
        assert!(!h.is_cancelled());
        assert!(m.cancel(h.id()));
        assert!(h.is_cancelled());
        assert!(!m.cancel(12345));
    }

    #[test]
    fn ids_are_unique() {
        let m = TransferManager::new(None);
        let ids: std::collections::HashSet<u64> = (0..1000).map(|_| m.next_id()).collect();
        assert_eq!(ids.len(), 1000);
    }
}
