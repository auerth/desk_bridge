//! Address selection for the LAN: which of a peer's addresses is worth trying first.
//!
//! mDNS advertises *every* interface of a device, including virtual adapters (Hyper-V, WSL,
//! VirtualBox, VPN) and IPv6 link-local addresses. Connecting to those yields errors such as
//! "Der Host war beim Socketvorgang nicht erreichbar" (`WSAEHOSTUNREACH`). We therefore rank the
//! candidates: same subnet as one of our own interfaces first, then other private IPv4, then
//! global IPv6; link-local and loopback only as a last resort.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

/// One local IPv4 interface address with its prefix length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalV4 {
    pub ip: Ipv4Addr,
    pub prefix: u8,
}

/// Non-loopback IPv4 addresses of this machine.
#[must_use]
pub fn local_v4() -> Vec<LocalV4> {
    if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| !i.is_loopback())
        .filter_map(|i| match i.addr {
            if_addrs::IfAddr::V4(v4) => Some(LocalV4 {
                ip: v4.ip,
                prefix: prefix_of(v4.netmask),
            }),
            if_addrs::IfAddr::V6(_) => None,
        })
        .collect()
}

fn prefix_of(mask: Ipv4Addr) -> u8 {
    u32::from(mask).count_ones() as u8
}

fn same_subnet(a: Ipv4Addr, b: Ipv4Addr, prefix: u8) -> bool {
    if prefix == 0 || prefix > 32 {
        return false;
    }
    let m = u32::MAX << (32 - u32::from(prefix));
    u32::from(a) & m == u32::from(b) & m
}

fn is_link_local_v6(ip: std::net::Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xffc0) == 0xfe80
}

/// Lower is better; `None` = never use.
fn score(ip: IpAddr, local: &[LocalV4]) -> Option<u32> {
    match ip {
        IpAddr::V4(v4) => {
            if v4.is_unspecified() || v4.is_multicast() || v4.is_broadcast() {
                return None;
            }
            if local.iter().any(|l| same_subnet(l.ip, v4, l.prefix)) {
                return Some(0);
            }
            if v4.is_loopback() {
                return Some(60);
            }
            if v4.is_link_local() {
                return Some(50);
            }
            Some(if v4.is_private() { 10 } else { 20 })
        }
        IpAddr::V6(v6) => {
            if v6.is_unspecified() || v6.is_multicast() {
                return None;
            }
            if v6.is_loopback() {
                return Some(61);
            }
            if is_link_local_v6(v6) {
                return Some(55);
            }
            Some(30)
        }
    }
}

/// Orders candidate addresses best first and drops unusable ones. Stable for equal scores.
#[must_use]
pub fn rank(candidates: &[SocketAddr], local: &[LocalV4]) -> Vec<SocketAddr> {
    let mut v: Vec<(u32, usize, SocketAddr)> = candidates
        .iter()
        .enumerate()
        .filter_map(|(i, a)| score(a.ip(), local).map(|s| (s, i, *a)))
        .collect();
    v.sort_by_key(|(s, i, _)| (*s, *i));
    let mut out: Vec<SocketAddr> = Vec::new();
    for (_, _, a) in v {
        if !out.contains(&a) {
            out.push(a);
        }
    }
    out
}

/// `ip:port` strings of this machine for the "tell the other device where to find me" hint.
#[must_use]
pub fn local_endpoints(port: u16, advertise: &[IpAddr]) -> Vec<String> {
    let mut ips: Vec<IpAddr> = if advertise.is_empty() {
        local_v4().into_iter().map(|l| IpAddr::V4(l.ip)).collect()
    } else {
        advertise.to_vec()
    };
    ips.sort_by_key(|ip| match ip {
        IpAddr::V4(v4) if v4.is_private() => 0,
        IpAddr::V4(_) => 1,
        IpAddr::V6(_) => 2,
    });
    ips.into_iter()
        .map(|ip| SocketAddr::new(ip, port).to_string())
        .collect()
}

/// Parses user supplied advertise addresses (invalid entries are ignored).
#[must_use]
pub fn parse_ips(list: &[String]) -> Vec<IpAddr> {
    list.iter().filter_map(|s| s.trim().parse().ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    fn lan() -> Vec<LocalV4> {
        vec![LocalV4 {
            ip: "192.168.1.20".parse().unwrap(),
            prefix: 24,
        }]
    }

    #[test]
    fn same_subnet_beats_virtual_adapters() {
        // The peer advertises a Hyper-V/WSL address first; the LAN address must win.
        let r = rank(
            &[
                a("172.28.80.1:47820"),
                a("192.168.1.55:47820"),
                a("[fe80::1]:47820"),
            ],
            &lan(),
        );
        assert_eq!(r[0], a("192.168.1.55:47820"));
        assert_eq!(*r.last().unwrap(), a("[fe80::1]:47820"));
    }

    #[test]
    fn unusable_addresses_are_dropped() {
        let r = rank(&[a("0.0.0.0:1"), a("224.0.0.1:1"), a("10.0.0.2:1")], &[]);
        assert_eq!(r, vec![a("10.0.0.2:1")]);
    }

    #[test]
    fn duplicates_collapse_and_order_is_stable() {
        let r = rank(&[a("10.0.0.2:1"), a("10.0.0.3:1"), a("10.0.0.2:1")], &[]);
        assert_eq!(r, vec![a("10.0.0.2:1"), a("10.0.0.3:1")]);
    }

    #[test]
    fn link_local_v4_is_a_last_resort() {
        let r = rank(&[a("169.254.3.4:1"), a("8.8.8.8:1")], &[]);
        assert_eq!(r[0], a("8.8.8.8:1"));
    }

    #[test]
    fn prefix_math() {
        assert_eq!(prefix_of("255.255.255.0".parse().unwrap()), 24);
        assert!(same_subnet(
            "10.1.2.3".parse().unwrap(),
            "10.1.9.9".parse().unwrap(),
            16
        ));
        assert!(!same_subnet(
            "10.1.2.3".parse().unwrap(),
            "10.2.2.3".parse().unwrap(),
            16
        ));
    }

    #[test]
    fn endpoints_use_the_configured_addresses() {
        let e = local_endpoints(47820, &["192.168.1.9".parse().unwrap()]);
        assert_eq!(e, vec!["192.168.1.9:47820".to_string()]);
    }
}
