//! Networking for Desk Bridge.
//!
//! * [`identity`]: device id + self-signed Ed25519 certificate, protected at rest
//! * [`peers`]: paired devices (`peers.toml`)
//! * [`tls`]: rustls configs with certificate *pinning* (fingerprint check after the handshake)
//! * [`pairing`]: SPAKE2 pairing with a 6-digit code, bound to the TLS certificates
//! * [`transport`]: QUIC endpoint, connections, streams and datagrams
//! * [`discovery`]: mDNS announce/browse

pub mod discovery;
pub mod error;
pub mod identity;
pub mod pairing;
pub mod peers;
pub mod tls;
pub mod transport;

pub use error::{NetError, Result};

/// SHA-256 certificate fingerprint.
pub type Fingerprint = [u8; 32];

/// Hex encoding helper (lowercase).
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Parses lowercase/uppercase hex.
#[must_use]
pub fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// Short, human friendly fingerprint (first 8 bytes, grouped) for the UI.
#[must_use]
pub fn short_fp(fp: &[u8]) -> String {
    let h = hex(&fp[..fp.len().min(8)]);
    h.as_bytes()
        .chunks(4)
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip() {
        let b = [0u8, 1, 0xab, 0xff];
        assert_eq!(unhex(&hex(&b)).unwrap(), b);
        assert!(unhex("abc").is_none());
        assert_eq!(
            short_fp(&[0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0]),
            "1234-5678-9abc-def0"
        );
    }
}
