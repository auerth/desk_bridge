//! QUIC endpoint, sessions, framed streams and datagrams.

use crate::identity::Identity;
use crate::peers::{Peer, PeerStore};
use crate::tls::{self, SERVER_NAME};
use crate::{Fingerprint, NetError, Result, hex};
use bytes::Bytes;
use deskbridge_proto::frame::{read_msg, write_msg};
use deskbridge_proto::{ALPN_PAIR, ALPN_SESSION, PROTO_VERSION, compatible, pb};
use prost::Message;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

/// First byte on the input stream, so the acceptor sees the stream at all (QUIC streams are lazy).
const INPUT_PREFACE: u8 = 0x49;

/// Close codes.
pub mod close {
    pub const NORMAL: u32 = 0;
    pub const UNKNOWN_PEER: u32 = 0x403;
    pub const VERSION: u32 = 0x426;
    pub const DUPLICATE: u32 = 0x409;
}

pub struct Transport {
    endpoint: quinn::Endpoint,
    identity: Arc<Identity>,
    peers: PeerStore,
}

/// An incoming connection whose handshake has not been awaited yet.
pub struct Pending {
    incoming: quinn::Incoming,
    peers: PeerStore,
}

pub enum Accepted {
    /// A known, pinned peer wants a session.
    Session(SessionConn),
    /// An unpaired device wants to pair (only meaningful while a pairing window is open).
    Pairing(quinn::Connection),
}

impl Transport {
    pub fn bind(identity: Arc<Identity>, peers: PeerStore, addr: SocketAddr) -> Result<Self> {
        let server = tls::server_config(&identity)?;
        let endpoint = quinn::Endpoint::server(server, addr)?;
        Ok(Self { endpoint, identity, peers })
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.endpoint.local_addr()?)
    }

    #[must_use]
    pub fn identity(&self) -> &Arc<Identity> {
        &self.identity
    }

    #[must_use]
    pub fn peers(&self) -> &PeerStore {
        &self.peers
    }

    /// Waits for the next incoming connection attempt. Returns `None` when the endpoint closed.
    pub async fn accept(&self) -> Option<Pending> {
        let incoming = self.endpoint.accept().await?;
        Some(Pending { incoming, peers: self.peers.clone() })
    }

    /// Connects to an already paired peer. Fails if the presented certificate does not match the
    /// pinned fingerprint.
    pub async fn connect_session(&self, addr: SocketAddr, peer: &Peer) -> Result<SessionConn> {
        let expected: Fingerprint = peer.fp().ok_or(NetError::FingerprintMismatch)?;
        let cfg = tls::client_config(&self.identity, ALPN_SESSION)?;
        let conn = tokio::time::timeout(
            Duration::from_secs(5),
            self.endpoint.connect_with(cfg, addr, SERVER_NAME)?,
        )
        .await
        .map_err(|_| NetError::Tls("connect timeout".into()))??;
        let fp = tls::peer_fingerprint(&conn)?;
        if fp != expected {
            conn.close(close::UNKNOWN_PEER.into(), b"fingerprint mismatch");
            return Err(NetError::FingerprintMismatch);
        }
        Ok(SessionConn { conn, peer: peer.clone(), initiator: true })
    }

    /// Opens a pairing connection to a device that is showing a code.
    pub async fn connect_pairing(&self, addr: SocketAddr) -> Result<quinn::Connection> {
        let cfg = tls::client_config(&self.identity, ALPN_PAIR)?;
        let conn = tokio::time::timeout(
            Duration::from_secs(5),
            self.endpoint.connect_with(cfg, addr, SERVER_NAME)?,
        )
        .await
        .map_err(|_| NetError::Tls("connect timeout".into()))??;
        Ok(conn)
    }

    pub fn close(&self) {
        self.endpoint.close(close::NORMAL.into(), b"shutdown");
    }

    /// Rebinds the socket after a network change (keeps connections that survive).
    pub fn rebind(&self) -> Result<()> {
        let sock = std::net::UdpSocket::bind(self.local_addr()?)?;
        self.endpoint.rebind(sock)?;
        Ok(())
    }
}

impl Pending {
    #[must_use]
    pub fn remote_address(&self) -> SocketAddr {
        self.incoming.remote_address()
    }

    /// Completes the handshake and classifies the connection.
    pub async fn finish(self) -> Result<Accepted> {
        let conn = self.incoming.await?;
        match tls::negotiated_alpn(&conn).as_deref() {
            Some(a) if a == ALPN_PAIR => Ok(Accepted::Pairing(conn)),
            Some(a) if a == ALPN_SESSION => {
                let fp = tls::peer_fingerprint(&conn)?;
                match self.peers.by_fingerprint(&fp) {
                    Some(peer) => Ok(Accepted::Session(SessionConn { conn, peer, initiator: false })),
                    None => {
                        conn.close(close::UNKNOWN_PEER.into(), b"unknown peer");
                        Err(NetError::UnknownPeer(hex(&fp)[..16].to_string()))
                    }
                }
            }
            _ => {
                conn.close(close::VERSION.into(), b"unsupported alpn");
                Err(NetError::Protocol("unsupported ALPN".into()))
            }
        }
    }
}

/// Write half of a length-delimited protobuf stream.
pub struct Tx {
    send: quinn::SendStream,
}

/// Read half of a length-delimited protobuf stream.
pub struct Rx {
    recv: quinn::RecvStream,
}

impl Tx {
    pub async fn send<M: Message>(&mut self, msg: &M) -> Result<()> {
        write_msg(&mut self.send, msg).await?;
        Ok(())
    }
}

impl Rx {
    pub async fn recv<M: Message + Default>(&mut self) -> Result<M> {
        Ok(read_msg(&mut self.recv).await?)
    }
}

/// A bidirectional framed channel.
pub struct Channel {
    pub tx: Tx,
    pub rx: Rx,
}

pub struct Channels {
    pub control: Channel,
    pub input: Channel,
}

/// An authenticated session connection to a paired peer.
#[derive(Clone)]
pub struct SessionConn {
    conn: quinn::Connection,
    peer: Peer,
    initiator: bool,
}

impl SessionConn {
    #[must_use]
    pub fn peer(&self) -> &Peer {
        &self.peer
    }

    #[must_use]
    pub fn remote_address(&self) -> SocketAddr {
        self.conn.remote_address()
    }

    #[must_use]
    pub fn is_initiator(&self) -> bool {
        self.initiator
    }

    #[must_use]
    pub fn rtt(&self) -> Duration {
        self.conn.rtt()
    }

    /// Opens (initiator) or accepts (acceptor) the control and input streams and exchanges
    /// `Hello`. Returns the peer's hello and the channels.
    pub async fn handshake(&self, own: pb::Hello) -> Result<(pb::Hello, Channels)> {
        let fut = async {
            if self.initiator {
                let (s, r) = self.conn.open_bi().await?;
                let mut control = Channel { tx: Tx { send: s }, rx: Rx { recv: r } };
                control
                    .tx
                    .send(&pb::Control { msg: Some(pb::control::Msg::Hello(own)) })
                    .await?;
                let (mut s2, r2) = self.conn.open_bi().await?;
                s2.write_all(&[INPUT_PREFACE]).await?;
                let input = Channel { tx: Tx { send: s2 }, rx: Rx { recv: r2 } };
                let theirs = Self::read_hello(&mut control.rx).await?;
                Ok::<_, NetError>((theirs, Channels { control, input }))
            } else {
                let (s, r) = self.conn.accept_bi().await?;
                let mut control = Channel { tx: Tx { send: s }, rx: Rx { recv: r } };
                let theirs = Self::read_hello(&mut control.rx).await?;
                let (s2, mut r2) = self.conn.accept_bi().await?;
                let mut pre = [0u8; 1];
                r2.read_exact(&mut pre).await.map_err(|e| NetError::Protocol(e.to_string()))?;
                if pre[0] != INPUT_PREFACE {
                    return Err(NetError::Protocol("bad input stream preface".into()));
                }
                let input = Channel { tx: Tx { send: s2 }, rx: Rx { recv: r2 } };
                control
                    .tx
                    .send(&pb::Control { msg: Some(pb::control::Msg::Hello(own)) })
                    .await?;
                Ok((theirs, Channels { control, input }))
            }
        };
        let (theirs, ch) = tokio::time::timeout(Duration::from_secs(5), fut)
            .await
            .map_err(|_| NetError::Protocol("hello timeout".into()))??;
        if !compatible(theirs.proto_version, PROTO_VERSION) {
            self.conn.close(close::VERSION.into(), b"incompatible version");
            return Err(NetError::IncompatibleVersion { peer: theirs.proto_version, own: PROTO_VERSION });
        }
        if theirs.device_id != self.peer.id.to_string() {
            self.conn.close(close::UNKNOWN_PEER.into(), b"device id mismatch");
            return Err(NetError::Protocol("hello device id does not match pinned peer".into()));
        }
        Ok((theirs, ch))
    }

    async fn read_hello(rx: &mut Rx) -> Result<pb::Hello> {
        let c: pb::Control = rx.recv().await?;
        match c.msg {
            Some(pb::control::Msg::Hello(h)) => Ok(h),
            _ => Err(NetError::Protocol("expected Hello".into())),
        }
    }

    pub fn send_datagram(&self, data: Bytes) -> Result<()> {
        self.conn
            .send_datagram(data)
            .map_err(|e| NetError::Protocol(format!("datagram: {e}")))
    }

    pub async fn read_datagram(&self) -> Result<Bytes> {
        Ok(self.conn.read_datagram().await?)
    }

    #[must_use]
    pub fn max_datagram_size(&self) -> Option<usize> {
        self.conn.max_datagram_size()
    }

    pub async fn open_uni(&self) -> Result<quinn::SendStream> {
        Ok(self.conn.open_uni().await?)
    }

    pub async fn accept_uni(&self) -> Result<quinn::RecvStream> {
        Ok(self.conn.accept_uni().await?)
    }

    pub async fn open_bi(&self) -> Result<(quinn::SendStream, quinn::RecvStream)> {
        Ok(self.conn.open_bi().await?)
    }

    pub async fn accept_bi(&self) -> Result<(quinn::SendStream, quinn::RecvStream)> {
        Ok(self.conn.accept_bi().await?)
    }

    pub fn close(&self, code: u32, reason: &str) {
        self.conn.close(code.into(), reason.as_bytes());
    }

    /// Resolves when the connection ends.
    pub async fn closed(&self) -> quinn::ConnectionError {
        self.conn.closed().await
    }

    #[must_use]
    pub fn stats(&self) -> quinn::ConnectionStats {
        self.conn.stats()
    }
}
