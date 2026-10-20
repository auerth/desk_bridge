//! Network profile watch (plan 7.5): the firewall rule never opens the "Public" profile, so a
//! network Windows classifies as Public silently blocks the peer connection. Both sides are told:
//! this device through the `network_public` hint (with "make private"), the partner through the
//! `net=public` mDNS flag and the `peer_network_public` hint.

use super::{Engine, Msg};
use serde_json::json;

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
