//! Watches this computer's addresses through rtnetlink, so pairing offers name the current
//! ones and peers stuck in backoff are retried as soon as the network comes back.

use std::net::{IpAddr, SocketAddr};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

use rustix::io::Errno;
use rustix::net::netlink::SocketAddrNetlink;
use rustix::net::{bind, recv, socket_with, AddressFamily, RecvFlags, SocketFlags, SocketType};

const RTMGRP_IPV4_IFADDR: u32 = 0x10;
const RTMGRP_IPV6_IFADDR: u32 = 0x100;
const NOTIFICATION_BUFFER: usize = 4096;

/// The addresses peers can reach the llts socket at, and a netlink socket that turns readable
/// when they may have changed.
#[derive(Debug)]
pub struct LinkMonitor {
    socket: OwnedFd,
    bound: SocketAddr,
    addresses: Vec<SocketAddr>,
}

impl LinkMonitor {
    /// Watches the addresses a socket bound to `bound` answers on.
    ///
    /// # Errors
    ///
    /// Fails when the kernel refuses a route netlink socket.
    pub fn open(bound: SocketAddr) -> std::io::Result<Self> {
        let socket = socket_with(
            AddressFamily::NETLINK,
            SocketType::RAW,
            SocketFlags::CLOEXEC | SocketFlags::NONBLOCK,
            None,
        )?;
        bind(
            &socket,
            &SocketAddrNetlink::new(0, RTMGRP_IPV4_IFADDR | RTMGRP_IPV6_IFADDR),
        )?;
        Ok(Self {
            socket,
            bound,
            addresses: reachable_addresses(bound),
        })
    }

    pub fn addresses(&self) -> &[SocketAddr] {
        &self.addresses
    }

    /// Drains pending notifications; `true` when the reachable addresses changed.
    pub fn refresh(&mut self) -> bool {
        let mut buffer = [0_u8; NOTIFICATION_BUFFER];
        loop {
            match recv(&self.socket, &mut buffer, RecvFlags::DONTWAIT) {
                Ok(_) => {}
                Err(Errno::INTR) => {}
                Err(_) => break,
            }
        }
        let current = reachable_addresses(self.bound);
        if current == self.addresses {
            return false;
        }
        self.addresses = current;
        true
    }
}

impl AsFd for LinkMonitor {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.socket.as_fd()
    }
}

/// A socket bound to one address is reachable only there; a wildcard one on every address
/// of its family (both, for a dual-stack IPv6 socket) except loopback and IPv6 link-local,
/// which peers cannot dial without a scope. IPv4 comes first, as the likelier route.
pub fn reachable_addresses(bound: SocketAddr) -> Vec<SocketAddr> {
    if !bound.ip().is_unspecified() {
        return vec![bound];
    }
    let port = bound.port();
    let mut addresses: Vec<IpAddr> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .map(|interface| interface.ip())
        .filter(|ip| dialable(*ip) && (bound.is_ipv6() || ip.is_ipv4()))
        .collect();
    addresses.sort_by_key(|ip| (ip.is_ipv6(), *ip));
    addresses.dedup();
    if addresses.is_empty() {
        addresses.push(if bound.is_ipv6() {
            IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)
        } else {
            IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
        });
    }
    addresses
        .into_iter()
        .map(|ip| SocketAddr::new(ip, port))
        .collect()
}

/// Whether a peer could dial `ip` as it is written.
pub const fn dialable(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => !v4.is_loopback() && !v4.is_unspecified(),
        IpAddr::V6(v6) => !v6.is_loopback() && !v6.is_unspecified() && !v6.is_unicast_link_local(),
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use super::*;

    #[test]
    fn a_socket_bound_to_one_address_is_reached_only_there() {
        let bound = SocketAddr::from((Ipv4Addr::LOCALHOST, 4000));
        assert_eq!(reachable_addresses(bound), [bound]);
    }

    #[test]
    fn a_wildcard_socket_announces_its_port_on_every_dialable_address() {
        let addresses = reachable_addresses(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 4001)));
        assert!(!addresses.is_empty());
        assert!(addresses
            .iter()
            .all(|address| address.port() == 4001 && address.is_ipv4()));
    }

    #[test]
    fn loopback_and_link_local_are_not_dialable() {
        assert!(!dialable(IpAddr::V6(Ipv6Addr::LOCALHOST)));
        assert!(!dialable(IpAddr::V6(
            "fe80::1".parse().unwrap_or(Ipv6Addr::UNSPECIFIED)
        )));
        assert!(dialable(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 4))));
    }
}
