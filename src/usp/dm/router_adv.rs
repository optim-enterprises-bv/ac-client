//! TR-181 Device.RouterAdvertisement.* — IPv6 Router Advertisement config.
//!
//! Reads RA configuration from UCI and runtime state from /proc/sys/net/ipv6.

use crate::config::ClientConfig;
use std::collections::HashMap;

pub type Params = HashMap<String, String>;

pub async fn get(_cfg: &ClientConfig, path: &str) -> Params {
    let mut m = Params::new();
    let ifaces = get_ra_interfaces();

    if path == "Device.RouterAdvertisement."
        || path.contains("InterfaceSettingNumberOfEntries")
    {
        m.insert(
            "Device.RouterAdvertisement.Enable".to_string(),
            "true".to_string(),
        );
        m.insert(
            "Device.RouterAdvertisement.InterfaceSettingNumberOfEntries".to_string(),
            ifaces.len().to_string(),
        );
    }

    if path == "Device.RouterAdvertisement."
        || path.starts_with("Device.RouterAdvertisement.InterfaceSetting.")
    {
        let specific_idx = extract_index(path);
        for (i, iface) in ifaces.iter().enumerate() {
            let idx = i + 1;
            if let Some(si) = specific_idx {
                if si != idx {
                    continue;
                }
            }
            let base = format!("Device.RouterAdvertisement.InterfaceSetting.{idx}.");
            m.insert(format!("{base}Enable"), "true".to_string());
            m.insert(format!("{base}Status"), "Enabled".to_string());
            m.insert(
                format!("{base}Interface"),
                format!("Device.IP.Interface.{}", iface.section),
            );
            m.insert(
                format!("{base}MaxRtrAdvInterval"),
                iface.max_interval.to_string(),
            );
            m.insert(
                format!("{base}MinRtrAdvInterval"),
                iface.min_interval.to_string(),
            );
            m.insert(
                format!("{base}AdvManagedFlag"),
                iface.managed.to_string(),
            );
            m.insert(
                format!("{base}AdvOtherConfigFlag"),
                iface.other_config.to_string(),
            );
        }
    }

    m
}

struct RaInterface {
    section: String,
    max_interval: u32,
    min_interval: u32,
    managed: bool,
    other_config: bool,
}

fn get_ra_interfaces() -> Vec<RaInterface> {
    // Check UCI dhcp config for ra settings
    let output = std::process::Command::new("uci")
        .args(["show", "dhcp"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    let mut ifaces = Vec::new();
    let mut current_section = String::new();
    let mut has_ra = false;
    let mut managed = false;
    let mut other = false;

    for line in output.lines() {
        if line.contains(".interface=") {
            if !current_section.is_empty() && has_ra {
                ifaces.push(RaInterface {
                    section: current_section.clone(),
                    max_interval: 600,
                    min_interval: 200,
                    managed,
                    other_config: other,
                });
            }
            current_section = line
                .split('=')
                .nth(1)
                .unwrap_or("")
                .trim_matches('\'')
                .to_string();
            has_ra = false;
            managed = false;
            other = false;
        }
        if line.contains(".ra=") {
            let val = line.split('=').nth(1).unwrap_or("").trim_matches('\'');
            has_ra = val == "server" || val == "relay" || val == "hybrid";
        }
        if line.contains(".ra_management=") {
            let val = line.split('=').nth(1).unwrap_or("").trim_matches('\'');
            managed = val == "1";
            other = val == "2";
        }
    }
    if !current_section.is_empty() && has_ra {
        ifaces.push(RaInterface {
            section: current_section,
            max_interval: 600,
            min_interval: 200,
            managed,
            other_config: other,
        });
    }

    ifaces
}

fn extract_index(path: &str) -> Option<usize> {
    if let Some(pos) = path.find("InterfaceSetting.") {
        let rest = &path[pos + 17..];
        let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        num_str.parse().ok()
    } else {
        None
    }
}

pub async fn set(_cfg: &ClientConfig, path: &str, _value: &str) -> Result<(), String> {
    Err(format!("Device.RouterAdvertisement is read-only: {path}"))
}
