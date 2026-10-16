//! Pairing glue: show a code (server) / enter a code (client).

use super::{Engine, Msg};
use crate::ipc::UiEvent;
use deskbridge_net::pairing::{self, PairedDevice, PairingWindow};
use deskbridge_net::peers::Peer;
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

impl Engine {
    pub(crate) fn start_pairing(&mut self) -> Result<Value, String> {
        let code = pairing::generate_code();
        let w = Arc::new(PairingWindow::new(code.clone()));
        let secs = w.seconds_left();
        self.pairing = Some(w);
        self.dirty = true;
        tracing::info!("pairing window opened");
        Ok(json!({ "code": code, "seconds_left": secs }))
    }

    pub(crate) fn cancel_pairing(&mut self) {
        self.pairing = None;
        self.dirty = true;
    }

    /// An unpaired device connected on the pairing ALPN.
    pub(crate) fn on_pairing_conn(&mut self, conn: quinn::Connection) {
        let Some(window) = self.pairing.clone() else {
            conn.close(0x403u32.into(), b"not pairing");
            return;
        };
        let identity = self.identity.clone();
        let name = self.cfg.device.name.clone();
        let tx = self.msg_tx.clone();
        tokio::spawn(async move {
            let res = pairing::serve(&conn, &identity, &name, &window).await;
            let msg = match res {
                Ok(p) => Msg::PairingFinished { ok: true, message: "Gekoppelt".into(), peer: Some(p), addr: None },
                Err(e) => Msg::PairingFinished { ok: false, message: e.to_string(), peer: None, addr: None },
            };
            // Give the final stream data a moment to flush before closing.
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            conn.close(0u32.into(), b"done");
            let _ = tx.send(msg);
        });
    }

    pub(crate) fn pair_with(
        &mut self,
        device_id: Option<String>,
        address: Option<String>,
        code: String,
    ) -> Result<Value, String> {
        let mut addr: Option<SocketAddr> = None;
        let mut host: Option<String> = None;
        if let Some(id) = device_id.as_deref().and_then(|s| uuid::Uuid::parse_str(s).ok()) {
            if let Some(e) = self.discovered.get(&id) {
                addr = e.d.socket_addrs().into_iter().find(SocketAddr::is_ipv4).or_else(|| e.d.socket_addrs().into_iter().next());
            }
        }
        if addr.is_none() {
            if let Some(a) = address {
                let a = a.trim().to_string();
                if let Ok(sa) = a.parse::<SocketAddr>() {
                    addr = Some(sa);
                } else if let Ok(ip) = a.parse::<std::net::IpAddr>() {
                    addr = Some(SocketAddr::new(ip, self.cfg.network.port));
                } else if !a.is_empty() {
                    host = Some(if a.contains(':') { a } else { format!("{a}:{}", self.cfg.network.port) });
                }
            }
        }
        if addr.is_none() && host.is_none() {
            return Err("Gerät nicht gefunden – bitte Adresse eingeben".into());
        }
        let code = code.trim().to_string();
        if code.len() != pairing::CODE_LEN || !code.bytes().all(|b| b.is_ascii_digit()) {
            return Err("Der Code besteht aus 6 Ziffern".into());
        }
        let transport = self.transport.clone();
        let identity = self.identity.clone();
        let name = self.cfg.device.name.clone();
        let tx = self.msg_tx.clone();
        tokio::spawn(async move {
            let target = match (addr, host) {
                (Some(a), _) => Some(a),
                (None, Some(h)) => tokio::net::lookup_host(h).await.ok().and_then(|mut i| i.next()),
                _ => None,
            };
            let Some(target) = target else {
                let _ = tx.send(Msg::PairingFinished { ok: false, message: "Adresse nicht auflösbar".into(), peer: None, addr: None });
                return;
            };
            let msg = match transport.connect_pairing(target).await {
                Err(e) => Msg::PairingFinished { ok: false, message: format!("Verbindung fehlgeschlagen: {e}"), peer: None, addr: None },
                Ok(conn) => {
                    let r = pairing::connect(&conn, &identity, &name, &code).await;
                    conn.close(0u32.into(), b"done");
                    match r {
                        Ok(p) => Msg::PairingFinished { ok: true, message: "Gekoppelt".into(), peer: Some(p), addr: Some(target) },
                        Err(e) => Msg::PairingFinished { ok: false, message: e.to_string(), peer: None, addr: None },
                    }
                }
            };
            let _ = tx.send(msg);
        });
        Ok(json!({ "started": true }))
    }

    pub(crate) fn on_pairing_finished(&mut self, ok: bool, message: String, peer: Option<PairedDevice>, addr: Option<SocketAddr>) {
        if ok {
            if let Some(p) = peer {
                // One paired peer in v1: replace older pairings.
                for old in self.peers.all() {
                    if old.id != p.device_id {
                        let _ = self.peers.remove(old.id);
                    }
                }
                let _ = self.peers.add(Peer::new(p.device_id, p.name.clone(), &p.fingerprint));
                if let Some(a) = addr {
                    let _ = self.peers.update_address(p.device_id, a.ip().to_string(), a.port());
                }
                self.pairing = None;
                self.backoff.reset();
                self.next_attempt = Instant::now();
                self.grace_applied = false;
                self.emit(UiEvent::PairingResult { ok: true, message, peer_name: Some(p.name) });
            }
        } else {
            self.emit(UiEvent::PairingResult { ok: false, message, peer_name: None });
        }
        self.dirty = true;
    }
}
