//! Miscellaneous utilities: MAC detection, IP retrieval, MD5, PID file, etc.

use std::fs;
use std::io::{self, Write};
use std::net::{IpAddr, Ipv4Addr, UdpSocket};
use std::path::Path;

use log::warn;

// ── MAC address ───────────────────────────────────────────────────────────────

/// Read the MAC address of a network interface from `/sys/class/net/<iface>/address`.
pub fn read_mac_from_sysfs(iface: &str) -> io::Result<String> {
    let path = format!("/sys/class/net/{iface}/address");
    let raw = fs::read_to_string(&path)?;
    Ok(raw.trim().to_string())
}

/// Try to detect the device MAC address.
///
/// Tries a broad set of interface names commonly found on OpenWrt devices:
///   br-lan        — LAN bridge (most home routers)
///   eth0 / eth1   — single-port or secondary Ethernet
///   eth0.1        — VLAN-tagged LAN port
///   phy0-ap0      — Wi-Fi AP interface (OpenWrt 21.02+ mac80211 naming)
///   phy1-ap0      — second radio AP interface
///   wlan0 / wlan1 — older or alternative Wi-Fi interface names
///   ra0           — Ralink/MediaTek Wi-Fi driver interface name
///
/// Returns an empty string if none of the above could be read from sysfs.
/// In that case the caller should require `mac_addr` to be set explicitly
/// in the UCI config or flat config file.
pub fn detect_mac() -> String {
    for iface in &[
        "br-lan",
        "eth0", "eth1",
        "eth0.1",
        "phy0-ap0", "phy1-ap0",
        "wlan0", "wlan1",
        "ra0",
    ] {
        if let Ok(mac) = read_mac_from_sysfs(iface) {
            if !mac.is_empty() && mac != "00:00:00:00:00:00" {
                return mac;
            }
        }
    }
    String::new()
}

/// Strip colons from a MAC address string: "aa:bb:cc:dd:ee:ff" → "aabbccddeeff".
pub fn mac_no_colons(mac: &str) -> String {
    mac.replace(':', "")
}

// ── IP address ────────────────────────────────────────────────────────────────

/// Detect the device's primary outbound IP address by making a dummy UDP
/// connection (no packets actually sent).  Falls back to "0.0.0.0".
pub fn get_own_ip() -> String {
    let ip = (|| -> io::Result<IpAddr> {
        let sock = UdpSocket::bind("0.0.0.0:0")?;
        sock.connect("8.8.8.8:80")?;
        Ok(sock.local_addr()?.ip())
    })()
    .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    ip.to_string()
}

/// Read the default gateway from `/proc/net/route`.
/// Returns an empty string if not found.
pub fn get_default_gateway() -> String {
    let content = match fs::read_to_string("/proc/net/route") {
        Ok(c) => c,
        Err(_) => return String::new(),
    };
    for line in content.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        // fields: Iface Destination Gateway Flags ...
        // Default route: Destination == "00000000"
        if fields.len() >= 3 && fields[1] == "00000000" {
            if let Ok(hex) = u32::from_str_radix(fields[2], 16) {
                let bytes = hex.to_le_bytes();
                return format!("{}.{}.{}.{}", bytes[0], bytes[1], bytes[2], bytes[3]);
            }
        }
    }
    String::new()
}

// ── MD5 ───────────────────────────────────────────────────────────────────────

/// Return the lowercase hex MD5 digest of `data`.
pub fn md5_hex(data: &[u8]) -> String {
    format!("{:x}", md5::compute(data))
}

/// Return the MD5 digest of the file at `path`.
pub fn md5_file(path: &Path) -> io::Result<String> {
    let data = fs::read(path)?;
    Ok(md5_hex(&data))
}

// ── Firmware version ──────────────────────────────────────────────────────────

/// Read the firmware version string from `/etc/openwrt_release` or
/// `/etc/openwrt_version`.  Returns an empty string on failure.
pub fn read_fw_version() -> String {
    // Try the release file first (DISTRIB_REVISION field)
    if let Ok(content) = fs::read_to_string("/etc/openwrt_release") {
        for line in content.lines() {
            if let Some(rest) = line.strip_prefix("DISTRIB_REVISION=") {
                return rest.trim_matches('\'').trim_matches('"').to_string();
            }
        }
    }
    // Fall back to the plain version file
    if let Ok(content) = fs::read_to_string("/etc/openwrt_version") {
        return content.trim().to_string();
    }
    String::new()
}

// ── System stats (/proc) ──────────────────────────────────────────────────────

/// Return uptime as a formatted string "Xd Xh Xm Xs".
pub fn read_uptime() -> String {
    let content = fs::read_to_string("/proc/uptime").unwrap_or_default();
    let secs_f: f64 = content
        .split_whitespace()
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);
    let secs = secs_f as u64;
    format!(
        "{}d {}h {}m {}s",
        secs / 86400,
        (secs % 86400) / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

/// Return the load average string (e.g. "0.10 0.05 0.01").
pub fn read_load_avg() -> String {
    let content = fs::read_to_string("/proc/loadavg").unwrap_or_default();
    let parts: Vec<&str> = content.split_whitespace().take(3).collect();
    parts.join(" ")
}

/// Return free memory in kB as a string, read from `/proc/meminfo`.
pub fn read_free_mem() -> String {
    let content = fs::read_to_string("/proc/meminfo").unwrap_or_default();
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("MemFree:") {
            let kb: u64 = rest
                .split_whitespace()
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            return kb.to_string();
        }
    }
    String::new()
}

// ── SSID ─────────────────────────────────────────────────────────────────────

/// Read the SSID of the first wireless interface via `iw`.
/// Returns an empty string on failure (e.g. no wireless iface, or `iw` absent).
pub fn read_ssid() -> String {
    let output = std::process::Command::new("iw")
        .args(["dev"])
        .output();
    let output = match output {
        Ok(o) => o,
        Err(_) => return String::new(),
    };
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        if let Some(rest) = line.trim().strip_prefix("ssid ") {
            return rest.trim().to_string();
        }
    }
    String::new()
}

// ── PID file ──────────────────────────────────────────────────────────────────

/// Write the current process PID to `path`.
pub fn write_pid_file(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut f = fs::File::create(path)?;
    writeln!(f, "{}", std::process::id())?;
    Ok(())
}

/// Remove the PID file (best-effort, logs a warning on failure).
#[allow(dead_code)]
pub fn remove_pid_file(path: &Path) {
    if let Err(e) = fs::remove_file(path) {
        warn!("failed to remove PID file {}: {e}", path.display());
    }
}

// ── ARP table parsing ─────────────────────────────────────────────────────────

/// An entry from `/proc/net/arp`.
#[derive(Debug, Clone)]
pub struct ArpEntry {
    pub ip:  String,
    pub mac: String,
}

/// Parse `/proc/net/arp` and return all complete entries.
pub fn read_arp_table() -> Vec<ArpEntry> {
    let content = match fs::read_to_string("/proc/net/arp") {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let mut entries = Vec::new();
    for line in content.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        // IP address | HW type | Flags | HW address | Mask | Device
        if fields.len() >= 4 {
            let ip  = fields[0].to_string();
            let mac = fields[3].to_string();
            // Skip incomplete entries (00:00:00:00:00:00)
            if mac != "00:00:00:00:00:00" {
                entries.push(ArpEntry { ip, mac });
            }
        }
    }
    entries
}
