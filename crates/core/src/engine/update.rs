//! "Partnergerät auch aktualisieren": the installer package travels over the existing connection,
//! the partner is asked first and never installs anything automatically (plan 12.3).

use super::Engine;
use super::transfers::UpdateMeta;
use crate::ipc::UiEvent;
use deskbridge_proto::pb;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

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

impl Engine {
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
