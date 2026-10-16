//! Paired devices, stored in `peers.toml`.

use crate::{Fingerprint, NetError, Result, hex, unhex};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Peer {
    pub id: Uuid,
    pub name: String,
    /// SHA-256 fingerprint of the peer certificate, hex.
    pub fingerprint: String,
    #[serde(default)]
    pub last_ip: Option<String>,
    #[serde(default)]
    pub last_port: Option<u16>,
}

impl Peer {
    #[must_use]
    pub fn fp(&self) -> Option<Fingerprint> {
        unhex(&self.fingerprint)?.try_into().ok()
    }

    #[must_use]
    pub fn new(id: Uuid, name: String, fp: &Fingerprint) -> Self {
        Self {
            id,
            name,
            fingerprint: hex(fp),
            last_ip: None,
            last_port: None,
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    peer: Vec<Peer>,
}

/// Thread-safe peer list, persisted on every change.
#[derive(Clone)]
pub struct PeerStore {
    inner: Arc<RwLock<Vec<Peer>>>,
    path: Option<PathBuf>,
}

impl PeerStore {
    /// In-memory store (tests).
    #[must_use]
    pub fn in_memory() -> Self {
        Self {
            inner: Arc::default(),
            path: None,
        }
    }

    pub fn load(path: PathBuf) -> Result<Self> {
        let peers = if path.exists() {
            let text = std::fs::read_to_string(&path)?;
            toml::from_str::<File>(&text)
                .map_err(|e| NetError::Config(e.to_string()))?
                .peer
        } else {
            Vec::new()
        };
        Ok(Self {
            inner: Arc::new(RwLock::new(peers)),
            path: Some(path),
        })
    }

    fn persist(&self) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let file = File {
            peer: self.inner.read().clone(),
        };
        let text = toml::to_string_pretty(&file).map_err(|e| NetError::Config(e.to_string()))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }

    #[must_use]
    pub fn all(&self) -> Vec<Peer> {
        self.inner.read().clone()
    }

    #[must_use]
    pub fn by_fingerprint(&self, fp: &Fingerprint) -> Option<Peer> {
        let h = hex(fp);
        self.inner
            .read()
            .iter()
            .find(|p| p.fingerprint == h)
            .cloned()
    }

    #[must_use]
    pub fn by_id(&self, id: Uuid) -> Option<Peer> {
        self.inner.read().iter().find(|p| p.id == id).cloned()
    }

    /// Adds a peer, replacing an existing entry with the same id.
    pub fn add(&self, peer: Peer) -> Result<()> {
        {
            let mut g = self.inner.write();
            g.retain(|p| p.id != peer.id);
            g.push(peer);
        }
        self.persist()
    }

    pub fn remove(&self, id: Uuid) -> Result<bool> {
        let removed = {
            let mut g = self.inner.write();
            let before = g.len();
            g.retain(|p| p.id != id);
            g.len() != before
        };
        if removed {
            self.persist()?;
        }
        Ok(removed)
    }

    pub fn update_address(&self, id: Uuid, ip: String, port: u16) -> Result<()> {
        {
            let mut g = self.inner.write();
            if let Some(p) = g.iter_mut().find(|p| p.id == id) {
                if p.last_ip.as_deref() == Some(ip.as_str()) && p.last_port == Some(port) {
                    return Ok(());
                }
                p.last_ip = Some(ip);
                p.last_port = Some(port);
            }
        }
        self.persist()
    }

    pub fn rename(&self, id: Uuid, name: String) -> Result<()> {
        {
            let mut g = self.inner.write();
            if let Some(p) = g.iter_mut().find(|p| p.id == id) {
                p.name = name;
            }
        }
        self.persist()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persist_and_lookup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("peers.toml");
        let store = PeerStore::load(path.clone()).unwrap();
        let id = Uuid::now_v7();
        let fp = [7u8; 32];
        store.add(Peer::new(id, "Laptop".into(), &fp)).unwrap();
        store
            .update_address(id, "192.168.1.5".into(), 47820)
            .unwrap();
        let again = PeerStore::load(path).unwrap();
        let p = again.by_fingerprint(&fp).unwrap();
        assert_eq!(p.name, "Laptop");
        assert_eq!(p.last_ip.as_deref(), Some("192.168.1.5"));
        assert!(again.remove(id).unwrap());
        assert!(again.by_id(id).is_none());
    }
}
