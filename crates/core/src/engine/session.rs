//! Per-connection plumbing: reader/writer tasks around the QUIC channels.

use super::Msg;
use bytes::Bytes;
use deskbridge_net::NetError;
use deskbridge_net::peers::Peer;
use deskbridge_net::transport::{Channels, SessionConn};
use deskbridge_proto::pb;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// First byte of every uni stream: what it carries.
pub mod uni {
    pub const CLIPBOARD_DATA: u8 = 1;
    pub const FILE: u8 = 2;
    pub const VIDEO: u8 = 3;
}

pub type UniSink = UnboundedSender<quinn::RecvStream>;

/// Handle to an established, authenticated session. Cheap to clone.
#[derive(Clone)]
pub struct PeerLink {
    pub session_id: u64,
    pub peer: Peer,
    /// The peer's `Hello`.
    pub hello: Arc<pb::Hello>,
    pub conn: SessionConn,
    control_tx: UnboundedSender<pb::Control>,
    input_tx: UnboundedSender<pb::InputEvent>,
    datagram_sink: Arc<Mutex<Option<UnboundedSender<Bytes>>>>,
    uni_sinks: Arc<Mutex<HashMap<u8, UniSink>>>,
}

impl PeerLink {
    pub fn send_control(&self, msg: pb::control::Msg) {
        let _ = self.control_tx.send(pb::Control { msg: Some(msg) });
    }

    pub fn send_input(&self, ev: pb::input_event::Ev) {
        let _ = self.input_tx.send(pb::InputEvent {
            ts_us: crate::clock::now_us(),
            ev: Some(ev),
        });
    }

    pub fn send_input_event(&self, ev: pb::InputEvent) {
        let _ = self.input_tx.send(ev);
    }

    pub fn send_datagram(&self, data: Bytes) -> Result<(), NetError> {
        self.conn.send_datagram(data)
    }

    /// Route incoming datagrams (audio) to a consumer.
    pub fn set_datagram_sink(&self, sink: Option<UnboundedSender<Bytes>>) {
        *self.datagram_sink.lock() = sink;
    }

    /// Route incoming uni streams of a kind (first byte) to a consumer.
    pub fn set_uni_sink(&self, kind: u8, sink: Option<UniSink>) {
        let mut g = self.uni_sinks.lock();
        match sink {
            Some(s) => {
                g.insert(kind, s);
            }
            None => {
                g.remove(&kind);
            }
        }
    }

    /// Opens a uni stream and writes the kind byte.
    pub async fn open_uni(&self, kind: u8) -> Result<quinn::SendStream, NetError> {
        let mut s = self.conn.open_uni().await?;
        s.write_all(&[kind]).await?;
        Ok(s)
    }

    pub fn close(&self, code: u32, reason: &str) {
        self.conn.close(code, reason);
    }
}

/// Performs the `Hello` exchange on an authenticated connection and spawns the I/O tasks.
pub(crate) async fn establish(
    sc: SessionConn,
    own_hello: pb::Hello,
    session_id: u64,
    msg_tx: UnboundedSender<Msg>,
) -> Result<PeerLink, NetError> {
    let (theirs, channels) = sc.handshake(own_hello).await?;
    Ok(spawn_io(sc, theirs, channels, session_id, msg_tx))
}

fn spawn_io(
    sc: SessionConn,
    theirs: pb::Hello,
    channels: Channels,
    session_id: u64,
    msg_tx: UnboundedSender<Msg>,
) -> PeerLink {
    let Channels { control, input } = channels;
    let (control_tx, control_rx) = unbounded_channel::<pb::Control>();
    let (input_tx, input_rx) = unbounded_channel::<pb::InputEvent>();
    let datagram_sink: Arc<Mutex<Option<UnboundedSender<Bytes>>>> = Arc::default();
    let uni_sinks: Arc<Mutex<HashMap<u8, UniSink>>> = Arc::default();

    // control writer
    tokio::spawn(writer(control.tx, control_rx));
    // input writer
    tokio::spawn(writer(input.tx, input_rx));

    // control reader
    {
        let tx = msg_tx.clone();
        let mut rx = control.rx;
        tokio::spawn(async move {
            loop {
                match rx.recv::<pb::Control>().await {
                    Ok(c) => {
                        if let Some(m) = c.msg
                            && tx.send(Msg::PeerControl { session_id, msg: m }).is_err()
                        {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Msg::SessionClosed {
                            session_id,
                            reason: format!("control: {e}"),
                        });
                        break;
                    }
                }
            }
        });
    }
    // input reader
    {
        let tx = msg_tx.clone();
        let mut rx = input.rx;
        tokio::spawn(async move {
            loop {
                match rx.recv::<pb::InputEvent>().await {
                    Ok(ev) => {
                        if tx.send(Msg::PeerInput { session_id, ev }).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Msg::SessionClosed {
                            session_id,
                            reason: format!("input: {e}"),
                        });
                        break;
                    }
                }
            }
        });
    }
    // datagram reader
    {
        let sc = sc.clone();
        let sink = datagram_sink.clone();
        tokio::spawn(async move {
            while let Ok(d) = sc.read_datagram().await {
                let s = sink.lock().clone();
                if let Some(s) = s {
                    let _ = s.send(d);
                }
            }
        });
    }
    // uni stream acceptor
    {
        let sc = sc.clone();
        let sinks = uni_sinks.clone();
        tokio::spawn(async move {
            while let Ok(mut r) = sc.accept_uni().await {
                let sinks = sinks.clone();
                tokio::spawn(async move {
                    let mut kind = [0u8; 1];
                    if r.read_exact(&mut kind).await.is_err() {
                        return;
                    }
                    let sink = sinks.lock().get(&kind[0]).cloned();
                    match sink {
                        Some(s) => {
                            let _ = s.send(r);
                        }
                        None => {
                            tracing::debug!(kind = kind[0], "uni stream without consumer dropped")
                        }
                    }
                });
            }
        });
    }
    // closed watcher
    {
        let sc = sc.clone();
        let tx = msg_tx;
        tokio::spawn(async move {
            let e = sc.closed().await;
            let _ = tx.send(Msg::SessionClosed {
                session_id,
                reason: format!("connection closed: {e}"),
            });
        });
    }

    PeerLink {
        session_id,
        peer: sc.peer().clone(),
        hello: Arc::new(theirs),
        conn: sc,
        control_tx,
        input_tx,
        datagram_sink,
        uni_sinks,
    }
}

async fn writer<M: prost::Message + 'static>(
    mut tx: deskbridge_net::transport::Tx,
    mut rx: UnboundedReceiver<M>,
) {
    while let Some(m) = rx.recv().await {
        if tx.send(&m).await.is_err() {
            break;
        }
    }
}
