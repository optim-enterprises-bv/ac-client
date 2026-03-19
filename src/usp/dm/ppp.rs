//! TR-181 Device.PPP.Interface.* — PPP/PPPoE interface objects.
//!
//! Reads PPPoE configuration from UCI interfaces with `proto=pppoe`
//! and runtime state from ubus.

use crate::config::ClientConfig;
use crate::usp::tp469::uci_backend::{uci_commit, uci_get, uci_set};
use std::collections::HashMap;

pub type Params = HashMap<String, String>;

pub async fn get(_cfg: &ClientConfig, path: &str) -> Params {
    let mut m = Params::new();
    let ifaces = get_ppp_interfaces();

    if path == "Device.PPP." || path.contains("InterfaceNumberOfEntries") {
        m.insert(
            "Device.PPP.InterfaceNumberOfEntries".to_string(),
            ifaces.len().to_string(),
        );
    }

    if path == "Device.PPP." || path.starts_with("Device.PPP.Interface.") {
        let specific_idx = extract_index(path);
        for (i, iface) in ifaces.iter().enumerate() {
            let idx = i + 1;
            if let Some(si) = specific_idx {
                if si != idx {
                    continue;
                }
            }
            let base = format!("Device.PPP.Interface.{idx}.");
            populate_ppp_interface(&base, iface, &mut m);
        }
    }

    m
}

pub async fn set(_cfg: &ClientConfig, path: &str, value: &str) -> Result<(), String> {
    let ifaces = get_ppp_interfaces();
    let idx = extract_index(path)
        .ok_or_else(|| format!("Cannot parse PPP.Interface index from: {path}"))?;
    if idx == 0 || idx > ifaces.len() {
        return Err(format!("PPP.Interface index {idx} out of range"));
    }
    let section = &ifaces[idx - 1].uci_section;

    if path.ends_with("Username") {
        uci_set(&format!("network.{section}.username"), value)?;
        uci_commit("network")?;
        return Ok(());
    } else if path.ends_with("Password") {
        uci_set(&format!("network.{section}.password"), value)?;
        uci_commit("network")?;
        return Ok(());
    }

    Err(format!("Read-only PPP param: {path}"))
}

struct PppInterface {
    uci_section: String,
    username: String,
    status: String,
    ip_address: String,
}

fn get_ppp_interfaces() -> Vec<PppInterface> {
    let uci_out = std::process::Command::new("uci")
        .args(["show", "network"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    let mut sections = Vec::new();
    for line in uci_out.lines() {
        if line.contains(".proto=") {
            let val = line.split('=').nth(1).unwrap_or("").trim_matches('\'');
            if val == "pppoe" || val == "pptp" || val == "l2tp" {
                let section = line.split('.').nth(1).unwrap_or("").to_string();
                if !section.is_empty() && !sections.contains(&section) {
                    sections.push(section);
                }
            }
        }
    }

    let mut ifaces = Vec::new();
    for section in sections {
        let username = uci_get(&format!("network.{section}.username"));

        // Get runtime status from ubus
        let ubus_out = std::process::Command::new("ubus")
            .args(["call", &format!("network.interface.{section}"), "status"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .unwrap_or_default();

        let status = if ubus_out.contains("\"up\": true") || ubus_out.contains("\"up\":true") {
            "Up".to_string()
        } else {
            "Down".to_string()
        };

        let mut ip_address = String::new();
        if let Some(pos) = ubus_out.find("\"ipv4-address\"") {
            let chunk = &ubus_out[pos..];
            if let Some(addr_pos) = chunk.find("\"address\"") {
                let after = &chunk[addr_pos + 9..];
                if let Some(start) = after.find('"') {
                    let rest = &after[start + 1..];
                    if let Some(end) = rest.find('"') {
                        ip_address = rest[..end].to_string();
                    }
                }
            }
        }

        ifaces.push(PppInterface {
            uci_section: section,
            username,
            status,
            ip_address,
        });
    }

    ifaces
}

fn populate_ppp_interface(base: &str, iface: &PppInterface, m: &mut Params) {
    m.insert(format!("{base}Enable"), "true".to_string());
    m.insert(format!("{base}Status"), iface.status.clone());
    m.insert(
        format!("{base}Name"),
        format!("pppoe-{}", iface.uci_section),
    );
    m.insert(format!("{base}Username"), iface.username.clone());
    m.insert(
        format!("{base}ConnectionStatus"),
        if iface.status == "Up" {
            "Connected"
        } else {
            "Disconnected"
        }
        .to_string(),
    );
    if !iface.ip_address.is_empty() {
        m.insert(
            format!("{base}IPCPLocalIPAddress"),
            iface.ip_address.clone(),
        );
    }
}

fn extract_index(path: &str) -> Option<usize> {
    if let Some(pos) = path.find("Interface.") {
        let rest = &path[pos + 10..];
        let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        num_str.parse().ok()
    } else {
        None
    }
}
