//! TR-181 Device.Hosts.Host.* — reads /etc/hosts.

use std::collections::HashMap;
use crate::config::ClientConfig;

pub async fn get(_cfg: &ClientConfig, _path: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    let content = std::fs::read_to_string("/etc/hosts").unwrap_or_default();
    let mut idx = 1u32;
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') { continue; }
        let mut parts = line.split_whitespace();
        let ip       = parts.next().unwrap_or("");
        let hostname = parts.next().unwrap_or("");
        if !ip.is_empty() && !hostname.is_empty() {
            let base = format!("Device.Hosts.Host.{idx}.");
            m.insert(format!("{base}IPAddress"),  ip.into());
            m.insert(format!("{base}HostName"),   hostname.into());
            idx += 1;
        }
    }
    m
}

pub async fn set(_cfg: &ClientConfig, _path: &str, _value: &str) -> Result<(), String> {
    Err("Device.Hosts.Host.* modification not yet implemented on agent side".into())
}
