//! Pairing with a short code (SPAKE2), bound to the TLS certificates.
//!
//! Roles: the device that *shows* the code is the pairing server (SPAKE2 side A), the device
//! where the code is typed is the pairing client (side B).
//!
//! ```text
//!   B -> A : PairSpake          A -> B : PairSpake      (both derive key K)
//!   B -> A : PairConfirm{mac}   A verifies; A -> B : PairConfirm{mac}   B verifies
//!   B -> A : PairResult{ok}
//! ```
//! The MACs cover both TLS certificate fingerprints as seen by the sender, so a
//! man-in-the-middle (who necessarily presents different certificates on each leg) is detected.

use crate::identity::Identity;
use crate::tls::peer_fingerprint;
use crate::{Fingerprint, NetError, Result};
use deskbridge_proto::frame::{read_msg, write_msg};
use deskbridge_proto::pb;
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use parking_lot::Mutex;
use rand::Rng;
use sha2::Sha256;
use spake2::{Ed25519Group, Identity as SpakeId, Password, Spake2};
use std::time::{Duration, Instant};
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

pub const CODE_LEN: usize = 6;
pub const CODE_TTL: Duration = Duration::from_secs(120);
pub const MAX_FAILURES: u32 = 3;
pub const LOCKOUT: Duration = Duration::from_secs(60);

/// Result of a successful pairing: the other device.
#[derive(Debug, Clone)]
pub struct PairedDevice {
    pub device_id: Uuid,
    pub name: String,
    pub fingerprint: Fingerprint,
}

/// Generates a random 6-digit code.
#[must_use]
pub fn generate_code() -> String {
    let n: u32 = rand::thread_rng().gen_range(0..1_000_000);
    format!("{n:06}")
}

/// Server-side state for one pairing window (code, expiry, rate limit).
pub struct PairingWindow {
    code: String,
    expires: Instant,
    state: Mutex<WindowState>,
}

struct WindowState {
    failures: u32,
    locked_until: Option<Instant>,
    done: bool,
}

impl PairingWindow {
    #[must_use]
    pub fn new(code: String) -> Self {
        Self::with_ttl(code, CODE_TTL)
    }

    #[must_use]
    pub fn with_ttl(code: String, ttl: Duration) -> Self {
        Self {
            code,
            expires: Instant::now() + ttl,
            state: Mutex::new(WindowState {
                failures: 0,
                locked_until: None,
                done: false,
            }),
        }
    }

    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }

    #[must_use]
    pub fn seconds_left(&self) -> u64 {
        self.expires
            .saturating_duration_since(Instant::now())
            .as_secs()
    }

    fn check_open(&self) -> Result<()> {
        if Instant::now() >= self.expires {
            return Err(NetError::Pairing("Code abgelaufen".into()));
        }
        let st = self.state.lock();
        if st.done {
            return Err(NetError::Pairing("Code bereits verwendet".into()));
        }
        if let Some(until) = st.locked_until {
            let now = Instant::now();
            if now < until {
                return Err(NetError::Locked((until - now).as_secs().max(1)));
            }
        }
        Ok(())
    }

    fn record_failure(&self) {
        let mut st = self.state.lock();
        st.failures += 1;
        if st.failures >= MAX_FAILURES {
            st.failures = 0;
            st.locked_until = Some(Instant::now() + LOCKOUT);
        }
    }

    fn record_success(&self) {
        self.state.lock().done = true;
    }
}

fn derive_key(spake_key: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(b"deskbridge-pair-v1"), spake_key);
    let mut out = [0u8; 32];
    hk.expand(b"confirm", &mut out)
        .expect("32 bytes is a valid length");
    out
}

fn mac(
    key: &[u8; 32],
    role: &[u8],
    own: &Fingerprint,
    other: &Fingerprint,
    device_id: &str,
) -> Vec<u8> {
    let mut m = HmacSha256::new_from_slice(key).expect("hmac accepts any key length");
    m.update(role);
    m.update(own);
    m.update(other);
    m.update(device_id.as_bytes());
    m.finalize().into_bytes().to_vec()
}

fn verify_mac(
    key: &[u8; 32],
    role: &[u8],
    sender_fp: &Fingerprint,
    receiver_fp: &Fingerprint,
    device_id: &str,
    tag: &[u8],
) -> bool {
    let mut m = HmacSha256::new_from_slice(key).expect("hmac accepts any key length");
    m.update(role);
    m.update(sender_fp);
    m.update(receiver_fp);
    m.update(device_id.as_bytes());
    m.verify_slice(tag).is_ok()
}

const ID_A: &[u8] = b"deskbridge-shows-code";
const ID_B: &[u8] = b"deskbridge-enters-code";

/// Runs the pairing *server* (the device showing the code) on an accepted pairing connection.
pub async fn serve(
    conn: &quinn::Connection,
    own: &Identity,
    own_name: &str,
    window: &PairingWindow,
) -> Result<PairedDevice> {
    window.check_open()?;
    let their_fp = peer_fingerprint(conn)?;
    let (mut send, mut recv) = conn.accept_bi().await?;

    let (spake, msg_a) = Spake2::<Ed25519Group>::start_a(
        &Password::new(window.code().as_bytes()),
        &SpakeId::new(ID_A),
        &SpakeId::new(ID_B),
    );
    let theirs: pb::PairSpake = read_msg(&mut recv).await?;
    write_msg(&mut send, &pb::PairSpake { msg: msg_a }).await?;
    let key = spake
        .finish(&theirs.msg)
        .map_err(|e| NetError::Pairing(format!("spake: {e:?}")))?;
    let key = derive_key(&key);

    let confirm: pb::PairConfirm = read_msg(&mut recv).await?;
    let ok = confirm.cert_fingerprint.as_slice() == their_fp
        && verify_mac(
            &key,
            b"B",
            &their_fp,
            &own.fingerprint,
            &confirm.device_id,
            &confirm.mac,
        );
    if !ok {
        window.record_failure();
        let _ = write_msg(
            &mut send,
            &pb::PairResult {
                ok: false,
                reason: "Falscher Code".into(),
            },
        )
        .await;
        let _ = send.finish();
        return Err(NetError::Pairing("Falscher Code".into()));
    }
    let own_id = own.device_id.to_string();
    write_msg(
        &mut send,
        &pb::PairConfirm {
            device_id: own_id.clone(),
            name: own_name.to_string(),
            cert_fingerprint: own.fingerprint.to_vec(),
            mac: mac(&key, b"A", &own.fingerprint, &their_fp, &own_id),
        },
    )
    .await?;
    let result: pb::PairResult = read_msg(&mut recv).await?;
    if !result.ok {
        return Err(NetError::Pairing(result.reason));
    }
    window.record_success();
    let _ = send.finish();
    let device_id = Uuid::parse_str(&confirm.device_id)
        .map_err(|_| NetError::Pairing("ungültige Geräte-ID".into()))?;
    Ok(PairedDevice {
        device_id,
        name: confirm.name,
        fingerprint: their_fp,
    })
}

/// Runs the pairing *client* (the device where the code is typed).
pub async fn connect(
    conn: &quinn::Connection,
    own: &Identity,
    own_name: &str,
    code: &str,
) -> Result<PairedDevice> {
    let their_fp = peer_fingerprint(conn)?;
    let (mut send, mut recv) = conn.open_bi().await?;

    let (spake, msg_b) = Spake2::<Ed25519Group>::start_b(
        &Password::new(code.as_bytes()),
        &SpakeId::new(ID_A),
        &SpakeId::new(ID_B),
    );
    write_msg(&mut send, &pb::PairSpake { msg: msg_b }).await?;
    let theirs: pb::PairSpake = read_msg(&mut recv).await?;
    let key = spake
        .finish(&theirs.msg)
        .map_err(|e| NetError::Pairing(format!("spake: {e:?}")))?;
    let key = derive_key(&key);

    let own_id = own.device_id.to_string();
    write_msg(
        &mut send,
        &pb::PairConfirm {
            device_id: own_id.clone(),
            name: own_name.to_string(),
            cert_fingerprint: own.fingerprint.to_vec(),
            mac: mac(&key, b"B", &own.fingerprint, &their_fp, &own_id),
        },
    )
    .await?;

    // Either the server's confirmation or a rejection arrives.
    let confirm = match read_confirm_or_result(&mut recv).await? {
        Reply::Confirm(c) => c,
        Reply::Result(r) => {
            return Err(NetError::Pairing(if r.reason.is_empty() {
                "abgelehnt".into()
            } else {
                r.reason
            }));
        }
    };
    let ok = confirm.cert_fingerprint.as_slice() == their_fp
        && verify_mac(
            &key,
            b"A",
            &their_fp,
            &own.fingerprint,
            &confirm.device_id,
            &confirm.mac,
        );
    if !ok {
        let _ = write_msg(
            &mut send,
            &pb::PairResult {
                ok: false,
                reason: "MAC ungültig".into(),
            },
        )
        .await;
        return Err(NetError::Pairing(
            "Bestätigung des Partners ungültig".into(),
        ));
    }
    write_msg(
        &mut send,
        &pb::PairResult {
            ok: true,
            reason: String::new(),
        },
    )
    .await?;
    let _ = send.finish();
    // Wait until the server has received everything; closing the connection right away would
    // discard the final message.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), send.stopped()).await;
    let device_id = Uuid::parse_str(&confirm.device_id)
        .map_err(|_| NetError::Pairing("ungültige Geräte-ID".into()))?;
    Ok(PairedDevice {
        device_id,
        name: confirm.name,
        fingerprint: their_fp,
    })
}

enum Reply {
    Confirm(pb::PairConfirm),
    Result(pb::PairResult),
}

/// The server answers with either `PairConfirm` or `PairResult{ok:false}`; both are framed
/// protobuf messages. `PairConfirm` always carries a non-empty `mac`, which tells them apart.
async fn read_confirm_or_result(recv: &mut quinn::RecvStream) -> Result<Reply> {
    use prost::Message;
    let mut len = [0u8; 4];
    recv.read_exact(&mut len)
        .await
        .map_err(|e| NetError::Pairing(format!("verbindung beendet: {e}")))?;
    let n = u32::from_le_bytes(len) as usize;
    if n > 64 * 1024 {
        return Err(NetError::Protocol("pair reply too large".into()));
    }
    let mut payload = vec![0u8; n];
    recv.read_exact(&mut payload)
        .await
        .map_err(|e| NetError::Pairing(e.to_string()))?;
    // A `PairResult` (varint field 1) fails to decode as `PairConfirm` (string field 1).
    if let Ok(confirm) = pb::PairConfirm::decode(payload.as_slice())
        && !confirm.mac.is_empty()
    {
        return Ok(Reply::Confirm(confirm));
    }
    let result = pb::PairResult::decode(payload.as_slice())
        .map_err(|e| NetError::Protocol(e.to_string()))?;
    Ok(Reply::Result(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_is_six_digits() {
        for _ in 0..100 {
            let c = generate_code();
            assert_eq!(c.len(), CODE_LEN);
            assert!(c.bytes().all(|b| b.is_ascii_digit()));
        }
    }

    #[test]
    fn rate_limit_locks_after_three_failures() {
        let w = PairingWindow::new("123456".into());
        for _ in 0..MAX_FAILURES {
            assert!(w.check_open().is_ok());
            w.record_failure();
        }
        assert!(matches!(w.check_open(), Err(NetError::Locked(_))));
    }

    #[test]
    fn expired_window_is_closed() {
        let w = PairingWindow::with_ttl("123456".into(), Duration::from_millis(0));
        assert!(matches!(w.check_open(), Err(NetError::Pairing(_))));
    }
}
