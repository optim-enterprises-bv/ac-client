//! TR-181 Device.Bridging.* — Standard bridge objects.
//!
//! Reads bridge configuration from UCI `network.@device[]` sections with
//! `type='bridge'` and runtime state from sysfs. Coexists with the existing
//! vendor-prefixed `Device.X_OptimACS_Network.Bridge.{i}.` paths in bridge.rs.

use crate::config::ClientConfig;
use std::collections::HashMap;

pub type Params = HashMap<String, String>;

pub async fn get(_cfg: &ClientConfig, path: &str) -> Params {
    let mut m = Params::new();
    let bridges = get_bridges();

    if path == "Device.Bridging."
        || path.starts_with("Device.Bridging.BridgeNumberOfEntries")
    {
        m.insert(
            "Device.Bridging.BridgeNumberOfEntries".to_string(),
            bridges.len().to_string(),
        );
    }

    if path == "Device.Bridging." || path.starts_with("Device.Bridging.Bridge.") {
        let specific_idx = extract_bridge_index(path);
        for (i, br) in bridges.iter().enumerate() {
            let idx = i + 1;
            if let Some(si) = specific_idx {
                if si != idx {
                    continue;
                }
            }
            let base = format!("Device.Bridging.Bridge.{idx}.");
            populate_bridge(&base, br, &mut m).await;
        }
    }

    m
}

struct BridgeInfo {
    name: String,       // e.g. "br-lan"
    uci_section: String, // UCI section name
}

fn get_bridges() -> Vec<BridgeInfo> {
    let mut bridges = Vec::new();

    // Find bridge devices in /sys/class/net/br-*
    if let Ok(entries) = std::fs::read_dir("/sys/class/net") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("br-") {
                // Check if it's actually a bridge
                let brif_path = format!("/sys/class/net/{name}/brif");
                if std::path::Path::new(&brif_path).exists() {
                    let uci_section = name.strip_prefix("br-").unwrap_or(&name).to_string();
                    bridges.push(BridgeInfo {
                        name,
                        uci_section,
                    });
                }
            }
        }
    }

    bridges.sort_by(|a, b| a.name.cmp(&b.name));
    bridges
}

async fn populate_bridge(base: &str, br: &BridgeInfo, m: &mut Params) {
    m.insert(format!("{base}Enable"), "true".to_string());
    m.insert(format!("{base}Status"), "Enabled".to_string());
    m.insert(
        format!("{base}Alias"),
        br.uci_section.clone(),
    );

    // Ports (bridge members)
    let ports = get_bridge_ports(&br.name);
    m.insert(
        format!("{base}PortNumberOfEntries"),
        ports.len().to_string(),
    );

    for (pi, port_name) in ports.iter().enumerate() {
        let pidx = pi + 1;
        let pb = format!("{base}Port.{pidx}.");
        m.insert(format!("{pb}Name"), port_name.clone());
        m.insert(format!("{pb}Enable"), "true".to_string());
        m.insert(format!("{pb}Status"), "Up".to_string());
        m.insert(
            format!("{pb}ManagementPort"),
            "false".to_string(),
        );

        // Port state from sysfs
        let state = tokio::fs::read_to_string(format!(
            "/sys/class/net/{}/brif/{port_name}/state",
            br.name
        ))
        .await
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

        let port_state = match state.as_str() {
            "0" => "Disabled",
            "1" => "Listening",
            "2" => "Learning",
            "3" => "Forwarding",
            "4" => "Blocking",
            _ => "Unknown",
        };
        m.insert(format!("{pb}PortState"), port_state.to_string());
    }
}

fn get_bridge_ports(bridge_name: &str) -> Vec<String> {
    let brif_path = format!("/sys/class/net/{bridge_name}/brif");
    let mut ports = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&brif_path) {
        for entry in entries.flatten() {
            ports.push(entry.file_name().to_string_lossy().to_string());
        }
    }
    ports.sort();
    ports
}

fn extract_bridge_index(path: &str) -> Option<usize> {
    if let Some(pos) = path.find("Bridge.") {
        let rest = &path[pos + 7..];
        let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        num_str.parse().ok()
    } else {
        None
    }
}

pub async fn set(_cfg: &ClientConfig, path: &str, value: &str) -> Result<(), String> {
    use crate::usp::tp469::uci_backend::{uci_commit, uci_set};

    let bridges = get_bridges();
    let idx = extract_bridge_index(path)
        .ok_or_else(|| format!("Cannot parse Bridge index from: {path}"))?;
    if idx == 0 || idx > bridges.len() {
        return Err(format!("Bridge index {idx} out of range"));
    }
    let br = &bridges[idx - 1];

    if path.ends_with("Enable") {
        // Bring bridge up or down via ifconfig
        let cmd = if value == "true" || value == "1" {
            "up"
        } else {
            "down"
        };
        let _ = std::process::Command::new("ip")
            .args(["link", "set", &br.name, cmd])
            .status();
        return Ok(());
    }

    if path.ends_with("Alias") {
        // Alias is informational; map to UCI device name
        uci_set(
            &format!("network.{}.name", br.uci_section),
            value,
        )?;
        uci_commit("network")?;
        return Ok(());
    }

    Err(format!("Read-only Bridging param: {path}"))
}
