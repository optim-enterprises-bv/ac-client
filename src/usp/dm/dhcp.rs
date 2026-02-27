//! TR-181 Device.DHCPv4.Server.Pool.* — reads/writes via UCI.

use std::collections::HashMap;
use crate::config::ClientConfig;

pub async fn get(_cfg: &ClientConfig, _path: &str) -> HashMap<String, String> {
    // Read static leases from UCI dnsmasq config
    let out = std::process::Command::new("uci")
        .args(["show", "dhcp"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    let mut m = HashMap::new();
    let mut idx = 1u32;
    for line in out.lines() {
        if line.contains("host.") && line.contains(".mac=") {
            let mac = line.split('=').nth(1).unwrap_or("").trim_matches('\'').to_string();
            let ip_key = line.replace(".mac=", ".ip=");
            let ip = out.lines()
                .find(|l| l.contains(&ip_key))
                .and_then(|l| l.split('=').nth(1))
                .unwrap_or("")
                .trim_matches('\'')
                .to_string();
            let base = format!("Device.DHCPv4.Server.Pool.1.StaticAddress.{idx}.");
            m.insert(format!("{base}Chaddr"), mac);
            m.insert(format!("{base}Yiaddr"), ip);
            idx += 1;
        }
    }
    m
}

pub async fn set(_cfg: &ClientConfig, _path: &str, _value: &str) -> Result<(), String> {
    // DHCP static lease modification via UCI would require more complex parsing
    Err("DHCPv4 static address modification not yet implemented on agent side".into())
}
