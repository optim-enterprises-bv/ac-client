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
    let ubus_map = build_ubus_iface_map();
    
    // Handle SSID requests
    if path.contains("SSID.") || path.ends_with("Device.WiFi.") {
        for (idx, iface) in ifaces.iter().enumerate() {
            let ssid_idx = idx + 1;
            let ssid = uci_get(&format!("wireless.{iface}.ssid"));
            let disabled = uci_get(&format!("wireless.{iface}.disabled"));
            let enable = disabled != "1";

            if !ssid.is_empty() {
                m.insert(format!("Device.WiFi.SSID.{ssid_idx}.SSID"), ssid);
                m.insert(format!("Device.WiFi.SSID.{ssid_idx}.Enable"), enable.to_string());
                // SSID Status: Up if enabled
                m.insert(format!("Device.WiFi.SSID.{ssid_idx}.Status"), if enable { "Up" } else { "Down" }.to_string());
                // Try to get BSSID for this SSID's interface
                let device = uci_get(&format!("wireless.{iface}.device"));
                let net_iface = {
                    // 1. ubus (works on single-chip multi-band radios like MT7996)
                    let ubus_iface = ubus_map.get(iface.as_str()).cloned().unwrap_or_default();
                    if !ubus_iface.is_empty() { ubus_iface }
                    // 2. UCI ifname property
                    else {
                        let ifname = uci_get(&format!("wireless.{iface}.ifname"));
                        if !ifname.is_empty() { ifname }
                        // 3. Legacy: derive from radio device name
                        else { get_phy_interface(&device) }
                    }
                };
                let mut bssid = String::new();
                if !net_iface.is_empty() {
                    bssid = get_iw_bssid(&net_iface);
                    if bssid.is_empty() {
                        // Fallback: read MAC from /sys/class/net/<iface>/address
                        bssid = get_sysfs_mac(&net_iface);
                    }
                }
                // Fallback: UCI macaddr for this wifi-iface
                if bssid.is_empty() {
                    bssid = uci_get(&format!("wireless.{iface}.macaddr"));
                }
                // Fallback: UCI macaddr for the parent radio device
                if bssid.is_empty() && !device.is_empty() {
                    bssid = uci_get(&format!("wireless.{device}.macaddr"));
                }
                if !bssid.is_empty() {
                    m.insert(format!("Device.WiFi.SSID.{ssid_idx}.BSSID"), bssid);
                }
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
            let disabled = uci_get(&format!("wireless.{iface}.disabled"));
            let enabled = disabled != "1";

            // ModeEnabled — return friendly name matching WPAEncryptionModes
            let mode_friendly = match enc.as_str() {
                "psk2" | "psk2+ccmp" => "WPA2-Personal",
                "psk-mixed" | "psk-mixed+ccmp" => "WPA-WPA2-Personal",
                "sae" => "WPA3-Personal",
                "sae-mixed" => "WPA2-WPA3-Personal",
                "wpa2" | "wpa2+ccmp" => "WPA2-Enterprise",
                "owe" => "OWE",
                "none" | "" => "None",
                other => other,
            };
            m.insert(format!("Device.WiFi.AccessPoint.{ap_idx}.Security.ModeEnabled"), mode_friendly.to_string());
            if !key.is_empty() {
                m.insert(format!("Device.WiFi.AccessPoint.{ap_idx}.Security.KeyPassphrase"), key);
            }
            if !mode.is_empty() {
                m.insert(format!("Device.WiFi.AccessPoint.{ap_idx}.Mode"), mode);
            }

            // AccessPoint Status
            m.insert(format!("Device.WiFi.AccessPoint.{ap_idx}.Status"),
                if enabled { "Enabled" } else { "Disabled" }.to_string());

            // BSSID for this AccessPoint (from the corresponding wireless interface)
            let ap_device = uci_get(&format!("wireless.{iface}.device"));
            let ap_phy = {
                let ubus_iface = ubus_map.get(iface.as_str()).cloned().unwrap_or_default();
                if !ubus_iface.is_empty() { ubus_iface }
                else {
                    let ifname = uci_get(&format!("wireless.{iface}.ifname"));
                    if !ifname.is_empty() { ifname }
                    else if !ap_device.is_empty() { get_phy_interface(&ap_device) }
                    else { String::new() }
                }
            };
            let mut ap_bssid = String::new();
            if !ap_phy.is_empty() {
                ap_bssid = get_iw_bssid(&ap_phy);
                if ap_bssid.is_empty() {
                    ap_bssid = get_sysfs_mac(&ap_phy);
                }
            }
            if ap_bssid.is_empty() {
                ap_bssid = uci_get(&format!("wireless.{iface}.macaddr"));
            }
            if ap_bssid.is_empty() && !ap_device.is_empty() {
                ap_bssid = uci_get(&format!("wireless.{ap_device}.macaddr"));
            }
            if !ap_bssid.is_empty() {
                m.insert(format!("Device.WiFi.AccessPoint.{ap_idx}.BSSID"), ap_bssid);
            }

            // SSIDAdvertisementEnabled (inverse of UCI hidden flag)
            let hidden = uci_get(&format!("wireless.{iface}.hidden"));
            let advertised = hidden != "1";
            m.insert(format!("Device.WiFi.AccessPoint.{ap_idx}.SSIDAdvertisementEnabled"), advertised.to_string());

            // WPA encryption modes (derive from enc)
            let wpa_modes = match enc.as_str() {
                "psk2" | "psk2+ccmp" => "WPA2-Personal",
                "psk-mixed" | "psk-mixed+ccmp" => "WPA-WPA2-Personal",
                "sae" => "WPA3-Personal",
                "sae-mixed" => "WPA2-WPA3-Personal",
                "wpa2" | "wpa2+ccmp" => "WPA2-Enterprise",
                "none" | "" => "None",
                other => other,
            };
            m.insert(format!("Device.WiFi.AccessPoint.{ap_idx}.Security.WPAEncryptionModes"), wpa_modes.to_string());

            // MFP (802.11w management frame protection)
            let ieee80211w = uci_get(&format!("wireless.{iface}.ieee80211w"));
            let mfp = match ieee80211w.as_str() {
                "2" => "Required",
                "1" => "Optional",
                _ => "Disabled",
            };
            m.insert(format!("Device.WiFi.AccessPoint.{ap_idx}.Security.MFPConfig"), mfp.to_string());

            // AssociatedDeviceNumberOfEntries for this AP
            let device = uci_get(&format!("wireless.{iface}.device"));
            let phy_iface = {
                let ubus_iface = ubus_map.get(iface.as_str()).cloned().unwrap_or_default();
                if !ubus_iface.is_empty() { ubus_iface }
                else if !device.is_empty() { get_phy_interface(&device) }
                else { String::new() }
            };
            if !phy_iface.is_empty() {
                let count = get_associated_device_count(&phy_iface);
                m.insert(format!("Device.WiFi.AccessPoint.{ap_idx}.AssociatedDeviceNumberOfEntries"), count.to_string());
            } else {
                m.insert(format!("Device.WiFi.AccessPoint.{ap_idx}.AssociatedDeviceNumberOfEntries"), "0".to_string());
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
            let enable = disabled != "1";

            if !chan.is_empty() {
                m.insert(format!("Device.WiFi.Radio.{radio_idx}.Channel"), chan);
            }
            m.insert(format!("Device.WiFi.Radio.{radio_idx}.Enable"), enable.to_string());
            if !band.is_empty() {
                // Map UCI band values to TR-181 OperatingFrequencyBand
                let band_friendly = match band.as_str() {
                    "2g" => "2.4GHz",
                    "5g" => "5GHz",
                    "6g" => "6GHz",
                    "60g" => "60GHz",
                    other => other,
                };
                m.insert(format!("Device.WiFi.Radio.{radio_idx}.OperatingFrequencyBand"), band_friendly.to_string());
            }
            if !htmode.is_empty() {
                m.insert(format!("Device.WiFi.Radio.{radio_idx}.OperatingChannelBandwidth"), htmode.clone());
                // Derive MaxBitRate from htmode and band
                let max_bitrate = estimate_max_bitrate(&htmode, &band);
                if !max_bitrate.is_empty() {
                    m.insert(format!("Device.WiFi.Radio.{radio_idx}.MaxBitRate"), max_bitrate);
                }
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

            // Additional radio params
            let rts_threshold = uci_get(&format!("wireless.{device}.rts"));
            if !rts_threshold.is_empty() {
                m.insert(format!("Device.WiFi.Radio.{radio_idx}.RTSThreshold"), rts_threshold);
            } else {
                m.insert(format!("Device.WiFi.Radio.{radio_idx}.RTSThreshold"), "2347".to_string());
            }

            let guard_interval = uci_get(&format!("wireless.{device}.short_gi"));
            let gi_value = match guard_interval.as_str() {
                "0" => "Long",
                "1" | "" => "Auto",
                _ => "Auto",
            };
            m.insert(format!("Device.WiFi.Radio.{radio_idx}.GuardInterval"), gi_value.to_string());

            // IEEE 802.11h (DFS/TPC) — enabled by default on 5GHz
            let band_val = uci_get(&format!("wireless.{device}.band"));
            let ieee80211h = band_val == "5g" || band_val == "6g";
            m.insert(format!("Device.WiFi.Radio.{radio_idx}.IEEE80211hEnabled"), ieee80211h.to_string());

            let max_assoc = uci_get(&format!("wireless.{device}.maxassoc"));
            if !max_assoc.is_empty() {
                m.insert(format!("Device.WiFi.Radio.{radio_idx}.MaxAssociatedDevices"), max_assoc);
            } else {
                m.insert(format!("Device.WiFi.Radio.{radio_idx}.MaxAssociatedDevices"), "0".to_string());
            }

            // Radio Name — hardware description from /sys/class/ieee80211/phy*/device
            let radio_name = get_radio_hardware_name(device);
            if !radio_name.is_empty() {
                m.insert(format!("Device.WiFi.Radio.{radio_idx}.Name"), radio_name);
            }

            // Radio Status: Up if enabled and phy interface exists, Down otherwise
            // Find the first iface that belongs to this radio device
            let phy_iface = {
                let mut found = String::new();
                for iface in &ifaces {
                    if uci_get(&format!("wireless.{iface}.device")) == *device {
                        found = ubus_map.get(iface.as_str()).cloned().unwrap_or_default();
                        if !found.is_empty() { break; }
                    }
                }
                if found.is_empty() { get_phy_interface(device) } else { found }
            };
            let status = if enable && !phy_iface.is_empty() { "Up" } else { "Down" };
            m.insert(format!("Device.WiFi.Radio.{radio_idx}.Status"), status.to_string());

            // BSSID with fallbacks
            let mut radio_bssid = String::new();
            if !phy_iface.is_empty() {
                radio_bssid = get_iw_bssid(&phy_iface);
                if radio_bssid.is_empty() {
                    radio_bssid = get_sysfs_mac(&phy_iface);
                }
            }
            if radio_bssid.is_empty() {
                radio_bssid = uci_get(&format!("wireless.{device}.macaddr"));
            }
            if !radio_bssid.is_empty() {
                m.insert(format!("Device.WiFi.Radio.{radio_idx}.X_OptimACS_BSSID"), radio_bssid);
            }

            if !phy_iface.is_empty() {
                let bitrate = get_iw_bitrate(&phy_iface);
                if !bitrate.is_empty() {
                    m.insert(format!("Device.WiFi.Radio.{radio_idx}.X_OptimACS_Bitrate"), bitrate);
                }
                let assoc_count = get_associated_device_count(&phy_iface);
                m.insert(format!("Device.WiFi.Radio.{radio_idx}.AssociatedDeviceNumberOfEntries"), assoc_count.to_string());
            }
        }
    }

    // Handle AssociatedDevice requests (connected WiFi clients)
    if path.contains("AssociatedDevice.") || path.ends_with("Device.WiFi.") {
        for (idx, iface) in ifaces.iter().enumerate() {
            let ap_idx = idx + 1;
            let device = uci_get(&format!("wireless.{iface}.device"));
            let phy_iface = {
                let ubus_iface = ubus_map.get(iface.as_str()).cloned().unwrap_or_default();
                if !ubus_iface.is_empty() { ubus_iface }
                else if !device.is_empty() { get_phy_interface(&device) }
                else { String::new() }
            };
            if !phy_iface.is_empty() {
                let stations = get_station_dump(&phy_iface);
                for (sta_idx, sta) in stations.iter().enumerate() {
                    let si = sta_idx + 1;
                    let base = format!("Device.WiFi.AccessPoint.{ap_idx}.AssociatedDevice.{si}");
                    if let Some(mac) = sta.get("mac") {
                        m.insert(format!("{base}.MACAddress"), mac.clone());
                    }
                    if let Some(signal) = sta.get("signal") {
                        m.insert(format!("{base}.SignalStrength"), signal.clone());
                    }
                    if let Some(tx_rate) = sta.get("tx_bitrate") {
                        m.insert(format!("{base}.LastDataDownlinkRate"), tx_rate.clone());
                    }
                    if let Some(rx_rate) = sta.get("rx_bitrate") {
                        m.insert(format!("{base}.LastDataUplinkRate"), rx_rate.clone());
                    }
                    if let Some(tx_bytes) = sta.get("tx_bytes") {
                        m.insert(format!("{base}.BytesSent"), tx_bytes.clone());
                    }
                    if let Some(rx_bytes) = sta.get("rx_bytes") {
                        m.insert(format!("{base}.BytesReceived"), rx_bytes.clone());
                    }
                    // Try to resolve IP from ARP table
                    if let Some(mac) = sta.get("mac") {
                        let ip = resolve_ip_from_arp(mac);
                        if !ip.is_empty() {
                            m.insert(format!("{base}.IPAddress"), ip);
                        }
                    }
                }
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

/// Get radio hardware description (e.g. "MediaTek MT7996E 802.11ax")
fn get_radio_hardware_name(device: &str) -> String {
    let idx = device.trim_start_matches("radio").parse::<usize>().unwrap_or(0);
    let phy = format!("phy{}", idx);

    // Try reading from /sys/class/ieee80211/<phy>/device/uevent for driver/vendor
    let uevent_path = format!("/sys/class/ieee80211/{}/device/uevent", phy);
    let driver = std::fs::read_to_string(&uevent_path)
        .ok()
        .and_then(|content| {
            content.lines()
                .find(|l| l.starts_with("DRIVER="))
                .map(|l| l.trim_start_matches("DRIVER=").to_string())
        })
        .unwrap_or_default();

    // Try to get device model from modalias or device name
    let device_path = format!("/sys/class/ieee80211/{}/device/device", phy);
    let device_id = std::fs::read_to_string(&device_path)
        .ok()
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    let vendor_path = format!("/sys/class/ieee80211/{}/device/vendor", phy);
    let vendor_id = std::fs::read_to_string(&vendor_path)
        .ok()
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    // Get supported modes from iw phy
    let band = uci_get(&format!("wireless.{}.band", device));
    let modes = match band.as_str() {
        "2g" => "802.11bgn",
        "5g" => "802.11ac/ax",
        "6g" => "802.11ax/be",
        _ => "802.11",
    };

    // Build a friendly name
    if !driver.is_empty() {
        // Map common driver names to friendly chip names
        let chip = match driver.as_str() {
            "mt7996e" => "MediaTek MT7996E",
            "mt7915e" => "MediaTek MT7915E",
            "mt7921e" | "mt7921_pci" => "MediaTek MT7921",
            "mt7622" => "MediaTek MT7622",
            "ath11k_pci" | "ath11k" => "Qualcomm IPQ8074/QCN9074",
            "ath10k_pci" | "ath10k" => "Qualcomm Atheros QCA9984",
            "ath9k" => "Qualcomm Atheros AR9xxx",
            "iwlwifi" => "Intel WiFi",
            "mac80211_hwsim" => "mac80211_hwsim",
            _ => &driver,
        };
        format!("{} {}", chip, modes)
    } else if !vendor_id.is_empty() && !device_id.is_empty() {
        format!("WiFi {} {} {}", vendor_id, device_id, modes)
    } else {
        format!("Generic {} Radio", modes)
    }
}

/// Build a section→ifname map from `ubus call network.wireless status`.
///
/// This is the canonical OpenWrt method and handles single-chip multi-band
/// radios (e.g. MT7996) where all bands share one phy and the radio→phy
/// naming assumption breaks.
fn build_ubus_iface_map() -> HashMap<String, String> {
    let mut map = HashMap::new();

    let ubus_out = std::process::Command::new("ubus")
        .args(["call", "network.wireless", "status"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    if ubus_out.is_empty() {
        return map;
    }

    // Minimal JSON parsing: find each "section"/"ifname" pair.
    // The output structure is:
    //   { "radioN": { "interfaces": [ { "section": "...", "ifname": "..." }, ... ] } }
    let mut search_from = 0;
    while let Some(sec_pos) = ubus_out[search_from..].find("\"section\": \"") {
        let abs_pos = search_from + sec_pos;
        let sec_start = abs_pos + "\"section\": \"".len();
        if let Some(sec_end) = ubus_out[sec_start..].find('"') {
            let section = &ubus_out[sec_start..sec_start + sec_end];
            // Look for "ifname" after this section
            let after = &ubus_out[sec_start + sec_end..];
            if let Some(ifname_pos) = after.find("\"ifname\": \"") {
                let if_start = ifname_pos + "\"ifname\": \"".len();
                if let Some(if_end) = after[if_start..].find('"') {
                    let ifname = &after[if_start..if_start + if_end];
                    if !ifname.is_empty() && !section.is_empty() {
                        map.insert(section.to_string(), ifname.to_string());
                    }
                }
            }
            search_from = sec_start + sec_end + 1;
        } else {
            break;
        }
    }

    map
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

/// Estimate max PHY bitrate from htmode and band
fn estimate_max_bitrate(htmode: &str, band: &str) -> String {
    // Approximate maximum PHY rates for common configurations
    let rate = match htmode {
        // 802.11be (EHT)
        "EHT320" => "46080 Mbps",
        "EHT160" => "23040 Mbps",
        "EHT80" if band == "6g" => "11520 Mbps",
        "EHT80" => "11520 Mbps",
        "EHT40" => "5760 Mbps",
        "EHT20" => "2880 Mbps",
        // 802.11ax (HE)
        "HE160" => "9608 Mbps",
        "HE80" => "4804 Mbps",
        "HE40" => "2402 Mbps",
        "HE20" => "1201 Mbps",
        // 802.11ac (VHT)
        "VHT160" => "6933 Mbps",
        "VHT80" => "3467 Mbps",
        "VHT40" => "1733 Mbps",
        "VHT20" => "867 Mbps",
        // 802.11n (HT)
        "HT40" => "300 Mbps",
        "HT20" => "144 Mbps",
        // Legacy
        _ => "",
    };
    rate.to_string()
}

/// Read MAC address from /sys/class/net/<iface>/address
fn get_sysfs_mac(iface: &str) -> String {
    std::fs::read_to_string(format!("/sys/class/net/{iface}/address"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && s != "00:00:00:00:00:00")
        .unwrap_or_default()
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

/// Parse `iw dev <iface> station dump` into per-station maps
fn get_station_dump(iface: &str) -> Vec<HashMap<String, String>> {
    let output = std::process::Command::new("iw")
        .args(["dev", iface, "station", "dump"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    let mut stations = Vec::new();
    let mut current: Option<HashMap<String, String>> = None;

    for line in output.lines() {
        if line.starts_with("Station ") {
            // Save previous station
            if let Some(sta) = current.take() {
                stations.push(sta);
            }
            let mut sta = HashMap::new();
            // "Station aa:bb:cc:dd:ee:ff (on wlan0)"
            if let Some(mac) = line.split_whitespace().nth(1) {
                sta.insert("mac".to_string(), mac.to_uppercase());
            }
            current = Some(sta);
        } else if let Some(ref mut sta) = current {
            let trimmed = line.trim();
            if let Some((key, val)) = trimmed.split_once(':') {
                let key = key.trim();
                let val = val.trim();
                match key {
                    "signal" => {
                        // "signal:  -42 [-42, -48] dBm" → extract first number
                        let dbm = val.split_whitespace().next().unwrap_or(val);
                        sta.insert("signal".to_string(), dbm.to_string());
                    }
                    "tx bitrate" => {
                        // "tx bitrate:  866.7 MBit/s ..." → extract rate in kbps
                        if let Some(rate_str) = val.split_whitespace().next() {
                            if let Ok(mbps) = rate_str.parse::<f64>() {
                                sta.insert("tx_bitrate".to_string(), format!("{}", (mbps * 1000.0) as u64));
                            } else {
                                sta.insert("tx_bitrate".to_string(), val.to_string());
                            }
                        }
                    }
                    "rx bitrate" => {
                        if let Some(rate_str) = val.split_whitespace().next() {
                            if let Ok(mbps) = rate_str.parse::<f64>() {
                                sta.insert("rx_bitrate".to_string(), format!("{}", (mbps * 1000.0) as u64));
                            } else {
                                sta.insert("rx_bitrate".to_string(), val.to_string());
                            }
                        }
                    }
                    "rx bytes" => {
                        sta.insert("rx_bytes".to_string(), val.to_string());
                    }
                    "tx bytes" => {
                        sta.insert("tx_bytes".to_string(), val.to_string());
                    }
                    _ => {}
                }
            }
        }
    }
    // Don't forget last station
    if let Some(sta) = current {
        stations.push(sta);
    }

    stations
}

/// Resolve IP address from ARP/neighbor table by MAC
fn resolve_ip_from_arp(mac: &str) -> String {
    let mac_lower = mac.to_lowercase();

    // Try /proc/net/arp first
    if let Ok(content) = std::fs::read_to_string("/proc/net/arp") {
        for line in content.lines().skip(1) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() >= 4 && fields[3].to_lowercase() == mac_lower {
                return fields[0].to_string();
            }
        }
    }

    // Fallback: ip neigh
    let output = std::process::Command::new("ip")
        .args(["neigh", "show"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    for line in output.lines() {
        if line.to_lowercase().contains(&mac_lower) {
            if let Some(ip) = line.split_whitespace().next() {
                return ip.to_string();
            }
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
