//! Passive NetBIOS Name Service (NBNS) sniffer for device fingerprinting.
//!
//! Captures UDP port 137 broadcast/multicast packets from Windows hosts
//! on the LAN.  Windows sends NBNS Name Registration Requests when joining
//! the network, which contain the NetBIOS computer name.
//!
//! The decoded name (e.g., "DESKTOP-ABCDE", "JOHN-LAPTOP") gives us:
//!   • Definitive confirmation of a Windows PC (no other OS uses NBNS)
//!   • A fallback hostname that supplements or replaces DHCP option 12
//!
//! NBNS name encoding (RFC 1002 Level 1):
//!   Each byte of the 15-char (space-padded) name is split into two nibbles,
//!   each nibble is added to 'A' (0x41).  The resulting 32-char string is
//!   then wrapped in a DNS label (length 0x20 = 32, then the 32 chars, then
//!   a null terminator label).
//!
//! On non-Linux platforms the sniffer is a no-op stub.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use log::warn;

// ── Public types ──────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default)]
pub struct NbnsFp {
    /// NetBIOS computer name, space-padding stripped (e.g., "DESKTOP-ABCDE").
    pub computer_name: Option<String>,
}

pub type FpTable = Arc<Mutex<HashMap<String, NbnsFp>>>;
static FP_TABLE: OnceLock<FpTable> = OnceLock::new();

pub fn init() -> FpTable {
    let table: FpTable = Arc::new(Mutex::new(HashMap::new()));
    let _ = FP_TABLE.set(table.clone());
    table
}

pub fn table() -> Option<&'static FpTable> {
    FP_TABLE.get()
}

pub fn spawn(iface: &str) {
    #[cfg(target_os = "linux")]
    {
        let iface = iface.to_string();
        tokio::task::spawn_blocking(move || {
            log::info!("nbns_snoop: starting on {iface}");
            loop {
                match linux::run_blocking(&iface) {
                    Ok(())  => log::info!("nbns_snoop: exited cleanly, restarting"),
                    Err(e)  => warn!("nbns_snoop: {e}, restarting in 10s"),
                }
                std::thread::sleep(std::time::Duration::from_secs(10));
            }
        });
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = iface;
        log::info!("nbns_snoop: raw sockets not available on this platform (stub mode)");
    }
}

// ── NetBIOS L1 name decoder ───────────────────────────────────────────────────

/// Decode a 32-byte NetBIOS Level-1 encoded name into a plain ASCII string.
///
/// Each pair of bytes (hi, lo) decodes to one original byte:
///   `byte = ((hi - 'A') << 4) | (lo - 'A')`
/// The resulting 15 bytes include space padding; we strip trailing spaces
/// and ignore the 16th byte (resource type suffix).
fn decode_nbns_name(encoded: &[u8]) -> Option<String> {
    if encoded.len() < 32 { return None; }
    let mut out = Vec::with_capacity(15);
    for i in (0..30).step_by(2) {
        let hi = encoded[i];
        let lo = encoded[i + 1];
        if hi < 0x41 || lo < 0x41 { return None; }
        let b = ((hi - 0x41) << 4) | (lo - 0x41);
        out.push(b);
    }
    let s = String::from_utf8(out).ok()?;
    let s = s.trim_end_matches(' ').to_string();
    // Reject wildcard and empty names
    if s.is_empty() || s == "*" { return None; }
    Some(s)
}

// ── Linux raw-socket implementation ──────────────────────────────────────────

#[cfg(target_os = "linux")]
mod linux {
    use super::{NbnsFp, FP_TABLE, decode_nbns_name};
    use log::debug;

    const AF_PACKET:   libc::c_int = 17;
    const ETH_P_IP:    u16         = 0x0800;
    const SOCK_RAW:    libc::c_int = libc::SOCK_RAW;
    const SOL_SOCKET:  libc::c_int = libc::SOL_SOCKET;
    const SO_RCVTIMEO: libc::c_int = libc::SO_RCVTIMEO;

    #[repr(C)]
    struct SockaddrLl {
        sll_family:   u16,
        sll_protocol: u16,
        sll_ifindex:  i32,
        sll_hatype:   u16,
        sll_pkttype:  u8,
        sll_halen:    u8,
        sll_addr:     [u8; 8],
    }

    fn iface_index(iface: &str) -> anyhow::Result<i32> {
        let path = format!("/sys/class/net/{iface}/ifindex");
        Ok(std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("cannot read {path}: {e}"))?
            .trim()
            .parse()
            .map_err(|e| anyhow::anyhow!("parse ifindex: {e}"))?)
    }

    pub fn run_blocking(iface: &str) -> anyhow::Result<()> {
        let fd = unsafe {
            libc::socket(AF_PACKET, SOCK_RAW, (ETH_P_IP as u16).to_be() as libc::c_int)
        };
        if fd < 0 {
            return Err(anyhow::anyhow!(
                "socket(AF_PACKET): {}",
                std::io::Error::last_os_error()
            ));
        }

        let idx = match iface_index(iface) {
            Ok(i)  => i,
            Err(e) => { unsafe { libc::close(fd); } return Err(e); }
        };

        let sa = SockaddrLl {
            sll_family:   AF_PACKET as u16,
            sll_protocol: ETH_P_IP.to_be(),
            sll_ifindex:  idx,
            sll_hatype:   0, sll_pkttype: 0, sll_halen: 0,
            sll_addr:     [0u8; 8],
        };

        let ret = unsafe {
            libc::bind(
                fd,
                &sa as *const SockaddrLl as *const libc::sockaddr,
                std::mem::size_of::<SockaddrLl>() as libc::socklen_t,
            )
        };
        if ret < 0 {
            unsafe { libc::close(fd); }
            return Err(anyhow::anyhow!("bind({iface}): {}", std::io::Error::last_os_error()));
        }

        let tv = libc::timeval { tv_sec: 5, tv_usec: 0 };
        unsafe {
            libc::setsockopt(
                fd, SOL_SOCKET, SO_RCVTIMEO,
                &tv as *const libc::timeval as *const libc::c_void,
                std::mem::size_of::<libc::timeval>() as libc::socklen_t,
            )
        };

        let mut buf = vec![0u8; 65536];
        loop {
            let n = unsafe {
                libc::recv(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0)
            };
            if n < 0 {
                let e = std::io::Error::last_os_error();
                match e.raw_os_error() {
                    Some(libc::EAGAIN) | Some(libc::EINTR) | Some(libc::ETIMEDOUT) => continue,
                    _ => { unsafe { libc::close(fd); } return Err(e.into()); }
                }
            }
            if n == 0 { continue; }

            if let Some((mac, fp)) = parse_nbns(&buf[..n as usize]) {
                debug!("nbns_snoop: {mac} name={:?}", fp.computer_name);
                if let Some(t) = FP_TABLE.get() {
                    let mut tbl = t.lock().unwrap();
                    let e = tbl.entry(mac).or_default();
                    if fp.computer_name.is_some() { e.computer_name = fp.computer_name; }
                }
            }
        }
    }

    fn parse_nbns(pkt: &[u8]) -> Option<(String, NbnsFp)> {
        // Min: Eth(14) + IP(20) + UDP(8) + NBNS header(12) + name label = ~57
        if pkt.len() < 57 { return None; }
        if u16::from_be_bytes([pkt[12], pkt[13]]) != 0x0800 { return None; }

        // Skip multicast/broadcast source MACs
        if pkt[6] & 0x01 != 0 { return None; }

        let src_mac = format!(
            "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
            pkt[6], pkt[7], pkt[8], pkt[9], pkt[10], pkt[11]
        );

        let ip_off = 14usize;
        let ihl    = ((pkt[ip_off] & 0x0f) as usize) * 4;
        if ihl < 20 || ip_off + ihl + 8 > pkt.len() { return None; }
        if pkt[ip_off + 9] != 17 { return None; } // not UDP
        // Skip fragments
        if u16::from_be_bytes([pkt[ip_off + 6], pkt[ip_off + 7]]) & 0x1fff != 0 {
            return None;
        }

        let udp_off  = ip_off + ihl;
        let src_port = u16::from_be_bytes([pkt[udp_off],     pkt[udp_off + 1]]);
        let dst_port = u16::from_be_bytes([pkt[udp_off + 2], pkt[udp_off + 3]]);
        if src_port != 137 && dst_port != 137 { return None; }

        let nbns_off = udp_off + 8;
        if nbns_off + 12 > pkt.len() { return None; }

        // NBNS header flags (bytes 2-3)
        let flags     = u16::from_be_bytes([pkt[nbns_off + 2], pkt[nbns_off + 3]]);
        let is_resp   = (flags >> 15) & 1 == 1;
        let opcode    = (flags >> 11) & 0x0f;

        // Accept:
        //   • Name Registration Request  (QR=0, OPCODE=5)
        //   • Name Query Response        (QR=1, OPCODE=0) — tells us the name too
        let is_registration = !is_resp && opcode == 5;
        let is_query_resp   =  is_resp && opcode == 0;
        if !is_registration && !is_query_resp { return None; }

        // Name starts after 12-byte NBNS header.
        // Format: 1-byte length (must be 0x20=32) + 32 encoded chars + 1-byte \0
        let name_off = nbns_off + 12;
        if name_off + 33 > pkt.len() { return None; }
        if pkt[name_off] != 0x20 { return None; }

        let encoded = &pkt[name_off + 1..name_off + 33];
        let name = decode_nbns_name(encoded)?;

        Some((src_mac, NbnsFp { computer_name: Some(name) }))
    }
}
