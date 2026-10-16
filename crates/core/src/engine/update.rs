//! "Partnergerät auch aktualisieren": the installer package travels over the existing connection,
//! the partner is asked first and never installs anything automatically (plan 12.3).
//!
//! Release check: the newest GitHub release is compared with the running version; a newer one
//! is shown with a link to its release page. Nothing is downloaded or installed automatically.

use super::transfers::UpdateMeta;
use super::{Engine, Msg};
use crate::ipc::UiEvent;
use crate::ipc::UpdateDto;
use deskbridge_proto::pb;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub(crate) struct OutgoingUpdate {
    pub path: PathBuf,
    pub version: String,
    pub sha256: Vec<u8>,
}

pub(crate) fn sha256_file(path: &std::path::Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().to_vec())
}

/// First automatic check after the agent started (lets the network come up).
const FIRST_CHECK_AFTER: Duration = Duration::from_secs(30);
const CHECK_EVERY: Duration = Duration::from_secs(24 * 3600);

/// A release newer than the running version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReleaseInfo {
    pub version: String,
    pub url: String,
    pub notes: String,
}

#[derive(Debug)]
pub(crate) struct ReleaseCheck {
    started: Instant,
    checking: bool,
    last_attempt: Option<Instant>,
    last_checked_unix_ms: Option<u64>,
    available: Option<ReleaseInfo>,
    error: Option<String>,
}

impl Default for ReleaseCheck {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            checking: false,
            last_attempt: None,
            last_checked_unix_ms: None,
            available: None,
            error: None,
        }
    }
}

/// "v1.2.3", "1.2.3", "v1.2.3-beta.1" → `(1, 2, 3)`.
pub(crate) fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim().trim_start_matches(['v', 'V']);
    let core = s.split(['-', '+']).next()?;
    let mut it = core.split('.').map(|p| p.parse::<u64>().ok());
    let v = (
        it.next()??,
        it.next().flatten().unwrap_or(0),
        it.next().flatten().unwrap_or(0),
    );
    Some(v)
}

/// Interprets the answer of `releases/latest`: the release if it is newer than `current`.
pub(crate) fn parse_latest_release(
    status: u16,
    body: &str,
    current: &str,
) -> Result<Option<ReleaseInfo>, String> {
    match status {
        200 => {}
        // No release published yet.
        404 => return Ok(None),
        403 | 429 => return Err("GitHub-Abfragelimit erreicht, später erneut".into()),
        s => return Err(format!("GitHub antwortet mit HTTP {s}")),
    }
    let v: Value = serde_json::from_str(body).map_err(|e| format!("Antwort unlesbar: {e}"))?;
    let tag = v["tag_name"].as_str().unwrap_or_default();
    let (Some(latest), Some(running)) = (parse_version(tag), parse_version(current)) else {
        return Err(format!("unbekanntes Versionsformat: {tag}"));
    };
    if latest <= running {
        return Ok(None);
    }
    Ok(Some(ReleaseInfo {
        version: tag.trim_start_matches(['v', 'V']).to_string(),
        url: v["html_url"]
            .as_str()
            .unwrap_or(crate::brand::RELEASES_URL)
            .to_string(),
        notes: v["body"].as_str().unwrap_or_default().to_string(),
    }))
}

impl Engine {
    pub(crate) fn update_dto(&self) -> UpdateDto {
        let c = &self.release_check;
        UpdateDto {
            current: crate::brand::VERSION.into(),
            available: c.available.as_ref().map(|r| r.version.clone()),
            checking: c.checking,
            url: c.available.as_ref().map(|r| r.url.clone()),
            error: c.error.clone(),
            last_checked_unix_ms: c.last_checked_unix_ms,
        }
    }

    /// Asks GitHub for the newest release in the background.
    pub(crate) fn start_release_check(&mut self) {
        if self.release_check.checking {
            return;
        }
        self.release_check.checking = true;
        self.release_check.last_attempt = Some(Instant::now());
        self.dirty = true;
        let web = self.platform.web.clone();
        let tx = self.msg_tx.clone();
        tokio::task::spawn_blocking(move || {
            let r = web
                .get(crate::brand::LATEST_RELEASE_API)
                .map_err(|e| e.to_string())
                .and_then(|(status, body)| {
                    parse_latest_release(status, &body, crate::brand::VERSION)
                });
            let _ = tx.send(Msg::UpdateChecked(r));
        });
    }

    /// Automatic check: 30 s after start, then daily (if enabled).
    pub(crate) fn tick_release_check(&mut self) {
        if !self.cfg.update.check_automatically || self.release_check.checking {
            return;
        }
        let due = match self.release_check.last_attempt {
            None => self.release_check.started.elapsed() >= FIRST_CHECK_AFTER,
            Some(t) => t.elapsed() >= CHECK_EVERY,
        };
        if due {
            self.start_release_check();
        }
    }

    pub(crate) fn on_update_checked(&mut self, r: Result<Option<ReleaseInfo>, String>) {
        let c = &mut self.release_check;
        c.checking = false;
        match r {
            Ok(found) => {
                c.error = None;
                c.last_checked_unix_ms = Some(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.as_millis() as u64),
                );
                let new = found.is_some() && found != c.available;
                c.available = found;
                if new && let Some(rel) = c.available.clone() {
                    tracing::info!(version = %rel.version, url = %rel.url, "new release available");
                    self.emit(UiEvent::UpdateAvailable {
                        version: rel.version,
                        notes: rel.notes,
                    });
                }
            }
            Err(e) => {
                tracing::info!("release check failed: {e}");
                c.error = Some(e);
            }
        }
        self.dirty = true;
    }

    /// UI: offer the installer at `path` (version `version`) to the connected partner.
    pub(crate) fn offer_partner_update(
        &mut self,
        path: String,
        version: String,
    ) -> Result<Value, String> {
        let Some(s) = &self.session else {
            return Err("Nicht verbunden".into());
        };
        if !self.cfg.update.offer_partner_update {
            return Err("Partner-Update ist in den Einstellungen deaktiviert".into());
        }
        let path = PathBuf::from(path);
        let size = std::fs::metadata(&path)
            .map_err(|e| format!("Installer nicht lesbar: {e}"))?
            .len();
        let sha = sha256_file(&path).map_err(|e| format!("Installer nicht lesbar: {e}"))?;
        s.link
            .send_control(pb::control::Msg::UpdateOffer(pb::UpdateOffer {
                version: version.clone(),
                size,
                sha256: sha.clone(),
            }));
        self.update_out = Some(OutgoingUpdate {
            path,
            version,
            sha256: sha,
        });
        Ok(json!({ "offered": true }))
    }

    pub(crate) fn on_update_offer(&mut self, o: pb::UpdateOffer) {
        let from = self
            .session
            .as_ref()
            .map(|s| s.link.peer.name.clone())
            .unwrap_or_default();
        self.emit(UiEvent::PartnerUpdateOffered {
            version: o.version.clone(),
            size: o.size,
            from,
        });
        self.update_in = Some(o);
    }

    /// UI: accept or decline the partner's offer.
    pub(crate) fn answer_partner_update(&mut self, accept: bool) -> Result<Value, String> {
        let Some(s) = &self.session else {
            return Err("Nicht verbunden".into());
        };
        if self.update_in.is_none() {
            return Err("Kein Update-Angebot vorhanden".into());
        }
        s.link
            .send_control(pb::control::Msg::UpdateAnswer(pb::UpdateAnswer { accept }));
        if !accept {
            self.update_in = None;
        }
        Ok(json!({ "ok": true }))
    }

    pub(crate) fn on_update_answer(&mut self, a: pb::UpdateAnswer) {
        let Some(out) = self.update_out.take() else {
            return;
        };
        if !a.accept {
            self.emit(UiEvent::Notification {
                level: "info".into(),
                text: "Der Partner hat das Update abgelehnt.".into(),
            });
            return;
        }
        let hint = format!(
            "update:{}:{}",
            out.version,
            deskbridge_net::hex(&out.sha256)
        );
        if let Err(e) =
            self.send_files_with_hint(vec![out.path.to_string_lossy().into_owned()], hint)
        {
            self.emit(UiEvent::Notification {
                level: "error".into(),
                text: e,
            });
        }
    }

    /// Called by the transfer code when an announced update file arrived.
    pub(crate) fn update_meta_from_hint(&self, hint: &str) -> Option<UpdateMeta> {
        let rest = hint.strip_prefix("update:")?;
        let (version, sha_hex) = rest.rsplit_once(':')?;
        let sha = deskbridge_net::unhex(sha_hex)?;
        // Only accept what we agreed to.
        let offer = self.update_in.as_ref()?;
        (offer.version == version && offer.sha256 == sha).then(|| UpdateMeta {
            version: version.to_string(),
            sha256: sha,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_parse() {
        assert_eq!(parse_version("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("0.2.0-beta.1"), Some((0, 2, 0)));
        assert_eq!(parse_version("V2.0"), Some((2, 0, 0)));
        assert_eq!(parse_version("release"), None);
    }

    #[test]
    fn latest_release_is_compared_with_the_running_version() {
        let body = r#"{"tag_name":"v0.2.0","html_url":"https://github.com/auerth/desk_bridge/releases/tag/v0.2.0","body":"Notes"}"#;
        let r = parse_latest_release(200, body, "0.1.0").unwrap().unwrap();
        assert_eq!(r.version, "0.2.0");
        assert!(r.url.ends_with("/tag/v0.2.0"));
        assert_eq!(r.notes, "Notes");
        assert_eq!(parse_latest_release(200, body, "0.2.0").unwrap(), None);
        assert_eq!(parse_latest_release(200, body, "0.3.1").unwrap(), None);
        // No release published yet.
        assert_eq!(parse_latest_release(404, "", "0.1.0").unwrap(), None);
        assert!(parse_latest_release(403, "", "0.1.0").is_err());
        assert!(parse_latest_release(200, "not json", "0.1.0").is_err());
    }
}
