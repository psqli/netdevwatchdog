use std::net::{IpAddr, Ipv4Addr};

use super::parse;

/// Reads the kernel ARP cache, which the preceding ping fills.
pub fn mac_address(ip: Ipv4Addr) -> Option<[u8; 6]> {
    parse::proc_net_arp(&std::fs::read_to_string("/proc/net/arp").ok()?, ip)
}

pub fn has_established_connection(addrs: &[IpAddr], port: u16) -> bool {
    ["/proc/net/tcp", "/proc/net/tcp6"]
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .flat_map(|t| parse::proc_net_tcp(&t))
        .any(|(ip, p)| p == port && addrs.contains(&ip))
}
