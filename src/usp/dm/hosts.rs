//! TR-181 Device.Hosts.Host.* — read-only table of discovered/connected hosts.
//!
//! Per TR-181, Device.Hosts.Host.{i}. is a READ-ONLY table reflecting hosts
//! discovered via ARP, DHCP leases, and neighbour discovery.  It is NOT a
//! configurable DNS host list.  SET operations on this object are not permitted.

#![allow(clippy::all)]

use crate::config::ClientConfig;
use std::collections::HashMap;

/// Read connected hosts from the ARP table (/proc/net/arp) combined with
/// DHCP lease information from /tmp/dhcp.leases.
pub async fn get(_cfg: &ClientConfig, _path: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    let mut idx = 1u32;

    // Primary source: DHCP lease file — contains IP, MAC, hostname, expiry.
    // Format: <expiry> <mac> <ip> <hostname> <client-id>
    let leases = std::fs::read_to_string("/tmp/dhcp.leases").unwrap_or_default();
    let mut seen_macs: std::collections::HashSet<String> = std::collections::HashSet::new();

    for line in leases.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 4 {
            continue;
        }
        let expiry_str = parts[0];
        let mac = parts[1].to_uppercase();
        let ip = parts[2];
        let hostname = if parts[3] == "*" { "" } else { parts[3] };

        // Determine Active: if expiry is 0 it's a static lease (always active),
        // otherwise check whether expiry is in the future.
        let active = if expiry_str == "0" {
            true
        } else {
            let expiry: u64 = expiry_str.parse().unwrap_or(0);
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            expiry > now
        };

        let base = format!("Device.Hosts.Host.{idx}.");
        m.insert(format!("{base}IPAddress"), ip.to_string());
        m.insert(format!("{base}MACAddress"), mac.clone());
        if !hostname.is_empty() {
            m.insert(format!("{base}HostName"), hostname.to_string());
        }
        m.insert(format!("{base}Active"), active.to_string());
        m.insert(format!("{base}AddressSource"), "DHCP".to_string());
        // Enrich with VendorClassID from DHCP snoop if available.
        if let Some(vc) = crate::dhcp_snoop::table()
            .and_then(|t| t.lock().ok())
            .and_then(|tbl| tbl.get(&mac).and_then(|fp| fp.vendor_class.clone()))
        {
            m.insert(format!("{base}VendorClassID"), vc);
        }
        // Layer references — DHCP leases don't specify the interface directly,
        // but on OpenWrt the LAN bridge (br-lan) serves DHCP clients.
        m.insert(
            format!("{base}Layer1Interface"),
            "Device.Bridging.Bridge.1.".to_string(),
        );
        m.insert(
            format!("{base}Layer3Interface"),
            "Device.IP.Interface.1.".to_string(),
        );
        seen_macs.insert(mac);
        idx += 1;
    }

    // Secondary source: ARP table for hosts not in DHCP leases (statically
    // configured or discovered via ARP without a DHCP lease).
    if let Ok(arp) = std::fs::read_to_string("/proc/net/arp") {
        for line in arp.lines().skip(1) {
            let parts: Vec<&str> = line.split_whitespace().collect();
            // Fields: IP HW_TYPE FLAGS MAC MASK IFACE
            if parts.len() < 6 {
                continue;
            }
            let ip = parts[0];
            let mac_raw = parts[3];
            // Skip incomplete entries
            if mac_raw == "00:00:00:00:00:00" || mac_raw == "<incomplete>" {
                continue;
            }
            let mac = mac_raw.to_uppercase();
            if seen_macs.contains(&mac) {
                continue;
            }
            // Active flag from ARP flags field (0x2 = completed)
            let flags: u32 = u32::from_str_radix(parts[2].trim_start_matches("0x"), 16)
                .unwrap_or(0);
            let active = flags & 0x2 != 0;

            let base = format!("Device.Hosts.Host.{idx}.");
            m.insert(format!("{base}IPAddress"), ip.to_string());
            m.insert(format!("{base}MACAddress"), mac.clone());
            m.insert(format!("{base}Active"), active.to_string());
            m.insert(format!("{base}AddressSource"), "None".to_string());
            // ARP provides the interface name (parts[5])
            let iface = parts[5];
            if iface.starts_with("br-") {
                m.insert(
                    format!("{base}Layer1Interface"),
                    "Device.Bridging.Bridge.1.".to_string(),
                );
            } else if iface.starts_with("eth") {
                m.insert(
                    format!("{base}Layer1Interface"),
                    format!("Device.Ethernet.Interface.1."),
                );
            }
            m.insert(
                format!("{base}Layer3Interface"),
                "Device.IP.Interface.1.".to_string(),
            );
            seen_macs.insert(mac);
            idx += 1;
        }
    }

    // HostNumberOfEntries at the parent object level
    m.insert(
        "Device.Hosts.HostNumberOfEntries".to_string(),
        (idx - 1).to_string(),
    );

    m
}

/// Device.Hosts.Host.{i}. is read-only per TR-181.
/// Controllers must not SET parameters on this object.
pub async fn set(_cfg: &ClientConfig, path: &str, _value: &str) -> Result<(), String> {
    Err(format!(
        "{path} is read-only: Device.Hosts.Host.{{i}}. reflects discovered hosts and cannot be written"
    ))
}
