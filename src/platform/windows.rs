//! Win32 implementations (IP Helper and spooler APIs).

use std::ffi::c_void;
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use windows_sys::Win32::Graphics::Printing::{ClosePrinter, EnumJobsW, JOB_INFO_1W, OpenPrinterW};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, ICMP_ECHO_REPLY, IcmpCloseHandle, IcmpCreateFile, IcmpSendEcho, SendARP, TCP_TABLE_BASIC_CONNECTIONS,
};

use super::PrintQueueProblem;

const AF_INET: u32 = 2;
const MIB_TCP_STATE_ESTAB: u32 = 5;
const NO_ERROR: u32 = 0;
const IP_SUCCESS: u32 = 0;

/// IPv4 address as the 32-bit value the IP Helper API expects (network order in memory).
fn ipaddr(ip: Ipv4Addr) -> u32 {
    u32::from_ne_bytes(ip.octets())
}

/// Asks the network stack for the MAC (sends an ARP request if needed).
pub fn mac_address(ip: Ipv4Addr) -> Option<[u8; 6]> {
    let mut mac = [0u8; 8];
    let mut len: u32 = mac.len() as u32;
    // SAFETY: the buffer is 8 bytes and its length is passed in `len`.
    let rc = unsafe { SendARP(ipaddr(ip), 0, mac.as_mut_ptr() as *mut c_void, &mut len) };
    (rc == NO_ERROR && len == 6).then(|| [mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]])
}

/// Walks the IPv4 TCP table (MIB_TCPTABLE of MIB_TCPROW: five u32 per row).
pub fn has_established_connection(addrs: &[IpAddr], port: u16) -> bool {
    let mut size: u32 = 0;
    // SAFETY: first call with a null buffer only reports the required size.
    unsafe { GetExtendedTcpTable(std::ptr::null_mut(), &mut size, 0, AF_INET, TCP_TABLE_BASIC_CONNECTIONS, 0) };
    if size == 0 {
        return false;
    }
    // u32 storage keeps the table aligned; extra room covers growth between calls.
    let mut buf = vec![0u32; (size as usize).div_ceil(4) + 64];
    let mut size = (buf.len() * 4) as u32;
    // SAFETY: `buf` holds `size` bytes.
    let rc = unsafe { GetExtendedTcpTable(buf.as_mut_ptr() as *mut c_void, &mut size, 0, AF_INET, TCP_TABLE_BASIC_CONNECTIONS, 0) };
    if rc != NO_ERROR {
        return false;
    }
    let rows = (buf[0] as usize).min((buf.len() - 1) / 5);
    (0..rows).any(|i| {
        let r = &buf[1 + i * 5..1 + i * 5 + 5];
        let (state, remote_addr, remote_port) = (r[0], r[3], r[4]);
        let remote = IpAddr::V4(Ipv4Addr::from(remote_addr.to_ne_bytes()));
        state == MIB_TCP_STATE_ESTAB && u16::from_be(remote_port as u16) == port && addrs.contains(&remote)
    })
}

/// ICMP echo through IcmpSendEcho (no administrator rights needed).
pub fn ping(ip: IpAddr, timeout: Duration) -> Option<bool> {
    let IpAddr::V4(v4) = ip else { return None };
    // SAFETY: plain Win32 calls; the reply buffer is large enough for one reply plus data.
    unsafe {
        let handle = IcmpCreateFile();
        if handle.is_null() || handle as isize == -1 {
            return None;
        }
        let payload = *b"netdevwatchdog";
        let mut reply = vec![0u8; std::mem::size_of::<ICMP_ECHO_REPLY>() + payload.len() + 8 + 64];
        let count = IcmpSendEcho(
            handle,
            ipaddr(v4),
            payload.as_ptr() as *const c_void,
            payload.len() as u16,
            std::ptr::null(),
            reply.as_mut_ptr() as *mut c_void,
            reply.len() as u32,
            timeout.as_millis().clamp(1, u32::MAX as u128) as u32,
        );
        IcmpCloseHandle(handle);
        if count == 0 {
            return Some(false);
        }
        // "Destination unreachable" from a router is also a reply: check its status.
        let first = std::ptr::read_unaligned(reply.as_ptr() as *const ICMP_ECHO_REPLY);
        Some(first.Status == IP_SUCCESS)
    }
}

// JOB_STATUS_* flags
const JOB_PAUSED: u32 = 0x1;
const JOB_ERROR: u32 = 0x2;
const JOB_OFFLINE: u32 = 0x20;
const JOB_PAPEROUT: u32 = 0x40;
const JOB_BLOCKED_DEVQ: u32 = 0x200;
const JOB_USER_INTERVENTION: u32 = 0x400;
const PROBLEM_MASK: u32 = JOB_ERROR | JOB_OFFLINE | JOB_PAPEROUT | JOB_BLOCKED_DEVQ | JOB_USER_INTERVENTION;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe fn from_wide(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut len = 0;
    // SAFETY: `p` points to a NUL-terminated UTF-16 string owned by the spooler buffer.
    while unsafe { *p.add(len) } != 0 {
        len += 1;
    }
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, len) })
}

/// First job in an error state (EnumJobs level 1).
pub fn print_queue_problem(queue: &str) -> Option<PrintQueueProblem> {
    let name = wide(queue);
    // SAFETY: standard OpenPrinter / EnumJobs / ClosePrinter sequence; the job
    // buffer is u64-aligned and its byte size is passed to EnumJobsW.
    unsafe {
        let mut printer = std::mem::zeroed();
        if OpenPrinterW(name.as_ptr(), &mut printer, std::ptr::null()) == 0 {
            return None; // queue missing or no access: nothing to report
        }
        let mut needed: u32 = 0;
        let mut returned: u32 = 0;
        EnumJobsW(printer, 0, 255, 1, std::ptr::null_mut(), 0, &mut needed, &mut returned);
        let mut result = None;
        if needed > 0 {
            let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
            if EnumJobsW(printer, 0, 255, 1, buf.as_mut_ptr() as *mut u8, (buf.len() * 8) as u32, &mut needed, &mut returned) != 0 {
                let jobs = std::slice::from_raw_parts(buf.as_ptr() as *const JOB_INFO_1W, returned as usize);
                result = jobs.iter().find(|j| j.Status & PROBLEM_MASK != 0).map(|j| {
                    let text = from_wide(j.pStatus);
                    PrintQueueProblem {
                        job_id: j.JobId,
                        document: from_wide(j.pDocument),
                        status: if text.is_empty() { describe(j.Status) } else { text },
                    }
                });
            }
        }
        ClosePrinter(printer);
        result
    }
}

fn describe(status: u32) -> String {
    [
        (JOB_ERROR, "Error"),
        (JOB_OFFLINE, "Offline"),
        (JOB_PAPEROUT, "PaperOut"),
        (JOB_BLOCKED_DEVQ, "Blocked"),
        (JOB_USER_INTERVENTION, "UserIntervention"),
        (JOB_PAUSED, "Paused"),
    ]
    .iter()
    .filter(|(flag, _)| status & flag != 0)
    .map(|(_, name)| *name)
    .collect::<Vec<_>>()
    .join(", ")
}
