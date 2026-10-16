//! Two endpoints on 127.0.0.1: pairing with a code, then a pinned session.

use deskbridge_net::identity::Identity;
use deskbridge_net::pairing::{self, PairingWindow};
use deskbridge_net::peers::{Peer, PeerStore};
use deskbridge_net::transport::{Accepted, Transport};
use deskbridge_net::NetError;
use deskbridge_proto::{PROTO_VERSION, pb};
use std::sync::Arc;

struct Node {
    transport: Arc<Transport>,
    id: Arc<Identity>,
    peers: PeerStore,
}

fn node() -> Node {
    let id = Arc::new(Identity::generate().unwrap());
    let peers = PeerStore::in_memory();
    let t = Transport::bind(id.clone(), peers.clone(), "127.0.0.1:0".parse().unwrap()).unwrap();
    Node { transport: Arc::new(t), id, peers }
}

fn hello(n: &Node, name: &str) -> pb::Hello {
    pb::Hello {
        device_id: n.id.device_id.to_string(),
        name: name.into(),
        os: "test".into(),
        app_version: "0".into(),
        proto_version: PROTO_VERSION,
        ..Default::default()
    }
}

/// Runs the server side of pairing in the background; returns the join handle.
fn spawn_pair_server(
    server: &Node,
    window: Arc<PairingWindow>,
) -> tokio::task::JoinHandle<Result<pairing::PairedDevice, NetError>> {
    let t = server.transport.clone();
    let id = server.id.clone();
    tokio::spawn(async move {
        let pending = t.accept().await.unwrap();
        let Accepted::Pairing(conn) = pending.finish().await? else {
            panic!("expected pairing connection");
        };
        pairing::serve(&conn, &id, "Laptop", &window).await
    })
}

async fn do_pair(server: &Node, client: &Node, code: &str) -> (Result<pairing::PairedDevice, NetError>, Result<pairing::PairedDevice, NetError>) {
    let window = Arc::new(PairingWindow::new("123456".into()));
    let srv = spawn_pair_server(server, window);
    let addr = server.transport.local_addr().unwrap();
    let conn = client.transport.connect_pairing(addr).await.unwrap();
    let c = pairing::connect(&conn, &client.id, "PC", code).await;
    let s = srv.await.unwrap();
    (c, s)
}

#[tokio::test(flavor = "multi_thread")]
async fn pairing_with_correct_code_then_session() {
    let (laptop, pc) = (node(), node());
    let (c, s) = do_pair(&laptop, &pc, "123456").await;
    let on_pc = c.expect("client side");
    let on_laptop = s.expect("server side");
    assert_eq!(on_pc.device_id, laptop.id.device_id);
    assert_eq!(on_laptop.device_id, pc.id.device_id);
    assert_eq!(on_pc.fingerprint, laptop.id.fingerprint);

    pc.peers.add(Peer::new(on_pc.device_id, on_pc.name, &on_pc.fingerprint)).unwrap();
    laptop.peers.add(Peer::new(on_laptop.device_id, on_laptop.name, &on_laptop.fingerprint)).unwrap();

    // Session: PC connects to the laptop.
    let lt = laptop.transport.clone();
    let lh = hello(&laptop, "Laptop");
    let server = tokio::spawn(async move {
        let pending = lt.accept().await.unwrap();
        let Accepted::Session(sc) = pending.finish().await.unwrap() else { panic!("session expected") };
        let (their_hello, mut ch) = sc.handshake(lh).await.unwrap();
        // echo one input event back as EdgeHit
        let ev: pb::InputEvent = ch.input.rx.recv().await.unwrap();
        ch.input.tx.send(&ev).await.unwrap();
        // datagram echo
        let d = sc.read_datagram().await.unwrap();
        sc.send_datagram(d).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        their_hello.name
    });

    let peer = pc.peers.by_id(laptop.id.device_id).unwrap();
    let sc = pc.transport.connect_session(laptop.transport.local_addr().unwrap(), &peer).await.unwrap();
    let (theirs, mut ch) = sc.handshake(hello(&pc, "PC")).await.unwrap();
    assert_eq!(theirs.name, "Laptop");

    let ev = pb::InputEvent {
        ts_us: 1,
        ev: Some(pb::input_event::Ev::Key(pb::Key { hid: 4, down: true })),
    };
    ch.input.tx.send(&ev).await.unwrap();
    let back: pb::InputEvent = ch.input.rx.recv().await.unwrap();
    assert_eq!(back, ev);

    sc.send_datagram(bytes::Bytes::from_static(b"audio")).unwrap();
    assert_eq!(&sc.read_datagram().await.unwrap()[..], b"audio");
    assert_eq!(server.await.unwrap(), "PC");
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_code_fails_on_both_sides() {
    let (laptop, pc) = (node(), node());
    let (c, s) = do_pair(&laptop, &pc, "654321").await;
    assert!(c.is_err(), "client must fail with wrong code");
    assert!(s.is_err(), "server must fail with wrong code");
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_device_is_rejected_for_sessions() {
    let (laptop, stranger) = (node(), node());
    // The stranger believes it is paired (it pinned the laptop), the laptop does not know it.
    let fake_peer = Peer::new(laptop.id.device_id, "Laptop".into(), &laptop.id.fingerprint);
    let lt = laptop.transport.clone();
    let server = tokio::spawn(async move {
        let pending = lt.accept().await.unwrap();
        pending.finish().await
    });
    let addr = laptop.transport.local_addr().unwrap();
    let sc = stranger.transport.connect_session(addr, &fake_peer).await;
    // The TLS handshake succeeds client-side, but the server closes right away.
    if let Ok(sc) = sc {
        let r = sc.handshake(hello(&stranger, "Stranger")).await;
        assert!(r.is_err());
    }
    assert!(matches!(server.await.unwrap(), Err(NetError::UnknownPeer(_))));
}

#[tokio::test(flavor = "multi_thread")]
async fn pinned_fingerprint_mismatch_is_detected() {
    let (laptop, pc, impostor) = (node(), node(), node());
    // PC pinned the laptop's fingerprint, but the endpoint answering is the impostor.
    let wrong = Peer::new(laptop.id.device_id, "Laptop".into(), &laptop.id.fingerprint);
    let it = impostor.transport.clone();
    tokio::spawn(async move {
        if let Some(p) = it.accept().await {
            let _ = p.finish().await;
        }
    });
    let r = pc.transport.connect_session(impostor.transport.local_addr().unwrap(), &wrong).await;
    assert!(matches!(r, Err(NetError::FingerprintMismatch)));
}
