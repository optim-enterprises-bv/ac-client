//! TR-181 Device.NAT.* — NAT interface settings and port mappings.
//!
//! Reads masquerading state from UCI firewall zones and port forwarding
//! rules from UCI `firewall.@redirect[]` sections. Supports ADD/DELETE
//! for PortMapping instances.

use crate::config::ClientConfig;
use crate::usp::tp469::uci_backend::{uci_commit, uci_set};

use std::collections::HashMap;

pub type Params = HashMap<String, String>;

pub async fn get(_cfg: &ClientConfig, path: &str) -> Params {
    let mut m = Params::new();

    if path == "Device.NAT." || path.contains("InterfaceSettingNumberOfEntries") {
        let zones = get_masq_zones();
        m.insert(
            "Device.NAT.InterfaceSettingNumberOfEntries".to_string(),
            zones.len().to_string(),
        );
    }

    if path == "Device.NAT." || path.starts_with("Device.NAT.InterfaceSetting.") {
        let zones = get_masq_zones();
        for (i, zone) in zones.iter().enumerate() {
            let idx = i + 1;
            let base = format!("Device.NAT.InterfaceSetting.{idx}.");
            m.insert(format!("{base}Enable"), zone.masq.to_string());
            m.insert(
                format!("{base}Status"),
                if zone.masq { "Enabled" } else { "Disabled" }.to_string(),
            );
            m.insert(format!("{base}Alias"), zone.name.clone());
            m.insert(
                format!("{base}Interface"),
                format!("Device.IP.Interface.{}", zone.name),
            );
        }
    }

    if path == "Device.NAT." || path.contains("PortMappingNumberOfEntries") {
        let mappings = get_port_mappings();
        m.insert(
            "Device.NAT.PortMappingNumberOfEntries".to_string(),
            mappings.len().to_string(),
        );
    }

    // DMZ settings
    if path == "Device.NAT." || path.contains("DMZEnable") {
        let dmz = std::process::Command::new("uci")
            .args(["get", "firewall.dmz.enabled"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim() == "1" || s.trim() == "true")
            .unwrap_or(false);
        m.insert("Device.NAT.DMZEnable".to_string(), dmz.to_string());
    }
    if path == "Device.NAT." || path.contains("DMZHost") {
        let host = std::process::Command::new("uci")
            .args(["get", "firewall.dmz.dest_ip"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        m.insert("Device.NAT.DMZHost".to_string(), host);
    }

    if path == "Device.NAT." || path.starts_with("Device.NAT.PortMapping.") {
        let mappings = get_port_mappings();
        let specific_idx = extract_index(path, "PortMapping.");
        for (i, pm) in mappings.iter().enumerate() {
            let idx = i + 1;
            if let Some(si) = specific_idx {
                if si != idx {
                    continue;
                }
            }
            let base = format!("Device.NAT.PortMapping.{idx}.");
            m.insert(format!("{base}Enable"), pm.enabled.to_string());
            m.insert(format!("{base}Status"), "Enabled".to_string());
            m.insert(format!("{base}Protocol"), pm.proto.clone());
            m.insert(
                format!("{base}ExternalPort"),
                pm.src_dport.clone(),
            );
            m.insert(format!("{base}InternalPort"), pm.dest_port.clone());
            m.insert(
                format!("{base}InternalClient"),
                pm.dest_ip.clone(),
            );
            m.insert(format!("{base}Description"), pm.name.clone());
            m.insert(format!("{base}RemoteHost"), pm.src_ip.clone());
        }
    }

    m
}

pub async fn set(_cfg: &ClientConfig, path: &str, value: &str) -> Result<(), String> {
    let mappings = get_port_mappings();
    let idx = extract_index(path, "PortMapping.")
        .ok_or_else(|| format!("Cannot parse PortMapping index from: {path}"))?;
    if idx == 0 || idx > mappings.len() {
        return Err(format!("PortMapping index {idx} out of range"));
    }

    let uci_idx = mappings[idx - 1].uci_index;
    let section = format!("firewall.@redirect[{uci_idx}]");

    if path.ends_with("Enable") {
        uci_set(&format!("{section}.enabled"), value)?;
    } else if path.ends_with("Protocol") {
        uci_set(&format!("{section}.proto"), value)?;
    } else if path.ends_with("ExternalPort") {
        uci_set(&format!("{section}.src_dport"), value)?;
    } else if path.ends_with("InternalPort") {
        uci_set(&format!("{section}.dest_port"), value)?;
    } else if path.ends_with("InternalClient") {
        uci_set(&format!("{section}.dest_ip"), value)?;
    } else if path.ends_with("Description") {
        uci_set(&format!("{section}.name"), value)?;
    } else if path.ends_with("RemoteHost") {
        uci_set(&format!("{section}.src_ip"), value)?;
    } else {
        return Err(format!("Unknown or read-only NAT param: {path}"));
    }

    uci_commit("firewall")?;
    restart_firewall();
    Ok(())
}

// ── Data types ───────────────────────────────────────────────────────────────

struct MasqZone {
    name: String,
    masq: bool,
}

struct PortMapping {
    uci_index: usize,
    name: String,
    enabled: bool,
    proto: String,
    src_dport: String,
    dest_port: String,
    dest_ip: String,
    src_ip: String,
}

// ── Data retrieval ───────────────────────────────────────────────────────────

fn get_masq_zones() -> Vec<MasqZone> {
    let output = std::process::Command::new("uci")
        .args(["show", "firewall"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    let mut zones = Vec::new();
    let mut current_zone = String::new();
    let mut current_masq = false;

    for line in output.lines() {
        if line.contains("@zone[") && line.contains(".name=") {
            // Save previous zone
            if !current_zone.is_empty() {
                zones.push(MasqZone {
                    name: current_zone.clone(),
                    masq: current_masq,
                });
            }
            current_zone = line
                .split('=')
                .nth(1)
                .unwrap_or("")
                .trim_matches('\'')
                .to_string();
            current_masq = false;
        } else if line.contains("@zone[") && line.contains(".masq=") {
            let val = line.split('=').nth(1).unwrap_or("").trim_matches('\'');
            current_masq = val == "1" || val == "true";
        }
    }
    if !current_zone.is_empty() {
        zones.push(MasqZone {
            name: current_zone,
            masq: current_masq,
        });
    }

    zones
}

fn get_port_mappings() -> Vec<PortMapping> {
    let output = std::process::Command::new("uci")
        .args(["show", "firewall"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    // Collect redirect entries keyed by UCI index to avoid duplicates.
    // `uci show firewall` emits one line per option:
    //   firewall.@redirect[0].name='...'
    //   firewall.@redirect[0].proto='tcp'
    //   firewall.@redirect[1].name='...'
    use std::collections::BTreeMap;
    let mut by_idx: BTreeMap<usize, PortMapping> = BTreeMap::new();

    for line in output.lines() {
        if !line.contains("@redirect[") {
            continue;
        }
        let start = match line.find("@redirect[") {
            Some(s) => s + 10,
            None => continue,
        };
        let rest = &line[start..];
        let end = match rest.find(']') {
            Some(e) => e,
            None => continue,
        };
        let idx: usize = match rest[..end].parse() {
            Ok(i) => i,
            Err(_) => continue,
        };

        // Extract key (last dot-segment before '=') and value
        let key_val: Vec<&str> = line.splitn(2, '=').collect();
        if key_val.len() != 2 {
            continue;
        }
        let key = key_val[0].rsplit('.').next().unwrap_or("");
        let val = key_val[1].trim_matches('\'').to_string();

        let pm = by_idx.entry(idx).or_insert_with(|| PortMapping {
            uci_index: idx,
            name: String::new(),
            enabled: true,
            proto: "tcp".to_string(),
            src_dport: String::new(),
            dest_port: String::new(),
            dest_ip: String::new(),
            src_ip: String::new(),
        });

        match key {
            "name" => pm.name = val,
            "enabled" => pm.enabled = val != "0" && val != "false",
            "proto" => pm.proto = val,
            "src_dport" => pm.src_dport = val,
            "dest_port" => pm.dest_port = val,
            "dest_ip" => pm.dest_ip = val,
            "src_ip" => pm.src_ip = val,
            _ => {}
        }
    }

    by_idx.into_values().collect()
}

fn extract_index(path: &str, key: &str) -> Option<usize> {
    if let Some(pos) = path.find(key) {
        let rest = &path[pos + key.len()..];
        let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        num_str.parse().ok()
    } else {
        None
    }
}

fn restart_firewall() {
    let _ = std::process::Command::new("/etc/init.d/firewall")
        .arg("reload")
        .status();
}
