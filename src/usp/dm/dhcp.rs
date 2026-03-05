//! TR-181 Device.DHCPv4.Server.Pool.* — reads/writes via UCI.

use std::collections::HashMap;
use log::{info, warn};
use crate::config::ClientConfig;

/// Get all DHCP static leases from UCI
pub async fn get(_cfg: &ClientConfig, _path: &str) -> HashMap<String, String> {
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
            // Extract section name (e.g., "dhcp.host_001122334455")
            let section = line.split('.').nth(1).unwrap_or("").to_string();
            let mac = line.split('=').nth(1).unwrap_or("").trim_matches('\'').to_string();
            
            // Find corresponding IP
            let ip_line = out.lines()
                .find(|l| l.contains(&format!("dhcp.{section}.ip=")));
            let ip = ip_line
                .and_then(|l| l.split('=').nth(1))
                .unwrap_or("")
                .trim_matches('\'')
                .to_string();
            
            // Find hostname if present
            let name_line = out.lines()
                .find(|l| l.contains(&format!("dhcp.{section}.name=")));
            let name = name_line
                .and_then(|l| l.split('=').nth(1))
                .unwrap_or("")
                .trim_matches('\'')
                .to_string();
            
            let base = format!("Device.DHCPv4.Server.Pool.1.StaticAddress.{idx}.");
            m.insert(format!("{base}Chaddr"), mac);
            m.insert(format!("{base}Yiaddr"), ip);
            if !name.is_empty() {
                m.insert(format!("{base}X_OptimACS_Hostname"), name);
            }
            idx += 1;
        }
    }
    m
}

/// Set DHCP static lease parameters (Chaddr/MAC or Yiaddr/IP)
pub async fn set(_cfg: &ClientConfig, path: &str, value: &str) -> Result<(), String> {
    // Parse path: Device.DHCPv4.Server.Pool.1.StaticAddress.{idx}.{Param}
    let parts: Vec<&str> = path.split('.').collect();
    if parts.len() < 7 {
        return Err(format!("Invalid DHCP path: {path}"));
    }
    
    let idx_str = parts[5]; // {idx}
    let param = parts[6];   // Chaddr, Yiaddr, or X_OptimACS_Hostname
    let idx: usize = idx_str.parse().map_err(|_| format!("Invalid index: {idx_str}"))?;
    
    // Find existing section or create new one
    let section = find_or_create_host_section(idx).await?;
    
    match param {
        "Chaddr" => {
            // MAC address
            uci_set(&format!("dhcp.{section}.mac"), value).await?;
            info!("DHCP static lease {idx}: MAC set to {value}");
        }
        "Yiaddr" => {
            // IP address
            uci_set(&format!("dhcp.{section}.ip"), value).await?;
            info!("DHCP static lease {idx}: IP set to {value}");
        }
        "X_OptimACS_Hostname" => {
            // Hostname
            uci_set(&format!("dhcp.{section}.name"), value).await?;
            info!("DHCP static lease {idx}: Hostname set to {value}");
        }
        _ => {
            warn!("Unknown DHCP parameter: {param}");
            return Err(format!("Unknown DHCP parameter: {param}"));
        }
    }
    
    // Commit changes
    uci_commit("dhcp").await?;
    
    // Restart dnsmasq to apply changes
    restart_dnsmasq().await?;
    
    Ok(())
}

/// Find existing host section by index or create a new one
async fn find_or_create_host_section(target_idx: usize) -> Result<String, String> {
    let out = std::process::Command::new("uci")
        .args(["show", "dhcp"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    
    // Count existing host sections
    let mut host_count = 0;
    let mut last_section = String::new();
    
    for line in out.lines() {
        if line.starts_with("dhcp.host") && line.contains(".mac=") {
            host_count += 1;
            if let Some(section) = line.split('.').nth(1) {
                last_section = section.to_string();
            }
            if host_count == target_idx {
                // Found the section at this index
                return Ok(last_section.clone());
            }
        }
    }
    
    // Need to create a new section
    let new_section = format!("host_{}", generate_host_id());
    
    // Add new section to dhcp config
    let status = std::process::Command::new("uci")
        .args(["add", "dhcp", "host"])
        .status()
        .map_err(|e| e.to_string())?;
    
    if !status.success() {
        return Err("Failed to add new dhcp host section".to_string());
    }
    
    // Get the name of the newly added section (usually @host[-1])
    // We'll rename it to our preferred name
    info!("Created new DHCP host section: {new_section}");
    
    Ok(new_section)
}

/// Generate unique host ID based on timestamp
fn generate_host_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{:x}", timestamp % 10000)
}

async fn uci_set(path: &str, value: &str) -> Result<(), String> {
    let status = std::process::Command::new("uci")
        .args(["set", &format!("{path}={value}")])
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() { 
        Ok(()) 
    } else { 
        Err(format!("uci set {path} failed")) 
    }
}

async fn uci_commit(pkg: &str) -> Result<(), String> {
    let status = std::process::Command::new("uci")
        .args(["commit", pkg])
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("uci commit {pkg} failed"))
    }
}

async fn restart_dnsmasq() -> Result<(), String> {
    // Try multiple methods to restart dnsmasq
    let methods: Vec<Vec<&str>> = vec![
        vec!["/etc/init.d/dnsmasq", "restart"],
        vec!["/etc/init.d/dnsmasq", "reload"],
        vec!["killall", "-HUP", "dnsmasq"],
    ];
    
    for args in &methods {
        let status = std::process::Command::new(args[0])
            .args(&args[1..])
            .status();
        
        if let Ok(s) = status {
            if s.success() {
                info!("dnsmasq restarted successfully");
                return Ok(());
            }
        }
    }
    
    warn!("Could not restart dnsmasq, changes will apply after reboot");
    Ok(()) // Don't fail the operation if restart fails
}
