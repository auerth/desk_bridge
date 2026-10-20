//! Network profile category via `INetworkListManager` (plan 7.5).
//!
//! The firewall rule never opens the **Public** profile, so a network Windows classifies as
//! "Public" silently blocks the peer connection and mDNS. The agent polls the category to show a
//! hint, and switching to Private (needs elevation) is offered after the user confirmed it.

use crate::audio::wasapi::ComGuard;
use windows::Win32::Networking::NetworkListManager::{
    INetwork, INetworkListManager, NLM_ENUM_NETWORK_CONNECTED, NLM_NETWORK_CATEGORY,
    NLM_NETWORK_CATEGORY_DOMAIN_AUTHENTICATED, NLM_NETWORK_CATEGORY_PRIVATE,
    NLM_NETWORK_CATEGORY_PUBLIC, NetworkListManager,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Public,
    Private,
    Domain,
    Unknown,
}

impl Category {
    fn from_raw(c: NLM_NETWORK_CATEGORY) -> Self {
        if c == NLM_NETWORK_CATEGORY_PUBLIC {
            Self::Public
        } else if c == NLM_NETWORK_CATEGORY_PRIVATE {
            Self::Private
        } else if c == NLM_NETWORK_CATEGORY_DOMAIN_AUTHENTICATED {
            Self::Domain
        } else {
            Self::Unknown
        }
    }
}

#[derive(Debug, Clone)]
pub struct NetworkInfo {
    pub name: String,
    pub category: Category,
}

fn connected() -> windows::core::Result<Vec<INetwork>> {
    // SAFETY: COM is initialised by the caller's guard; plain interface calls.
    unsafe {
        let nlm: INetworkListManager = CoCreateInstance(&NetworkListManager, None, CLSCTX_ALL)?;
        let en = nlm.GetNetworks(NLM_ENUM_NETWORK_CONNECTED)?;
        let mut out = Vec::new();
        loop {
            let mut slot: [Option<INetwork>; 1] = [None];
            let mut fetched = 0u32;
            en.Next(&mut slot, Some(&mut fetched))?;
            match slot[0].take() {
                Some(n) if fetched == 1 => out.push(n),
                _ => break,
            }
        }
        Ok(out)
    }
}

fn info(n: &INetwork) -> NetworkInfo {
    // SAFETY: plain interface calls on a live COM object.
    unsafe {
        NetworkInfo {
            name: n.GetName().map(|b| b.to_string()).unwrap_or_default(),
            category: n
                .GetCategory()
                .map_or(Category::Unknown, Category::from_raw),
        }
    }
}

/// All currently connected networks with their category (read-only, no elevation needed).
pub fn list_connected() -> windows::core::Result<Vec<NetworkInfo>> {
    let _com = ComGuard::init();
    Ok(connected()?.iter().map(info).collect())
}

/// Names of the connected networks classified as **Public**.
pub fn public_networks() -> windows::core::Result<Vec<String>> {
    Ok(list_connected()?
        .into_iter()
        .filter(|n| n.category == Category::Public)
        .map(|n| n.name)
        .collect())
}

/// Switches every connected **Public** network to Private and returns the changed names.
/// Domain and already-private networks are left alone. Needs elevation (`E_ACCESSDENIED`
/// otherwise).
pub fn make_private(dry_run: bool) -> windows::core::Result<Vec<String>> {
    let _com = ComGuard::init();
    let mut changed = Vec::new();
    for n in connected()? {
        let i = info(&n);
        if i.category == Category::Public {
            if !dry_run {
                // SAFETY: plain interface call on a live COM object.
                unsafe { n.SetCategory(NLM_NETWORK_CATEGORY_PRIVATE)? };
            }
            changed.push(i.name);
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    #[test]
    fn lists_connected_networks() {
        // Read-only; works without elevation (an offline machine just yields an empty list).
        let nets = super::list_connected().expect("INetworkListManager");
        eprintln!("{nets:?}");
        let public = super::public_networks().expect("public networks");
        assert!(public.len() <= nets.len());
    }
}
