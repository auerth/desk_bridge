//! Network profile category via `INetworkListManager` (plan 7.5: the firewall rule is Private-only).

use anyhow::{Context, Result};
use serde::Serialize;
use windows::Win32::Networking::NetworkListManager::{
    INetwork, INetworkListManager, NLM_ENUM_NETWORK_CONNECTED, NLM_NETWORK_CATEGORY,
    NLM_NETWORK_CATEGORY_DOMAIN_AUTHENTICATED, NLM_NETWORK_CATEGORY_PRIVATE,
    NLM_NETWORK_CATEGORY_PUBLIC, NetworkListManager,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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

#[derive(Debug, Clone, Serialize)]
pub struct NetworkInfo {
    pub name: String,
    pub category: Category,
}

fn manager() -> Result<INetworkListManager> {
    unsafe { CoCreateInstance(&NetworkListManager, None, CLSCTX_ALL) }
        .context("CoCreateInstance(NetworkListManager)")
}

fn connected() -> Result<Vec<INetwork>> {
    let nlm = manager()?;
    let en = unsafe { nlm.GetNetworks(NLM_ENUM_NETWORK_CONNECTED) }.context("GetNetworks")?;
    let mut out = Vec::new();
    loop {
        let mut slot: [Option<INetwork>; 1] = [None];
        let mut fetched = 0u32;
        unsafe { en.Next(&mut slot, Some(&mut fetched)) }.context("IEnumNetworks::Next")?;
        match slot[0].take() {
            Some(n) if fetched == 1 => out.push(n),
            _ => break,
        }
    }
    Ok(out)
}

/// All currently connected networks with their category (read-only).
pub fn list_connected() -> Result<Vec<NetworkInfo>> {
    connected()?
        .into_iter()
        .map(|n| {
            Ok(NetworkInfo {
                name: unsafe { n.GetName() }
                    .map(|b| b.to_string())
                    .unwrap_or_default(),
                category: Category::from_raw(unsafe { n.GetCategory() }.context("GetCategory")?),
            })
        })
        .collect()
}

/// Switch every connected **Public** network to Private. Returns the names that were changed.
/// Domain and already-private networks are left alone. Needs elevation.
pub fn make_private(dry_run: bool) -> Result<Vec<String>> {
    let mut changed = Vec::new();
    for n in connected()? {
        let name = unsafe { n.GetName() }
            .map(|b| b.to_string())
            .unwrap_or_default();
        let cat = Category::from_raw(unsafe { n.GetCategory() }.context("GetCategory")?);
        if cat == Category::Public {
            if !dry_run {
                unsafe { n.SetCategory(NLM_NETWORK_CATEGORY_PRIVATE) }
                    .with_context(|| format!("SetCategory(Private) for {name:?}"))?;
            }
            changed.push(name);
        }
    }
    Ok(changed)
}
