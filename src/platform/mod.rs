//! Operating-system services used by the checks: MAC lookup, the TCP
//! connection table, ICMP echo and Windows print queues.

use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

/// A print-queue job that needs attention.
#[derive(Clone, Debug, PartialEq)]
pub struct PrintQueueProblem {
    pub job_id: u32,
    pub document: String,
    pub status: String,
}

/// Replaceable in tests.
pub trait Platform: Send + Sync {
    /// MAC address answering for `ip` on the local subnet, if known.
    fn mac_address(&self, ip: Ipv4Addr) -> Option<[u8; 6]>;

    /// Whether this computer has an established TCP connection to any of `addrs` on `port`.
    fn has_established_connection(&self, addrs: &[IpAddr], port: u16) -> bool;

    /// First job in an error state in a Windows print queue.
    fn print_queue_problem(&self, queue: &str) -> Option<PrintQueueProblem>;

    /// Whether print queues can be inspected at all.
    fn supports_print_queues(&self) -> bool;

    /// ICMP echo where the OS offers an unprivileged API (Windows). `None` means
    /// "not available here, use the portable implementation".
    fn ping(&self, _ip: IpAddr, _timeout: Duration) -> Option<bool> {
        None
    }
}

/// The real operating system.
#[derive(Default)]
pub struct SystemPlatform;

impl Platform for SystemPlatform {
    fn mac_address(&self, ip: Ipv4Addr) -> Option<[u8; 6]> {
        #[cfg(target_os = "linux")]
        return linux::mac_address(ip);
        #[cfg(windows)]
        return windows::mac_address(ip);
        #[allow(unreachable_code)]
        {
            let _ = ip;
            None
        }
    }

    fn has_established_connection(&self, addrs: &[IpAddr], port: u16) -> bool {
        #[cfg(target_os = "linux")]
        return linux::has_established_connection(addrs, port);
        #[cfg(windows)]
        return windows::has_established_connection(addrs, port);
        #[allow(unreachable_code)]
        {
            let _ = (addrs, port);
            false
        }
    }

    fn print_queue_problem(&self, queue: &str) -> Option<PrintQueueProblem> {
        #[cfg(windows)]
        return windows::print_queue_problem(queue);
        #[allow(unreachable_code)]
        {
            let _ = queue;
            None
        }
    }

    fn supports_print_queues(&self) -> bool {
        cfg!(windows)
    }

    fn ping(&self, ip: IpAddr, timeout: Duration) -> Option<bool> {
        #[cfg(windows)]
        return windows::ping(ip, timeout);
        #[allow(unreachable_code)]
        {
            let _ = (ip, timeout);
            None
        }
    }
}

/// Parsers shared with tests on every OS.
pub mod parse {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    /// Parses `/proc/net/arp`: `IP address  HW type  Flags  HW address  Mask  Device`.
    /// Flags 0x0 means the entry is incomplete.
    pub fn proc_net_arp(table: &str, ip: Ipv4Addr) -> Option<[u8; 6]> {
        let wanted = ip.to_string();
        for line in table.lines().skip(1) {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 4 || f[0] != wanted {
                continue;
            }
            let flags = u32::from_str_radix(f[2].trim_start_matches("0x"), 16).ok()?;
            if flags == 0 {
                return None;
            }
            let mac = crate::config::parse_mac(f[3])?;
            return (mac != [0; 6]).then_some(mac);
        }
        None
    }

    /// Remote endpoints of ESTABLISHED connections in `/proc/net/tcp` or `/proc/net/tcp6`.
    pub fn proc_net_tcp(table: &str) -> Vec<(IpAddr, u16)> {
        let mut out = Vec::new();
        for line in table.lines().skip(1) {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 4 || f[3] != "01" {
                continue; // 01 = TCP_ESTABLISHED
            }
            let Some((addr, port)) = f[2].split_once(':') else { continue };
            let Ok(port) = u16::from_str_radix(port, 16) else { continue };
            let ip = match addr.len() {
                8 => u32::from_str_radix(addr, 16).ok().map(|v| IpAddr::V4(Ipv4Addr::from(v.to_le_bytes()))),
                32 => {
                    let mut bytes = [0u8; 16];
                    let mut ok = true;
                    for (i, chunk) in bytes.chunks_mut(4).enumerate() {
                        match u32::from_str_radix(&addr[i * 8..i * 8 + 8], 16) {
                            Ok(w) => chunk.copy_from_slice(&w.to_le_bytes()),
                            Err(_) => ok = false,
                        }
                    }
                    ok.then(|| {
                        let v6 = Ipv6Addr::from(bytes);
                        v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(IpAddr::V6(v6))
                    })
                }
                _ => None,
            };
            if let Some(ip) = ip {
                out.push((ip, port));
            }
        }
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        const ARP: &str = "IP address       HW type     Flags       HW address            Mask     Device\n\
            192.168.10.21    0x1         0x2         00:11:22:33:44:55     *        eth0\n\
            192.168.10.22    0x1         0x0         00:00:00:00:00:00     *        eth0\n";

        #[test]
        fn arp_table() {
            assert_eq!(proc_net_arp(ARP, "192.168.10.21".parse().unwrap()), Some([0, 0x11, 0x22, 0x33, 0x44, 0x55]));
            assert_eq!(proc_net_arp(ARP, "192.168.10.22".parse().unwrap()), None);
            assert_eq!(proc_net_arp(ARP, "192.168.10.99".parse().unwrap()), None);
        }

        #[test]
        fn tcp_tables() {
            let v4 = "  sl  local_address rem_address   st tx_queue rx_queue\n\
                0: 0100007F:1F90 1500A8C0:23C3 01 00000000:00000000\n\
                1: 0100007F:1F91 1600A8C0:23C3 0A 00000000:00000000\n";
            assert_eq!(proc_net_tcp(v4), vec![("192.168.0.21".parse().unwrap(), 9155)]);

            let v6 = "  sl  local_address                         remote_address                        st\n\
                0: 00000000000000000000000001000000:1F90 0000000000000000FFFF00001500A8C0:238C 01\n";
            assert_eq!(proc_net_tcp(v6), vec![("192.168.0.21".parse().unwrap(), 9100)]);
        }
    }
}
