//! TR-181 Device.InterfaceStack.* — maps L2/L3 interface relationships.
//!
//! Builds the interface stack by correlating IP interfaces (from UCI) with
//! their underlying Ethernet/bridge interfaces (from sysfs).

use crate::config::ClientConfig;
use std::collections::HashMap;

pub type Params = HashMap<String, String>;

pub async fn get(_cfg: &ClientConfig, path: &str) -> Params {
    let mut m = Params::new();
    let entries = build_interface_stack();

    if path == "Device.InterfaceStack."
        || path.contains("InterfaceStackNumberOfEntries")
    {
        m.insert(
            "Device.InterfaceStackNumberOfEntries".to_string(),
            entries.len().to_string(),
        );
    }

    if path == "Device.InterfaceStack." || path.starts_with("Device.InterfaceStack.") {
        let specific_idx = extract_index(path);
        for (i, entry) in entries.iter().enumerate() {
            let idx = i + 1;
            if let Some(si) = specific_idx {
                if si != idx {
                    continue;
                }
            }
            let base = format!("Device.InterfaceStack.{idx}.");
            m.insert(
                format!("{base}HigherLayer"),
                entry.higher_layer.clone(),
            );
            m.insert(
                format!("{base}LowerLayer"),
                entry.lower_layer.clone(),
            );
            m.insert(
                format!("{base}HigherAlias"),
                entry.higher_alias.clone(),
            );
            m.insert(
                format!("{base}LowerAlias"),
                entry.lower_alias.clone(),
            );
        }
    }

    m
}

struct StackEntry {
    higher_layer: String, // TR-181 path of the higher-layer interface
    lower_layer: String,  // TR-181 path of the lower-layer interface
    higher_alias: String,
    lower_alias: String,
}

/// Build the interface stack by finding IP interfaces and their underlying links.
fn build_interface_stack() -> Vec<StackEntry> {
    let mut entries = Vec::new();

    // Get UCI network interfaces
    let uci_out = std::process::Command::new("uci")
        .args(["show", "network"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    let mut ip_ifaces = Vec::new();
    for line in uci_out.lines() {
        if line.contains(".proto=") {
            let section = line.split('.').nth(1).unwrap_or("").to_string();
            if !section.is_empty()
                && section != "globals"
                && section != "loopback"
                && !section.starts_with('@')
            {
                if !ip_ifaces.contains(&section) {
                    ip_ifaces.push(section);
                }
            }
        }
    }

    // Get ethernet interfaces
    let mut eth_ifaces = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/sys/class/net") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("eth") {
                eth_ifaces.push(name);
            }
        }
    }
    eth_ifaces.sort();

    // For each IP interface, find its lower-layer link
    for (ip_idx, section) in ip_ifaces.iter().enumerate() {
        let ip_idx_1 = ip_idx + 1;
        let higher = format!("Device.IP.Interface.{ip_idx_1}.");

        // Check if it's a bridge
        let br_name = format!("br-{section}");
        if std::path::Path::new(&format!("/sys/class/net/{br_name}")).exists() {
            // IP -> Bridge
            let br_idx = find_bridge_index(&br_name);
            if br_idx > 0 {
                entries.push(StackEntry {
                    higher_layer: higher.clone(),
                    lower_layer: format!("Device.Bridging.Bridge.{br_idx}."),
                    higher_alias: section.clone(),
                    lower_alias: br_name.clone(),
                });
            }

            // Bridge -> Ethernet ports
            let brif_path = format!("/sys/class/net/{br_name}/brif");
            if let Ok(ports) = std::fs::read_dir(&brif_path) {
                for port in ports.flatten() {
                    let port_name = port.file_name().to_string_lossy().to_string();
                    if let Some(eth_idx) = eth_ifaces.iter().position(|e| e == &port_name) {
                        entries.push(StackEntry {
                            higher_layer: format!("Device.Bridging.Bridge.{br_idx}."),
                            lower_layer: format!("Device.Ethernet.Interface.{}.", eth_idx + 1),
                            higher_alias: br_name.clone(),
                            lower_alias: port_name,
                        });
                    }
                }
            }
        } else {
            // Direct IP -> Ethernet (e.g., WAN on eth1)
            let os_iface = resolve_os_iface(section);
            if let Some(eth_idx) = eth_ifaces.iter().position(|e| e == &os_iface) {
                entries.push(StackEntry {
                    higher_layer: higher,
                    lower_layer: format!("Device.Ethernet.Interface.{}.", eth_idx + 1),
                    higher_alias: section.clone(),
                    lower_alias: os_iface,
                });
            }
        }
    }

    entries
}

fn find_bridge_index(br_name: &str) -> usize {
    let mut bridges: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/sys/class/net") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("br-") {
                let brif = format!("/sys/class/net/{name}/brif");
                if std::path::Path::new(&brif).exists() {
                    bridges.push(name);
                }
            }
        }
    }
    bridges.sort();
    bridges.iter().position(|b| b == br_name).map(|i| i + 1).unwrap_or(0)
}

fn resolve_os_iface(section: &str) -> String {
    let bridge = format!("br-{section}");
    if std::path::Path::new(&format!("/sys/class/net/{bridge}")).exists() {
        return bridge;
    }
    if std::path::Path::new(&format!("/sys/class/net/{section}")).exists() {
        return section.to_string();
    }
    let pppoe = format!("pppoe-{section}");
    if std::path::Path::new(&format!("/sys/class/net/{pppoe}")).exists() {
        return pppoe;
    }
    section.to_string()
}

fn extract_index(path: &str) -> Option<usize> {
    if let Some(pos) = path.find("InterfaceStack.") {
        let rest = &path[pos + 15..];
        let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        num_str.parse().ok()
    } else {
        None
    }
}

pub async fn set(_cfg: &ClientConfig, path: &str, _value: &str) -> Result<(), String> {
    Err(format!("Device.InterfaceStack is read-only: {path}"))
}
