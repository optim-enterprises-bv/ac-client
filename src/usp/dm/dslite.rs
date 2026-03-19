//! TR-181 Device.DSLite.* — DS-Lite tunnel configuration.
//!
//! Reads DS-Lite tunnel config from UCI interfaces with proto=dslite.

use crate::config::ClientConfig;
use std::collections::HashMap;

pub type Params = HashMap<String, String>;

pub async fn get(_cfg: &ClientConfig, path: &str) -> Params {
    let mut m = Params::new();
    let tunnels = get_dslite_tunnels();

    if path == "Device.DSLite." || path.contains("InterfaceSettingNumberOfEntries") {
        m.insert(
            "Device.DSLite.Enable".to_string(),
            (!tunnels.is_empty()).to_string(),
        );
        m.insert(
            "Device.DSLite.InterfaceSettingNumberOfEntries".to_string(),
            tunnels.len().to_string(),
        );
    }

    if path == "Device.DSLite." || path.starts_with("Device.DSLite.InterfaceSetting.") {
        for (i, t) in tunnels.iter().enumerate() {
            let idx = i + 1;
            let base = format!("Device.DSLite.InterfaceSetting.{idx}.");
            m.insert(format!("{base}Enable"), "true".to_string());
            m.insert(format!("{base}Status"), "Enabled".to_string());
            m.insert(format!("{base}Alias"), t.section.clone());
            m.insert(
                format!("{base}EndpointAssignmentPrecedence"),
                "DHCPv6".to_string(),
            );
            m.insert(
                format!("{base}EndpointAddress"),
                t.peeraddr.clone(),
            );
            m.insert(format!("{base}Origin"), "Static".to_string());
        }
    }

    m
}

struct DsliteTunnel {
    section: String,
    peeraddr: String,
}

fn get_dslite_tunnels() -> Vec<DsliteTunnel> {
    let output = std::process::Command::new("uci")
        .args(["show", "network"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    let mut tunnels = Vec::new();
    let mut current = String::new();
    let mut is_dslite = false;
    let mut peeraddr = String::new();

    for line in output.lines() {
        if line.contains(".proto=") {
            if !current.is_empty() && is_dslite {
                tunnels.push(DsliteTunnel {
                    section: current.clone(),
                    peeraddr: peeraddr.clone(),
                });
            }
            current = line.split('.').nth(1).unwrap_or("").to_string();
            let val = line.split('=').nth(1).unwrap_or("").trim_matches('\'');
            is_dslite = val == "dslite";
            peeraddr.clear();
        }
        if is_dslite && line.contains(".peeraddr=") {
            peeraddr = line.split('=').nth(1).unwrap_or("").trim_matches('\'').to_string();
        }
    }
    if !current.is_empty() && is_dslite {
        tunnels.push(DsliteTunnel {
            section: current,
            peeraddr,
        });
    }

    tunnels
}

pub async fn set(_cfg: &ClientConfig, path: &str, _value: &str) -> Result<(), String> {
    Err(format!("Device.DSLite is read-only: {path}"))
}
