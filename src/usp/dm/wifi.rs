//! TR-181 Device.WiFi.* — reads/writes via UCI with multi-SSID support.

use std::collections::HashMap;
use log::{info, warn};
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

/// Get all wireless sections from UCI
fn uci_show_wireless() -> HashMap<String, HashMap<String, String>> {
    let mut sections = HashMap::new();
    
    let out = std::process::Command::new("uci")
        .args(["show", "wireless"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    
    let mut current_section = String::new();
    
    for line in out.lines() {
        if line.starts_with("wireless.") {
            let parts: Vec<&str> = line.split('.').collect();
            if parts.len() >= 3 {
                let section = parts[1].to_string();
                let key = parts[2].split('=').next().unwrap_or("").to_string();
                let value = line.split('=').nth(1).unwrap_or("").trim_matches('\'').to_string();
                
                sections.entry(section)
                    .or_insert_with(HashMap::new)
                    .insert(key, value);
            }
        }
    }
    
    sections
}

/// Get list of wifi-iface sections in order
fn get_wifi_ifaces() -> Vec<String> {
    let mut ifaces = Vec::new();
    
    let out = std::process::Command::new("uci")
        .args(["show", "wireless"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    
    for line in out.lines() {
        if line.starts_with("wireless.") && line.contains(".ssid=") {
            // Extract section name (e.g., "@wifi-iface[0]" or "default_radio0")
            if let Some(section) = line.split('.').nth(1) {
                if !ifaces.contains(&section.to_string()) {
                    ifaces.push(section.to_string());
                }
            }
        }
    }
    
    ifaces
}

/// Get list of wifi-device (radio) sections
fn get_wifi_devices() -> Vec<String> {
    let mut devices = Vec::new();
    
    let out = std::process::Command::new("uci")
        .args(["show", "wireless"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    
    for line in out.lines() {
        if line.starts_with("wireless.") && line.contains(".channel=") {
            if let Some(section) = line.split('.').nth(1) {
                if !devices.contains(&section.to_string()) {
                    devices.push(section.to_string());
                }
            }
        }
    }
    
    devices
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

/// Parse SSID index from path like "Device.WiFi.SSID.2.SSID"
fn parse_ssid_index(path: &str) -> Option<usize> {
    // Find the number after "SSID."
    if let Some(start) = path.find("SSID.") {
        let rest = &path[start + 5..];
        if let Some(end) = rest.find('.') {
            rest[..end].parse().ok()
        } else {
            rest.parse().ok()
        }
    } else {
        None
    }
}

/// Parse Radio index from path like "Device.WiFi.Radio.1.Channel"
fn parse_radio_index(path: &str) -> Option<usize> {
    if let Some(start) = path.find("Radio.") {
        let rest = &path[start + 6..];
        if let Some(end) = rest.find('.') {
            rest[..end].parse().ok()
        } else {
            rest.parse().ok()
        }
    } else {
        None
    }
}

/// Parse AccessPoint index from path like "Device.WiFi.AccessPoint.1.Security.ModeEnabled"
fn parse_ap_index(path: &str) -> Option<usize> {
    if let Some(start) = path.find("AccessPoint.") {
        let rest = &path[start + 12..];
        if let Some(end) = rest.find('.') {
            rest[..end].parse().ok()
        } else {
            rest.parse().ok()
        }
    } else {
        None
    }
}

pub async fn get(_cfg: &ClientConfig, path: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    let ifaces = get_wifi_ifaces();
    let devices = get_wifi_devices();
    
    // Handle SSID requests
    if path.contains("SSID.") || path.ends_with("Device.WiFi.") {
        for (idx, iface) in ifaces.iter().enumerate() {
            let ssid_idx = idx + 1;
            let ssid = uci_get(&format!("wireless.{iface}.ssid"));
            let disabled = uci_get(&format!("wireless.{iface}.disabled"));
            let enable = if disabled == "1" { "false" } else { "true" };
            
            if !ssid.is_empty() {
                m.insert(format!("Device.WiFi.SSID.{ssid_idx}.SSID"), ssid);
                m.insert(format!("Device.WiFi.SSID.{ssid_idx}.Enable"), enable.to_string());
            }
        }
    }
    
    // Handle AccessPoint requests
    if path.contains("AccessPoint.") || path.ends_with("Device.WiFi.") {
        for (idx, iface) in ifaces.iter().enumerate() {
            let ap_idx = idx + 1;
            let enc = uci_get(&format!("wireless.{iface}.encryption"));
            let key = uci_get(&format!("wireless.{iface}.key"));
            let mode = uci_get(&format!("wireless.{iface}.mode"));
            
            m.insert(format!("Device.WiFi.AccessPoint.{ap_idx}.Security.ModeEnabled"), enc);
            if !key.is_empty() {
                m.insert(format!("Device.WiFi.AccessPoint.{ap_idx}.Security.KeyPassphrase"), key);
            }
            if !mode.is_empty() {
                m.insert(format!("Device.WiFi.AccessPoint.{ap_idx}.Mode"), mode);
            }
        }
    }
    
    // Handle Radio requests
    if path.contains("Radio.") || path.ends_with("Device.WiFi.") {
        for (idx, device) in devices.iter().enumerate() {
            let radio_idx = idx + 1;
            let chan = uci_get(&format!("wireless.{device}.channel"));
            let disabled = uci_get(&format!("wireless.{device}.disabled"));
            let band = uci_get(&format!("wireless.{device}.band"));
            let htmode = uci_get(&format!("wireless.{device}.htmode"));
            
            // Enable is inverse of disabled in UCI
            let enable = if disabled == "1" { "false" } else { "true" };
            
            if !chan.is_empty() {
                m.insert(format!("Device.WiFi.Radio.{radio_idx}.Channel"), chan);
            }
            m.insert(format!("Device.WiFi.Radio.{radio_idx}.Enable"), enable.to_string());
            if !band.is_empty() {
                m.insert(format!("Device.WiFi.Radio.{radio_idx}.OperatingFrequencyBand"), band);
            }
            if !htmode.is_empty() {
                m.insert(format!("Device.WiFi.Radio.{radio_idx}.OperatingChannelBandwidth"), htmode);
            }
        }
    }
    
    m
}

pub async fn set(_cfg: &ClientConfig, path: &str, value: &str) -> Result<(), String> {
    let ifaces = get_wifi_ifaces();
    let devices = get_wifi_devices();
    
    // Handle SSID settings
    if path.ends_with(".SSID") {
        if let Some(idx) = parse_ssid_index(path) {
            if idx > 0 && idx <= ifaces.len() {
                let iface = &ifaces[idx - 1];
                uci_set(&format!("wireless.{iface}.ssid"), value)?;
                uci_commit("wireless")?;
                info!("WiFi SSID {idx} set to '{value}' on {iface}");
            } else {
                return Err(format!("SSID index {idx} out of range (max: {})", ifaces.len()));
            }
        }
    } 
    // Handle SSID Enable
    else if path.ends_with(".Enable") && path.contains("SSID.") {
        if let Some(idx) = parse_ssid_index(path) {
            if idx > 0 && idx <= ifaces.len() {
                let iface = &ifaces[idx - 1];
                // Enable: true/false maps to disabled: 0/1 (inverted)
                let disabled = if value == "true" || value == "1" { "0" } else { "1" };
                uci_set(&format!("wireless.{iface}.disabled"), disabled)?;
                uci_commit("wireless")?;
                wifi_reload().await?;
                info!("WiFi SSID {idx} enable set to '{value}' (disabled={disabled})");
            } else {
                return Err(format!("SSID index {idx} out of range"));
            }
        }
    }
    // Handle AccessPoint KeyPassphrase
    else if path.ends_with(".KeyPassphrase") {
        if let Some(idx) = parse_ap_index(path) {
            if idx > 0 && idx <= ifaces.len() {
                let iface = &ifaces[idx - 1];
                uci_set(&format!("wireless.{iface}.key"), value)?;
                uci_commit("wireless")?;
                wifi_reload().await?;
                info!("WiFi AccessPoint {idx} key updated");
            } else {
                return Err(format!("AccessPoint index {idx} out of range"));
            }
        }
    }
    // Handle AccessPoint ModeEnabled (encryption type)
    else if path.ends_with(".ModeEnabled") {
        if let Some(idx) = parse_ap_index(path) {
            if idx > 0 && idx <= ifaces.len() {
                let iface = &ifaces[idx - 1];
                uci_set(&format!("wireless.{iface}.encryption"), value)?;
                uci_commit("wireless")?;
                wifi_reload().await?;
                info!("WiFi AccessPoint {idx} encryption set to '{value}'");
            } else {
                return Err(format!("AccessPoint index {idx} out of range"));
            }
        }
    }
    // Handle Radio Channel
    else if path.ends_with(".Channel") {
        if let Some(idx) = parse_radio_index(path) {
            if idx > 0 && idx <= devices.len() {
                let device = &devices[idx - 1];
                uci_set(&format!("wireless.{device}.channel"), value)?;
                uci_commit("wireless")?;
                wifi_reload().await?;
                info!("WiFi Radio {idx} channel set to '{value}'");
            } else {
                return Err(format!("Radio index {idx} out of range (max: {})", devices.len()));
            }
        }
    }
    // Handle Radio Enable
    else if path.ends_with(".Enable") && path.contains("Radio.") {
        if let Some(idx) = parse_radio_index(path) {
            if idx > 0 && idx <= devices.len() {
                let device = &devices[idx - 1];
                // Enable: true/false maps to disabled: 0/1 (inverted)
                let disabled = if value == "true" || value == "1" { "0" } else { "1" };
                uci_set(&format!("wireless.{device}.disabled"), disabled)?;
                uci_commit("wireless")?;
                wifi_reload().await?;
                info!("WiFi Radio {idx} enable set to '{value}' (disabled={disabled})");
            } else {
                return Err(format!("Radio index {idx} out of range"));
            }
        }
    }
    // Handle OperatingChannelBandwidth (htmode)
    else if path.ends_with(".OperatingChannelBandwidth") {
        if let Some(idx) = parse_radio_index(path) {
            if idx > 0 && idx <= devices.len() {
                let device = &devices[idx - 1];
                uci_set(&format!("wireless.{device}.htmode"), value)?;
                uci_commit("wireless")?;
                wifi_reload().await?;
                info!("WiFi Radio {idx} bandwidth set to '{value}'");
            } else {
                return Err(format!("Radio index {idx} out of range"));
            }
        }
    }
    else {
        warn!("DM SET WiFi: unknown path {path}");
        return Err(format!("Unknown WiFi path: {path}"));
    }
    
    Ok(())
}

/// Reload WiFi configuration
async fn wifi_reload() -> Result<(), String> {
    let status = std::process::Command::new("wifi")
        .status()
        .map_err(|e| e.to_string())?;
    
    if status.success() {
        info!("WiFi configuration reloaded");
        Ok(())
    } else {
        // Try alternative methods
        let status2 = std::process::Command::new("/sbin/wifi")
            .status()
            .map_err(|e| e.to_string())?;
        
        if status2.success() {
            info!("WiFi configuration reloaded (via /sbin/wifi)");
            Ok(())
        } else {
            warn!("WiFi reload failed, changes will apply on reboot");
            Ok(()) // Don't fail the operation
        }
    }
}
