//! mDNS / DNS-SD announce and browse (`_deskbridge._udp.local.`).

use crate::{NetError, Result};
use deskbridge_proto::{MDNS_SERVICE, PROTO_VERSION};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use tokio::sync::mpsc::UnboundedSender;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovered {
    pub device_id: Uuid,
    pub name: String,
    pub addrs: Vec<IpAddr>,
    pub port: u16,
    pub proto: u32,
    pub version: String,
}

impl Discovered {
    #[must_use]
    pub fn socket_addrs(&self) -> Vec<SocketAddr> {
        self.addrs
            .iter()
            .map(|ip| SocketAddr::new(*ip, self.port))
            .collect()
    }
}

#[derive(Debug, Clone)]
pub enum DiscoveryEvent {
    Found(Discovered),
    Lost { device_id: Uuid },
}

pub struct Discovery {
    daemon: ServiceDaemon,
    own_id: Uuid,
    fullname: Option<String>,
}

fn sanitize_label(s: &str) -> String {
    let t: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if t.is_empty() { "deskbridge".into() } else { t }
}

impl Discovery {
    pub fn new(own_id: Uuid) -> Result<Self> {
        let daemon = ServiceDaemon::new().map_err(|e| NetError::Discovery(e.to_string()))?;
        Ok(Self {
            daemon,
            own_id,
            fullname: None,
        })
    }

    /// Announces this device. Call again after a name or port change.
    pub fn announce(&mut self, name: &str, port: u16, app_version: &str) -> Result<()> {
        if let Some(old) = self.fullname.take() {
            let _ = self.daemon.unregister(&old);
        }
        let instance = format!(
            "{}-{}",
            sanitize_label(name),
            &self.own_id.simple().to_string()[..8]
        );
        let host = format!("{instance}.local.");
        let props: HashMap<String, String> = HashMap::from([
            ("id".to_string(), self.own_id.to_string()),
            ("name".to_string(), name.to_string()),
            ("ver".to_string(), app_version.to_string()),
            ("proto".to_string(), PROTO_VERSION.to_string()),
        ]);
        let info = ServiceInfo::new(MDNS_SERVICE, &instance, &host, "", port, props)
            .map_err(|e| NetError::Discovery(e.to_string()))?
            .enable_addr_auto();
        self.fullname = Some(info.get_fullname().to_string());
        self.daemon
            .register(info)
            .map_err(|e| NetError::Discovery(e.to_string()))?;
        Ok(())
    }

    /// Browses for other Desk Bridge devices; events are delivered on `tx` until it is dropped.
    pub fn browse(&self, tx: UnboundedSender<DiscoveryEvent>) -> Result<()> {
        let rx = self
            .daemon
            .browse(MDNS_SERVICE)
            .map_err(|e| NetError::Discovery(e.to_string()))?;
        let own = self.own_id;
        std::thread::Builder::new()
            .name("mdns-browse".into())
            .spawn(move || {
                let mut by_fullname: HashMap<String, Uuid> = HashMap::new();
                while let Ok(ev) = rx.recv() {
                    match ev {
                        ServiceEvent::ServiceResolved(info) => {
                            let Some(id) = info
                                .get_property_val_str("id")
                                .and_then(|s| Uuid::parse_str(s).ok())
                            else {
                                continue;
                            };
                            if id == own {
                                continue;
                            }
                            by_fullname.insert(info.get_fullname().to_string(), id);
                            let d = Discovered {
                                device_id: id,
                                name: info.get_property_val_str("name").unwrap_or("?").to_string(),
                                addrs: info.get_addresses().iter().copied().collect(),
                                port: info.get_port(),
                                proto: info
                                    .get_property_val_str("proto")
                                    .and_then(|s| s.parse().ok())
                                    .unwrap_or(0),
                                version: info.get_property_val_str("ver").unwrap_or("").to_string(),
                            };
                            if tx.send(DiscoveryEvent::Found(d)).is_err() {
                                break;
                            }
                        }
                        ServiceEvent::ServiceRemoved(_, fullname) => {
                            if let Some(id) = by_fullname.remove(&fullname)
                                && tx.send(DiscoveryEvent::Lost { device_id: id }).is_err()
                            {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
            })
            .map_err(NetError::Io)?;
        Ok(())
    }

    pub fn shutdown(&self) {
        if let Some(f) = &self.fullname {
            let _ = self.daemon.unregister(f);
        }
        let _ = self.daemon.shutdown();
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        self.shutdown();
    }
}
