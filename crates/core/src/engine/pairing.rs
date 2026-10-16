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
                Ok(p) => Msg::PairingFinished {
                    ok: true,
                    message: "Gekoppelt".into(),
                    peer: Some(p),
                    addr: None,
                },
                Err(e) => Msg::PairingFinished {
                    ok: false,
                    message: e.to_string(),
                    peer: None,
                    addr: None,
                },
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
        let port = self.cfg.network.port;
        let mut direct: Vec<SocketAddr> = Vec::new();
        let mut hosts: Vec<String> = Vec::new();
        if let Some(id) = device_id
            .as_deref()
            .and_then(|s| uuid::Uuid::parse_str(s).ok())
            && let Some(e) = self.discovered.get(&id)
        {
            direct.extend(e.d.socket_addrs());
        }
        // Manually entered address(es): "192.168.1.5", "192.168.1.5:47820", "laptop.local", several
        // separated by comma, semicolon or space.
        if let Some(a) = address {
            for part in a
                .split([',', ';', ' '])
                .map(str::trim)
                .filter(|p| !p.is_empty())
            {
                if let Ok(sa) = part.parse::<SocketAddr>() {
                    direct.push(sa);
                } else if let Ok(ip) = part.parse::<std::net::IpAddr>() {
                    direct.push(SocketAddr::new(ip, port));
                } else {
                    hosts.push(if part.contains(':') {
                        part.to_string()
                    } else {
                        format!("{part}:{port}")
                    });
                }
            }
        }
        if direct.is_empty() && hosts.is_empty() {
            return Err(
                "Gerät nicht gefunden – bitte die IP-Adresse des anderen Geräts eingeben".into(),
            );
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
            let fail = |message: String| Msg::PairingFinished {
                ok: false,
                message,
                peer: None,
                addr: None,
            };
            for h in hosts {
                if let Ok(it) = tokio::net::lookup_host(h).await {
                    direct.extend(it);
                }
            }
            let targets = crate::netaddr::rank(&direct, &crate::netaddr::local_v4());
            if targets.is_empty() {
                let _ = tx.send(fail("Adresse nicht auflösbar".into()));
                return;
            }
            // Try the best addresses first; later ones start with a short stagger so that one
            // unreachable (virtual adapter) address does not delay the working one.
            let mut set = tokio::task::JoinSet::new();
            for (i, addr) in targets.iter().copied().enumerate() {
                let t = transport.clone();
                set.spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(250 * i as u64)).await;
                    (
                        addr,
                        t.connect_pairing(addr).await.map_err(|e| e.to_string()),
                    )
                });
            }
            let mut errors: Vec<String> = Vec::new();
            let mut connected = None;
            while let Some(r) = set.join_next().await {
                match r {
                    Ok((addr, Ok(conn))) => {
                        connected = Some((addr, conn));
                        set.abort_all();
                        break;
                    }
                    Ok((addr, Err(e))) => errors.push(format!("{addr}: {e}")),
                    Err(_) => {}
                }
            }
            let Some((target, conn)) = connected else {
                let _ = tx.send(fail(format!(
                    "Keine Verbindung möglich. Versucht: {}. Bitte prüfen: beide Geräte im selben Netzwerk, \
                     Netzwerkprofil „Privat“ (die Firewall-Regel gilt nur dafür) und die IP-Adresse des \
                     anderen Geräts (steht in der Kopplungsansicht).",
                    errors.join(" | ")
                )));
                return;
            };
            let r = pairing::connect(&conn, &identity, &name, &code).await;
            conn.close(0u32.into(), b"done");
            let msg = match r {
                Ok(p) => Msg::PairingFinished {
                    ok: true,
                    message: "Gekoppelt".into(),
                    peer: Some(p),
                    addr: Some(target),
                },
                Err(e) => fail(e.to_string()),
            };
            let _ = tx.send(msg);
        });
        Ok(json!({ "started": true }))
    }

    pub(crate) fn on_pairing_finished(
        &mut self,
        ok: bool,
        message: String,
        peer: Option<PairedDevice>,
        addr: Option<SocketAddr>,
    ) {
        if ok {
            if let Some(p) = peer {
                // One paired peer in v1: replace older pairings.
                for old in self.peers.all() {
                    if old.id != p.device_id {
                        let _ = self.peers.remove(old.id);
                    }
                }
                let _ = self
                    .peers
                    .add(Peer::new(p.device_id, p.name.clone(), &p.fingerprint));
                if let Some(a) = addr {
                    let _ = self
                        .peers
                        .update_address(p.device_id, a.ip().to_string(), a.port());
                }
                self.pairing = None;
                self.backoff.reset();
                self.next_attempt = Instant::now();
                self.grace_applied = false;
                self.emit(UiEvent::PairingResult {
                    ok: true,
                    message,
                    peer_name: Some(p.name),
                });
            }
        } else {
            self.emit(UiEvent::PairingResult {
                ok: false,
                message,
                peer_name: None,
            });
        }
        self.dirty = true;
    }
}
