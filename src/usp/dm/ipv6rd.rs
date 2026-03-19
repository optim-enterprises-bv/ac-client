//! TR-181 Device.IPv6rd.* — 6rd tunnel configuration.
//!
//! Reads 6rd tunnel config from UCI interfaces with proto=6rd.

use crate::config::ClientConfig;
use std::collections::HashMap;

pub type Params = HashMap<String, String>;

pub async fn get(_cfg: &ClientConfig, path: &str) -> Params {
    let mut m = Params::new();
    let tunnels = get_6rd_tunnels();

    if path == "Device.IPv6rd." || path.contains("InterfaceSettingNumberOfEntries") {
        m.insert(
            "Device.IPv6rd.Enable".to_string(),
            (!tunnels.is_empty()).to_string(),
        );
        m.insert(
            "Device.IPv6rd.InterfaceSettingNumberOfEntries".to_string(),
            tunnels.len().to_string(),
        );
    }

    if path == "Device.IPv6rd." || path.starts_with("Device.IPv6rd.InterfaceSetting.") {
        for (i, t) in tunnels.iter().enumerate() {
            let idx = i + 1;
            let base = format!("Device.IPv6rd.InterfaceSetting.{idx}.");
            m.insert(format!("{base}Enable"), "true".to_string());
            m.insert(format!("{base}Status"), "Enabled".to_string());
            m.insert(format!("{base}Alias"), t.section.clone());
            m.insert(format!("{base}BorderRelayIPv4Addresses"), t.peeraddr.clone());
            m.insert(format!("{base}SPIPv6Prefix"), t.ip6prefix.clone());
            m.insert(format!("{base}IPv4MaskLength"), t.ip4prefixlen.clone());
        }
    }

    m
}

struct Tunnel6rd {
    section: String,
    peeraddr: String,
    ip6prefix: String,
    ip4prefixlen: String,
}

fn get_6rd_tunnels() -> Vec<Tunnel6rd> {
    let output = std::process::Command::new("uci")
        .args(["show", "network"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    let mut tunnels = Vec::new();
    let mut current = String::new();
    let mut is_6rd = false;
    let mut peeraddr = String::new();
    let mut ip6prefix = String::new();
    let mut ip4prefixlen = String::new();

    for line in output.lines() {
        if line.contains(".proto=") {
            if !current.is_empty() && is_6rd {
                tunnels.push(Tunnel6rd {
                    section: current.clone(),
                    peeraddr: peeraddr.clone(),
                    ip6prefix: ip6prefix.clone(),
                    ip4prefixlen: ip4prefixlen.clone(),
                });
            }
            current = line.split('.').nth(1).unwrap_or("").to_string();
            let val = line.split('=').nth(1).unwrap_or("").trim_matches('\'');
            is_6rd = val == "6rd";
            peeraddr.clear();
            ip6prefix.clear();
            ip4prefixlen.clear();
        }
        if is_6rd {
            if line.contains(".peeraddr=") {
                peeraddr = line.split('=').nth(1).unwrap_or("").trim_matches('\'').to_string();
            } else if line.contains(".ip6prefix=") {
                ip6prefix = line.split('=').nth(1).unwrap_or("").trim_matches('\'').to_string();
            } else if line.contains(".ip4prefixlen=") {
                ip4prefixlen = line.split('=').nth(1).unwrap_or("").trim_matches('\'').to_string();
            }
        }
    }
    if !current.is_empty() && is_6rd {
        tunnels.push(Tunnel6rd {
            section: current,
            peeraddr,
            ip6prefix,
            ip4prefixlen,
        });
    }

    tunnels
}

pub async fn set(_cfg: &ClientConfig, path: &str, _value: &str) -> Result<(), String> {
    Err(format!("Device.IPv6rd is read-only: {path}"))
}
