//! TR-181 Device.WiFi.* — reads/writes via UCI.

use std::collections::HashMap;
use log::warn;
use crate::config::ClientConfig;

/// Run a UCI command and return stdout.
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

fn uci_commit(pkg: &str) -> Result<(), String> {
    std::process::Command::new("uci")
        .args(["commit", pkg])
        .status()
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub async fn get(_cfg: &ClientConfig, path: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    // Read SSID from UCI wireless config
    if path.contains("SSID.") || path.ends_with("Device.WiFi.") {
        let ssid = uci_get("wireless.@wifi-iface[0].ssid");
        if !ssid.is_empty() {
            m.insert("Device.WiFi.SSID.1.SSID".into(), ssid);
        }
    }
    if path.contains("AccessPoint.") || path.ends_with("Device.WiFi.") {
        let enc = uci_get("wireless.@wifi-iface[0].encryption");
        let key = uci_get("wireless.@wifi-iface[0].key");
        m.insert("Device.WiFi.AccessPoint.1.Security.ModeEnabled".into(), enc);
        m.insert("Device.WiFi.AccessPoint.1.Security.KeyPassphrase".into(), key);
    }
    if path.contains("Radio.") || path.ends_with("Device.WiFi.") {
        let chan = uci_get("wireless.radio0.channel");
        m.insert("Device.WiFi.Radio.1.Channel".into(), chan);
    }
    m
}

pub async fn set(_cfg: &ClientConfig, path: &str, value: &str) -> Result<(), String> {
    if path.ends_with(".SSID") {
        uci_set("wireless.@wifi-iface[0].ssid", value)?;
        uci_commit("wireless")?;
    } else if path.ends_with(".KeyPassphrase") {
        uci_set("wireless.@wifi-iface[0].key", value)?;
        uci_commit("wireless")?;
    } else if path.ends_with(".ModeEnabled") {
        uci_set("wireless.@wifi-iface[0].encryption", value)?;
        uci_commit("wireless")?;
    } else if path.ends_with(".Channel") {
        uci_set("wireless.radio0.channel", value)?;
        uci_commit("wireless")?;
    } else {
        warn!("DM SET WiFi: unknown path {path}");
    }
    Ok(())
}
