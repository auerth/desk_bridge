//! Network profile watch (plan 7.5): the firewall rule never opens the "Public" profile, so a
//! network Windows classifies as Public silently blocks the peer connection. Both sides are told:
//! this device through the `network_public` hint (with "make private"), the partner through the
//! `net=public` mDNS flag and the `peer_network_public` hint.
//!
//! VPN route watch: a VPN tunnel whose routes cover the LAN (WireGuard with the LAN subnet in
//! `AllowedIPs`, a Tailscale subnet router) takes the unicast route to the partner, while mDNS
//! (multicast, bound to the LAN interface) still finds it: the partner shows as online and every
//! connection times out. While not connected, both sides check how the partner's addresses are
//! routed: this device through the `vpn_route` hint, the partner through `route=vpn` in mDNS and
//! the `peer_vpn_route` hint.

use super::{Engine, Msg};
use serde_json::json;
use std::net::IpAddr;

/// A partner address that a VPN tunnel captures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VpnRoute {
    /// Partner name (or the manual host).
    pub peer: String,
    pub ip: IpAddr,
    /// Interface name of the tunnel.
    pub iface: String,
}

/// Re-check every 10 s (40 ticks): the category is assigned a few seconds after a network
/// (re)connects and can also be changed by the user in the Windows settings.
const CHECK_EVERY_TICKS: u64 = 40;

impl Engine {
    /// Starts a background check of the network categories (no-op while one is running).
    pub(crate) fn check_network_profile(&mut self) {
        if self.net_check_pending {
            return;
        }
        self.net_check_pending = true;
        let system = self.platform.system.clone();
        let tx = self.msg_tx.clone();
        tokio::task::spawn_blocking(move || {
            let _ = tx.send(Msg::NetworkProfile(system.public_networks()));
        });
    }

    pub(crate) fn tick_network_profile(&mut self) {
        if self.tick_count.is_multiple_of(CHECK_EVERY_TICKS) {
            self.check_network_profile();
            self.check_vpn_route();
        }
        if self.tick_count.is_multiple_of(4) {
            self.update_peer_network_hint();
        }
    }

    pub(crate) fn on_network_profile(&mut self, public: Option<Vec<String>>) {
        self.net_check_pending = false;
        let Some(public) = public else { return };
        if public != self.public_networks {
            if public.is_empty() {
                tracing::info!("network profile ok (no public network)");
            } else {
                tracing::warn!(networks = ?public, "network is classified as Public: peers are blocked");
            }
        }
        if public.is_empty() {
            self.clear_warning("network_public");
        } else {
            // Replace the text when the set of networks changed.
            if public != self.public_networks {
                self.clear_warning("network_public");
            }
            self.warn_once(
                "network_public",
                &format!(
                    "Netzwerk „{}“ ist als „Öffentlich“ eingestuft: Windows blockiert dort die Verbindung zum anderen Gerät.",
                    public.join("“, „")
                ),
            );
        }
        let is_public = !public.is_empty();
        self.public_networks = public;
        if let Some(d) = self.discovery.as_mut()
            && d.set_public_network(is_public)
        {
            self.announce_self();
        }
    }

    /// Starts a background check whether a VPN captures the route to a partner address (no-op
    /// while connected or while one is running).
    pub(crate) fn check_vpn_route(&mut self) {
        if self.session.is_some() || self.route_check_pending {
            return;
        }
        let mut targets: Vec<(String, IpAddr)> = Vec::new();
        for p in self.peers.all() {
            if let Some(e) = self.discovered.get(&p.id) {
                targets.extend(e.d.addrs.iter().map(|ip| (p.name.clone(), *ip)));
            }
            if let Some(ip) = p.last_ip.as_deref().and_then(|s| s.parse().ok()) {
                targets.push((p.name.clone(), ip));
            }
        }
        // manual hosts: "ip" or "ip:port" (host names are not resolved here)
        targets.extend(self.cfg.network.manual_hosts.iter().filter_map(|h| {
            let h = h.trim();
            let ip = h
                .parse::<IpAddr>()
                .ok()
                .or_else(|| h.parse::<std::net::SocketAddr>().ok().map(|a| a.ip()))?;
            Some((h.to_string(), ip))
        }));
        targets.retain(|(_, ip)| ip.is_ipv4());
        targets.dedup();
        if targets.is_empty() {
            return;
        }
        self.route_check_pending = true;
        let system = self.platform.system.clone();
        let tx = self.msg_tx.clone();
        tokio::task::spawn_blocking(move || {
            let found = targets.into_iter().find_map(|(peer, ip)| {
                system
                    .diverted_route(ip)
                    .map(|iface| VpnRoute { peer, ip, iface })
            });
            let _ = tx.send(Msg::VpnRoute(found));
        });
    }

    pub(crate) fn on_vpn_route(&mut self, r: Option<VpnRoute>) {
        self.route_check_pending = false;
        // A check that started before the session came up must not bring the hint back.
        let r = r.filter(|_| self.session.is_none());
        if r != self.vpn_route {
            match &r {
                Some(v) => tracing::warn!(
                    peer = %v.peer, ip = %v.ip, interface = %v.iface,
                    "the route to the partner goes through a VPN tunnel instead of the LAN"
                ),
                None if self.vpn_route.is_some() => {
                    tracing::info!("route to the partner is direct again");
                }
                None => {}
            }
            self.clear_warning("vpn_route");
        }
        if let Some(v) = &r {
            self.warn_once(
                "vpn_route",
                &format!(
                    "Die Verbindung zu „{}“ ({}) läuft über „{}“ statt über das lokale Netz: Ein VPN leitet das lokale Netz um, deshalb kommt keine Verbindung zustande. Trenne das VPN oder nimm das lokale Netz aus seinen Routen heraus (bei WireGuard: AllowedIPs).",
                    v.peer, v.ip, v.iface
                ),
            );
        }
        let iface = r.as_ref().map(|v| v.iface.clone());
        self.vpn_route = r;
        if let Some(d) = self.discovery.as_mut()
            && d.set_vpn_route(iface)
        {
            self.announce_self();
        }
    }

    /// The paired partner announces `net=public` and is not connected: explain why.
    fn update_peer_network_hint(&mut self) {
        let blocked = if self.session.is_some() {
            None
        } else {
            self.peers
                .all()
                .into_iter()
                .find(|p| {
                    self.discovered
                        .get(&p.id)
                        .is_some_and(|e| e.d.public_network)
                })
                .map(|p| p.name)
        };
        match blocked {
            Some(name) => self.warn_once(
                "peer_network_public",
                &format!(
                    "Auf „{name}“ ist das Netzwerk als „Öffentlich“ eingestuft, deshalb ist das Gerät nicht erreichbar. Öffne Desk Bridge dort und markiere das Netzwerk als privat."
                ),
            ),
            None => self.clear_warning("peer_network_public"),
        }
        let vpn = if self.session.is_some() {
            None
        } else {
            self.peers.all().into_iter().find_map(|p| {
                self.discovered
                    .get(&p.id)
                    .and_then(|e| e.d.vpn_route.clone())
                    .map(|iface| (p.name, iface))
            })
        };
        match vpn {
            Some((name, iface)) => {
                let tunnel = if iface.is_empty() {
                    "ein VPN".to_string()
                } else {
                    format!("das VPN „{iface}“")
                };
                self.warn_once(
                    "peer_vpn_route",
                    &format!(
                        "Auf „{name}“ leitet {tunnel} das lokale Netz um, deshalb ist das Gerät nicht erreichbar, obwohl es gefunden wird. Trenne dort das VPN oder nimm das lokale Netz aus seinen Routen heraus (bei WireGuard: AllowedIPs)."
                    ),
                );
            }
            None => self.clear_warning("peer_vpn_route"),
        }
    }

    /// `MakeNetworkPrivate` from the UI (the user confirmed the system change there).
    pub(crate) fn make_network_private(&mut self) -> Result<serde_json::Value, String> {
        self.platform
            .system
            .make_network_private()
            .map_err(|e| format!("Das Netzwerk konnte nicht auf „Privat“ gestellt werden: {e}"))?;
        tracing::info!("network switched to Private on user request");
        self.clear_warning("network_public");
        self.public_networks.clear();
        if let Some(d) = self.discovery.as_mut()
            && d.set_public_network(false)
        {
            self.announce_self();
        }
        self.check_network_profile();
        self.backoff.reset();
        self.next_attempt = std::time::Instant::now();
        Ok(json!({ "ok": true }))
    }

    /// (Re-)announces this device over mDNS with the current name, port and flags.
    pub(crate) fn announce_self(&mut self) {
        let port = self.transport.local_addr().map_or(0, |a| a.port());
        if let Some(d) = self.discovery.as_mut()
            && let Err(e) = d.announce(
                &self.cfg.device.name,
                port,
                crate::brand::VERSION,
                &crate::netaddr::parse_ips(&self.cfg.network.advertise_addresses),
            )
        {
            tracing::debug!("mDNS announce failed: {e}");
        }
    }
}
