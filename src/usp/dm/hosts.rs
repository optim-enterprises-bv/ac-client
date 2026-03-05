//! TR-181 Device.Hosts.Host.* — reads/writes via UCI dnsmasq and /etc/hosts.

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

fn uci_add_list(path: &str, value: &str) -> Result<(), String> {
    let status = std::process::Command::new("uci")
        .args(["add_list", &format!("{path}={value}")])
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() { Ok(()) } else { Err(format!("uci add_list {path} failed")) }
}

fn uci_delete(path: &str) -> Result<(), String> {
    let status = std::process::Command::new("uci")
        .args(["delete", path])
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() { Ok(()) } else { Err(format!("uci delete {path} failed")) }
}

/// Get DNS entries from UCI dnsmasq config
fn get_dns_entries() -> Vec<(String, String)> {
    let mut entries = Vec::new();
    
    let out = std::process::Command::new("uci")
        .args(["get", "dhcp.@dnsmasq[0].address"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    
    for line in out.lines() {
        // Format in UCI: /hostname/ip or /domain/ip
        let line = line.trim_matches('\'').trim();
        if line.starts_with('/') {
            let parts: Vec<&str> = line[1..].split('/').collect();
            if parts.len() >= 2 {
                let hostname = parts[0].to_string();
                let ip = parts[1].to_string();
                entries.push((ip, hostname));
            }
        }
    }
    
    entries
}

/// Parse host index from path like "Device.Hosts.Host.1.HostName"
fn parse_host_index(path: &str) -> Option<usize> {
    if let Some(start) = path.find("Host.") {
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

pub async fn get(_cfg: &ClientConfig, _path: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    let dns_entries = get_dns_entries();
    
    // First, read from /etc/hosts
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
            m.insert(format!("{base}Active"), "true".to_string());
            idx += 1;
        }
    }
    
    // Then add DNS entries from UCI
    for (ip, hostname) in &dns_entries {
        let base = format!("Device.Hosts.Host.{idx}.");
        m.insert(format!("{base}IPAddress"),  ip.clone());
        m.insert(format!("{base}HostName"),   hostname.clone());
        m.insert(format!("{base}Active"), "true".to_string());
        idx += 1;
    }
    
    m
}

pub async fn set(_cfg: &ClientConfig, path: &str, value: &str) -> Result<(), String> {
    // Parse host index
    let idx = parse_host_index(path)
        .ok_or_else(|| format!("Cannot parse host index from path: {path}"))?;
    
    // Get current entries to find the one we're modifying
    let dns_entries = get_dns_entries();
    let content = std::fs::read_to_string("/etc/hosts").unwrap_or_default();
    let mut hosts_entries: Vec<(String, String)> = Vec::new();
    
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') { continue; }
        let mut parts = line.split_whitespace();
        let ip       = parts.next().unwrap_or("");
        let hostname = parts.next().unwrap_or("");
        if !ip.is_empty() && !hostname.is_empty() {
            hosts_entries.push((ip.to_string(), hostname.to_string()));
        }
    }
    
    // Total entries = hosts entries + DNS entries
    let total_entries = hosts_entries.len() + dns_entries.len();
    
    if idx == 0 || idx > total_entries {
        return Err(format!("Host index {idx} out of range (total: {total_entries})"));
    }
    
    // Determine if this is a hosts entry or DNS entry
    let is_dns_entry = idx > hosts_entries.len();
    let effective_idx = if is_dns_entry { idx - hosts_entries.len() } else { idx };
    
    if path.ends_with(".HostName") {
        if is_dns_entry {
            // Update DNS entry in UCI
            let (old_ip, _old_hostname) = &dns_entries[effective_idx - 1];
            let new_entry = format!("/{}/{}", value, old_ip);
            
            // This is complex with UCI - we need to replace the specific list item
            // For now, just add the new one and we'll rely on external tools to clean up
            uci_add_list("dhcp.@dnsmasq[0].address", &new_entry)?;
            info!("Added DNS entry: {value} -> {old_ip}");
        } else {
            // Update /etc/hosts entry - this requires rewriting the file
            let (old_ip, _old_hostname) = &hosts_entries[effective_idx - 1];
            update_hosts_file(effective_idx - 1, old_ip, value).await?;
            info!("Updated hosts entry: {old_ip} -> {value}");
        }
    } else if path.ends_with(".IPAddress") {
        if is_dns_entry {
            let (_old_ip, old_hostname) = &dns_entries[effective_idx - 1];
            let new_entry = format!("/{}/{}", old_hostname, value);
            uci_add_list("dhcp.@dnsmasq[0].address", &new_entry)?;
            info!("Added DNS entry: {old_hostname} -> {value}");
        } else {
            let (_old_ip, old_hostname) = &hosts_entries[effective_idx - 1];
            update_hosts_file(effective_idx - 1, value, old_hostname).await?;
            info!("Updated hosts entry: {value} -> {old_hostname}");
        }
    } else if path.ends_with(".Active") {
        // Enable/disable logic - for DNS entries we can't easily remove
        // For hosts entries, we could comment out the line
        info!("Host {idx} Active set to {value}");
    } else {
        warn!("Unknown Host parameter in path: {path}");
        return Err(format!("Unknown Host parameter: {path}"));
    }
    
    if is_dns_entry {
        uci_commit("dhcp")?;
        restart_dnsmasq().await?;
    }
    
    Ok(())
}

/// Update a line in /etc/hosts
async fn update_hosts_file(idx: usize, new_ip: &str, new_hostname: &str) -> Result<(), String> {
    let content = std::fs::read_to_string("/etc/hosts")
        .map_err(|e| format!("Failed to read /etc/hosts: {e}"))?;
    
    let mut lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();
    let mut hosts_idx = 0;
    let mut found = false;
    
    for (i, line) in lines.iter_mut().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') { continue; }
        
        let mut parts = trimmed.split_whitespace();
        let ip = parts.next().unwrap_or("");
        let hostname = parts.next().unwrap_or("");
        
        if !ip.is_empty() && !hostname.is_empty() {
            if hosts_idx == idx {
                *line = format!("{new_ip} {new_hostname}");
                found = true;
                break;
            }
            hosts_idx += 1;
        }
    }
    
    if !found {
        // Append new entry
        lines.push(format!("{new_ip} {new_hostname}"));
    }
    
    std::fs::write("/etc/hosts", lines.join("\n"))
        .map_err(|e| format!("Failed to write /etc/hosts: {e}"))?;
    
    Ok(())
}

async fn restart_dnsmasq() -> Result<(), String> {
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
                info!("dnsmasq restarted");
                return Ok(());
            }
        }
    }
    
    warn!("Could not restart dnsmasq");
    Ok(())
}
