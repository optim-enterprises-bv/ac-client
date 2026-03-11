//! TR-181 Device.IP.Interface.* — reads/writes via UCI with multi-interface support.

use std::collections::HashMap;
use log::{info, warn};
use crate::config::ClientConfig;
use crate::usp::tp469::uci_backend::{uci_get, uci_set, uci_commit};

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
                if section == "globals" || section == "switch" || section == "loopback" {
                    continue;
                }
                
                if line.contains(".proto=") {
                    // Check if this is a new section
                    if section != current_section {
                        if !current_section.is_empty() && has_proto {
                            interfaces.push((current_section.clone(), current_section.clone()));
                        }
                        current_section = section.clone();
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

            let mut ip = uci_get(&format!("network.{section}.ipaddr"));
            let mut mask = uci_get(&format!("network.{section}.netmask"));
            let proto = uci_get(&format!("network.{section}.proto"));
            let mut gateway = uci_get(&format!("network.{section}.gateway"));
            let mut dns = uci_get(&format!("network.{section}.dns"));

            // For DHCP/dynamic protocols, get runtime state from ubus
            if ip.is_empty() || proto == "dhcp" || proto == "dhcpv6" || proto == "pppoe" {
                let rt = get_ubus_interface_status(section);
                if let Some(v) = rt.get("ipaddr") { if !v.is_empty() { ip = v.clone(); } }
                if let Some(v) = rt.get("netmask") { if !v.is_empty() { mask = v.clone(); } }
                if let Some(v) = rt.get("gateway") { if !v.is_empty() { gateway = v.clone(); } }
                if let Some(v) = rt.get("dns") { if !v.is_empty() { dns = v.clone(); } }
            }
            
            // Get interface name and stats
            let bridge_name = format!("br-{section}");
            let stats = get_interface_stats(&bridge_name).await;
            let mac = get_interface_mac(&bridge_name).await;
            let status = get_interface_status(&bridge_name).await;
            
            // Insert name
            m.insert(format!("Device.IP.Interface.{iface_idx}.X_OptimACS_Name"), section.clone());
            
            if !ip.is_empty() {
                m.insert(format!("{base}IPAddress"), ip);
            }
            if !mask.is_empty() {
                m.insert(format!("{base}SubnetMask"), mask);
            }
            if !proto.is_empty() {
                m.insert(format!("{base}AddressingType"), proto.clone());
            }
            if !gateway.is_empty() {
                m.insert(format!("Device.IP.Interface.{iface_idx}.X_OptimACS_Gateway"), gateway.clone());
            }
            if !dns.is_empty() {
                m.insert(format!("Device.IP.Interface.{iface_idx}.X_OptimACS_DNS"), dns);
            }
            if !mac.is_empty() {
                m.insert(format!("Device.IP.Interface.{iface_idx}.MACAddress"), mac);
            }
            m.insert(format!("Device.IP.Interface.{iface_idx}.Status"), status);

            // Upstream flag: true for wan/wan6 interfaces
            let is_upstream = section.starts_with("wan");
            m.insert(format!("Device.IP.Interface.{iface_idx}.X_OptimACS_Upstream"), is_upstream.to_string());

            // Friendly protocol name
            let proto_friendly = match proto.as_str() {
                "dhcp" => "DHCPv4",
                "dhcpv6" => "DHCPv6",
                "static" => "Static",
                "pppoe" => "PPPoE",
                "pptp" => "PPTP",
                "l2tp" => "L2TP",
                "none" => "Unmanaged",
                other => other,
            };
            m.insert(format!("Device.IP.Interface.{iface_idx}.X_OptimACS_Protocol"), proto_friendly.to_string());

            // GatewayIPv4 alias for Gateway
            if !gateway.is_empty() {
                m.insert(format!("Device.IP.Interface.{iface_idx}.X_OptimACS_GatewayIPv4"), gateway.clone());
            }

            // Add stats if available
            if let Some(rx_bytes) = stats.get("rx_bytes") {
                m.insert(format!("Device.IP.Interface.{iface_idx}.X_OptimACS_RXBytes"), rx_bytes.clone());
            }
            if let Some(rx_packets) = stats.get("rx_packets") {
                m.insert(format!("Device.IP.Interface.{iface_idx}.X_OptimACS_RXPackets"), rx_packets.clone());
            }
            if let Some(tx_bytes) = stats.get("tx_bytes") {
                m.insert(format!("Device.IP.Interface.{iface_idx}.X_OptimACS_TXBytes"), tx_bytes.clone());
            }
            if let Some(tx_packets) = stats.get("tx_packets") {
                m.insert(format!("Device.IP.Interface.{iface_idx}.X_OptimACS_TXPackets"), tx_packets.clone());
            }
            if let Some(uptime) = stats.get("uptime") {
                m.insert(format!("Device.IP.Interface.{iface_idx}.X_OptimACS_Uptime"), uptime.clone());
            }

            // IPv6 — always from runtime state
            let rt6 = get_ubus_interface_status(section);
            if let Some(v6) = rt6.get("ipv6addr") {
                if !v6.is_empty() {
                    m.insert(format!("Device.IP.Interface.{iface_idx}.IPv6Address.1.IPAddress"), v6.clone());
                    if let Some(prefix) = rt6.get("ipv6prefix") {
                        m.insert(format!("Device.IP.Interface.{iface_idx}.IPv6Address.1.PrefixLength"), prefix.clone());
                    }
                }
            }
        }
    } else if let Some(idx) = specific_idx {
        // Specific interface requested
        if idx > 0 && idx <= interfaces.len() {
            let (section, _name) = &interfaces[idx - 1];
            let base = format!("Device.IP.Interface.{idx}.IPv4Address.1.");
            let bridge_name = format!("br-{section}");

            let mut ip = uci_get(&format!("network.{section}.ipaddr"));
            let mut mask = uci_get(&format!("network.{section}.netmask"));
            let proto = uci_get(&format!("network.{section}.proto"));
            let mut gateway = uci_get(&format!("network.{section}.gateway"));
            let mut dns = uci_get(&format!("network.{section}.dns"));

            // For DHCP/dynamic protocols, get runtime state from ubus
            if ip.is_empty() || proto == "dhcp" || proto == "dhcpv6" || proto == "pppoe" {
                let rt = get_ubus_interface_status(section);
                if let Some(v) = rt.get("ipaddr") { if !v.is_empty() { ip = v.clone(); } }
                if let Some(v) = rt.get("netmask") { if !v.is_empty() { mask = v.clone(); } }
                if let Some(v) = rt.get("gateway") { if !v.is_empty() { gateway = v.clone(); } }
                if let Some(v) = rt.get("dns") { if !v.is_empty() { dns = v.clone(); } }
            }
            let stats = get_interface_stats(&bridge_name).await;
            let mac = get_interface_mac(&bridge_name).await;
            let status = get_interface_status(&bridge_name).await;
            
            // Insert name
            m.insert(format!("Device.IP.Interface.{idx}.X_OptimACS_Name"), section.clone());
            
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
                    m.insert(format!("{base}AddressingType"), proto.clone());
                }
                if !gateway.is_empty() {
                    m.insert(format!("Device.IP.Interface.{idx}.X_OptimACS_Gateway"), gateway.clone());
                }
                if !dns.is_empty() {
                    m.insert(format!("Device.IP.Interface.{idx}.X_OptimACS_DNS"), dns);
                }
                if !mac.is_empty() {
                    m.insert(format!("Device.IP.Interface.{idx}.MACAddress"), mac);
                }
                m.insert(format!("Device.IP.Interface.{idx}.Status"), status);

                // Upstream flag
                let is_upstream = section.starts_with("wan");
                m.insert(format!("Device.IP.Interface.{idx}.X_OptimACS_Upstream"), is_upstream.to_string());

                // Friendly protocol name
                let proto_friendly = match proto.as_str() {
                    "dhcp" => "DHCPv4",
                    "dhcpv6" => "DHCPv6",
                    "static" => "Static",
                    "pppoe" => "PPPoE",
                    "pptp" => "PPTP",
                    "l2tp" => "L2TP",
                    "none" => "Unmanaged",
                    other => other,
                };
                m.insert(format!("Device.IP.Interface.{idx}.X_OptimACS_Protocol"), proto_friendly.to_string());

                // GatewayIPv4 alias
                if !gateway.is_empty() {
                    m.insert(format!("Device.IP.Interface.{idx}.X_OptimACS_GatewayIPv4"), gateway.clone());
                }

                // Add stats if available
                if let Some(rx_bytes) = stats.get("rx_bytes") {
                    m.insert(format!("Device.IP.Interface.{idx}.X_OptimACS_RXBytes"), rx_bytes.clone());
                }
                if let Some(rx_packets) = stats.get("rx_packets") {
                    m.insert(format!("Device.IP.Interface.{idx}.X_OptimACS_RXPackets"), rx_packets.clone());
                }
                if let Some(tx_bytes) = stats.get("tx_bytes") {
                    m.insert(format!("Device.IP.Interface.{idx}.X_OptimACS_TXBytes"), tx_bytes.clone());
                }
                if let Some(tx_packets) = stats.get("tx_packets") {
                    m.insert(format!("Device.IP.Interface.{idx}.X_OptimACS_TXPackets"), tx_packets.clone());
                }
                if let Some(uptime) = stats.get("uptime") {
                    m.insert(format!("Device.IP.Interface.{idx}.X_OptimACS_Uptime"), uptime.clone());
                }

                // IPv6 — always from runtime state
                let rt6 = get_ubus_interface_status(section);
                if let Some(v6) = rt6.get("ipv6addr") {
                    if !v6.is_empty() {
                        m.insert(format!("Device.IP.Interface.{idx}.IPv6Address.1.IPAddress"), v6.clone());
                        if let Some(prefix) = rt6.get("ipv6prefix") {
                            m.insert(format!("Device.IP.Interface.{idx}.IPv6Address.1.PrefixLength"), prefix.clone());
                        }
                    }
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

/// Query `ubus call network.interface.<name> status` for runtime IP state.
/// Returns a map with keys: ipaddr, netmask, gateway, dns.
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
            let after = after.trim_start_matches(|c: char| !c.is_ascii_digit());
            if let Some(end) = after.find(|c: char| !c.is_ascii_digit()) {
                if let Ok(cidr) = after[..end].parse::<u32>() {
                    result.insert("netmask".to_string(), cidr_to_netmask(cidr));
                }
            }
        }
    }

    // Parse default route gateway
    if let Some(pos) = out.find("\"route\"") {
        let chunk = &out[pos..];
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

    // Parse ipv6-address[0].address and mask
    if let Some(pos) = out.find("\"ipv6-address\"") {
        let chunk = &out[pos..];
        if let Some(addr_pos) = chunk.find("\"address\"") {
            let after = &chunk[addr_pos + 9..];
            if let Some(start) = after.find('"') {
                let rest = &after[start + 1..];
                if let Some(end) = rest.find('"') {
                    result.insert("ipv6addr".to_string(), rest[..end].to_string());
                }
            }
        }
        if let Some(mask_pos) = chunk.find("\"mask\"") {
            let after = &chunk[mask_pos + 5..];
            let after = after.trim_start_matches(|c: char| !c.is_ascii_digit());
            if let Some(end) = after.find(|c: char| !c.is_ascii_digit()) {
                result.insert("ipv6prefix".to_string(), after[..end].to_string());
            }
        }
    }

    // Parse ipv6-prefix-assignment for delegated prefix
    if let Some(pos) = out.find("\"ipv6-prefix-assignment\"") {
        let chunk = &out[pos..];
        if let Some(addr_pos) = chunk.find("\"address\"") {
            let after = &chunk[addr_pos + 9..];
            if let Some(start) = after.find('"') {
                let rest = &after[start + 1..];
                if let Some(end) = rest.find('"') {
                    let addr = &rest[..end];
                    if !addr.is_empty() {
                        result.insert("ipv6prefix_addr".to_string(), addr.to_string());
                    }
                }
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

/// Get interface statistics from /proc/net/dev
async fn get_interface_stats(iface: &str) -> HashMap<String, String> {
    let mut stats = HashMap::new();
    
    // Read /proc/net/dev
    if let Ok(content) = tokio::fs::read_to_string("/proc/net/dev").await {
        for line in content.lines() {
            if line.contains(iface) {
                // Parse line like: "  br-lan: 123456789 1234567    0    0    0     0          0         0 987654321 9876543    0    0    0    0       0          0"
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 9 {
                    if let Ok(rx_bytes) = parts[1].parse::<u64>() {
                        stats.insert("rx_bytes".to_string(), format_bytes(rx_bytes));
                    }
                    if let Ok(rx_packets) = parts[2].parse::<u64>() {
                        stats.insert("rx_packets".to_string(), format_number(rx_packets));
                    }
                    if let Ok(tx_bytes) = parts[9].parse::<u64>() {
                        stats.insert("tx_bytes".to_string(), format_bytes(tx_bytes));
                    }
                    if let Ok(tx_packets) = parts[10].parse::<u64>() {
                        stats.insert("tx_packets".to_string(), format_number(tx_packets));
                    }
                }
                break;
            }
        }
    }
    
    // Get interface uptime from /sys/class/net/{iface}/operstate or similar
    if let Ok(content) = tokio::fs::read_to_string(format!("/sys/class/net/{}/operstate", iface)).await {
        let state = content.trim();
        if state == "up" {
            // Try to get carrier uptime if available
            if let Ok(carrier) = tokio::fs::read_to_string(format!("/sys/class/net/{}/carrier_up_time", iface)).await {
                let seconds = carrier.trim().parse::<u64>().unwrap_or(0);
                stats.insert("uptime".to_string(), format_duration(seconds));
            }
        }
    }
    
    stats
}

/// Get MAC address from /sys/class/net/{iface}/address
async fn get_interface_mac(iface: &str) -> String {
    if let Ok(content) = tokio::fs::read_to_string(format!("/sys/class/net/{}/address", iface)).await {
        content.trim().to_string()
    } else {
        String::new()
    }
}

/// Get interface operational status
async fn get_interface_status(iface: &str) -> String {
    if let Ok(content) = tokio::fs::read_to_string(format!("/sys/class/net/{}/operstate", iface)).await {
        match content.trim() {
            "up" => "Up",
            "down" => "Down",
            _ => "Unknown",
        }.to_string()
    } else {
        "Down".to_string()
    }
}

/// Return raw byte count as a string (UI handles formatting)
fn format_bytes(bytes: u64) -> String {
    bytes.to_string()
}

/// Format number with commas
fn format_number(num: u64) -> String {
    num.to_string()
}

/// Format seconds to human-readable duration
fn format_duration(seconds: u64) -> String {
    let days = seconds / 86400;
    let hours = (seconds % 86400) / 3600;
    let mins = (seconds % 3600) / 60;
    let secs = seconds % 60;
    
    if days > 0 {
        format!("{}d {}h {}m {}s", days, hours, mins, secs)
    } else if hours > 0 {
        format!("{}h {}m {}s", hours, mins, secs)
    } else if mins > 0 {
        format!("{}m {}s", mins, secs)
    } else {
        format!("{}s", secs)
    }
}
