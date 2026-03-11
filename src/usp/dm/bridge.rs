//! TR-181 Device.X_OptimACS_Network.Bridge.* — vendor extension for bridge configuration
//! Maps to UCI /etc/config/network bridge interfaces like br-wan

use std::collections::HashMap;
use log::{info, warn};
use crate::config::ClientConfig;
use crate::usp::tp469::uci_backend::{uci_get, uci_set, uci_commit};

pub type Params = HashMap<String, String>;

/// Find the UCI section index for a bridge by name
/// Returns (section_type, index) like ("device", 0) for @device[0]
fn find_bridge_section(bridge_name: &str) -> Option<(String, usize)> {
    let out = std::process::Command::new("uci")
        .args(["show", "network"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    
    let mut current_section: Option<(String, usize)> = None;
    
    for line in out.lines() {
        // Check for device section start
        if line.starts_with("network.@device[") {
            // Parse section index from network.@device[N].name
            if let Some(start) = line.find('[') {
                if let Some(end) = line.find(']') {
                    if let Ok(idx) = line[start+1..end].parse::<usize>() {
                        current_section = Some(("device".to_string(), idx));
                    }
                }
            }
        }
        
        // Check if this section has the matching name
        if let Some((_, idx)) = current_section {
            let name_key = format!("network.@device[{}].name", idx);
            if line.starts_with(&name_key) {
                if let Some(name_val) = line.split('=').nth(1) {
                    let name = name_val.trim_matches('\'');
                    if name == bridge_name {
                        return Some(("device".to_string(), idx));
                    }
                }
            }
        }
    }
    
    None
}

/// Get UCI path for a bridge option
fn get_uci_path(bridge_name: &str, option: &str) -> String {
    if let Some((section_type, idx)) = find_bridge_section(bridge_name) {
        format!("network.@{}[{}].{}", section_type, idx, option)
    } else {
        // Fallback to legacy format (for bridges with named sections)
        format!("network.{}.{}", bridge_name, option)
    }
}

/// Get bridge configuration from UCI network config
pub async fn get(cfg: &ClientConfig, path: &str) -> Params {
    let mut result = Params::new();
    
    // Parse bridge index from path like "Device.X_OptimACS_Network.Bridge.1.Name"
    let bridge_idx = parse_bridge_index(path).unwrap_or(1);
    let bridge_name = get_bridge_name_by_index(bridge_idx);
    
    // Check if this is a query for bridge count
    if path.ends_with("BridgeNumberOfEntries") || path.contains("BridgeNumberOfEntries") {
        let count = count_bridges();
        result.insert(path.to_string(), count.to_string());
        return result;
    }
    
    // Check if requesting all bridge parameters
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
    let bridge_name = get_bridge_name_by_index(bridge_idx);
    let param = path.split('.').last().unwrap_or("");
    
    info!("Setting bridge config: {}.{} = {}", bridge_name, param, value);
    
    match param {
        "Enable" => {
            // For devices, we can't easily disable them via UCI
            // This would require deleting the device section
            warn!("Bridge enable/disable not supported via UCI device sections");
        }
        "Name" => {
            // Name is the section name property
            let uci_path = get_uci_path(&bridge_name, "name");
            uci_set(&uci_path, value)?;
        }
        "Type" => {
            // Type is always 'bridge' for bridge devices
            if value != "bridge" {
                return Err(format!("Bridge type must be 'bridge', got: {}", value));
            }
        }
        "Ports" => {
            // Ports are set on the interface that uses this bridge, not the device itself
            // Find the interface using this bridge
            let iface = find_interface_using_bridge(&bridge_name);
            if let Some(iface_name) = iface {
                let ports_uci = value.replace(",", " ");
                uci_set(&format!("network.{}.ports", iface_name), &ports_uci)?;
            } else {
                warn!("No interface found using bridge {}", bridge_name);
            }
        }
        "Proto" => {
            // Proto is set on the interface that uses this bridge
            let iface = find_interface_using_bridge(&bridge_name);
            if let Some(iface_name) = iface {
                uci_set(&format!("network.{}.proto", iface_name), value)?;
            } else {
                warn!("No interface found using bridge {}", bridge_name);
            }
        }
        "IPAddress" => {
            let iface = find_interface_using_bridge(&bridge_name);
            if let Some(iface_name) = iface {
                uci_set(&format!("network.{}.ipaddr", iface_name), value)?;
            }
        }
        "Netmask" => {
            let iface = find_interface_using_bridge(&bridge_name);
            if let Some(iface_name) = iface {
                uci_set(&format!("network.{}.netmask", iface_name), value)?;
            }
        }
        "Gateway" => {
            let iface = find_interface_using_bridge(&bridge_name);
            if let Some(iface_name) = iface {
                uci_set(&format!("network.{}.gateway", iface_name), value)?;
            }
        }
        "Status" => {
            // Status is read-only
            return Err(format!("Status is read-only: {}", path));
        }
        _ => {
            return Err(format!("Unknown bridge parameter: {}", param));
        }
    }
    
    // Commit the changes
    uci_commit("network")?;
    
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

/// Get bridge name by index (1-based)
/// Index 1 = br-wan (WAN bridge), Index 2+ = other bridges
fn get_bridge_name_by_index(idx: usize) -> String {
    match idx {
        1 => "br-wan".to_string(),  // First bridge is WAN
        2 => "br-lan".to_string(),  // Second bridge is LAN
        n => format!("br{}", n),     // Others
    }
}

/// Find interface name that uses a specific bridge
fn find_interface_using_bridge(bridge_name: &str) -> Option<String> {
    let out = std::process::Command::new("uci")
        .args(["show", "network"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    
    for line in out.lines() {
        // Look for interface sections (not device sections)
        if line.starts_with("network.") && !line.starts_with("network.@device[") {
            // Check if it has a device option pointing to our bridge
            if line.contains(".device=") {
                if let Some(val) = line.split('=').nth(1) {
                    let device = val.trim_matches('\'');
                    if device == bridge_name {
                        // Extract section name from network.SECTION.device
                        let parts: Vec<&str> = line.split('.').collect();
                        if parts.len() >= 2 {
                            let section = parts[1];
                            // Skip if it's a device section or globals
                            if section != "globals" && section != "switch" && !section.starts_with('@') {
                                return Some(section.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    
    None
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
    
    for line in out.lines() {
        if line.starts_with("network.@device[") && line.contains(".name=") {
            count += 1;
        }
    }
    
    count.max(2) // Report at least 2 (br-lan and br-wan)
}

/// Get all parameters for a bridge
async fn get_bridge_params(bridge_name: &str) -> Params {
    let mut params = Params::new();
    
    // Check if bridge section exists
    let section_info = find_bridge_section(bridge_name);
    if section_info.is_none() {
        warn!("Bridge {} not found in UCI", bridge_name);
        return params;
    }
    
    // Get name from UCI
    let name_path = get_uci_path(bridge_name, "name");
    let name = uci_get(&name_path);
    params.insert("Name".to_string(), 
        if name.is_empty() { bridge_name.to_string() } else { name });
    
    // Type is always bridge for device sections
    params.insert("Type".to_string(), "bridge".to_string());
    
    // Find interface using this bridge
    let iface = find_interface_using_bridge(bridge_name);
    
    if let Some(iface_name) = iface {
        // Get ports from interface
        let ports = uci_get(&format!("network.{}.ports", iface_name));
        if !ports.is_empty() {
            params.insert("Ports".to_string(), ports.replace(" ", ","));
        } else {
            params.insert("Ports".to_string(), "".to_string());
        }

        // Get proto from interface
        let proto = uci_get(&format!("network.{}.proto", iface_name));
        params.insert("Proto".to_string(),
            if proto.is_empty() { "none".to_string() } else { proto.clone() });

        // Get IP/netmask/gateway from UCI first, then fall back to runtime state
        let mut ipaddr = uci_get(&format!("network.{}.ipaddr", iface_name));
        let mut netmask = uci_get(&format!("network.{}.netmask", iface_name));
        let mut gateway = uci_get(&format!("network.{}.gateway", iface_name));
        let mut dns = String::new();

        // For DHCP/dynamic protocols, UCI won't have IP info — get from ubus runtime
        if ipaddr.is_empty() || (proto == "dhcp" || proto == "dhcpv6" || proto == "pppoe") {
            let rt = get_ubus_interface_status(&iface_name);
            if let Some(ip) = rt.get("ipaddr") {
                if !ip.is_empty() { ipaddr = ip.clone(); }
            }
            if let Some(mask) = rt.get("netmask") {
                if !mask.is_empty() { netmask = mask.clone(); }
            }
            if let Some(gw) = rt.get("gateway") {
                if !gw.is_empty() { gateway = gw.clone(); }
            }
            if let Some(d) = rt.get("dns") {
                dns = d.clone();
            }
        }

        params.insert("IPAddress".to_string(), ipaddr);
        params.insert("Netmask".to_string(), netmask);
        params.insert("Gateway".to_string(), gateway);
        if !dns.is_empty() {
            params.insert("DNS".to_string(), dns);
        }

        // Enable - check if interface is disabled
        let enabled = uci_get(&format!("network.{}.enabled", iface_name));
        params.insert("Enable".to_string(),
            if enabled == "0" { "false".to_string() } else { "true".to_string() });
    } else {
        // No interface found using this bridge
        params.insert("Ports".to_string(), "".to_string());
        params.insert("Proto".to_string(), "none".to_string());
        params.insert("IPAddress".to_string(), "".to_string());
        params.insert("Netmask".to_string(), "".to_string());
        params.insert("Gateway".to_string(), "".to_string());
        params.insert("Enable".to_string(), "false".to_string());
    }
    
    // Determine status from interface state
    let status = get_interface_status(bridge_name).await;
    params.insert("Status".to_string(), status);
    
    params
}

/// Get specific bridge parameter
async fn get_bridge_param(bridge_name: &str, param: &str) -> String {
    let params = get_bridge_params(bridge_name).await;
    params.get(param).cloned().unwrap_or_default()
}

/// Query `ubus call network.interface.<name> status` for runtime IP state.
/// Returns a map with keys: ipaddr, netmask, gateway, dns, uptime.
fn get_ubus_interface_status(iface_name: &str) -> HashMap<String, String> {
    let mut result = HashMap::new();

    let out = std::process::Command::new("ubus")
        .args(["call", &format!("network.interface.{}", iface_name), "status"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    if out.is_empty() {
        return result;
    }

    // Parse ipv4-address[0].address and mask
    // Format: "ipv4-address": [ { "address": "...", "mask": N } ]
    if let Some(pos) = out.find("\"ipv4-address\"") {
        let chunk = &out[pos..];
        if let Some(addr_pos) = chunk.find("\"address\"") {
            let after = &chunk[addr_pos + 9..];
            if let Some(start) = after.find('"') {
                let rest = &after[start + 1..];
                if let Some(end) = rest.find('"') {
                    result.insert("ipaddr".to_string(), rest[..end].to_string());
                }
            }
        }
        if let Some(mask_pos) = chunk.find("\"mask\"") {
            let after = &chunk[mask_pos + 5..];
            // Find the number after ":"
            let after = after.trim_start_matches(|c: char| !c.is_ascii_digit());
            if let Some(end) = after.find(|c: char| !c.is_ascii_digit()) {
                if let Ok(cidr) = after[..end].parse::<u32>() {
                    result.insert("netmask".to_string(), cidr_to_netmask(cidr));
                }
            }
        }
    }

    // Parse default route gateway
    // "route": [ { "target": "0.0.0.0", "mask": 0, "nexthop": "..." } ]
    if let Some(pos) = out.find("\"route\"") {
        let chunk = &out[pos..];
        // Find nexthop from the default route (target 0.0.0.0)
        let mut search = chunk;
        while let Some(nh_pos) = search.find("\"nexthop\"") {
            let after = &search[nh_pos + 9..];
            if let Some(start) = after.find('"') {
                let rest = &after[start + 1..];
                if let Some(end) = rest.find('"') {
                    let nexthop = &rest[..end];
                    if nexthop != "0.0.0.0" && !nexthop.is_empty() {
                        result.insert("gateway".to_string(), nexthop.to_string());
                        break;
                    }
                }
            }
            search = &search[nh_pos + 10..];
        }
    }

    // Parse dns-server array
    if let Some(pos) = out.find("\"dns-server\"") {
        let chunk = &out[pos..];
        if let Some(arr_start) = chunk.find('[') {
            if let Some(arr_end) = chunk[arr_start..].find(']') {
                let arr = &chunk[arr_start..arr_start + arr_end];
                let servers: Vec<&str> = arr
                    .split('"')
                    .filter(|s| !s.is_empty() && !s.contains('[') && !s.contains(',') && s.trim() != ",")
                    .filter(|s| s.contains('.') || s.contains(':'))
                    .collect();
                if !servers.is_empty() {
                    result.insert("dns".to_string(), servers.join(","));
                }
            }
        }
    }

    // Parse uptime
    if let Some(pos) = out.find("\"uptime\"") {
        let after = &out[pos + 8..];
        let after = after.trim_start_matches(|c: char| !c.is_ascii_digit());
        if let Some(end) = after.find(|c: char| !c.is_ascii_digit()) {
            if let Ok(secs) = after[..end].parse::<u64>() {
                result.insert("uptime".to_string(), secs.to_string());
            }
        }
    }

    result
}

/// Convert CIDR prefix length to dotted-decimal subnet mask
fn cidr_to_netmask(cidr: u32) -> String {
    if cidr == 0 {
        return "0.0.0.0".to_string();
    }
    let mask: u32 = !0u32 << (32 - cidr.min(32));
    format!(
        "{}.{}.{}.{}",
        (mask >> 24) & 0xFF,
        (mask >> 16) & 0xFF,
        (mask >> 8) & 0xFF,
        mask & 0xFF,
    )
}

/// Get interface operational status
async fn get_interface_status(bridge_name: &str) -> String {
    // Check if interface is up using ip command
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
        "unknown".to_string()
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
        let bridge_idx = parse_bridge_index(command).unwrap_or(1);
        let bridge_name = get_bridge_name_by_index(bridge_idx);
        
        info!("Restarting bridge interface: {}", bridge_name);
        
        // Use ifdown/ifup via the interface name
        let iface = find_interface_using_bridge(&bridge_name);
        if let Some(iface_name) = iface {
            let _ = std::process::Command::new("ifdown")
                .arg(&iface_name)
                .output();
            
            let result = std::process::Command::new("ifup")
                .arg(&iface_name)
                .output();
            
            match result {
                Ok(_) => {
                    output.insert("status".to_string(), "success".to_string());
                    output.insert("message".to_string(), format!("Bridge {} restarted via {}", bridge_name, iface_name));
                }
                Err(e) => {
                    return Err(format!("Failed to restart bridge {}: {}", bridge_name, e));
                }
            }
        } else {
            return Err(format!("No interface found using bridge {}", bridge_name));
        }
    } else {
        return Err(format!("Unknown bridge command: {}", command));
    }
    
    Ok(output)
}
