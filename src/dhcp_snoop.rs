//! Passive DHCP sniffer for device fingerprinting.
//!
//! Opens a raw AF_PACKET/SOCK_RAW socket on the LAN bridge, captures DHCP
//! Discover and Request frames (client → server), and extracts:
//!
//!   - Option 12  — client-supplied hostname
//!   - Option 55  — parameter request list (OS signature)
//!   - Option 60  — vendor class identifier (Android version, Windows, Xbox, PS4/5)
//!
//! Results accumulate in a process-wide table shared with the data-model layer.
//! The sniffer runs as a `spawn_blocking` task; DHCP is infrequent traffic so
//! blocking one thread is fine.
//!
//! On non-Linux targets (e.g., macOS dev machines) the sniffer is a no-op stub;
//! the table is still initialised so the rest of the code compiles unchanged.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use log::{info, warn};

// ── Public types ──────────────────────────────────────────────────────────────

/// DHCP-derived fingerprint data for one client MAC.
#[derive(Clone, Debug, Default)]
pub struct DhcpFp {
    /// DHCP option 12: device-supplied hostname.
    pub hostname: Option<String>,
    /// DHCP option 55: comma-separated parameter request list (e.g. "1,3,6,15").
    pub param_list: Option<String>,
    /// DHCP option 60: vendor class identifier (e.g. "android-dhcp-13", "MSFT 5.0").
    pub vendor_class: Option<String>,
}

/// Shared table: uppercase colon-MAC → DhcpFp.
pub type FpTable = Arc<Mutex<HashMap<String, DhcpFp>>>;

static FP_TABLE: OnceLock<FpTable> = OnceLock::new();

/// Initialise the global table and return a clone of the Arc.
/// Must be called once at startup before `spawn()`.
pub fn init() -> FpTable {
    let table: FpTable = Arc::new(Mutex::new(HashMap::new()));
    let _ = FP_TABLE.set(table.clone());
    table
}

/// Return a reference to the global table, or None before `init()`.
pub fn table() -> Option<&'static FpTable> {
    FP_TABLE.get()
}

/// Spawn the sniffer as a background blocking task.
/// Non-fatal: if the interface doesn't exist the sniffer logs a warning and exits.
pub fn spawn(iface: &str) {
    #[cfg(target_os = "linux")]
    {
        let iface = iface.to_string();
        tokio::task::spawn_blocking(move || {
            info!("dhcp_snoop: starting on {iface}");
            loop {
                match linux::run_blocking(&iface) {
                    Ok(()) => info!("dhcp_snoop: exited cleanly, restarting"),
                    Err(e) => warn!("dhcp_snoop: {e}, restarting in 10s"),
                }
                std::thread::sleep(std::time::Duration::from_secs(10));
            }
        });
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = iface;
        info!("dhcp_snoop: raw sockets not available on this platform (stub mode)");
    }
}

// ── Linux raw-socket implementation ──────────────────────────────────────────

#[cfg(target_os = "linux")]
mod linux {
    use super::{DhcpFp, FP_TABLE};
    use log::debug;

    // AF_PACKET / ETH_P_IP / sockaddr_ll constants & layout.
    // Defined explicitly to avoid depending on libc feature-gated Linux symbols.
    const AF_PACKET:  libc::c_int = 17;
    const ETH_P_IP:   u16         = 0x0800;
    const SOCK_RAW:   libc::c_int = libc::SOCK_RAW;
    const SOL_SOCKET: libc::c_int = libc::SOL_SOCKET;
    const SO_RCVTIMEO:libc::c_int = libc::SO_RCVTIMEO;

    /// Linux sockaddr_ll — not exposed by the libc crate on all versions.
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

    fn create_socket(iface: &str) -> anyhow::Result<libc::c_int> {
        let fd = unsafe {
            libc::socket(AF_PACKET, SOCK_RAW, (ETH_P_IP as u16).to_be() as libc::c_int)
        };
        if fd < 0 {
            return Err(anyhow::anyhow!("socket(AF_PACKET): {}", std::io::Error::last_os_error()));
        }

        let idx = match iface_index(iface) {
            Ok(i) => i,
            Err(e) => { unsafe { libc::close(fd); } return Err(e); }
        };

        let sa = SockaddrLl {
            sll_family:   AF_PACKET as u16,
            sll_protocol: ETH_P_IP.to_be(),
            sll_ifindex:  idx,
            sll_hatype:   0,
            sll_pkttype:  0,
            sll_halen:    0,
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

        // 5-second receive timeout so the thread wakes periodically.
        let tv = libc::timeval { tv_sec: 5, tv_usec: 0 };
        unsafe {
            libc::setsockopt(
                fd,
                SOL_SOCKET,
                SO_RCVTIMEO,
                &tv as *const libc::timeval as *const libc::c_void,
                std::mem::size_of::<libc::timeval>() as libc::socklen_t,
            )
        };

        Ok(fd)
    }

    pub fn run_blocking(iface: &str) -> anyhow::Result<()> {
        let fd = create_socket(iface)?;
        let mut buf = vec![0u8; 65536];

        loop {
            let n = unsafe {
                libc::recv(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0)
            };

            if n < 0 {
                let e = std::io::Error::last_os_error();
                match e.raw_os_error() {
                    Some(libc::EAGAIN) | Some(libc::EINTR) => continue,
                    Some(libc::ETIMEDOUT) => continue,
                    _ => { unsafe { libc::close(fd); } return Err(e.into()); }
                }
            }
            if n == 0 { continue; }

            if let Some((mac, fp)) = parse_dhcp(&buf[..n as usize]) {
                debug!("dhcp_snoop: {mac} vc={:?} hn={:?}", fp.vendor_class, fp.hostname);
                if let Some(t) = FP_TABLE.get() {
                    let mut tbl = t.lock().unwrap();
                    let e = tbl.entry(mac).or_default();
                    if fp.hostname.is_some()     { e.hostname     = fp.hostname; }
                    if fp.param_list.is_some()   { e.param_list   = fp.param_list; }
                    if fp.vendor_class.is_some() { e.vendor_class = fp.vendor_class; }
                }
            }
        }
    }

    fn parse_dhcp(pkt: &[u8]) -> Option<(String, DhcpFp)> {
        // Minimum: Eth(14) + IP(20) + UDP(8) + BOOTP(236) + magic(4) = 282
        if pkt.len() < 282 { return None; }

        // EtherType == IPv4
        if u16::from_be_bytes([pkt[12], pkt[13]]) != 0x0800 { return None; }

        let ip_off = 14usize;
        let ihl    = ((pkt[ip_off] & 0x0f) as usize) * 4;
        if ihl < 20 || ip_off + ihl + 8 > pkt.len() { return None; }

        if pkt[ip_off + 9] != 17 { return None; } // not UDP

        // No fragmentation
        if u16::from_be_bytes([pkt[ip_off + 6], pkt[ip_off + 7]]) & 0x1fff != 0 { return None; }

        let udp_off = ip_off + ihl;
        let src_port = u16::from_be_bytes([pkt[udp_off],     pkt[udp_off + 1]]);
        let dst_port = u16::from_be_bytes([pkt[udp_off + 2], pkt[udp_off + 3]]);
        if !(src_port == 68 && dst_port == 67) { return None; } // client→server only

        let dhcp_off = udp_off + 8;
        if dhcp_off + 240 > pkt.len() { return None; }
        if pkt[dhcp_off] != 1 { return None; } // BOOTREQUEST

        // Client MAC (chaddr, offset 28, 6 bytes)
        let ch = &pkt[dhcp_off + 28..dhcp_off + 34];
        if ch == [0u8; 6] { return None; }
        let mac = format!(
            "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
            ch[0], ch[1], ch[2], ch[3], ch[4], ch[5]
        );

        // Magic cookie
        if pkt[dhcp_off + 236..dhcp_off + 240] != [0x63, 0x82, 0x53, 0x63] { return None; }

        let mut fp = DhcpFp::default();
        let mut msg_type = 0u8;
        let mut i = dhcp_off + 240;

        while i < pkt.len() {
            let code = pkt[i];
            match code {
                255 => break,
                0   => { i += 1; continue; }
                _   => {}
            }
            if i + 1 >= pkt.len() { break; }
            let len  = pkt[i + 1] as usize;
            let data = i + 2;
            let next = data + len;
            if next > pkt.len() { break; }

            match code {
                53 => if len >= 1 { msg_type = pkt[data]; },
                12 => if let Ok(s) = std::str::from_utf8(&pkt[data..next]) {
                    let s = s.trim_matches('\0').trim();
                    if !s.is_empty() { fp.hostname = Some(s.to_string()); }
                },
                55 => {
                    let list: Vec<String> = pkt[data..next].iter().map(|b| b.to_string()).collect();
                    if !list.is_empty() { fp.param_list = Some(list.join(",")); }
                },
                60 => if let Ok(s) = std::str::from_utf8(&pkt[data..next]) {
                    let s = s.trim_matches('\0').trim();
                    if !s.is_empty() { fp.vendor_class = Some(s.to_string()); }
                },
                _ => {}
            }
            i = next;
        }

        // Only Discover (1) and Request (3)
        if msg_type != 1 && msg_type != 3 { return None; }
        Some((mac, fp))
    }
}
