//! TR-181 Device.Ethernet.* — Ethernet interface and link objects.
//!
//! Reads physical Ethernet interface data from sysfs (/sys/class/net/eth*)
//! and provides standard TR-181 paths for interface stats, speed, and duplex.

use crate::config::ClientConfig;
use std::collections::HashMap;

pub type Params = HashMap<String, String>;

pub async fn get(_cfg: &ClientConfig, path: &str) -> Params {
    let mut m = Params::new();
    let ifaces = get_ethernet_interfaces();

    if path == "Device.Ethernet."
        || path == "Device.Ethernet.Interface."
        || path.starts_with("Device.Ethernet.InterfaceNumberOfEntries")
    {
        m.insert(
            "Device.Ethernet.InterfaceNumberOfEntries".to_string(),
            ifaces.len().to_string(),
        );
        m.insert(
            "Device.Ethernet.LinkNumberOfEntries".to_string(),
            ifaces.len().to_string(),
        );
    }

    if path == "Device.Ethernet." || path.starts_with("Device.Ethernet.Interface.") {
        let specific_idx = extract_iface_index(path, "Interface.");
        for (i, iface) in ifaces.iter().enumerate() {
            let idx = i + 1;
            if let Some(si) = specific_idx {
                if si != idx {
                    continue;
                }
            }
            let base = format!("Device.Ethernet.Interface.{idx}.");
            populate_interface(&base, iface, &mut m).await;
        }
    }

    if path == "Device.Ethernet." || path.starts_with("Device.Ethernet.Link.") {
        let specific_idx = extract_iface_index(path, "Link.");
        for (i, iface) in ifaces.iter().enumerate() {
            let idx = i + 1;
            if let Some(si) = specific_idx {
                if si != idx {
                    continue;
                }
            }
            let base = format!("Device.Ethernet.Link.{idx}.");
            populate_link(&base, iface, &mut m).await;
        }
    }

    m
}

async fn populate_interface(base: &str, iface: &str, m: &mut Params) {
    m.insert(format!("{base}Name"), iface.to_string());
    m.insert(format!("{base}Enable"), "true".to_string());

    // Status from operstate
    let status = read_sysfs(iface, "operstate").await;
    let status_str = match status.as_str() {
        "up" => "Up",
        "down" => "Down",
        _ => "Unknown",
    };
    m.insert(format!("{base}Status"), status_str.to_string());

    // MACAddress
    let mac = read_sysfs(iface, "address").await;
    if !mac.is_empty() {
        m.insert(format!("{base}MACAddress"), mac);
    }

    // MaxBitRate from speed (in Mbps, -1 if unknown)
    let speed = read_sysfs(iface, "speed").await;
    if !speed.is_empty() && speed != "-1" {
        m.insert(format!("{base}MaxBitRate"), speed);
    }

    // DuplexMode
    let duplex = read_sysfs(iface, "duplex").await;
    if !duplex.is_empty() && duplex != "unknown" {
        let dm = match duplex.as_str() {
            "full" => "Full",
            "half" => "Half",
            _ => "Auto",
        };
        m.insert(format!("{base}DuplexMode"), dm.to_string());
    }

    // Stats
    let stats_base = format!("{base}Stats.");
    let stats = [
        ("BytesSent", "tx_bytes"),
        ("BytesReceived", "rx_bytes"),
        ("PacketsSent", "tx_packets"),
        ("PacketsReceived", "rx_packets"),
        ("ErrorsSent", "tx_errors"),
        ("ErrorsReceived", "rx_errors"),
        ("DiscardPacketsSent", "tx_dropped"),
        ("DiscardPacketsReceived", "rx_dropped"),
    ];
    for (param, sysfs_name) in &stats {
        let val = read_sysfs_stat(iface, sysfs_name).await;
        if !val.is_empty() {
            m.insert(format!("{stats_base}{param}"), val);
        }
    }
}

async fn populate_link(base: &str, iface: &str, m: &mut Params) {
    m.insert(format!("{base}Name"), iface.to_string());
    m.insert(format!("{base}Enable"), "true".to_string());

    let status = read_sysfs(iface, "operstate").await;
    let status_str = match status.as_str() {
        "up" => "Up",
        "down" => "Down",
        _ => "Unknown",
    };
    m.insert(format!("{base}Status"), status_str.to_string());

    let mac = read_sysfs(iface, "address").await;
    if !mac.is_empty() {
        m.insert(format!("{base}MACAddress"), mac);
    }
}

fn get_ethernet_interfaces() -> Vec<String> {
    let mut ifaces = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/sys/class/net") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("eth") {
                ifaces.push(name);
            }
        }
    }
    ifaces.sort();
    ifaces
}

fn extract_iface_index(path: &str, key: &str) -> Option<usize> {
    if let Some(pos) = path.find(key) {
        let rest = &path[pos + key.len()..];
        let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        num_str.parse().ok()
    } else {
        None
    }
}

async fn read_sysfs(iface: &str, attr: &str) -> String {
    tokio::fs::read_to_string(format!("/sys/class/net/{iface}/{attr}"))
        .await
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

async fn read_sysfs_stat(iface: &str, stat: &str) -> String {
    tokio::fs::read_to_string(format!("/sys/class/net/{iface}/statistics/{stat}"))
        .await
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

pub async fn set(_cfg: &ClientConfig, path: &str, value: &str) -> Result<(), String> {
    if path.ends_with("Enable") {
        let ifaces = get_ethernet_interfaces();
        let idx = extract_iface_index(path, "Interface.")
            .ok_or_else(|| format!("Cannot parse Ethernet index from: {path}"))?;
        if idx == 0 || idx > ifaces.len() {
            return Err(format!("Ethernet.Interface index {idx} out of range"));
        }
        let iface = &ifaces[idx - 1];
        let cmd = if value == "true" || value == "1" {
            "up"
        } else {
            "down"
        };
        std::process::Command::new("ip")
            .args(["link", "set", iface, cmd])
            .status()
            .map_err(|e| format!("Failed to set {iface} {cmd}: {e}"))?;
        return Ok(());
    }
    Err(format!("Read-only Ethernet param: {path}"))
}
