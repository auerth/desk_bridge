//! Connector: reconnect loop, accepting sessions, duplicate resolution, teardown.

use super::session::{self, PeerLink};
use super::{ActiveSession, Engine, Msg, since_unix_ms};
use crate::ipc::{ConnState, UiEvent};
use deskbridge_net::NetError;
use deskbridge_net::peers::Peer;
use deskbridge_net::transport::{Accepted, close};
use deskbridge_proto::pb;
use std::net::SocketAddr;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// How long the device with the *smaller* id waits before dialling, so that normally the device
/// with the larger id initiates (avoids simultaneous double connections).
const LESSER_GRACE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
enum Candidate {
    Addr(SocketAddr),
    Host(String),
}

impl Engine {
    pub(crate) fn maybe_connect(&mut self) {
        if self.cfg.general.paused || self.connecting.is_some() || self.session.is_some() {
            return;
        }
        let Some(peer) = self.peers.all().into_iter().next() else { return };
        let now = Instant::now();
        if !self.grace_applied {
            self.grace_applied = true;
            if self.own_id() < peer.id {
                self.next_attempt = self.next_attempt.max(now + LESSER_GRACE);
            }
        }
        if now < self.next_attempt {
            return;
        }
        let candidates = self.candidates(&peer);
        if candidates.is_empty() {
            // Nothing known yet: wait for discovery.
            self.next_attempt = now + Duration::from_millis(500);
            return;
        }
        self.refresh_hello();
        self.connecting = Some(peer.id);
        self.conn_state = ConnState::Connecting;
        self.dirty = true;

        let transport = self.transport.clone();
        let hello = self.hello.read().clone();
        let tx = self.msg_tx.clone();
        let sid = self.next_session_id();
        tokio::spawn(async move {
            let res = dial(&transport, &peer, candidates).await;
            match res {
                Ok(sc) => match session::establish(sc, hello, sid, tx.clone()).await {
                    Ok(link) => {
                        let _ = tx.send(Msg::SessionReady(link));
                    }
                    Err(e) => {
                        let _ = tx.send(Msg::ConnectFailed { peer: peer.id, error: e.to_string() });
                    }
                },
                Err(e) => {
                    let _ = tx.send(Msg::ConnectFailed { peer: peer.id, error: e.to_string() });
                }
            }
        });
    }

    fn candidates(&self, peer: &Peer) -> Vec<Candidate> {
        let mut out: Vec<Candidate> = Vec::new();
        let port_default = self.cfg.network.port;
        if let Some(e) = self.discovered.get(&peer.id) {
            for a in e.d.socket_addrs() {
                out.push(Candidate::Addr(a));
            }
        }
        if let (Some(ip), Some(port)) = (&peer.last_ip, peer.last_port) {
            if let Ok(ip) = ip.parse() {
                let a = SocketAddr::new(ip, port);
                if !out.iter().any(|c| matches!(c, Candidate::Addr(x) if *x == a)) {
                    out.push(Candidate::Addr(a));
                }
            }
        }
        for h in &self.cfg.network.manual_hosts {
            if let Ok(a) = h.parse::<SocketAddr>() {
                out.push(Candidate::Addr(a));
            } else if let Ok(ip) = h.parse::<std::net::IpAddr>() {
                out.push(Candidate::Addr(SocketAddr::new(ip, port_default)));
            } else {
                let host = if h.contains(':') { h.clone() } else { format!("{h}:{port_default}") };
                out.push(Candidate::Host(host));
            }
        }
        out
    }

    pub(crate) fn on_connect_failed(&mut self, peer: Uuid, error: &str) {
        tracing::debug!(%peer, error, "connect failed");
        self.connecting = None;
        self.last_error = Some(error.to_string());
        if matches!(self.conn_state, ConnState::Connecting) {
            self.conn_state = if self.had_session { ConnState::Reconnecting } else { ConnState::Discovering };
        }
        if error.contains("incompatible protocol") {
            self.version_mismatch = true;
        }
        self.next_attempt = Instant::now() + self.backoff.next_delay();
        self.dirty = true;
    }

    pub(crate) fn on_accepted(&mut self, a: Accepted) {
        match a {
            Accepted::Pairing(conn) => self.on_pairing_conn(conn),
            Accepted::Session(sc) => {
                if self.cfg.general.paused {
                    sc.close(close::NORMAL, "paused");
                    return;
                }
                self.refresh_hello();
                let hello = self.hello.read().clone();
                let tx = self.msg_tx.clone();
                let sid = self.next_session_id();
                let peer = sc.peer().id;
                tokio::spawn(async move {
                    match session::establish(sc, hello, sid, tx.clone()).await {
                        Ok(link) => {
                            let _ = tx.send(Msg::SessionReady(link));
                        }
                        Err(e) => {
                            tracing::debug!("incoming session handshake failed: {e}");
                            if matches!(e, NetError::IncompatibleVersion { .. }) {
                                let _ = tx.send(Msg::ConnectFailed { peer, error: e.to_string() });
                            }
                        }
                    }
                });
            }
        }
    }

    pub(crate) fn on_session_ready(&mut self, link: PeerLink) {
        self.connecting = None;
        let own = self.own_id();
        let initiator_id = if link.conn.is_initiator() { own } else { link.peer.id };
        let other_id = if link.conn.is_initiator() { link.peer.id } else { own };
        let preferred = initiator_id > other_id;

        if let Some(existing) = &self.session {
            if existing.initiated_by_preferred || !preferred {
                tracing::debug!("duplicate session rejected");
                link.close(close::DUPLICATE, "duplicate session");
                return;
            }
            tracing::debug!("replacing session with preferred one");
            self.close_session("replaced by preferred session");
        }

        tracing::info!(peer = %link.peer.name, addr = %link.conn.remote_address(), "session up");
        if link.conn.is_initiator() {
            let a = link.conn.remote_address();
            let _ = self.peers.update_address(link.peer.id, a.ip().to_string(), a.port());
        }
        if self.peers.by_id(link.peer.id).map(|p| p.name) != Some(link.hello.name.clone()) && !link.hello.name.is_empty() {
            let _ = self.peers.rename(link.peer.id, link.hello.name.clone());
        }
        self.version_mismatch = false;
        self.last_error = None;
        self.had_session = true;
        self.grace_applied = false;
        self.backoff.reset();
        self.clock.reset();
        self.rtt_history.clear();
        let remote_screens = link.hello.screens.iter().map(super::screen_from_pb).collect();
        self.session = Some(ActiveSession {
            link: link.clone(),
            since_unix_ms: since_unix_ms(),
            remote_screens,
            last_pong: Instant::now(),
            initiated_by_preferred: preferred,
        });
        self.conn_state = ConnState::Connected;
        link.send_control(pb::control::Msg::Ping(pb::Ping { t0_us: crate::clock::now_us() }));
        self.start_roles();
        self.on_session_up_features();
        self.dirty = true;
    }

    pub(crate) fn on_session_closed(&mut self, session_id: u64, reason: &str) {
        let matches = self.session.as_ref().is_some_and(|s| s.link.session_id == session_id);
        if matches {
            tracing::info!(reason, "session closed");
            self.teardown_session(reason);
        }
    }

    /// Gracefully ends the current session.
    pub(crate) fn close_session(&mut self, reason: &str) {
        if let Some(s) = &self.session {
            s.link.send_control(pb::control::Msg::Goodbye(pb::Goodbye { reason: reason.to_string() }));
            s.link.close(close::NORMAL, reason);
        }
        self.teardown_session(reason);
    }

    fn teardown_session(&mut self, reason: &str) {
        let Some(s) = self.session.take() else { return };
        let was_remote = self.router.is_remote();
        self.stop_roles();
        self.on_session_down_features();
        if was_remote {
            self.emit(UiEvent::Osd { icon: "alert".into(), text: "Verbindung verloren – Eingabe zurück am PC".into() });
        }
        drop(s);
        self.last_error = Some(reason.to_string());
        self.conn_state = if self.cfg.general.paused { ConnState::Paused } else { ConnState::Reconnecting };
        self.next_attempt = Instant::now() + self.backoff.next_delay();
        self.dirty = true;
    }
}

/// Races all candidate addresses; the first successful pinned handshake wins.
async fn dial(
    transport: &std::sync::Arc<deskbridge_net::transport::Transport>,
    peer: &Peer,
    candidates: Vec<Candidate>,
) -> Result<deskbridge_net::transport::SessionConn, NetError> {
    let mut set = tokio::task::JoinSet::new();
    for c in candidates {
        let t = transport.clone();
        let p = peer.clone();
        set.spawn(async move {
            let addrs: Vec<SocketAddr> = match c {
                Candidate::Addr(a) => vec![a],
                Candidate::Host(h) => tokio::net::lookup_host(h).await.map(Iterator::collect).unwrap_or_default(),
            };
            let mut last = NetError::Protocol("no address".into());
            for a in addrs {
                match t.connect_session(a, &p).await {
                    Ok(sc) => return Ok(sc),
                    Err(e) => last = e,
                }
            }
            Err(last)
        });
    }
    let mut last = NetError::Protocol("no candidates".into());
    while let Some(r) = set.join_next().await {
        match r {
            Ok(Ok(sc)) => {
                set.abort_all();
                return Ok(sc);
            }
            Ok(Err(e)) => last = e,
            Err(_) => {}
        }
    }
    Err(last)
}
