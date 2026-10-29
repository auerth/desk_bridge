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

/// An IPv4 interface with its unicast addresses (prefix = on-link prefix length).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Iface {
    pub index: u32,
    pub name: String,
    pub addrs: Vec<(std::net::Ipv4Addr, u8)>,
}

fn in_subnet(a: std::net::Ipv4Addr, b: std::net::Ipv4Addr, prefix: u8) -> bool {
    if prefix == 0 || prefix > 32 {
        return false;
    }
    let mask = u32::MAX << (32 - u32::from(prefix));
    u32::from(a) & mask == u32::from(b) & mask
}

/// Pure part of [`diverted_route`]: `peer` lies in the local subnet of some interface, but
/// Windows sends to it through `best` (another interface, typically a VPN tunnel whose routes
/// cover the LAN, such as WireGuard with `AllowedIPs = 192.168.2.0/24`). Returns that interface.
#[must_use]
pub fn diverting_interface(
    peer: std::net::Ipv4Addr,
    best: u32,
    ifaces: &[Iface],
) -> Option<String> {
    let on_link = |i: &Iface| {
        i.addrs
            .iter()
            .any(|&(a, p)| p < 32 && in_subnet(a, peer, p))
    };
    let best_iface = ifaces.iter().find(|i| i.index == best)?;
    if on_link(best_iface) {
        return None;
    }
    ifaces
        .iter()
        .any(|i| i.index != best && on_link(i))
        .then(|| best_iface.name.clone())
}

/// All IPv4 interfaces (read-only, no elevation needed).
pub fn ipv4_interfaces() -> Vec<Iface> {
    use windows::Win32::NetworkManagement::IpHelper::{
        GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
        GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH,
    };
    use windows::Win32::Networking::WinSock::{AF_INET, SOCKADDR_IN};
    let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
    let mut size = 16 * 1024u32;
    let mut buf: Vec<u64> = Vec::new();
    // SAFETY: the buffer is u64-aligned and `size` bytes long; the linked lists returned by the
    // API point into it and are only read while it is alive.
    unsafe {
        for _ in 0..3 {
            buf.resize((size as usize).div_ceil(8), 0);
            let r = GetAdaptersAddresses(
                u32::from(AF_INET.0),
                flags,
                None,
                Some(buf.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>()),
                &mut size,
            );
            if r == 0 {
                break;
            }
            if r != 111 {
                // anything but ERROR_BUFFER_OVERFLOW
                return Vec::new();
            }
        }
        let mut out = Vec::new();
        let mut a = buf.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        while !a.is_null() {
            let ad = &*a;
            let mut addrs = Vec::new();
            let mut u = ad.FirstUnicastAddress;
            while !u.is_null() {
                let ua = &*u;
                let sa = ua.Address.lpSockaddr;
                if !sa.is_null() && (*sa).sa_family == AF_INET {
                    let sin = &*sa.cast::<SOCKADDR_IN>();
                    let ip = std::net::Ipv4Addr::from(u32::from_be(sin.sin_addr.S_un.S_addr));
                    addrs.push((ip, ua.OnLinkPrefixLength));
                }
                u = ua.Next;
            }
            out.push(Iface {
                index: ad.Anonymous1.Anonymous.IfIndex,
                name: ad.FriendlyName.to_string().unwrap_or_default(),
                addrs,
            });
            a = ad.Next;
        }
        out
    }
}

/// Index of the interface Windows routes `peer` through.
fn best_interface(peer: std::net::Ipv4Addr) -> Option<u32> {
    use windows::Win32::NetworkManagement::IpHelper::GetBestInterfaceEx;
    use windows::Win32::Networking::WinSock::{AF_INET, IN_ADDR, IN_ADDR_0, SOCKADDR, SOCKADDR_IN};
    let sin = SOCKADDR_IN {
        sin_family: AF_INET,
        sin_port: 0,
        sin_addr: IN_ADDR {
            S_un: IN_ADDR_0 {
                S_addr: u32::from(peer).to_be(),
            },
        },
        sin_zero: [0; 8],
    };
    let mut index = 0u32;
    // SAFETY: `sin` is a valid IPv4 socket address for the duration of the call.
    let r = unsafe { GetBestInterfaceEx((&raw const sin).cast::<SOCKADDR>(), &mut index) };
    (r == 0).then_some(index)
}

/// The interface (VPN tunnel) that captures the traffic to `peer` although `peer` is in the local
/// network of another interface; `None` when the route is direct or nothing is known.
#[must_use]
pub fn diverted_route(peer: std::net::Ipv4Addr) -> Option<String> {
    let best = best_interface(peer)?;
    diverting_interface(peer, best, &ipv4_interfaces())
}

#[cfg(test)]
mod tests {
    use super::{Iface, diverting_interface};
    use std::net::Ipv4Addr;

    fn ifaces() -> Vec<Iface> {
        vec![
            Iface {
                index: 5,
                name: "Ethernet 5".into(),
                addrs: vec![(Ipv4Addr::new(192, 168, 2, 42), 24)],
            },
            Iface {
                index: 59,
                name: "peer-Storage-wg0-1".into(),
                addrs: vec![(Ipv4Addr::new(10, 253, 0, 2), 32)],
            },
            Iface {
                index: 7,
                name: "vEthernet (WSL)".into(),
                addrs: vec![(Ipv4Addr::new(172, 19, 144, 1), 20)],
            },
        ]
    }

    #[test]
    fn a_tunnel_that_captures_the_lan_is_reported() {
        let pc = Ipv4Addr::new(192, 168, 2, 90);
        assert_eq!(
            diverting_interface(pc, 59, &ifaces()).as_deref(),
            Some("peer-Storage-wg0-1")
        );
        // direct route over the LAN
        assert_eq!(diverting_interface(pc, 5, &ifaces()), None);
    }

    #[test]
    fn remote_or_unknown_destinations_are_not_reported() {
        // not in any local subnet: going through the tunnel (or the gateway) is normal
        assert_eq!(
            diverting_interface(Ipv4Addr::new(8, 8, 8, 8), 59, &ifaces()),
            None
        );
        // unknown interface index
        assert_eq!(
            diverting_interface(Ipv4Addr::new(192, 168, 2, 90), 99, &ifaces()),
            None
        );
    }

    #[test]
    fn route_lookup_does_not_panic() {
        let _ = super::diverted_route(Ipv4Addr::new(192, 168, 2, 90));
        assert!(super::ipv4_interfaces().iter().all(|i| !i.name.is_empty()));
    }

    #[test]
    fn lists_connected_networks() {
        // Read-only; works without elevation (an offline machine just yields an empty list).
        let nets = super::list_connected().expect("INetworkListManager");
        eprintln!("{nets:?}");
        let public = super::public_networks().expect("public networks");
        assert!(public.len() <= nets.len());
    }
}
