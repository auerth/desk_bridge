//! "Partnergerät auch aktualisieren": the installer package travels over the existing connection.
//! A plain offer is confirmed by the partner's user (plan 12.3). "Partner aktualisieren" (only
//! towards an older version) asks the partner to install it right away: it does so when its
//! user allows it (`update.allow_partner_install`) or asked for the update itself
//! ("Update vom Partner holen", `UpdateRequest`). The newer device sends the installer of its own
//! version, which the setup keeps next to the agent (`installer\DeskBridge-Setup-<ver>.exe`).
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
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub(crate) struct OutgoingUpdate {
    pub path: PathBuf,
    pub version: String,
    pub sha256: Vec<u8>,
}

/// An offer from the partner we accepted or still have to answer.
pub(crate) struct IncomingUpdate {
    pub offer: pb::UpdateOffer,
    /// Accepted without asking: install as soon as it arrived.
    pub auto: bool,
}

/// How long "Partner wird aktualisiert" waits for the partner to come back with the new version
/// (transfer of the ~250 MB installer plus the installation).
const PARTNER_UPDATE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// How long an `UpdateRequest` of ours lets an `install` offer through without the setting.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2 * 60);

/// Remote installs, both directions.
#[derive(Default)]
pub(crate) struct PartnerUpdate {
    /// Installer of the running version on this device (to pass on to an older partner).
    own_installer: Option<PathBuf>,
    /// What we update the partner to.
    target: Option<Target>,
    /// We asked the partner for its installer.
    requested: Option<Instant>,
}

struct Target {
    version: String,
    since: Instant,
    /// The partner reported that its installer runs: the connection drop that follows is
    /// expected.
    installing: bool,
}

impl PartnerUpdate {
    pub(crate) fn new(updates_dir: &Path) -> Self {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf));
        let own_installer =
            find_own_installer(exe_dir.as_deref(), updates_dir, crate::brand::VERSION);
        // Received packages are ~250 MB each: keep only what can still be used.
        let kept_by_setup = own_installer
            .as_deref()
            .is_some_and(|p| !p.starts_with(updates_dir));
        prune_updates(updates_dir, crate::brand::VERSION, !kept_by_setup);
        if let Some(p) = &own_installer {
            tracing::info!(path = %p.display(), "installer for the partner update available");
        }
        Self {
            own_installer,
            ..Self::default()
        }
    }
}

fn installer_name(version: &str) -> String {
    format!("DeskBridge-Setup-{version}.exe")
}

/// The installer of `version`: the copy the setup keeps next to the agent, else one received
/// from the partner.
pub(crate) fn find_own_installer(
    exe_dir: Option<&Path>,
    updates_dir: &Path,
    version: &str,
) -> Option<PathBuf> {
    let name = installer_name(version);
    exe_dir
        .map(|d| d.join("installer").join(&name))
        .into_iter()
        .chain(std::iter::once(updates_dir.join(&name)))
        .find(|p| p.is_file())
}

/// Deletes received installers older than `running` (and the one of `running` itself unless
/// `keep_running`: then it is the only copy to pass on). Newer ones stay ("Später").
pub(crate) fn prune_updates(dir: &Path, running: &str, keep_running: bool) {
    let Some(current) = parse_version(running) else {
        return;
    };
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_ascii_lowercase();
        let Some(v) = name
            .strip_prefix("deskbridge-setup-")
            .and_then(|r| r.strip_suffix(".exe"))
            .and_then(parse_version)
        else {
            continue;
        };
        if v < current || (v == current && !keep_running) {
            match std::fs::remove_file(e.path()) {
                Ok(()) => tracing::info!(file = %name, "old update package removed"),
                Err(err) => {
                    tracing::debug!(file = %name, "cannot remove old update package: {err}")
                }
            }
        }
    }
}

/// `a` is a newer version than `b`.
pub(crate) fn is_newer(a: &str, b: &str) -> bool {
    matches!((parse_version(a), parse_version(b)), (Some(a), Some(b)) if a > b)
}

/// What to do with an offer that asks to be installed right away.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RemoteInstall {
    Install,
    /// Not allowed without asking: show the normal offer dialog.
    Ask,
    Decline(String),
}

pub(crate) fn remote_install_decision(
    offered: &str,
    running: &str,
    allowed: bool,
    requested: bool,
) -> RemoteInstall {
    if parse_version(offered).is_none() {
        return RemoteInstall::Decline(format!("unbekanntes Versionsformat: {offered}"));
    }
    if !is_newer(offered, running) {
        return RemoteInstall::Decline(format!("Dort ist bereits Version {running} installiert."));
    }
    if allowed || requested {
        RemoteInstall::Install
    } else {
        RemoteInstall::Ask
    }
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
            installer_available: self.partner_update.own_installer.is_some(),
            partner_remote: self.peer_has_cap(deskbridge_proto::caps::UPDATE_REMOTE),
            partner_updating: self
                .partner_update
                .target
                .as_ref()
                .map(|t| t.version.clone()),
            updating_from_partner: self.partner_update.requested.is_some()
                || self.update_in.as_ref().is_some_and(|u| u.auto),
        }
    }

    fn peer_name(&self) -> String {
        self.session.as_ref().map_or_else(
            || "Das andere Gerät".to_string(),
            |s| s.link.peer.name.clone(),
        )
    }

    fn notify(&self, level: &str, text: String) {
        self.emit(UiEvent::Notification {
            level: level.into(),
            text,
        });
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

    /// UI: offer the installer at `path` (version `version`) to the connected partner;
    /// `install`: have it installed there right away.
    pub(crate) fn offer_partner_update(
        &mut self,
        path: PathBuf,
        version: String,
        install: bool,
    ) -> Result<Value, String> {
        let Some(s) = &self.session else {
            return Err("Nicht verbunden".into());
        };
        if !self.cfg.update.offer_partner_update {
            return Err("Partner-Update ist in den Einstellungen deaktiviert".into());
        }
        let peer_version = s.link.hello.app_version.clone();
        if install && !is_newer(&version, &peer_version) {
            return Err(format!(
                "Das andere Gerät hat bereits Version {peer_version}."
            ));
        }
        if self.partner_update.target.is_some() {
            return Err("Das andere Gerät wird bereits aktualisiert.".into());
        }
        if !path.is_file() {
            return Err(format!("Installer nicht gefunden: {}", path.display()));
        }
        if install {
            self.partner_update.target = Some(Target {
                version: version.clone(),
                since: Instant::now(),
                installing: false,
            });
        }
        self.dirty = true;
        // ~250 MB: hash off the actor thread (input keeps flowing).
        let tx = self.msg_tx.clone();
        tokio::task::spawn_blocking(move || {
            let result = std::fs::metadata(&path)
                .and_then(|m| Ok((m.len(), sha256_file(&path)?)))
                .map_err(|e| format!("Installer nicht lesbar: {e}"));
            let _ = tx.send(Msg::PartnerUpdateHashed {
                path,
                version,
                install,
                result,
            });
        });
        // An older partner does not know `install` yet and asks its user.
        let confirm = !install || !self.peer_has_cap(deskbridge_proto::caps::UPDATE_REMOTE);
        Ok(json!({ "offered": true, "confirm_on_partner": confirm }))
    }

    /// UI: send our own installer to the older partner and have it installed there.
    pub(crate) fn update_partner(&mut self) -> Result<Value, String> {
        let path = self.partner_update.own_installer.clone().ok_or_else(|| {
            format!(
                "Für Version {} liegt kein Installer vor. Bitte über „Installer an Partnergerät senden“ auswählen.",
                crate::brand::VERSION
            )
        })?;
        self.offer_partner_update(path, crate::brand::VERSION.to_string(), true)
    }

    pub(crate) fn on_partner_update_hashed(
        &mut self,
        path: PathBuf,
        version: String,
        install: bool,
        result: Result<(u64, Vec<u8>), String>,
    ) {
        self.dirty = true;
        let (size, sha) = match result {
            Ok(r) => r,
            Err(e) => {
                self.partner_update.target = None;
                self.notify("error", e);
                return;
            }
        };
        let Some(s) = &self.session else {
            self.partner_update.target = None;
            return;
        };
        tracing::info!(%version, size, install, "offering the partner an update");
        s.link
            .send_control(pb::control::Msg::UpdateOffer(pb::UpdateOffer {
                version: version.clone(),
                size,
                sha256: sha.clone(),
                install,
            }));
        self.update_out = Some(OutgoingUpdate {
            path,
            version,
            sha256: sha,
        });
    }

    /// UI: ask the newer partner for its installer (installed here as soon as it arrived).
    pub(crate) fn request_partner_update(&mut self) -> Result<Value, String> {
        let Some(s) = &self.session else {
            return Err("Nicht verbunden".into());
        };
        let peer_version = s.link.hello.app_version.clone();
        if !is_newer(&peer_version, crate::brand::VERSION) {
            return Err("Das andere Gerät hat keine neuere Version.".into());
        }
        if !self.peer_has_cap(deskbridge_proto::caps::UPDATE_REMOTE) {
            return Err(format!(
                "Das andere Gerät ({peer_version}) kann sein Update noch nicht auf Anfrage senden. Bitte dort „Installer an Partnergerät senden“ wählen."
            ));
        }
        s.link
            .send_control(pb::control::Msg::UpdateRequest(pb::UpdateRequest {}));
        tracing::info!(%peer_version, "asked the partner for its installer");
        self.partner_update.requested = Some(Instant::now());
        self.dirty = true;
        Ok(json!({ "requested": true }))
    }

    /// The older partner asks for our installer: push it (installed there right away).
    pub(crate) fn on_update_request(&mut self) {
        let peer = self.peer_name();
        tracing::info!(%peer, "partner asks for our installer");
        match self.update_partner() {
            Ok(_) => self.notify(
                "info",
                format!(
                    "{peer} holt sich Version {} von diesem Gerät.",
                    crate::brand::VERSION
                ),
            ),
            Err(e) => {
                tracing::info!("cannot serve the partner's update request: {e}");
                if let Some(s) = &self.session {
                    s.link
                        .send_control(pb::control::Msg::UpdateStatus(pb::UpdateStatus {
                            version: crate::brand::VERSION.into(),
                            error: e,
                        }));
                }
            }
        }
    }

    pub(crate) fn on_update_offer(&mut self, o: pb::UpdateOffer) {
        let Some(s) = &self.session else { return };
        let from = s.link.peer.name.clone();
        if o.install {
            let requested = self.partner_update.requested.is_some();
            match remote_install_decision(
                &o.version,
                crate::brand::VERSION,
                self.cfg.update.allow_partner_install,
                requested,
            ) {
                RemoteInstall::Install => {
                    tracing::info!(version = %o.version, %from, requested, "partner pushes an update: installing it");
                    s.link
                        .send_control(pb::control::Msg::UpdateAnswer(pb::UpdateAnswer {
                            accept: true,
                            reason: String::new(),
                        }));
                    self.notify(
                        "info",
                        format!(
                            "{from} aktualisiert Desk Bridge auf Version {}. Desk Bridge startet danach neu.",
                            o.version
                        ),
                    );
                    self.update_in = Some(IncomingUpdate {
                        offer: o,
                        auto: true,
                    });
                    self.partner_update.requested = None;
                    self.dirty = true;
                    return;
                }
                RemoteInstall::Decline(reason) => {
                    tracing::info!(version = %o.version, "partner update declined: {reason}");
                    s.link
                        .send_control(pb::control::Msg::UpdateAnswer(pb::UpdateAnswer {
                            accept: false,
                            reason,
                        }));
                    return;
                }
                RemoteInstall::Ask => {}
            }
        }
        self.emit(UiEvent::PartnerUpdateOffered {
            version: o.version.clone(),
            size: o.size,
            from,
        });
        self.update_in = Some(IncomingUpdate {
            offer: o,
            auto: false,
        });
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
            .send_control(pb::control::Msg::UpdateAnswer(pb::UpdateAnswer {
                accept,
                reason: String::new(),
            }));
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
            self.partner_update.target = None;
            self.dirty = true;
            let text = if a.reason.is_empty() {
                "Der Partner hat das Update abgelehnt.".to_string()
            } else {
                format!("Der Partner hat das Update abgelehnt: {}", a.reason)
            };
            self.notify("info", text);
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
            self.partner_update.target = None;
            self.dirty = true;
            self.notify("error", e);
        }
    }

    /// A verified package arrived: install it right away when that was agreed, otherwise let the
    /// user start it ("Update empfangen").
    pub(crate) fn on_partner_update_received(&mut self, path: PathBuf, version: String) {
        let auto = self
            .update_in
            .take()
            .is_some_and(|u| u.auto && u.offer.version == version);
        self.dirty = true;
        let link = self.session.as_ref().map(|s| s.link.clone());
        let (true, Some(link)) = (auto, link) else {
            self.emit(UiEvent::PartnerUpdateReady {
                path: path.to_string_lossy().into_owned(),
                version,
            });
            return;
        };
        tracing::info!(%version, path = %path.display(), "starting the update from the partner");
        let system = self.platform.system.clone();
        let pushes = self.pushes.clone();
        // Off the actor thread: without elevation Windows asks for consent first.
        tokio::task::spawn_blocking(move || {
            let error = match system.launch_installer(&path) {
                Ok(()) => String::new(),
                Err(e) => {
                    tracing::warn!("cannot start the partner's installer: {e}");
                    // The user here can still start it from the dialog.
                    let _ = pushes.send(crate::ipc::Push::Event {
                        event: UiEvent::PartnerUpdateReady {
                            path: path.to_string_lossy().into_owned(),
                            version: version.clone(),
                        },
                    });
                    e.to_string()
                }
            };
            link.send_control(pb::control::Msg::UpdateStatus(pb::UpdateStatus {
                version,
                error,
            }));
        });
    }

    /// The partner reports how its installation (or our request) went.
    pub(crate) fn on_update_status(&mut self, st: pb::UpdateStatus) {
        let peer = self.peer_name();
        self.dirty = true;
        if st.error.is_empty() {
            tracing::info!(version = %st.version, "partner started the installer");
            if let Some(t) = &mut self.partner_update.target {
                t.installing = true;
            }
            self.notify(
                "info",
                format!(
                    "{peer} installiert jetzt Version {}. Die Verbindung trennt sich kurz und kommt mit der neuen Version zurück.",
                    st.version
                ),
            );
            return;
        }
        tracing::info!(version = %st.version, "partner update failed: {}", st.error);
        let text = if self.partner_update.requested.take().is_some() {
            format!("Update von {peer} nicht möglich: {}", st.error)
        } else {
            format!("Update auf {peer} fehlgeschlagen: {}", st.error)
        };
        self.partner_update.target = None;
        self.notify("error", text);
    }

    /// Connected (again): is the partner back with the version we pushed?
    pub(crate) fn on_session_up_update(&mut self) {
        let Some(t) = self.partner_update.target.take() else {
            return;
        };
        let Some(s) = &self.session else { return };
        let (peer, running) = (s.link.peer.name.clone(), s.link.hello.app_version.clone());
        self.dirty = true;
        if running == t.version {
            tracing::info!(version = %t.version, "partner is back with the pushed version");
            self.notify("info", format!("{peer} läuft jetzt mit Version {running}."));
        } else {
            tracing::info!(version = %t.version, %running, "partner is back without the pushed version");
            self.notify(
                "error",
                format!(
                    "Das Update auf {peer} hat nicht geklappt, dort läuft noch Version {running}. Details in den Logs des Partnergeräts."
                ),
            );
        }
    }

    /// Connection lost: unless the partner's installer is running (that ends the connection),
    /// the update did not happen and may be started again.
    pub(crate) fn on_session_down_update(&mut self) {
        if self
            .partner_update
            .target
            .as_ref()
            .is_some_and(|t| !t.installing)
        {
            self.partner_update.target = None;
        }
        self.update_out = None;
        if self.update_in.as_ref().is_some_and(|u| u.auto) {
            self.update_in = None;
        }
        self.partner_update.requested = None;
        self.dirty = true;
    }

    pub(crate) fn tick_partner_update(&mut self) {
        let p = &mut self.partner_update;
        if p.target
            .as_ref()
            .is_some_and(|t| t.since.elapsed() > PARTNER_UPDATE_TIMEOUT)
        {
            tracing::info!("partner did not come back with the pushed version");
            p.target = None;
            self.dirty = true;
        }
        if p.requested.is_some_and(|t| t.elapsed() > REQUEST_TIMEOUT) {
            p.requested = None;
            self.dirty = true;
        }
    }

    /// Called by the transfer code when an announced update file arrived.
    pub(crate) fn update_meta_from_hint(&self, hint: &str) -> Option<UpdateMeta> {
        let rest = hint.strip_prefix("update:")?;
        let (version, sha_hex) = rest.rsplit_once(':')?;
        let sha = deskbridge_net::unhex(sha_hex)?;
        // Only accept what we agreed to.
        let offer = &self.update_in.as_ref()?.offer;
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

    #[test]
    fn remote_install_only_upgrades_and_respects_the_setting() {
        use RemoteInstall::{Ask, Install};
        assert_eq!(
            remote_install_decision("0.1.7", "0.1.6", true, false),
            Install
        );
        assert_eq!(remote_install_decision("0.1.7", "0.1.6", false, false), Ask);
        // Asked for it on this device: no setting needed.
        assert_eq!(
            remote_install_decision("0.1.7", "0.1.6", false, true),
            Install
        );
        // Never the same or an older version, never something unreadable.
        for (offered, running) in [("0.1.6", "0.1.6"), ("0.1.5", "0.1.6"), ("x", "0.1.6")] {
            assert!(
                matches!(
                    remote_install_decision(offered, running, true, true),
                    RemoteInstall::Decline(_)
                ),
                "{offered} over {running}"
            );
        }
        assert!(is_newer("0.2.0", "0.1.9"));
        assert!(!is_newer("0.1.9", "0.1.9"));
        assert!(!is_newer("", "0.1.9"));
    }

    #[test]
    fn own_installer_prefers_the_setup_copy_and_old_packages_are_pruned() {
        let root = tempfile::tempdir().unwrap();
        let exe_dir = root.path().join("app");
        let updates = root.path().join("Updates");
        std::fs::create_dir_all(exe_dir.join("installer")).unwrap();
        std::fs::create_dir_all(&updates).unwrap();
        assert_eq!(find_own_installer(Some(&exe_dir), &updates, "0.1.7"), None);

        let received = updates.join("DeskBridge-Setup-0.1.7.exe");
        std::fs::write(&received, "r").unwrap();
        assert_eq!(
            find_own_installer(Some(&exe_dir), &updates, "0.1.7").as_deref(),
            Some(received.as_path())
        );
        let kept = exe_dir.join("installer").join("DeskBridge-Setup-0.1.7.exe");
        std::fs::write(&kept, "k").unwrap();
        assert_eq!(
            find_own_installer(Some(&exe_dir), &updates, "0.1.7").as_deref(),
            Some(kept.as_path())
        );

        for v in ["0.1.5", "0.1.8"] {
            std::fs::write(updates.join(format!("DeskBridge-Setup-{v}.exe")), "x").unwrap();
        }
        std::fs::write(updates.join("notes.txt"), "x").unwrap();
        prune_updates(&updates, "0.1.7", true);
        assert!(!updates.join("DeskBridge-Setup-0.1.5.exe").exists());
        assert!(received.exists(), "only copy to pass on");
        assert!(
            updates.join("DeskBridge-Setup-0.1.8.exe").exists(),
            "not installed yet"
        );
        assert!(updates.join("notes.txt").exists());
        prune_updates(&updates, "0.1.7", false);
        assert!(!received.exists());
    }
}
