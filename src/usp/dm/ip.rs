//! TR-181 Device.IP.Interface.* — reads/writes via UCI.

use std::collections::HashMap;
use crate::config::ClientConfig;

fn uci_get(path: &str) -> String {
    std::process::Command::new("uci")
        .args(["get", path])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn uci_set(path: &str, value: &str) -> Result<(), String> {
    let status = std::process::Command::new("uci")
        .args(["set", &format!("{path}={value}")])
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() { Ok(()) } else { Err(format!("uci set {path} failed")) }
}

pub async fn get(_cfg: &ClientConfig, path: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    let prefix = "Device.IP.Interface.1.IPv4Address.1.";
    if path.starts_with(prefix) || path == "Device.IP.Interface." || path == "Device.IP.Interface.1." {
        let ip   = uci_get("network.lan.ipaddr");
        let mask = uci_get("network.lan.netmask");
        let proto = uci_get("network.lan.proto");
        m.insert(format!("{prefix}IPAddress"),      ip);
        m.insert(format!("{prefix}SubnetMask"),     mask);
        m.insert(format!("{prefix}AddressingType"), proto);
    }
    m
}

pub async fn set(_cfg: &ClientConfig, path: &str, value: &str) -> Result<(), String> {
    if path.ends_with(".IPAddress") {
        uci_set("network.lan.ipaddr", value)?;
    } else if path.ends_with(".SubnetMask") {
        uci_set("network.lan.netmask", value)?;
    } else if path.ends_with(".AddressingType") {
        uci_set("network.lan.proto", value)?;
    }
    let _ = std::process::Command::new("uci").args(["commit", "network"]).status();
    Ok(())
}
