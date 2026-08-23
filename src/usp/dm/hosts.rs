//! TR-181 Device.Hosts.Host.* — reads/writes via UCI dnsmasq and /etc/hosts.

#![allow(clippy::all)]

use crate::config::ClientConfig;
use crate::usp::tp469::uci_backend::uci_commit;
use log::{info, warn};
use std::collections::HashMap;

fn uci_add_list(path: &str, value: &str) -> Result<(), String> {
    let status = std::process::Command::new("uci")
        .args(["add_list", &format!("{path}={value}")])
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("uci add_list {path} failed"))
    }
}

fn uci_delete(path: &str) -> Result<(), String> {
    let status = std::process::Command::new("uci")
        .args(["delete", path])
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("uci delete {path} failed"))
    }
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

/// One entry in the LAN host table.
struct Host {
    mac: String,
    ip: String,
    hostname: String,
    active: bool,
    source: &'static str,
}

/// The bridge carrying LAN clients, e.g. `br-lan`.
///
/// Used to keep the WAN side out of the host table: the upstream gateway shows
/// up in the neighbour table like any other peer, and reporting it as a LAN
/// host would attribute its traffic to this subscriber.
fn lan_device() -> String {
    let out = std::process::Command::new("uci")
        .args(["get", "network.lan.device"])
        .output()
        .ok();
    let dev = out
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    if dev.is_empty() {
        "br-lan".to_string()
    } else {
        dev
    }
}

/// DHCP leases: `<expiry> <mac> <ip> <hostname> <clientid>`.
///
/// Authoritative for the MAC/hostname pairing. dnsmasq writes `*` when the
/// client offered no hostname.
fn leases() -> Vec<Host> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut out = Vec::new();
    for path in ["/tmp/dhcp.leases", "/var/dhcp.leases"] {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for line in text.lines() {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 4 {
                continue;
            }
            let expiry: u64 = f[0].parse().unwrap_or(0);
            out.push(Host {
                mac: f[1].to_ascii_lowercase(),
                ip: f[2].to_string(),
                // `*` means the client sent no hostname; report empty rather
                // than a literal asterisk.
                hostname: if f[3] == "*" {
                    String::new()
                } else {
                    f[3].to_string()
                },
                // expiry 0 means an infinite lease.
                active: expiry == 0 || expiry > now,
                source: "DHCP",
            });
        }
        break;
    }
    out
}

/// ARP/neighbour entries on the LAN bridge.
///
/// Picks up statically-addressed clients that never took a lease, which a
/// lease-only view would miss entirely.
fn neighbours(lan: &str) -> Vec<Host> {
    let Ok(text) = std::fs::read_to_string("/proc/net/arp") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 6 {
            continue;
        }
        let (ip, flags, mac, dev) = (f[0], f[2], f[3].to_ascii_lowercase(), f[5]);
        // flags 0x0 is an incomplete entry: an address we probed and never
        // heard from. Reporting it would invent a client.
        if dev != lan || flags == "0x0" || mac == "00:00:00:00:00:00" {
            continue;
        }
        out.push(Host {
            mac,
            ip: ip.to_string(),
            hostname: String::new(),
            active: true,
            source: "ARP",
        });
    }
    out
}

/// TR-181 `Device.Hosts.` — the LAN host table.
///
/// Previously this reported `/etc/hosts` plus dnsmasq's static DNS entries,
/// which on a default OpenWrt box means four loopback and IPv6 multicast rows
/// and nothing else. It also never emitted `PhysAddress` at all.
///
/// That matters beyond tidiness: a controller resolves a client's identity from
/// this table, and DPI or policy data keyed on a MAC it has never seen through
/// an independent source cannot be attributed to anyone. A host table without
/// MACs is not a partial answer, it is an unusable one.
///
/// Real sources, merged by MAC: DHCP leases first (they carry the hostname),
/// then ARP for anything statically addressed that never took a lease.
pub async fn get(_cfg: &ClientConfig, _path: &str) -> HashMap<String, String> {
    let lan = lan_device();
    let mut hosts: Vec<Host> = leases();

    for n in neighbours(&lan) {
        if let Some(existing) = hosts.iter_mut().find(|h| h.mac == n.mac) {
            // A live ARP entry is better evidence of presence than a lease that
            // merely has not expired.
            existing.active = true;
            if existing.ip.is_empty() {
                existing.ip = n.ip;
            }
        } else {
            hosts.push(n);
        }
    }

    let mut m = HashMap::new();
    for (i, h) in hosts.iter().enumerate() {
        let base = format!("Device.Hosts.Host.{}.", i + 1);
        m.insert(format!("{base}PhysAddress"), h.mac.clone());
        m.insert(format!("{base}IPAddress"), h.ip.clone());
        m.insert(format!("{base}HostName"), h.hostname.clone());
        m.insert(format!("{base}Active"), h.active.to_string());
        m.insert(format!("{base}AddressSource"), h.source.to_string());
        m.insert(format!("{base}InterfaceType"), "Ethernet".to_string());
    }
    m.insert(
        "Device.Hosts.HostNumberOfEntries".into(),
        hosts.len().to_string(),
    );
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
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let ip = parts.next().unwrap_or("");
        let hostname = parts.next().unwrap_or("");
        if !ip.is_empty() && !hostname.is_empty() {
            hosts_entries.push((ip.to_string(), hostname.to_string()));
        }
    }

    // Total entries = hosts entries + DNS entries
    let total_entries = hosts_entries.len() + dns_entries.len();

    if idx == 0 || idx > total_entries {
        return Err(format!(
            "Host index {idx} out of range (total: {total_entries})"
        ));
    }

    // Determine if this is a hosts entry or DNS entry
    let is_dns_entry = idx > hosts_entries.len();
    let effective_idx = if is_dns_entry {
        idx - hosts_entries.len()
    } else {
        idx
    };

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

    for (_i, line) in lines.iter_mut().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

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
