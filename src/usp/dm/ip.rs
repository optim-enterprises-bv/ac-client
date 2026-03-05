//! TR-181 Device.IP.Interface.* — reads/writes via UCI with multi-interface support.

use std::collections::HashMap;
use log::{info, warn};
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

fn uci_commit(pkg: &str) -> Result<(), String> {
    std::process::Command::new("uci")
        .args(["commit", pkg])
        .status()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Get all network interface sections from UCI
fn get_network_interfaces() -> Vec<(String, String)> {
    // Returns vec of (section_name, network_name) tuples
    let mut interfaces = Vec::new();
    
    let out = std::process::Command::new("uci")
        .args(["show", "network"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    
    // Look for sections that have proto= (indicating they're interfaces)
    let mut current_section = String::new();
    let mut has_proto = false;
    
    for line in out.lines() {
        if line.starts_with("network.") {
            let parts: Vec<&str> = line.split('.').collect();
            if parts.len() >= 2 {
                let section = parts[1].to_string();
                
                // Skip non-interface sections
                if section == "globals" || section == "switch" {
                    continue;
                }
                
                if line.contains(".proto=") {
                    // Check if this is a new section
                    if section != current_section {
                        if !current_section.is_empty() && has_proto {
                            interfaces.push((current_section.clone(), current_section.clone()));
                        }
                        current_section = section.clone();
                        has_proto = false;
                    }
                    has_proto = true;
                }
            }
        }
    }
    
    // Don't forget the last section
    if !current_section.is_empty() && has_proto {
        interfaces.push((current_section.clone(), current_section.clone()));
    }
    
    // Filter out @named sections (aliases) and ensure common interfaces are present
    let common_interfaces = vec!["lan", "wan", "wan6"];
    let mut filtered = Vec::new();
    
    for (section, name) in interfaces {
        if !section.starts_with('@') {
            filtered.push((section, name));
        }
    }
    
    // Ensure common interfaces exist even if not found
    for iface in &common_interfaces {
        if !filtered.iter().any(|(s, _)| s == *iface) {
            // Check if it actually exists in UCI
            let test = uci_get(&format!("network.{iface}.proto"));
            if !test.is_empty() {
                filtered.push((iface.to_string(), iface.to_string()));
            }
        }
    }
    
    filtered
}

/// Parse interface index from path like "Device.IP.Interface.1.IPv4Address.1.IPAddress"
fn parse_interface_index(path: &str) -> Option<usize> {
    if let Some(start) = path.find("Interface.") {
        let rest = &path[start + 10..];
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
    let interfaces = get_network_interfaces();
    
    // Check if this is a specific interface request or a general request
    let specific_idx = parse_interface_index(path);
    
    if path == "Device.IP.Interface." || path.ends_with("IPv4Address.") || path.ends_with("IPv4Address.1.") {
        // Return all interfaces
        for (idx, (section, _name)) in interfaces.iter().enumerate() {
            let iface_idx = idx + 1;
            let base = format!("Device.IP.Interface.{iface_idx}.IPv4Address.1.");
            
            let ip = uci_get(&format!("network.{section}.ipaddr"));
            let mask = uci_get(&format!("network.{section}.netmask"));
            let proto = uci_get(&format!("network.{section}.proto"));
            let gateway = uci_get(&format!("network.{section}.gateway"));
            let dns = uci_get(&format!("network.{section}.dns"));
            
            if !ip.is_empty() {
                m.insert(format!("{base}IPAddress"), ip);
            }
            if !mask.is_empty() {
                m.insert(format!("{base}SubnetMask"), mask);
            }
            if !proto.is_empty() {
                m.insert(format!("{base}AddressingType"), proto);
            }
            if !gateway.is_empty() {
                m.insert(format!("Device.IP.Interface.{iface_idx}.X_OptimACS_Gateway"), gateway);
            }
            if !dns.is_empty() {
                m.insert(format!("Device.IP.Interface.{iface_idx}.X_OptimACS_DNS"), dns);
            }
        }
    } else if let Some(idx) = specific_idx {
        // Specific interface requested
        if idx > 0 && idx <= interfaces.len() {
            let (section, _name) = &interfaces[idx - 1];
            let base = format!("Device.IP.Interface.{idx}.IPv4Address.1.");
            
            let ip = uci_get(&format!("network.{section}.ipaddr"));
            let mask = uci_get(&format!("network.{section}.netmask"));
            let proto = uci_get(&format!("network.{section}.proto"));
            let gateway = uci_get(&format!("network.{section}.gateway"));
            let dns = uci_get(&format!("network.{section}.dns"));
            
            if path.ends_with(".IPAddress") {
                m.insert(format!("{base}IPAddress"), ip);
            } else if path.ends_with(".SubnetMask") {
                m.insert(format!("{base}SubnetMask"), mask);
            } else if path.ends_with(".AddressingType") {
                m.insert(format!("{base}AddressingType"), proto);
            } else {
                // Return all parameters for this interface
                if !ip.is_empty() {
                    m.insert(format!("{base}IPAddress"), ip);
                }
                if !mask.is_empty() {
                    m.insert(format!("{base}SubnetMask"), mask);
                }
                if !proto.is_empty() {
                    m.insert(format!("{base}AddressingType"), proto);
                }
                if !gateway.is_empty() {
                    m.insert(format!("Device.IP.Interface.{idx}.X_OptimACS_Gateway"), gateway);
                }
                if !dns.is_empty() {
                    m.insert(format!("Device.IP.Interface.{idx}.X_OptimACS_DNS"), dns);
                }
            }
        }
    }
    
    m
}

pub async fn set(_cfg: &ClientConfig, path: &str, value: &str) -> Result<(), String> {
    let interfaces = get_network_interfaces();
    
    // Parse the interface index from the path
    let idx = parse_interface_index(path)
        .ok_or_else(|| format!("Cannot parse interface index from path: {path}"))?;
    
    if idx == 0 || idx > interfaces.len() {
        return Err(format!("Interface index {idx} out of range (max: {})", interfaces.len()));
    }
    
    let (section, _name) = &interfaces[idx - 1];
    info!("Setting IP parameter for interface {idx} (section: {section}): {path} = {value}");
    
    if path.ends_with(".IPAddress") {
        uci_set(&format!("network.{section}.ipaddr"), value)?;
    } else if path.ends_with(".SubnetMask") {
        uci_set(&format!("network.{section}.netmask"), value)?;
    } else if path.ends_with(".AddressingType") {
        uci_set(&format!("network.{section}.proto"), value)?;
    } else if path.contains("X_OptimACS_Gateway") {
        uci_set(&format!("network.{section}.gateway"), value)?;
    } else if path.contains("X_OptimACS_DNS") {
        uci_set(&format!("network.{section}.dns"), value)?;
    } else {
        warn!("Unknown IP parameter in path: {path}");
        return Err(format!("Unknown IP parameter: {path}"));
    }
    
    uci_commit("network")?;
    
    // Reload network
    reload_network().await?;
    
    Ok(())
}

/// Reload network configuration
async fn reload_network() -> Result<(), String> {
    // Try multiple methods
    let methods: Vec<Vec<&str>> = vec![
        vec!["/etc/init.d/network", "reload"],
        vec!["/etc/init.d/network", "restart"],
        vec!["killall", "-HUP", "netifd"],
    ];
    
    for args in &methods {
        let status = std::process::Command::new(args[0])
            .args(&args[1..])
            .status();
        
        if let Ok(s) = status {
            if s.success() {
                info!("Network configuration reloaded");
                return Ok(());
            }
        }
    }
    
    warn!("Network reload command failed, changes will apply on reboot");
    Ok(()) // Don't fail the operation
}
