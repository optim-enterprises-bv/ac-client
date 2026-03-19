//! TR-181 Device.DHCPv4.Server.Pool.* — reads/writes via UCI.

#![allow(clippy::all)]

use crate::config::ClientConfig;
use log::{info, warn};
use std::collections::HashMap;

/// UCI helper — read a single value, returning None if empty/missing
fn uci_get_raw(key: &str) -> Option<String> {
    let out = std::process::Command::new("uci")
        .args(["get", key])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    out
}

/// Get DHCP pool sections that have a 'start' option (real pools)
fn get_dhcp_pools() -> Vec<String> {
    let out = std::process::Command::new("uci")
        .args(["show", "dhcp"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    let mut pools = Vec::new();
    for line in out.lines() {
        if line.starts_with("dhcp.") && line.contains(".start=") {
            if let Some(section) = line.split('.').nth(1) {
                if !pools.contains(&section.to_string()) {
                    pools.push(section.to_string());
                }
            }
        }
    }
    if pools.is_empty() {
        pools.push("lan".to_string());
    }
    pools
}

/// Get DHCP parameters — pool config + static leases
pub async fn get(_cfg: &ClientConfig, path: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    let pools = get_dhcp_pools();

    // ── Pool-level config ──
    if path.contains("PoolNumberOfEntries") {
        m.insert(path.to_string(), pools.len().to_string());
        return m;
    }

    if path.contains("Server.Pool.") {
        let pool_idx: usize = path
            .split("Pool.")
            .nth(1)
            .and_then(|s| s.split('.').next())
            .and_then(|s| s.parse().ok())
            .unwrap_or(1);
        let pool_name = pools
            .get(pool_idx - 1)
            .cloned()
            .unwrap_or_else(|| "lan".to_string());

        if path.ends_with("Enable") {
            let ignore = uci_get_raw(&format!("dhcp.{pool_name}.ignore")).unwrap_or_default();
            m.insert(path.to_string(), (ignore != "1").to_string());
        } else if path.ends_with("Status") {
            let ignore = uci_get_raw(&format!("dhcp.{pool_name}.ignore")).unwrap_or_default();
            m.insert(
                path.to_string(),
                if ignore == "1" { "Disabled" } else { "Enabled" }.to_string(),
            );
        } else if path.ends_with("MinAddress") {
            // TR-181 MinAddress must be a dotted-decimal IPv4 address.
            // UCI dhcp.{pool}.start is an offset from the network base IP.
            let iface = uci_get_raw(&format!("dhcp.{pool_name}.interface"))
                .unwrap_or_else(|| pool_name.clone());
            let base_ip = uci_get_raw(&format!("network.{iface}.ipaddr"))
                .unwrap_or_else(|| "192.168.1.0".to_string());
            let start_offset: u32 = uci_get_raw(&format!("dhcp.{pool_name}.start"))
                .and_then(|s| s.parse().ok())
                .unwrap_or(100);
            m.insert(path.to_string(), offset_ip(&base_ip, start_offset));
        } else if path.ends_with("MaxAddress") {
            // TR-181 MaxAddress must be a dotted-decimal IPv4 address.
            // UCI dhcp.{pool}.limit is a count of addresses in the range.
            let iface = uci_get_raw(&format!("dhcp.{pool_name}.interface"))
                .unwrap_or_else(|| pool_name.clone());
            let base_ip = uci_get_raw(&format!("network.{iface}.ipaddr"))
                .unwrap_or_else(|| "192.168.1.0".to_string());
            let start_offset: u32 = uci_get_raw(&format!("dhcp.{pool_name}.start"))
                .and_then(|s| s.parse().ok())
                .unwrap_or(100);
            let limit: u32 = uci_get_raw(&format!("dhcp.{pool_name}.limit"))
                .and_then(|s| s.parse().ok())
                .unwrap_or(150);
            m.insert(
                path.to_string(),
                offset_ip(&base_ip, start_offset + limit - 1),
            );
        } else if path.ends_with("SubnetMask") {
            let iface = uci_get_raw(&format!("dhcp.{pool_name}.interface"))
                .unwrap_or_else(|| pool_name.clone());
            let mask = uci_get_raw(&format!("network.{iface}.netmask"))
                .unwrap_or_else(|| "255.255.255.0".to_string());
            m.insert(path.to_string(), mask);
        } else if path.ends_with("DomainName") {
            let domain =
                uci_get_raw("dhcp.@dnsmasq[0].domain").unwrap_or_else(|| "lan".to_string());
            m.insert(path.to_string(), domain);
        } else if path.ends_with("LeaseTime") {
            // TR-181 LeaseTime is an int in seconds.  UCI stores human-readable
            // strings like "12h", "30m", "infinite".
            let lt_str = uci_get_raw(&format!("dhcp.{pool_name}.leasetime"))
                .unwrap_or_else(|| "12h".to_string());
            let lt_secs = parse_leasetime(&lt_str);
            m.insert(path.to_string(), lt_secs.to_string());
        } else if path.ends_with("LeaseNumberOfEntries") {
            let count = std::fs::read_to_string("/tmp/dhcp.leases")
                .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count())
                .unwrap_or(0);
            m.insert(path.to_string(), count.to_string());
        } else if path.ends_with("StaticAddressNumberOfEntries") {
            let out = std::process::Command::new("uci")
                .args(["show", "dhcp"])
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .unwrap_or_default();
            let count = out
                .lines()
                .filter(|l| l.contains("@host[") && l.contains(".mac="))
                .count();
            m.insert(path.to_string(), count.to_string());
        } else if path.ends_with("Interface") {
            let iface = uci_get_raw(&format!("dhcp.{pool_name}.interface"))
                .unwrap_or_else(|| pool_name.clone());
            m.insert(path.to_string(), iface);
        } else if path.ends_with("DNSServers") {
            let dns = uci_get_raw(&format!("dhcp.{pool_name}.dhcp_option")).unwrap_or_default();
            let servers: String = dns
                .split_whitespace()
                .filter(|o| o.starts_with("6,"))
                .map(|o| o.trim_start_matches("6,"))
                .collect::<Vec<&str>>()
                .join(",");
            m.insert(path.to_string(), servers);
        } else if path.contains("Client.") {
            // Active DHCP leases from /tmp/dhcp.leases
            // Format: <expiry_epoch> <mac> <ip> <hostname> <duid>
            let leases = get_active_leases();
            for (li, lease) in leases.iter().enumerate() {
                let ci = li + 1;
                let base = format!("Device.DHCPv4.Server.Pool.{pool_idx}.Client.{ci}");
                m.insert(format!("{base}.Chaddr"), lease.mac.clone());
                m.insert(
                    format!("{base}.IPv4Address.1.IPAddress"),
                    lease.ip.clone(),
                );
                // LeaseTimeRemaining belongs on the IPv4Address sub-object per TR-181.
                m.insert(
                    format!("{base}.IPv4Address.1.LeaseTimeRemaining"),
                    lease.remaining.clone(),
                );
                if !lease.hostname.is_empty() && lease.hostname != "*" {
                    m.insert(
                        format!("{base}.X_OptimACS_Hostname"),
                        lease.hostname.clone(),
                    );
                }
            }
        } else if path.contains("StaticAddress.") {
            // Static lease query — original logic
            let uci_out = std::process::Command::new("uci")
                .args(["show", "dhcp"])
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .unwrap_or_default();
            let mut idx = 1u32;
            for line in uci_out.lines() {
                if line.contains("host.") && line.contains(".mac=") {
                    let section = line.split('.').nth(1).unwrap_or("").to_string();
                    let mac = line
                        .split('=')
                        .nth(1)
                        .unwrap_or("")
                        .trim_matches('\'')
                        .to_string();
                    let ip = uci_out
                        .lines()
                        .find(|l| l.contains(&format!("dhcp.{section}.ip=")))
                        .and_then(|l| l.split('=').nth(1))
                        .unwrap_or("")
                        .trim_matches('\'')
                        .to_string();
                    let name = uci_out
                        .lines()
                        .find(|l| l.contains(&format!("dhcp.{section}.name=")))
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
        }
    }

    m
}

/// Compute an IPv4 address by adding an offset to a base IP.
/// Returns the dotted-decimal address required by TR-181 MinAddress/MaxAddress.
fn offset_ip(base: &str, offset: u32) -> String {
    let parts: Vec<u32> = base
        .split('.')
        .filter_map(|s| s.parse().ok())
        .collect();
    if parts.len() == 4 {
        let base_int: u32 =
            (parts[0] << 24) | (parts[1] << 16) | (parts[2] << 8) | parts[3];
        let result = base_int.wrapping_add(offset);
        format!(
            "{}.{}.{}.{}",
            (result >> 24) & 0xFF,
            (result >> 16) & 0xFF,
            (result >> 8) & 0xFF,
            result & 0xFF,
        )
    } else {
        base.to_string()
    }
}

/// Parse UCI leasetime string to seconds.
/// Examples: "12h" → 43200, "30m" → 1800, "infinite" → 2147483647
fn parse_leasetime(s: &str) -> i64 {
    if s == "infinite" || s == "0" {
        return i64::MAX / 2; // effectively infinite per TR-181 convention
    }
    let s = s.trim();
    if let Some(h) = s.strip_suffix('h') {
        h.parse::<i64>().unwrap_or(12) * 3600
    } else if let Some(m) = s.strip_suffix('m') {
        m.parse::<i64>().unwrap_or(30) * 60
    } else if let Some(d) = s.strip_suffix('d') {
        d.parse::<i64>().unwrap_or(1) * 86400
    } else {
        s.parse::<i64>().unwrap_or(43200)
    }
}

struct DhcpLease {
    mac: String,
    ip: String,
    hostname: String,
    remaining: String,
}

/// Parse active DHCP leases from /tmp/dhcp.leases
fn get_active_leases() -> Vec<DhcpLease> {
    let content = std::fs::read_to_string("/tmp/dhcp.leases").unwrap_or_default();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() >= 4 {
                let expiry = fields[0].parse::<u64>().unwrap_or(0);
                let remaining = if expiry > now { expiry - now } else { 0 };
                Some(DhcpLease {
                    mac: fields[1].to_uppercase(),
                    ip: fields[2].to_string(),
                    hostname: fields[3].to_string(),
                    remaining: remaining.to_string(),
                })
            } else {
                None
            }
        })
        .collect()
}

/// Set DHCP static lease parameters (Chaddr/MAC or Yiaddr/IP)
pub async fn set(_cfg: &ClientConfig, path: &str, value: &str) -> Result<(), String> {
    // Parse path: Device.DHCPv4.Server.Pool.1.StaticAddress.{idx}.{Param}
    let parts: Vec<&str> = path.split('.').collect();
    if parts.len() < 7 {
        return Err(format!("Invalid DHCP path: {path}"));
    }

    let idx_str = parts[5]; // {idx}
    let param = parts[6]; // Chaddr, Yiaddr, or X_OptimACS_Hostname
    let idx: usize = idx_str
        .parse()
        .map_err(|_| format!("Invalid index: {idx_str}"))?;

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
