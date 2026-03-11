//! TR-181 Device.WiFi.* — reads/writes via UCI with multi-SSID support.

use std::collections::HashMap;
use log::{info, warn};
use crate::config::ClientConfig;
use crate::usp::tp469::uci_backend::{uci_get, uci_set, uci_commit};

/// Get all wireless sections from UCI
fn uci_show_wireless() -> HashMap<String, HashMap<String, String>> {
    let mut sections = HashMap::new();
    
    let out = std::process::Command::new("uci")
        .args(["show", "wireless"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    
    let _current_section = String::new();
    
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
            let txpower = uci_get(&format!("wireless.{device}.txpower"));
            let beacon_int = uci_get(&format!("wireless.{device}.beacon_int"));
            let dtim_period = uci_get(&format!("wireless.{device}.dtim_period"));

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
            if !txpower.is_empty() {
                m.insert(format!("Device.WiFi.Radio.{radio_idx}.TransmitPower"), txpower);
            }
            if !beacon_int.is_empty() {
                m.insert(format!("Device.WiFi.Radio.{radio_idx}.BeaconPeriod"), beacon_int);
            }
            if !dtim_period.is_empty() {
                m.insert(format!("Device.WiFi.Radio.{radio_idx}.DTIMPeriod"), dtim_period);
            }

            // Get BSSID and bitrate from iw dev for this radio's interface
            let phy_iface = get_phy_interface(device);
            if !phy_iface.is_empty() {
                let bssid = get_iw_bssid(&phy_iface);
                if !bssid.is_empty() {
                    m.insert(format!("Device.WiFi.Radio.{radio_idx}.X_OptimACS_BSSID"), bssid);
                }
                let bitrate = get_iw_bitrate(&phy_iface);
                if !bitrate.is_empty() {
                    m.insert(format!("Device.WiFi.Radio.{radio_idx}.X_OptimACS_Bitrate"), bitrate);
                }
                let assoc_count = get_associated_device_count(&phy_iface);
                m.insert(format!("Device.WiFi.Radio.{radio_idx}.AssociatedDeviceNumberOfEntries"), assoc_count.to_string());
            }
        }
    }

    // Handle AccessPoint extra params (IsolationEnable, MaxAssociatedDevices, WMMEnable)
    if path.contains("AccessPoint.") || path.ends_with("Device.WiFi.") {
        for (idx, iface) in ifaces.iter().enumerate() {
            let ap_idx = idx + 1;
            let maxassoc = uci_get(&format!("wireless.{iface}.maxassoc"));
            let wmm = uci_get(&format!("wireless.{iface}.wmm"));
            let isolate = uci_get(&format!("wireless.{iface}.isolate"));

            if !maxassoc.is_empty() {
                m.insert(format!("Device.WiFi.AccessPoint.{ap_idx}.MaxAssociatedDevices"), maxassoc);
            }
            let wmm_enabled = wmm != "0";
            m.insert(format!("Device.WiFi.AccessPoint.{ap_idx}.WMMEnable"), wmm_enabled.to_string());
            let isolation = isolate == "1";
            m.insert(format!("Device.WiFi.AccessPoint.{ap_idx}.IsolationEnable"), isolation.to_string());
        }
    }

    m
}

/// Get the wireless interface name for a radio device (e.g. radio0 -> phy0-ap0 or wlan0)
fn get_phy_interface(device: &str) -> String {
    // Try to find the interface from iw dev output
    let output = std::process::Command::new("iw")
        .arg("dev")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    // Look for interface belonging to the phy matching this radio
    // radio0 -> phy0, radio1 -> phy1, etc.
    let phy_name = device.replace("radio", "phy");
    let mut in_phy = false;
    let mut iface_name = String::new();

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("phy#") {
            let phy_num = trimmed.trim_start_matches("phy#");
            let expected_num = phy_name.trim_start_matches("phy");
            in_phy = phy_num == expected_num;
        }
        if in_phy && trimmed.starts_with("Interface ") {
            iface_name = trimmed.trim_start_matches("Interface ").trim().to_string();
            break;
        }
    }

    // Fallback: try common naming patterns
    if iface_name.is_empty() {
        let idx = device.trim_start_matches("radio").parse::<usize>().unwrap_or(0);
        for candidate in &[
            format!("phy{idx}-ap0"),
            format!("wlan{idx}"),
        ] {
            if std::path::Path::new(&format!("/sys/class/net/{candidate}")).exists() {
                return candidate.clone();
            }
        }
    }

    iface_name
}

/// Get BSSID from `iw dev <iface> info`
fn get_iw_bssid(iface: &str) -> String {
    let output = std::process::Command::new("iw")
        .args(["dev", iface, "info"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("addr ") {
            return trimmed.trim_start_matches("addr ").trim().to_string();
        }
    }
    String::new()
}

/// Get TX bitrate from `iw dev <iface> link`
fn get_iw_bitrate(iface: &str) -> String {
    let output = std::process::Command::new("iw")
        .args(["dev", iface, "link"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("tx bitrate:") {
            return trimmed.trim_start_matches("tx bitrate:").trim().to_string();
        }
    }
    String::new()
}

/// Count associated devices (stations) via `iw dev <iface> station dump`
fn get_associated_device_count(iface: &str) -> usize {
    let output = std::process::Command::new("iw")
        .args(["dev", iface, "station", "dump"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    output.lines().filter(|l| l.starts_with("Station ")).count()
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
    // Handle SSID Advertisement (hidden SSID)
    else if path.ends_with(".SSIDAdvertisementEnabled") {
        if let Some(idx) = parse_ssid_index(path) {
            if idx > 0 && idx <= ifaces.len() {
                let iface = &ifaces[idx - 1];
                // SSIDAdvertisementEnabled: true = visible (hidden=0), false = hidden (hidden=1)
                let hidden = if value == "true" || value == "1" { "0" } else { "1" };
                uci_set(&format!("wireless.{iface}.hidden"), hidden)?;
                uci_commit("wireless")?;
                wifi_reload().await?;
                info!("WiFi SSID {idx} advertisement set to '{value}' (hidden={hidden})");
            } else {
                return Err(format!("SSID index {idx} out of range"));
            }
        }
    }
    // Handle AccessPoint MaxAssociatedDevices (max stations)
    else if path.ends_with(".MaxAssociatedDevices") {
        if let Some(idx) = parse_ap_index(path) {
            if idx > 0 && idx <= ifaces.len() {
                let iface = &ifaces[idx - 1];
                uci_set(&format!("wireless.{iface}.maxassoc"), value)?;
                uci_commit("wireless")?;
                wifi_reload().await?;
                info!("WiFi AccessPoint {idx} max associations set to '{value}'");
            } else {
                return Err(format!("AccessPoint index {idx} out of range"));
            }
        }
    }
    // Handle AccessPoint WMM Enable
    else if path.ends_with(".WMMEnable") {
        if let Some(idx) = parse_ap_index(path) {
            if idx > 0 && idx <= ifaces.len() {
                let iface = &ifaces[idx - 1];
                let wmm = if value == "true" || value == "1" { "1" } else { "0" };
                uci_set(&format!("wireless.{iface}.wmm"), wmm)?;
                uci_commit("wireless")?;
                wifi_reload().await?;
                info!("WiFi AccessPoint {idx} WMM set to '{wmm}'");
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
