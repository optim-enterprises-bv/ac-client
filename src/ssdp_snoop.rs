//! Passive UPnP/SSDP sniffer for device fingerprinting.
//!
//! Listens passively on UDP 239.255.255.250:1900 (SSDP multicast) using a
//! raw AF_PACKET socket.  Devices send SSDP NOTIFY messages to announce
//! their UPnP device type when they join the network and periodically
//! thereafter.  We record the NT: (Notification Type) and SERVER: headers.
//!
//! Key NT: patterns and what they mean:
//!   urn:schemas-upnp-org:device:MediaServer:1    → NAS / media server
//!   urn:schemas-upnp-org:device:MediaRenderer:1  → smart TV / AV receiver
//!   urn:schemas-upnp-org:device:InternetGatewayDevice:1 → router / gateway
//!   urn:roku-com:device:player:1-0               → Roku streaming stick/box
//!   urn:dial-multiscreen-org:service:dial:1      → Google Cast (Chromecast)
//!   urn:samsung.com:device:...                   → Samsung smart TV
//!   SERVER: LGE_DLNA_SDK/...                     → LG smart TV
//!   urn:schemas-upnp-org:device:Printer:1        → printer
//!   urn:Belkin:device:*                          → Belkin/WeMo
//!
//! On non-Linux platforms the sniffer is a no-op stub.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use log::warn;

// ── Public types ──────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default)]
pub struct SsdpFp {
    /// NT: header — UPnP notification type / device class URN.
    pub nt:     Option<String>,
    /// SERVER: header — OS/product identification string.
    pub server: Option<String>,
}

pub type FpTable = Arc<Mutex<HashMap<String, SsdpFp>>>;
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
            log::info!("ssdp_snoop: starting on {iface}");
            loop {
                match linux::run_blocking(&iface) {
                    Ok(())  => log::info!("ssdp_snoop: exited cleanly, restarting"),
                    Err(e)  => warn!("ssdp_snoop: {e}, restarting in 10s"),
                }
                std::thread::sleep(std::time::Duration::from_secs(10));
            }
        });
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = iface;
        log::info!("ssdp_snoop: raw sockets not available on this platform (stub mode)");
    }
}

// ── Classification helper ─────────────────────────────────────────────────────

/// Returns `(vendor, class, model)` if the SSDP fingerprint is conclusive.
pub fn classify_fp(fp: &SsdpFp) -> Option<(String, String, String)> {
    let nt     = fp.nt.as_deref().unwrap_or("").to_lowercase();
    let server = fp.server.as_deref().unwrap_or("").to_lowercase();

    if nt.contains("roku") {
        return Some(("Roku".into(), "tv".into(), "Roku".into()));
    }
    if nt.contains("dial-multiscreen") {
        return Some(("Google".into(), "tv".into(), "Chromecast".into()));
    }
    if nt.contains("samsung.com:device") || server.contains("samsung") {
        return Some(("Samsung".into(), "tv".into(), String::new()));
    }
    if server.contains("lge_dlna") || server.contains("lg smart tv") {
        return Some(("LG".into(), "tv".into(), String::new()));
    }
    if nt.contains("sonos") || server.contains("sonos") {
        return Some(("Sonos".into(), "speaker".into(), String::new()));
    }
    if nt.contains("belkin") || server.contains("wemo") {
        return Some(("Belkin".into(), "iot".into(), String::new()));
    }
    if nt.contains(":device:printer") {
        return Some((String::new(), "printer".into(), String::new()));
    }
    if nt.contains("internetgatewaydevice") {
        return Some((String::new(), "router".into(), String::new()));
    }
    if nt.contains("mediarenderer") {
        return Some((String::new(), "tv".into(), String::new()));
    }
    if nt.contains("mediaserver") {
        return Some((String::new(), "nas".into(), String::new()));
    }
    None
}

// ── Linux raw-socket implementation ──────────────────────────────────────────

#[cfg(target_os = "linux")]
mod linux {
    use super::{SsdpFp, FP_TABLE};
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

            if let Some((mac, fp)) = parse_ssdp(&buf[..n as usize]) {
                debug!("ssdp_snoop: {mac} nt={:?} server={:?}", fp.nt, fp.server);
                if let Some(t) = FP_TABLE.get() {
                    let mut tbl = t.lock().unwrap();
                    let e = tbl.entry(mac).or_default();
                    if fp.nt.is_some()     { e.nt     = fp.nt; }
                    if fp.server.is_some() { e.server = fp.server; }
                }
            }
        }
    }

    fn parse_ssdp(pkt: &[u8]) -> Option<(String, SsdpFp)> {
        // Must be IPv4 UDP to port 1900
        if pkt.len() < 42 { return None; }
        if u16::from_be_bytes([pkt[12], pkt[13]]) != 0x0800 { return None; }

        // Source MAC (bytes 6-11 of Ethernet header)
        let src_mac = format!(
            "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
            pkt[6], pkt[7], pkt[8], pkt[9], pkt[10], pkt[11]
        );
        // Skip multicast and broadcast MACs (these are re-announcements by the router)
        if pkt[6] & 0x01 != 0 { return None; }

        let ip_off = 14usize;
        let ihl    = ((pkt[ip_off] & 0x0f) as usize) * 4;
        if ihl < 20 || ip_off + ihl + 8 > pkt.len() { return None; }
        if pkt[ip_off + 9] != 17 { return None; } // not UDP
        // Skip fragments
        if u16::from_be_bytes([pkt[ip_off + 6], pkt[ip_off + 7]]) & 0x1fff != 0 {
            return None;
        }

        let udp_off  = ip_off + ihl;
        let dst_port = u16::from_be_bytes([pkt[udp_off + 2], pkt[udp_off + 3]]);
        if dst_port != 1900 { return None; }

        let payload_off = udp_off + 8;
        if payload_off >= pkt.len() { return None; }
        let text = std::str::from_utf8(&pkt[payload_off..]).ok()?;

        // Only NOTIFY (device announcements), not M-SEARCH (queries from clients)
        if !text.starts_with("NOTIFY") { return None; }

        let mut fp = SsdpFp::default();
        for line in text.lines() {
            // Header names are case-insensitive per HTTP spec
            let lower = line.to_lowercase();
            if let Some(v) = lower.strip_prefix("nt:") {
                let v = v.trim();
                // "NT: uuid::urn:..." lines combine two values — skip them
                if !v.is_empty() && !v.contains("::") {
                    fp.nt = Some(v.to_string());
                }
            } else if let Some(v) = lower.strip_prefix("server:") {
                let v = v.trim();
                if !v.is_empty() { fp.server = Some(v.to_string()); }
            }
        }

        if fp.nt.is_none() && fp.server.is_none() { return None; }
        Some((src_mac, fp))
    }
}
