//! TR-181 miscellaneous paths — stub implementations for data model completeness
//! Returns empty/default values for paths not yet fully implemented

use std::collections::HashMap;
use log::debug;
use crate::config::ClientConfig;

pub type Params = HashMap<String, String>;

/// Get miscellaneous TR-181 parameters (stub implementations)
pub async fn get(_cfg: &ClientConfig, path: &str) -> Params {
    let mut result = Params::new();
    
    // Device.IP.InterfaceNumberOfEntries
    if path.contains("InterfaceNumberOfEntries") {
        result.insert(path.to_string(), "4".to_string());
        return result;
    }
    
    // Device.DNS.Client
    if path.starts_with("Device.DNS.Client.") {
        return handle_dns(path);
    }
    
    // Device.DNS.Relay
    if path.starts_with("Device.DNS.Relay.") {
        result.insert(path.to_string(), "false".to_string());
        return result;
    }
    
    // Device.Routing.Router.1.IPv4Forwarding
    if path.starts_with("Device.Routing.Router.") {
        return handle_routing(path);
    }
    
    // Device.NAT
    if path.starts_with("Device.NAT.") {
        return handle_nat(path);
    }
    
    // Device.Firewall
    if path.starts_with("Device.Firewall.") {
        return handle_firewall(path);
    }
    
    // Device.QoS
    if path.starts_with("Device.QoS.") {
        return handle_qos(path);
    }
    
    // Device.WireGuard
    if path.starts_with("Device.WireGuard.") {
        return handle_wireguard(path);
    }
    
    // Device.X_TP_OpenVPN
    if path.starts_with("Device.X_TP_OpenVPN.") {
        return handle_openvpn(path);
    }
    
    // Device.Time
    if path.starts_with("Device.Time.") {
        return handle_time(path);
    }
    
    // Device.USB
    if path.starts_with("Device.USB.") {
        return handle_usb(path);
    }
    
    // Device.Cellular
    if path.starts_with("Device.Cellular.") {
        return handle_cellular(path);
    }
    
    // Device.NeighborDiscovery
    if path.starts_with("Device.NeighborDiscovery.") {
        return handle_neighbor_discovery(path);
    }
    
    debug!("MISC GET: unimplemented path: {path}");
    result
}

/// Set miscellaneous TR-181 parameters (most are read-only)
pub async fn set(_cfg: &ClientConfig, path: &str, _value: &str) -> Result<(), String> {
    // Most of these paths are read-only in stub implementation
    Err(format!("Read-only or not implemented: {path}"))
}

// ── DNS ─────────────────────────────────────────────────────────────────────

fn handle_dns(path: &str) -> Params {
    let mut result = Params::new();
    
    if path.contains("ServerNumberOfEntries") {
        // Count configured DNS servers
        let count = std::process::Command::new("uci")
            .args(["show", "network", "@dns"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.lines().count())
            .unwrap_or(0);
        result.insert(path.to_string(), count.to_string());
    } else if path.contains("Server.") && path.ends_with("DNSServer") {
        // Get DNS server IP
        let idx = extract_index(path, "Server.").unwrap_or(1);
        let dns = get_dns_server(idx);
        result.insert(path.to_string(), dns);
    }
    
    result
}

fn get_dns_server(idx: usize) -> String {
    // Read from /etc/resolv.conf or UCI
    let output = std::process::Command::new("cat")
        .arg("/tmp/resolv.conf.d/resolv.conf.auto")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    
    let nameservers: Vec<&str> = output
        .lines()
        .filter(|l| l.starts_with("nameserver"))
        .map(|l| l.split_whitespace().nth(1).unwrap_or(""))
        .collect();
    
    nameservers.get(idx - 1).unwrap_or(&"").to_string()
}

// ── Routing ─────────────────────────────────────────────────────────────────

fn handle_routing(path: &str) -> Params {
    let mut result = Params::new();
    
    if path.contains("IPv4ForwardingNumberOfEntries") {
        // Count routes
        let count = std::process::Command::new("ip")
            .args(["route", "show"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.lines().count())
            .unwrap_or(0);
        result.insert(path.to_string(), count.to_string());
    } else if path.contains("IPv4Forwarding.") {
        // Get specific route - parse from ip route
        let idx = extract_index(path, "IPv4Forwarding.").unwrap_or(1);
        if let Some(route) = get_route(idx) {
            if path.ends_with("DestIPAddress") {
                result.insert(path.to_string(), route.dest);
            } else if path.ends_with("DestSubnetMask") {
                result.insert(path.to_string(), route.mask);
            } else if path.ends_with("GatewayIPAddress") {
                result.insert(path.to_string(), route.gateway);
            } else if path.ends_with("Interface") {
                result.insert(path.to_string(), route.interface);
            }
        }
    }
    
    result
}

struct Route {
    dest: String,
    mask: String,
    gateway: String,
    interface: String,
}

fn get_route(idx: usize) -> Option<Route> {
    let output = std::process::Command::new("ip")
        .args(["route", "show"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())?;
    
    let lines: Vec<&str> = output.lines().collect();
    let line = lines.get(idx - 1)?;
    
    // Parse: "default via 192.168.1.1 dev br-wan" or "192.168.1.0/24 dev br-lan"
    let parts: Vec<&str> = line.split_whitespace().collect();
    
    let (dest, mask) = if parts[0] == "default" {
        ("0.0.0.0".to_string(), "0.0.0.0".to_string())
    } else if let Some(slash_idx) = parts[0].find('/') {
        let dest = parts[0][..slash_idx].to_string();
        let prefix: u8 = parts[0][slash_idx + 1..].parse().unwrap_or(24);
        let mask = prefix_to_mask(prefix);
        (dest, mask)
    } else {
        (parts[0].to_string(), "255.255.255.255".to_string())
    };
    
    let gateway = if let Some(pos) = parts.iter().position(|&p| p == "via") {
        parts.get(pos + 1).unwrap_or(&"").to_string()
    } else {
        "".to_string()
    };
    
    let interface = if let Some(pos) = parts.iter().position(|&p| p == "dev") {
        parts.get(pos + 1).unwrap_or(&"").to_string()
    } else {
        "".to_string()
    };
    
    Some(Route { dest, mask, gateway, interface })
}

fn prefix_to_mask(prefix: u8) -> String {
    let mask_u32 = !((1u32 << (32 - prefix)) - 1);
    format!("{}.{}.{}.{}", 
        (mask_u32 >> 24) & 0xff,
        (mask_u32 >> 16) & 0xff,
        (mask_u32 >> 8) & 0xff,
        mask_u32 & 0xff
    )
}

// ── NAT ────────────────────────────────────────────────────────────────────

fn handle_nat(path: &str) -> Params {
    let mut result = Params::new();
    
    if path.contains("InterfaceSetting.1.Enable") {
        result.insert(path.to_string(), "true".to_string());
    } else if path.contains("InterfaceSetting.1.Status") {
        result.insert(path.to_string(), "Enabled".to_string());
    } else if path.contains("PortMappingNumberOfEntries") {
        result.insert(path.to_string(), "0".to_string());
    } else if path.contains("DMZEnable") {
        // Check if DMZ is enabled in firewall config
        let dmz = std::process::Command::new("uci")
            .args(["get", "firewall.dmz.enabled"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim() == "1" || s.trim() == "true")
            .unwrap_or(false);
        result.insert(path.to_string(), dmz.to_string());
    } else if path.contains("DMZHost") {
        let host = std::process::Command::new("uci")
            .args(["get", "firewall.dmz.dest_ip"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        result.insert(path.to_string(), host);
    }
    
    result
}

// ── Firewall ────────────────────────────────────────────────────────────────

fn handle_firewall(path: &str) -> Params {
    let mut result = Params::new();
    
    if path.ends_with("Level") {
        // Read from UCI
        let level = std::process::Command::new("uci")
            .args(["get", "firewall.@defaults[0].input"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| match s.trim() {
                "ACCEPT" => "Low",
                "REJECT" => "High",
                "DROP" => "High",
                _ => "Medium",
            })
            .unwrap_or("Medium");
        result.insert(path.to_string(), level.to_string());
    } else if path.ends_with("Config") {
        result.insert(path.to_string(), "Standard".to_string());
    }
    
    result
}

// ── QoS ────────────────────────────────────────────────────────────────────

fn handle_qos(path: &str) -> Params {
    let mut result = Params::new();
    
    if path.contains("QueueNumberOfEntries") {
        result.insert(path.to_string(), "0".to_string());
    } else if path.contains("ClassificationNumberOfEntries") {
        result.insert(path.to_string(), "0".to_string());
    } else if path.contains("Queue.1.") {
        // Stub values for first queue
        if path.ends_with("Enable") {
            result.insert(path.to_string(), "false".to_string());
        } else if path.ends_with("Status") {
            result.insert(path.to_string(), "Disabled".to_string());
        } else if path.ends_with("Interface") || path.ends_with("Bandwidth") {
            result.insert(path.to_string(), "".to_string());
        }
    }
    
    result
}

// ── WireGuard ───────────────────────────────────────────────────────────────

fn handle_wireguard(path: &str) -> Params {
    let mut result = Params::new();
    
    if path.contains("InterfaceNumberOfEntries") || path.contains("PeersNumberOfEntries") {
        result.insert(path.to_string(), "0".to_string());
    } else if path.contains("Interface.") {
        // Check if WireGuard interface exists
        let iface_num = extract_index(path, "Interface.").unwrap_or(1);
        let iface_name = format!("wg{}", iface_num);
        
        // Check if interface exists
        let exists = std::process::Command::new("ip")
            .args(["link", "show", &iface_name])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        
        if exists {
            if path.ends_with("ListenPort") {
                let port = std::process::Command::new("wg")
                    .args(["show", &iface_name, "listen-port"])
                    .output()
                    .ok()
                    .and_then(|o| String::from_utf8(o.stdout).ok())
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
                result.insert(path.to_string(), port);
            } else if path.ends_with("PublicKey") {
                let key = std::process::Command::new("wg")
                    .args(["show", &iface_name, "public-key"])
                    .output()
                    .ok()
                    .and_then(|o| String::from_utf8(o.stdout).ok())
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
                result.insert(path.to_string(), key);
            }
        } else {
            // Return empty for non-existent interfaces
            result.insert(path.to_string(), "".to_string());
        }
    }
    
    result
}

// ── OpenVPN ────────────────────────────────────────────────────────────────

fn handle_openvpn(path: &str) -> Params {
    let mut result = Params::new();
    
    if path.contains("ClientNumberOfEntries") {
        // Count OpenVPN client instances
        let count = std::process::Command::new("uci")
            .args(["show", "openvpn"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.lines().filter(|l| l.contains("openvpn.@client") || l.contains("openvpn.client")).count())
            .unwrap_or(0);
        result.insert(path.to_string(), count.to_string());
    } else if path.contains("Client.") {
        // Get client status
        let idx = extract_index(path, "Client.").unwrap_or(1);
        let client_name = format!("client{}", idx);
        
        if path.ends_with("Enable") {
            let enabled = std::process::Command::new("uci")
                .args(["get", &format!("openvpn.{}.enabled", client_name)])
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim() == "1")
                .unwrap_or(false);
            result.insert(path.to_string(), enabled.to_string());
        } else if path.ends_with("Status") {
            // Check if OpenVPN process is running
            let running = std::process::Command::new("pgrep")
                .args(["-f", &format!("openvpn.*{}", client_name)])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            result.insert(path.to_string(), if running { "Connected" } else { "Disconnected" }.to_string());
        } else if path.ends_with("RemoteIP") || path.ends_with("RemotePort") || 
                  path.ends_with("BytesSent") || path.ends_with("BytesReceived") {
            // These would require parsing OpenVPN status file
            result.insert(path.to_string(), "".to_string());
        }
    }
    
    result
}

// ── Time ────────────────────────────────────────────────────────────────────

fn handle_time(path: &str) -> Params {
    let mut result = Params::new();
    
    if path.ends_with("Enable") {
        result.insert(path.to_string(), "true".to_string());
    } else if path.ends_with("Status") {
        // Check if NTP is synchronized
        let synced = std::process::Command::new("ntpq")
            .args(["-p"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.contains("*") || s.contains("+"))
            .unwrap_or(false);
        result.insert(path.to_string(), if synced { "Synchronized" } else { "Unsynchronized" }.to_string());
    } else if path.contains("NTPServerNumberOfEntries") {
        let count = std::process::Command::new("uci")
            .args(["show", "system", "ntp"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.lines().filter(|l| l.contains("server")).count())
            .unwrap_or(0);
        result.insert(path.to_string(), count.to_string());
    } else if path.contains("NTPServer.") && path.ends_with("Status") {
        result.insert(path.to_string(), "Up".to_string());
    } else if path.ends_with("LocalTimeZone") {
        let tz = std::process::Command::new("uci")
            .args(["get", "system.@system[0].zonename"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "UTC".to_string());
        result.insert(path.to_string(), tz);
    } else if path.ends_with("CurrentLocalTime") {
        let now = std::process::Command::new("date")
            .args(["+%Y-%m-%d %H:%M:%S"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        result.insert(path.to_string(), now);
    }
    
    result
}

// ── USB ─────────────────────────────────────────────────────────────────────

fn handle_usb(path: &str) -> Params {
    let mut result = Params::new();
    
    if path.contains("HostNumberOfEntries") || path.contains("DeviceNumberOfEntries") {
        // Count USB devices from /sys/bus/usb/devices
        let count = std::fs::read_dir("/sys/bus/usb/devices")
            .map(|entries| entries.filter(|e| {
                if let Ok(entry) = e {
                    entry.file_name().to_string_lossy().starts_with("1-") ||
                    entry.file_name().to_string_lossy().starts_with("2-")
                } else {
                    false
                }
            }).count())
            .unwrap_or(0);
        result.insert(path.to_string(), count.to_string());
    } else if path.contains("Device.1.") {
        // Get first USB device info
        if path.ends_with("DeviceNumber") {
            result.insert(path.to_string(), "1".to_string());
        } else {
            // Read from /sys/bus/usb/devices
            let device_path = "/sys/bus/usb/devices/1-1";
            let value = if path.ends_with("VendorID") {
                read_usb_attr(device_path, "idVendor")
            } else if path.ends_with("ProductID") {
                read_usb_attr(device_path, "idProduct")
            } else if path.ends_with("Manufacturer") {
                read_usb_attr(device_path, "manufacturer")
            } else if path.ends_with("ProductClass") || path.ends_with("SerialNumber") {
                read_usb_attr(device_path, "product")
            } else {
                "".to_string()
            };
            result.insert(path.to_string(), value);
        }
    }
    
    result
}

fn read_usb_attr(device_path: &str, attr: &str) -> String {
    std::fs::read_to_string(format!("{}/{}", device_path, attr))
        .unwrap_or_default()
        .trim()
        .to_string()
}

// ── Cellular ────────────────────────────────────────────────────────────────

fn handle_cellular(path: &str) -> Params {
    let mut result = Params::new();
    
    if path.contains("InterfaceNumberOfEntries") {
        // Check if modem exists using mmcli or qmi
        let has_modem = std::process::Command::new("which")
            .arg("mmcli")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        result.insert(path.to_string(), if has_modem { "1" } else { "0" }.to_string());
    } else if path.contains("Interface.1.") {
        // Check if modem is available
        if path.ends_with("Status") {
            let status = std::process::Command::new("mmcli")
                .args(["-L"])
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| if s.contains("No modems") { "Not present" } else { "Present" }.to_string())
                .unwrap_or_else(|| "Not present".to_string());
            result.insert(path.to_string(), status);
        } else {
            // These would require parsing modem manager output
            result.insert(path.to_string(), "".to_string());
        }
    }
    
    result
}

// ── NeighborDiscovery ───────────────────────────────────────────────────────

fn handle_neighbor_discovery(path: &str) -> Params {
    let mut result = Params::new();
    
    if path.contains("NeighborNumberOfEntries") {
        // Count neighbors from ip neigh or /proc/net/arp
        let count = std::fs::read_to_string("/proc/net/arp")
            .map(|s| s.lines().skip(1).count())
            .unwrap_or(0);
        result.insert(path.to_string(), count.to_string());
    } else if path.contains("Neighbor.") {
        // Get specific neighbor from /proc/net/arp
        let idx = extract_index(path, "Neighbor.").unwrap_or(1);
        if let Some(neighbor) = get_neighbor(idx) {
            if path.ends_with("IPAddress") {
                result.insert(path.to_string(), neighbor.ip);
            } else if path.ends_with("PhysAddress") {
                result.insert(path.to_string(), neighbor.mac);
            }
        }
    }
    
    result
}

struct Neighbor {
    ip: String,
    mac: String,
}

fn get_neighbor(idx: usize) -> Option<Neighbor> {
    let arp = std::fs::read_to_string("/proc/net/arp").ok()?;
    let lines: Vec<&str> = arp.lines().skip(1).collect();
    let line = lines.get(idx - 1)?;
    
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() >= 4 {
        Some(Neighbor {
            ip: parts[0].to_string(),
            mac: parts[3].to_string(),
        })
    } else {
        None
    }
}

// ── Helper Functions ────────────────────────────────────────────────────────

fn extract_index(path: &str, prefix: &str) -> Option<usize> {
    if let Some(start) = path.find(prefix) {
        let after = &path[start + prefix.len()..];
        if let Some(dot_idx) = after.find('.') {
            after[..dot_idx].parse::<usize>().ok()
        } else {
            after.parse::<usize>().ok()
        }
    } else {
        None
    }
}
