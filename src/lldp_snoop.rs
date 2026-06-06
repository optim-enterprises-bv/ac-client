//! Passive LLDP sniffer for device fingerprinting.
//!
//! Captures EtherType 0x88CC frames from the LAN bridge using an ETH_P_ALL
//! raw socket.  LLDP is used by switches, APs, IP phones, and enterprise
//! laptops to announce their identity on the local link.
//!
//! TLVs parsed (IEEE 802.1AB-2016):
//!   Type 5 — System Name        → device hostname
//!   Type 6 — System Description → OS / firmware string
//!   Type 7 — System Capabilities → enabled capability bitmap:
//!              bit  1: Repeater
//!              bit  2: Bridge (switch)
//!              bit  3: WLAN Access Point
//!              bit  5: Router
//!              bit  7: Telephone (IP phone)
//!              bit 10: Station Only (PC / laptop)
//!
//! LLDP is not common on consumer home networks but is valuable for
//! enterprise / ISP CPE deployments where managed switches and IP phones
//! are present on the LAN.
//!
//! On non-Linux platforms the sniffer is a no-op stub.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use log::warn;

// ── Public types ──────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default)]
pub struct LldpFp {
    /// TLV type 5 — System Name.
    pub system_name: Option<String>,
    /// TLV type 6 — System Description (OS, firmware).
    pub system_desc: Option<String>,
    /// TLV type 7 — *enabled* System Capabilities bitmap.
    pub capabilities: u16,
}

pub const CAP_BRIDGE:  u16 = 1 << 2;
pub const CAP_WLAN_AP: u16 = 1 << 3;
pub const CAP_ROUTER:  u16 = 1 << 5;
pub const CAP_PHONE:   u16 = 1 << 7;
pub const CAP_STATION: u16 = 1 << 10;

pub type FpTable = Arc<Mutex<HashMap<String, LldpFp>>>;
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
            log::info!("lldp_snoop: starting on {iface}");
            loop {
                match linux::run_blocking(&iface) {
                    Ok(())  => log::info!("lldp_snoop: exited cleanly, restarting"),
                    Err(e)  => warn!("lldp_snoop: {e}, restarting in 10s"),
                }
                std::thread::sleep(std::time::Duration::from_secs(10));
            }
        });
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = iface;
        log::info!("lldp_snoop: raw sockets not available on this platform (stub mode)");
    }
}

// ── Classification helper ─────────────────────────────────────────────────────

/// Returns `(vendor, class)` if the LLDP fingerprint is conclusive.
pub fn classify_fp(fp: &LldpFp) -> Option<(String, String)> {
    // Capabilities are the most reliable signal
    if fp.capabilities & CAP_PHONE != 0 {
        return Some((String::new(), "phone".into()));
    }
    if fp.capabilities & CAP_WLAN_AP != 0 {
        return Some((String::new(), "router".into()));
    }
    if fp.capabilities & CAP_ROUTER != 0 {
        return Some((String::new(), "router".into()));
    }
    if fp.capabilities & CAP_BRIDGE != 0 {
        return Some((String::new(), "router".into()));
    }

    // Description-based fallback
    let desc = fp.system_desc.as_deref().unwrap_or("").to_lowercase();
    if desc.contains("cisco ip phone") || desc.contains("avaya") {
        return Some(("Cisco".into(), "phone".into()));
    }
    if desc.contains("windows") || desc.contains("microsoft windows") {
        return Some(("Microsoft".into(), "pc".into()));
    }
    if desc.contains("openwrt") || desc.contains("dd-wrt") || desc.contains("lede") {
        return Some((String::new(), "router".into()));
    }
    if desc.contains("linux") && fp.capabilities & CAP_STATION != 0 {
        return Some((String::new(), "pc".into()));
    }

    None
}

// ── Linux raw-socket implementation ──────────────────────────────────────────

#[cfg(target_os = "linux")]
mod linux {
    use super::{LldpFp, FP_TABLE};
    use log::debug;

    const AF_PACKET:       libc::c_int = 17;
    /// ETH_P_ALL — receive all EtherTypes (needed for 0x88CC which ETH_P_IP misses)
    const ETH_P_ALL:       u16         = 0x0003;
    const SOCK_RAW:        libc::c_int = libc::SOCK_RAW;
    const SOL_SOCKET:      libc::c_int = libc::SOL_SOCKET;
    const SO_RCVTIMEO:     libc::c_int = libc::SO_RCVTIMEO;
    const LLDP_ETHERTYPE:  u16         = 0x88CC;

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
        // ETH_P_ALL is required to receive non-IP EtherTypes like 0x88CC
        let fd = unsafe {
            libc::socket(
                AF_PACKET,
                SOCK_RAW,
                (ETH_P_ALL as u16).to_be() as libc::c_int,
            )
        };
        if fd < 0 {
            return Err(anyhow::anyhow!(
                "socket(AF_PACKET,ETH_P_ALL): {}",
                std::io::Error::last_os_error()
            ));
        }

        let idx = match iface_index(iface) {
            Ok(i)  => i,
            Err(e) => { unsafe { libc::close(fd); } return Err(e); }
        };

        let sa = SockaddrLl {
            sll_family:   AF_PACKET as u16,
            sll_protocol: (ETH_P_ALL as u16).to_be(),
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

            if let Some((mac, fp)) = parse_lldp(&buf[..n as usize]) {
                debug!(
                    "lldp_snoop: {mac} name={:?} caps=0x{:04x}",
                    fp.system_name, fp.capabilities
                );
                if let Some(t) = FP_TABLE.get() {
                    let mut tbl = t.lock().unwrap();
                    let e = tbl.entry(mac).or_default();
                    if fp.system_name.is_some() { e.system_name  = fp.system_name; }
                    if fp.system_desc.is_some() { e.system_desc  = fp.system_desc; }
                    if fp.capabilities != 0     { e.capabilities = fp.capabilities; }
                }
            }
        }
    }

    fn parse_lldp(pkt: &[u8]) -> Option<(String, LldpFp)> {
        // Ethernet header is 14 bytes; need at least that + 2 for first TLV header
        if pkt.len() < 16 { return None; }

        // Filter to LLDP EtherType only
        if u16::from_be_bytes([pkt[12], pkt[13]]) != LLDP_ETHERTYPE { return None; }

        let src_mac = format!(
            "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
            pkt[6], pkt[7], pkt[8], pkt[9], pkt[10], pkt[11]
        );

        let mut fp = LldpFp::default();
        let mut i  = 14usize; // start of LLDP PDU

        while i + 2 <= pkt.len() {
            // TLV header: top 7 bits = type, low 9 bits = length
            let hdr     = u16::from_be_bytes([pkt[i], pkt[i + 1]]);
            let tlv_type = (hdr >> 9) as u8;
            let tlv_len  = (hdr & 0x01ff) as usize;
            i += 2;

            if tlv_type == 0 { break; } // End of LLDPDU
            if i + tlv_len > pkt.len() { break; }

            let val = &pkt[i..i + tlv_len];

            match tlv_type {
                5 => { // System Name
                    if let Ok(s) = std::str::from_utf8(val) {
                        let s = s.trim().to_string();
                        if !s.is_empty() { fp.system_name = Some(s); }
                    }
                }
                6 => { // System Description
                    if let Ok(s) = std::str::from_utf8(val) {
                        let s = s.trim().to_string();
                        if !s.is_empty() { fp.system_desc = Some(s); }
                    }
                }
                7 => { // System Capabilities: 2 bytes system + 2 bytes enabled
                    if val.len() >= 4 {
                        // Use the *enabled* capabilities (bytes 2-3)
                        fp.capabilities = u16::from_be_bytes([val[2], val[3]]);
                    }
                }
                _ => {}
            }

            i += tlv_len;
        }

        if fp.system_name.is_none() && fp.system_desc.is_none() && fp.capabilities == 0 {
            return None;
        }
        Some((src_mac, fp))
    }
}
