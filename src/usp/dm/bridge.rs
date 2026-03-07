//! TR-181 Device.X_OptimACS_Network.Bridge.* — vendor extension for bridge configuration
//! Maps to UCI /etc/config/network bridge interfaces like br-wan

use std::collections::HashMap;
use log::{info, warn};
use crate::config::ClientConfig;
use crate::usp::tp469::uci_backend::{uci_get, uci_set, uci_commit};

pub type Params = HashMap<String, String>;

/// Get bridge configuration from UCI network config
pub async fn get(cfg: &ClientConfig, path: &str) -> Params {
    let mut result = Params::new();
    
    // Parse bridge index from path like "Device.X_OptimACS_Network.Bridge.1.Name"
    let bridge_idx = parse_bridge_index(path).unwrap_or(1);
    let bridge_name = format!("br{}", bridge_idx);
    
    // Check if this is a query for multiple bridges or specific bridge
    if path.ends_with("BridgeNumberOfEntries") {
        let count = count_bridges();
        result.insert(path.to_string(), count.to_string());
        return result;
    }
    
    if path.ends_with(".") || path.ends_with("*") || path == "Device.X_OptimACS_Network.Bridge." {
        // Return all bridge parameters
        let bridge_params = get_bridge_params(&bridge_name).await;
        for (param, value) in bridge_params {
            let full_path = format!("Device.X_OptimACS_Network.Bridge.{}.{}", bridge_idx, param);
            result.insert(full_path, value);
        }
        return result;
    }
    
    // Specific parameter request
    let param = path.split('.').last().unwrap_or("");
    let value = get_bridge_param(&bridge_name, param).await;
    result.insert(path.to_string(), value);
    
    result
}

/// Set bridge configuration parameter
pub async fn set(_cfg: &ClientConfig, path: &str, value: &str) -> Result<(), String> {
    let bridge_idx = parse_bridge_index(path).unwrap_or(1);
    let bridge_name = format!("br{}", bridge_idx);
    let param = path.split('.').last().unwrap_or("");
    
    info!("Setting bridge config: {}.{} = {}", bridge_name, param, value);
    
    match param {
        "Enable" => {
            let enabled = if value.eq_ignore_ascii_case("true") || value == "1" {
                "1"
            } else {
                "0"
            };
            uci_set(&format!("network.{}.enabled", bridge_name), enabled)?;
        }
        "Name" => {
            // Bridge name is the section name itself - store as alias if needed
            uci_set(&format!("network.{}.bridge_name", bridge_name), value)?;
        }
        "Type" => {
            // Type is always 'bridge' for bridge interfaces
            if value != "bridge" {
                return Err(format!("Bridge type must be 'bridge', got: {}", value));
            }
        }
        "Ports" => {
            // Store port list (space-separated in UCI)
            let ports_uci = value.replace(",", " ");
            uci_set(&format!("network.{}.ports", bridge_name), &ports_uci)?;
        }
        "Proto" => {
            uci_set(&format!("network.{}.proto", bridge_name), value)?;
        }
        "IPAddress" => {
            uci_set(&format!("network.{}.ipaddr", bridge_name), value)?;
        }
        "Netmask" => {
            uci_set(&format!("network.{}.netmask", bridge_name), value)?;
        }
        "Gateway" => {
            uci_set(&format!("network.{}.gateway", bridge_name), value)?;
        }
        "Status" => {
            // Status is read-only, determined by interface state
            return Err(format!("Status is read-only: {}", path));
        }
        _ => {
            return Err(format!("Unknown bridge parameter: {}", param));
        }
    }
    
    // Commit the changes
    uci_commit("network")?;
    
    // Optionally restart network service (careful with this in production)
    // std::process::Command::new("/etc/init.d/network").arg("restart").spawn().ok();
    
    info!("Bridge config updated successfully: {}.{} = {}", bridge_name, param, value);
    Ok(())
}

/// Parse bridge index from path
fn parse_bridge_index(path: &str) -> Option<usize> {
    // Path format: Device.X_OptimACS_Network.Bridge.1.Name
    if let Some(bridge_pos) = path.find("Bridge.") {
        let after_bridge = &path[bridge_pos + 7..]; // Skip "Bridge."
        if let Some(dot_pos) = after_bridge.find('.') {
            after_bridge[..dot_pos].parse::<usize>().ok()
        } else if !after_bridge.is_empty() && !after_bridge.contains('.') {
            // Path ends with number like "Device.X_OptimACS_Network.Bridge.1"
            after_bridge.parse::<usize>().ok()
        } else {
            Some(1) // Default to first bridge
        }
    } else {
        Some(1)
    }
}

/// Count configured bridges
fn count_bridges() -> usize {
    let out = std::process::Command::new("uci")
        .args(["show", "network"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    
    let mut count = 0;
    let mut seen_sections = std::collections::HashSet::new();
    
    for line in out.lines() {
        if line.starts_with("network.br") {
            // Extract section name (e.g., "br1" from "network.br1.proto")
            let parts: Vec<&str> = line.split('.').collect();
            if parts.len() >= 2 {
                let section = parts[1].to_string();
                // Check if it's a bridge section (has .ports or is named brX)
                if section.starts_with("br") && !seen_sections.contains(&section) {
                    if line.contains(".ports=") || line.contains(".proto=") {
                        seen_sections.insert(section);
                        count += 1;
                    }
                }
            }
        }
    }
    
    // Always return at least 1 if br-wan exists or br1 section exists
    if count == 0 {
        // Check if br-wan exists (common name)
        let test = uci_get("network.br-wan.proto");
        if !test.is_empty() {
            return 1;
        }
        // Check for br1
        let test = uci_get("network.br1.proto");
        if !test.is_empty() {
            return 1;
        }
    }
    
    count.max(1) // Always report at least 1 bridge slot
}

/// Get all parameters for a bridge
async fn get_bridge_params(bridge_name: &str) -> Params {
    let mut params = Params::new();
    
    // Try br-wan first, then br{index}
    let section = if bridge_name == "br1" {
        // Try br-wan first, fallback to br1
        let test = uci_get("network.br-wan.proto");
        if !test.is_empty() {
            "br-wan"
        } else {
            "br1"
        }
    } else {
        bridge_name
    };
    
    let prefix = format!("network.{}", section);
    
    // Get enabled status
    let enabled = uci_get(&format!("{}.enabled", prefix));
    params.insert("Enable".to_string(), 
        if enabled == "0" { "false".to_string() } else { "true".to_string() });
    
    // Get name
    let name = uci_get(&format!("{}.bridge_name", prefix));
    if name.is_empty() {
        params.insert("Name".to_string(), section.to_string());
    } else {
        params.insert("Name".to_string(), name);
    }
    
    // Type is always bridge
    params.insert("Type".to_string(), "bridge".to_string());
    
    // Get ports (space-separated in UCI, comma-separated in TR-181)
    let ports = uci_get(&format!("{}.ports", prefix));
    if !ports.is_empty() {
        params.insert("Ports".to_string(), ports.replace(" ", ","));
    } else {
        params.insert("Ports".to_string(), "".to_string());
    }
    
    // Get proto
    let proto = uci_get(&format!("{}.proto", prefix));
    params.insert("Proto".to_string(), 
        if proto.is_empty() { "dhcp".to_string() } else { proto });
    
    // Get IP address
    let ipaddr = uci_get(&format!("{}.ipaddr", prefix));
    params.insert("IPAddress".to_string(), ipaddr);
    
    // Get netmask
    let netmask = uci_get(&format!("{}.netmask", prefix));
    params.insert("Netmask".to_string(), netmask);
    
    // Get gateway
    let gateway = uci_get(&format!("{}.gateway", prefix));
    params.insert("Gateway".to_string(), gateway);
    
    // Determine status from interface state
    let status = get_interface_status(section).await;
    params.insert("Status".to_string(), status);
    
    params
}

/// Get specific bridge parameter
async fn get_bridge_param(bridge_name: &str, param: &str) -> String {
    let params = get_bridge_params(bridge_name).await;
    params.get(param).cloned().unwrap_or_default()
}

/// Get interface operational status
async fn get_interface_status(bridge_name: &str) -> String {
    // Check if interface is up using ip command or network status
    let out = std::process::Command::new("ip")
        .args(["link", "show", bridge_name])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    
    if out.contains("state UP") || out.contains("UP") {
        "up".to_string()
    } else if out.contains("state DOWN") || out.contains("DOWN") {
        "down".to_string()
    } else {
        // Interface might not exist or be configured
        // Check UCI enabled status
        let enabled = uci_get(&format!("network.{}.enabled", bridge_name));
        if enabled == "0" {
            "disabled".to_string()
        } else {
            "unknown".to_string()
        }
    }
}

/// Handle OPERATE commands for bridge management
pub async fn operate(
    _cfg: &ClientConfig,
    command: &str,
    _input_args: &HashMap<String, String>,
) -> Result<HashMap<String, String>, String> {
    let mut output = HashMap::new();
    
    if command.ends_with(".Restart()") {
        // Restart the bridge interface
        let bridge_idx = parse_bridge_index(command).unwrap_or(1);
        let bridge_name = format!("br{}", bridge_idx);
        
        info!("Restarting bridge interface: {}", bridge_name);
        
        // Use ifup/ifdown to restart interface
        let _ = std::process::Command::new("ifdown")
            .arg(&bridge_name)
            .output();
        
        let result = std::process::Command::new("ifup")
            .arg(&bridge_name)
            .output();
        
        match result {
            Ok(_) => {
                output.insert("status".to_string(), "success".to_string());
                output.insert("message".to_string(), format!("Bridge {} restarted", bridge_name));
            }
            Err(e) => {
                return Err(format!("Failed to restart bridge {}: {}", bridge_name, e));
            }
        }
    } else {
        return Err(format!("Unknown bridge command: {}", command));
    }
    
    Ok(output)
}
