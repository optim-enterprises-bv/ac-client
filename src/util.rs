//! Miscellaneous utilities: MAC detection, IP retrieval, PID file, etc.

use std::fs;
use std::io::{self, Write};
use std::net::{IpAddr, Ipv4Addr, UdpSocket};
use std::path::Path;

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
        "br-lan", "eth0", "eth1", "eth0.1", "phy0-ap0", "phy1-ap0", "wlan0", "wlan1", "ra0",
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

/// Return total memory in kB as a string, read from `/proc/meminfo`.
pub fn read_mem_total() -> String {
    let content = fs::read_to_string("/proc/meminfo").unwrap_or_default();
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
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
    let output = std::process::Command::new("iw").args(["dev"]).output();
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

// ── ARP table parsing ─────────────────────────────────────────────────────────

/// An entry from `/proc/net/arp`.
#[derive(Debug, Clone)]
pub struct ArpEntry {
    pub ip: String,
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
            let ip = fields[0].to_string();
            let mac = fields[3].to_string();
            // Skip incomplete entries (00:00:00:00:00:00)
            if mac != "00:00:00:00:00:00" {
                entries.push(ArpEntry { ip, mac });
            }
        }
    }
    entries
}

/// Get the primary local IP address
pub fn get_local_ip() -> String {
    // Try to get IP from network interface using ip command
    if let Ok(output) = std::process::Command::new("ip")
        .args(["-4", "addr", "show", "scope", "global"])
        .output()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            if line.contains("inet ") {
                // Parse line like: "    inet 192.168.1.100/24 brd 192.168.1.255 scope global eth0"
                if let Some(ip_part) = line.trim().split_whitespace().nth(1) {
                    // Remove CIDR suffix (/24)
                    return ip_part.split('/').next().unwrap_or("").to_string();
                }
            }
        }
    }

    // Fallback: try hostname command
    if let Ok(output) = std::process::Command::new("hostname").arg("-I").output() {
        let text = String::from_utf8_lossy(&output.stdout);
        return text
            .trim()
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_string();
    }

    String::new()
}

/// Get OpenWrt device model (like LuCI shows)
pub fn read_device_model() -> String {
    // Try /tmp/sysinfo/model first (this is what LuCI uses)
    if let Ok(model) = fs::read_to_string("/tmp/sysinfo/model") {
        let model = model.trim();
        if !model.is_empty() {
            return model.to_string();
        }
    }

    // Fallback: try ubus call system board
    if let Ok(output) = std::process::Command::new("ubus")
        .args(["call", "system", "board"])
        .output()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        // Parse JSON-like output to find model
        for line in text.lines() {
            if let Some(idx) = line.find("\"model\":") {
                if let Some(start) = line[idx..].find('"').map(|i| idx + i + 1) {
                    if let Some(end) = line[start..].find('"') {
                        return line[start..start + end].to_string();
                    }
                }
            }
        }
    }

    // Final fallback: board name
    if let Ok(board) = fs::read_to_string("/tmp/sysinfo/board_name") {
        return board.trim().to_string();
    }

    String::new()
}

/// Get OpenWrt architecture/target (like LuCI shows)
pub fn read_device_arch() -> String {
    // Read CPU architecture from /proc/cpuinfo (same as LuCI)
    if let Ok(cpuinfo) = fs::read_to_string("/proc/cpuinfo") {
        // Look for "model name" (ARM/x86) or "cpu model" (MIPS)
        for line in cpuinfo.lines() {
            if let Some(idx) = line.find("model name") {
                if let Some(start) = line[idx..].find(':').map(|i| idx + i + 1) {
                    let arch = line[start..].trim();
                    if !arch.is_empty() {
                        return arch.to_string();
                    }
                }
            }
            // MIPS uses "cpu model" instead
            if let Some(idx) = line.find("cpu model") {
                if let Some(start) = line[idx..].find(':').map(|i| idx + i + 1) {
                    let arch = line[start..].trim();
                    if !arch.is_empty() {
                        return arch.to_string();
                    }
                }
            }
        }
        // Fallback: look for "Processor" (older ARM format)
        for line in cpuinfo.lines() {
            if let Some(idx) = line.find("Processor") {
                if let Some(start) = line[idx..].find(':').map(|i| idx + i + 1) {
                    let arch = line[start..].trim();
                    if !arch.is_empty() && arch != "ARMv7" && arch != "ARMv8" {
                        return arch.to_string();
                    }
                }
            }
        }
    }

    // Try ubus call system board for target info
    if let Ok(output) = std::process::Command::new("ubus")
        .args(["call", "system", "board"])
        .output()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            // Look for "system" field which often contains CPU info
            if let Some(idx) = line.find("\"system\":") {
                if let Some(start) = line[idx..].find('"').map(|i| idx + i + 1) {
                    if let Some(end) = line[start..].find('"') {
                        let system = &line[start..start + end];
                        if !system.is_empty() && system != "target" {
                            return system.to_string();
                        }
                    }
                }
            }
        }
    }

    // Final fallback: use uname -m
    if let Ok(output) = std::process::Command::new("uname").arg("-m").output() {
        return String::from_utf8_lossy(&output.stdout).trim().to_string();
    }

    String::new()
}

/// Get Manufacturer OUI from MAC address (first 3 bytes)
pub fn read_manufacturer_oui(mac_addr: &str) -> String {
    // Extract first 3 octets from MAC address
    let clean_mac: String = mac_addr.chars().filter(|c| c.is_alphanumeric()).collect();
    if clean_mac.len() >= 6 {
        // Format as XX:XX:XX
        format!(
            "{}:{}:{}",
            &clean_mac[0..2],
            &clean_mac[2..4],
            &clean_mac[4..6]
        )
    } else {
        String::new()
    }
}

/// Get device description (board name + model)
pub fn read_device_description() -> String {
    let board = fs::read_to_string("/tmp/sysinfo/board_name")
        .unwrap_or_default()
        .trim()
        .to_string();

    let model = read_device_model();

    if !board.is_empty() && !model.is_empty() {
        format!("{} - {}", board, model)
    } else if !model.is_empty() {
        model
    } else if !board.is_empty() {
        board
    } else {
        "OpenWrt Device".to_string()
    }
}

/// Get kernel version (AdditionalSoftwareVersion)
pub fn read_kernel_version() -> String {
    if let Ok(output) = std::process::Command::new("uname").arg("-r").output() {
        return String::from_utf8_lossy(&output.stdout).trim().to_string();
    }
    String::new()
}

/// Get device status - always returns "Up" if agent is running
pub fn read_device_status() -> String {
    "Up".to_string()
}
