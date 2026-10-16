//! Device identity: UUIDv7 device id and a self-signed Ed25519 certificate.
//!
//! The private key lives in `identity.bin`, protected by the platform secret store (DPAPI on
//! Windows).

use crate::{Fingerprint, NetError, Result, hex, unhex};
use deskbridge_platform::secret::SecretProtector;
use rcgen::{CertificateParams, KeyPair, PKCS_ED25519};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
use uuid::Uuid;

#[derive(Clone)]
pub struct Identity {
    pub device_id: Uuid,
    pub cert_der: Vec<u8>,
    pub key_pkcs8_der: Vec<u8>,
    pub fingerprint: Fingerprint,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("device_id", &self.device_id)
            .field("fingerprint", &hex(&self.fingerprint))
            .finish_non_exhaustive()
    }
}

#[derive(Serialize, Deserialize)]
struct Stored {
    device_id: String,
    cert_hex: String,
    key_hex: String,
}

/// SHA-256 over the DER encoding of a certificate.
#[must_use]
pub fn fingerprint_of(cert_der: &[u8]) -> Fingerprint {
    Sha256::digest(cert_der).into()
}

impl Identity {
    /// Generates a fresh identity.
    pub fn generate() -> Result<Self> {
        let key =
            KeyPair::generate_for(&PKCS_ED25519).map_err(|e| NetError::Identity(e.to_string()))?;
        let params = CertificateParams::new(vec!["deskbridge".to_string()])
            .map_err(|e| NetError::Identity(e.to_string()))?;
        let cert = params
            .self_signed(&key)
            .map_err(|e| NetError::Identity(e.to_string()))?;
        let cert_der = cert.der().to_vec();
        Ok(Self {
            device_id: Uuid::now_v7(),
            fingerprint: fingerprint_of(&cert_der),
            cert_der,
            key_pkcs8_der: key.serialize_der(),
        })
    }

    /// Loads `identity.bin` or creates it on first start.
    pub fn load_or_create(path: &Path, protector: &dyn SecretProtector) -> Result<Self> {
        if path.exists() {
            let blob = std::fs::read(path)?;
            let plain = protector.unprotect(&blob)?;
            let stored: Stored = serde_json::from_slice(&plain)
                .map_err(|e| NetError::Identity(format!("corrupt identity: {e}")))?;
            let cert_der =
                unhex(&stored.cert_hex).ok_or_else(|| NetError::Identity("bad cert".into()))?;
            let key = unhex(&stored.key_hex).ok_or_else(|| NetError::Identity("bad key".into()))?;
            let device_id = Uuid::parse_str(&stored.device_id)
                .map_err(|e| NetError::Identity(format!("bad device id: {e}")))?;
            return Ok(Self {
                device_id,
                fingerprint: fingerprint_of(&cert_der),
                cert_der,
                key_pkcs8_der: key,
            });
        }
        let id = Self::generate()?;
        id.save(path, protector)?;
        Ok(id)
    }

    pub fn save(&self, path: &Path, protector: &dyn SecretProtector) -> Result<()> {
        let stored = Stored {
            device_id: self.device_id.to_string(),
            cert_hex: hex(&self.cert_der),
            key_hex: hex(&self.key_pkcs8_der),
        };
        let plain = serde_json::to_vec(&stored).expect("serialise");
        let blob = protector.protect(&plain)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, blob)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deskbridge_platform::secret::PlainProtector;

    #[test]
    fn create_then_reload_is_stable() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("identity.bin");
        let a = Identity::load_or_create(&p, &PlainProtector).unwrap();
        let b = Identity::load_or_create(&p, &PlainProtector).unwrap();
        assert_eq!(a.device_id, b.device_id);
        assert_eq!(a.fingerprint, b.fingerprint);
        assert_ne!(a.fingerprint, Identity::generate().unwrap().fingerprint);
    }
}
