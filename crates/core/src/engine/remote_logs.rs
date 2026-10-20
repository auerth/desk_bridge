//! Remote logs (capability `logs`): fetch the recent log files of the paired device over the
//! control stream, keep them on disk (`<data>/peer-logs/<peer>-<ts>/`) and show them in the UI.
//! Only log files and the system info block travel, never config secrets or keys.

use super::Engine;
use crate::ipc::{PeerLogFileDto, UiEvent};
use deskbridge_proto::pb;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// Newest log files sent per request.
const MAX_FILES: usize = 10;
/// Tail of one file that is sent.
const MAX_FILE_BYTES: u64 = 768 * 1024;
/// All files together (the control stream carries frames up to 8 MiB).
const MAX_TOTAL_BYTES: u64 = 4 * 1024 * 1024;
/// Give up waiting for the answer.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Default)]
pub(crate) struct RemoteLogs {
    next_id: u32,
    pending: Option<(u32, Instant)>,
    /// Last fetched set: folder and files (content kept for `GetPeerLog`).
    last: Option<(PathBuf, Vec<pb::LogFile>)>,
}

impl Engine {
    /// UI: ask the partner for its logs; the answer arrives as `UiEvent::PeerLogs`.
    pub(crate) fn fetch_peer_logs(&mut self) -> Result<Value, String> {
        let Some(s) = &self.session else {
            return Err("Kein Gerät verbunden".into());
        };
        if !s
            .link
            .hello
            .caps
            .iter()
            .any(|c| c.name == deskbridge_proto::caps::LOGS)
        {
            return Err(
                "Das andere Gerät kann noch keine Logs senden (bitte dort auf die gleiche Version aktualisieren)."
                    .into(),
            );
        }
        self.remote_logs.next_id = self.remote_logs.next_id.wrapping_add(1).max(1);
        let id = self.remote_logs.next_id;
        self.remote_logs.pending = Some((id, Instant::now()));
        s.link
            .send_control(pb::control::Msg::LogsRequest(pb::LogsRequest {
                request_id: id,
            }));
        Ok(json!({ "started": true }))
    }

    /// UI: content of one file of the last fetched set.
    pub(crate) fn get_peer_log(&self, name: &str) -> Result<Value, String> {
        let Some((_, files)) = &self.remote_logs.last else {
            return Err("Noch keine Logs abgerufen".into());
        };
        let f = files
            .iter()
            .find(|f| f.name == name)
            .ok_or_else(|| format!("Log „{name}“ nicht gefunden"))?;
        Ok(
            json!({ "name": f.name, "content": f.content, "truncated": f.truncated, "size": f.size }),
        )
    }

    /// Partner asks for our logs: collect them off the actor thread and answer.
    pub(crate) fn on_logs_request(&mut self, r: pb::LogsRequest) {
        let Some(s) = &self.session else { return };
        let link = s.link.clone();
        let logs_dir = self.paths.logs_dir();
        let info = self.system_info();
        tracing::info!("partner requested our logs");
        tokio::task::spawn_blocking(move || {
            let mut files = vec![pb::LogFile {
                name: "system-info.txt".into(),
                size: info.len() as u64,
                content: info,
                truncated: false,
            }];
            files.extend(collect_logs(&log_dirs(&logs_dir)));
            link.send_control(pb::control::Msg::LogsResponse(pb::LogsResponse {
                request_id: r.request_id,
                files,
                error: String::new(),
            }));
        });
    }

    pub(crate) fn on_logs_response(&mut self, r: pb::LogsResponse) {
        if self.remote_logs.pending.map(|(id, _)| id) != Some(r.request_id) {
            return;
        }
        self.remote_logs.pending = None;
        let peer = self
            .session
            .as_ref()
            .map_or_else(|| "partner".to_string(), |s| s.link.peer.name.clone());
        if !r.error.is_empty() {
            self.emit_peer_logs(&peer, None, &[], &r.error);
            return;
        }
        let dir = self.paths.data_dir.join("peer-logs").join(format!(
            "{}-{}",
            deskbridge_files::sanitize::sanitize_component(&peer),
            unix_secs()
        ));
        let saved = save(&dir, &r.files);
        if let Err(e) = &saved {
            tracing::warn!("cannot save the partner logs: {e}");
        }
        tracing::info!(files = r.files.len(), dir = %dir.display(), "partner logs received");
        let dir = saved.is_ok().then_some(dir);
        self.emit_peer_logs(&peer, dir.as_deref(), &r.files, "");
        self.remote_logs.last = Some((dir.unwrap_or_default(), r.files));
    }

    pub(crate) fn tick_remote_logs(&mut self) {
        if let Some((_, since)) = self.remote_logs.pending
            && since.elapsed() > ANSWER_TIMEOUT
        {
            self.remote_logs.pending = None;
            self.emit_peer_logs("", None, &[], "Das andere Gerät hat nicht geantwortet.");
        }
    }

    fn emit_peer_logs(&self, peer: &str, dir: Option<&Path>, files: &[pb::LogFile], error: &str) {
        self.emit(UiEvent::PeerLogs {
            peer: peer.to_string(),
            folder: dir.map(|d| d.to_string_lossy().into_owned()),
            files: files
                .iter()
                .map(|f| PeerLogFileDto {
                    name: f.name.clone(),
                    size: f.size,
                    truncated: f.truncated,
                })
                .collect(),
            error: (!error.is_empty()).then(|| error.to_string()),
        });
    }
}

/// The user's log folder plus the service's (`%ProgramData%\softwelop\DeskBridge\logs`).
fn log_dirs(user_logs: &Path) -> Vec<(PathBuf, &'static str)> {
    let mut v = vec![(user_logs.to_path_buf(), "")];
    if let Some(pd) = std::env::var_os("ProgramData") {
        let p = PathBuf::from(pd)
            .join("softwelop")
            .join("DeskBridge")
            .join("logs");
        if p != user_logs {
            v.push((p, "service/"));
        }
    }
    v
}

/// The newest log files of `dirs` (by modification time), tails only, within the size budget.
fn collect_logs(dirs: &[(PathBuf, &str)]) -> Vec<pb::LogFile> {
    let mut found: Vec<(SystemTime, PathBuf, String, u64)> = Vec::new();
    for (dir, prefix) in dirs {
        let Ok(rd) = std::fs::read_dir(dir) else {
            continue;
        };
        for e in rd.flatten() {
            let Ok(m) = e.metadata() else { continue };
            if !m.is_file() {
                continue;
            }
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.contains(".log") {
                continue;
            }
            let mtime = m.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            found.push((mtime, e.path(), format!("{prefix}{name}"), m.len()));
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0));
    let mut out = Vec::new();
    let mut budget = MAX_TOTAL_BYTES;
    for (_, path, name, size) in found.into_iter().take(MAX_FILES) {
        if budget == 0 {
            break;
        }
        let take = size.min(MAX_FILE_BYTES).min(budget);
        let Ok(content) = read_tail(&path, take) else {
            continue;
        };
        budget = budget.saturating_sub(take);
        out.push(pb::LogFile {
            name,
            content,
            truncated: take < size,
            size,
        });
    }
    out
}

/// The last `n` bytes of a file as text, starting at a line boundary when cut.
fn read_tail(path: &Path, n: u64) -> std::io::Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let start = len.saturating_sub(n);
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::with_capacity(usize::try_from(n).unwrap_or(0));
    f.take(n).read_to_end(&mut buf)?;
    let mut text = String::from_utf8_lossy(&buf).into_owned();
    if start > 0
        && let Some(i) = text.find('\n')
    {
        text.drain(..=i);
    }
    Ok(text)
}

fn save(dir: &Path, files: &[pb::LogFile]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    for f in files {
        let rel: PathBuf = f
            .name
            .split('/')
            .map(deskbridge_files::sanitize::sanitize_component)
            .collect();
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(p, f.content.as_bytes())?;
    }
    Ok(())
}

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_newest_tails_within_budget() {
        let dir = std::env::temp_dir().join(format!("db-remote-logs-{}", unix_secs()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.log.2026-10-19"), "old\n").unwrap();
        let big: String = (0..100_000).map(|i| format!("line {i}\n")).collect();
        std::fs::write(dir.join("agent.log.2026-10-20"), &big).unwrap();
        std::fs::write(dir.join("identity.bin"), "secret").unwrap();
        let files = collect_logs(&[(dir.clone(), "")]);
        assert_eq!(files.len(), 2, "only *.log* files");
        let newest = files
            .iter()
            .find(|f| f.name == "agent.log.2026-10-20")
            .unwrap();
        assert!(newest.truncated);
        assert!(newest.content.len() as u64 <= MAX_FILE_BYTES);
        assert!(
            newest.content.starts_with("line "),
            "cut at a line boundary"
        );
        assert!(newest.content.ends_with("line 99999\n"));
        assert!(files.iter().all(|f| !f.name.contains("identity")));

        let out = dir.join("saved");
        save(&out, &files).unwrap();
        assert!(out.join("agent.log.2026-10-19").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
