//! UDP socket under the QUIC endpoint: quinn's socket plus a plain `send_to` fallback.
//!
//! quinn-udp sends every datagram with control messages (the source address as `IP_PKTINFO`,
//! the ECN bits) and only logs a failed send. On the laptop, after a resume from sleep, Windows
//! rejected those sends for many minutes with `WSAEINVAL` while plain datagrams (mDNS) went
//! through: every handshake timed out on both devices although the network was fine. When a
//! send with control messages fails, the datagram goes out again without them (the OS picks
//! source address and route) and later sends skip the control messages for a while.

use parking_lot::Mutex;
use quinn::udp::{RecvMeta, Transmit, UdpSocketState};
use quinn::{AsyncUdpSocket, UdpPoller};
use std::io::{self, IoSliceMut};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, ready};
use std::time::{Duration, Instant};
use tokio::io::Interest;

/// How long sends stay plain after a failed send with control messages.
const PLAIN_FOR: Duration = Duration::from_secs(30);
/// At most one warning per interval for sends that fail even without control messages.
const LOG_INTERVAL: Duration = Duration::from_secs(60);

/// Wraps a bound socket for [`quinn::Endpoint::new_with_abstract_socket`].
pub(crate) fn wrap(sock: std::net::UdpSocket) -> io::Result<Arc<dyn AsyncUdpSocket>> {
    Ok(Arc::new(FallbackSocket {
        inner: UdpSocketState::new((&sock).into())?,
        io: tokio::net::UdpSocket::from_std(sock)?,
        state: Mutex::new(SendState::default()),
    }))
}

#[derive(Debug, Default)]
struct SendState {
    /// Sends skip the control messages until then.
    plain_until: Option<Instant>,
    /// A fallback happened since the last successful send with control messages.
    degraded: bool,
    last_plain_error_log: Option<Instant>,
}

#[derive(Debug)]
struct FallbackSocket {
    io: tokio::net::UdpSocket,
    inner: UdpSocketState,
    state: Mutex<SendState>,
}

impl FallbackSocket {
    fn plain_now(&self, now: Instant) -> bool {
        self.state.lock().plain_until.is_some_and(|t| now < t)
    }

    fn start_plain(&self, now: Instant, err: &io::Error, t: &Transmit) {
        let mut s = self.state.lock();
        if !s.degraded {
            tracing::warn!(
                error = %err,
                destination = %t.destination,
                src_ip = ?t.src_ip,
                "UDP send with source address/ECN failed, sending plain datagrams"
            );
        }
        s.degraded = true;
        s.plain_until = Some(now + PLAIN_FOR);
    }

    fn send_plain(&self, t: &Transmit) -> io::Result<()> {
        let seg = t.segment_size.unwrap_or(t.contents.len()).max(1);
        for (i, chunk) in t.contents.chunks(seg).enumerate() {
            match self.io.try_send_to(chunk, t.destination) {
                Ok(_) => {}
                // Nothing sent yet: quinn waits for writability and retries the transmit.
                Err(e) if e.kind() == io::ErrorKind::WouldBlock && i == 0 => return Err(e),
                // Like quinn-udp: a lost datagram is QUIC's business, not an I/O error.
                Err(e) => {
                    let now = Instant::now();
                    let mut s = self.state.lock();
                    if s
                        .last_plain_error_log
                        .is_none_or(|l| now.duration_since(l) >= LOG_INTERVAL)
                    {
                        s.last_plain_error_log = Some(now);
                        tracing::warn!(error = %e, destination = %t.destination, "UDP send failed");
                    }
                    return Ok(());
                }
            }
        }
        Ok(())
    }
}

impl AsyncUdpSocket for FallbackSocket {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
        Box::pin(Writable(self))
    }

    fn try_send(&self, t: &Transmit) -> io::Result<()> {
        let now = Instant::now();
        if !self.plain_now(now) {
            let res = self.io.try_io(Interest::WRITABLE, || {
                self.inner.try_send((&self.io).into(), t)
            });
            match res {
                Ok(()) => {
                    let mut s = self.state.lock();
                    if s.degraded {
                        s.degraded = false;
                        tracing::info!("UDP sends with source address/ECN work again");
                    }
                    return Ok(());
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Err(e),
                Err(e) => self.start_plain(now, &e, t),
            }
        }
        self.send_plain(t)
    }

    fn poll_recv(
        &self,
        cx: &mut Context,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        loop {
            ready!(self.io.poll_recv_ready(cx))?;
            if let Ok(res) = self.io.try_io(Interest::READABLE, || {
                self.inner.recv((&self.io).into(), bufs, meta)
            }) {
                return Poll::Ready(Ok(res));
            }
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.io.local_addr()
    }

    fn may_fragment(&self) -> bool {
        self.inner.may_fragment()
    }

    fn max_transmit_segments(&self) -> usize {
        self.inner.max_gso_segments()
    }

    fn max_receive_segments(&self) -> usize {
        self.inner.gro_segments()
    }
}

#[derive(Debug)]
struct Writable(Arc<FallbackSocket>);

impl UdpPoller for Writable {
    fn poll_writable(self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
        self.0.io.poll_send_ready(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quinn::udp::EcnCodepoint;
    use std::net::{IpAddr, Ipv6Addr};

    /// Sends like quinn's endpoint driver: wait for writability, retry on `WouldBlock`.
    async fn send(sock: &Arc<dyn AsyncUdpSocket>, t: &Transmit<'_>) {
        let mut poller = sock.clone().create_io_poller();
        loop {
            std::future::poll_fn(|cx| poller.as_mut().poll_writable(cx))
                .await
                .unwrap();
            match sock.try_send(t) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                r => return r.unwrap(),
            }
        }
    }

    /// An IPv6 source address on the IPv4 socket makes the send with control messages fail
    /// (Windows rejected the laptop's own IPv4 address the same way after the resume).
    #[tokio::test]
    async fn datagram_gets_through_when_the_source_address_is_rejected() {
        let rx = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let tx = wrap(std::net::UdpSocket::bind("127.0.0.1:0").unwrap()).unwrap();
        let t = Transmit {
            destination: rx.local_addr().unwrap(),
            ecn: Some(EcnCodepoint::Ect0),
            contents: b"hello",
            segment_size: None,
            src_ip: Some(IpAddr::V6(Ipv6Addr::LOCALHOST)),
        };

        // quinn-udp alone fails (and its `send` would only log it)
        let raw = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let state = UdpSocketState::new((&raw).into()).unwrap();
        assert!(state.try_send((&raw).into(), &t).is_err());

        send(&tx, &t).await;
        let mut buf = [0u8; 16];
        let n = tokio::time::timeout(Duration::from_secs(2), rx.recv(&mut buf))
            .await
            .expect("datagram arrives")
            .unwrap();
        assert_eq!(&buf[..n], b"hello");

        // the next send skips the control messages right away
        send(&tx, &Transmit { contents: b"again", ..t }).await;
        let n = tokio::time::timeout(Duration::from_secs(2), rx.recv(&mut buf))
            .await
            .expect("second datagram arrives")
            .unwrap();
        assert_eq!(&buf[..n], b"again");
    }

    #[tokio::test]
    async fn segments_are_sent_one_by_one_on_the_plain_path() {
        let rx = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let tx = wrap(std::net::UdpSocket::bind("127.0.0.1:0").unwrap()).unwrap();
        let t = Transmit {
            destination: rx.local_addr().unwrap(),
            ecn: None,
            contents: b"aaabbbcc",
            segment_size: Some(3),
            src_ip: Some(IpAddr::V6(Ipv6Addr::LOCALHOST)),
        };
        send(&tx, &t).await;
        let mut buf = [0u8; 16];
        for want in [&b"aaa"[..], b"bbb", b"cc"] {
            let n = tokio::time::timeout(Duration::from_secs(2), rx.recv(&mut buf))
                .await
                .expect("segment arrives")
                .unwrap();
            assert_eq!(&buf[..n], want);
        }
    }
}
