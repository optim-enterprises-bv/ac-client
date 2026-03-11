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
    
    // Device.DHCPv4
    if path.starts_with("Device.DHCPv4.") {
        return handle_dhcpv4(path);
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
        let level = uci_get_raw("firewall.@defaults[0].input")
            .map(|s| match s.as_str() {
                "ACCEPT" => "Low",
                "REJECT" => "High",
                "DROP" => "High",
                _ => "Medium",
            })
            .unwrap_or("Medium");
        result.insert(path.to_string(), level.to_string());
    } else if path.ends_with("Config") {
        result.insert(path.to_string(), "Standard".to_string());
    } else if path.ends_with("X_OptimACS_SynFlood") {
        let val = uci_get_raw("firewall.@defaults[0].syn_flood").unwrap_or_default();
        let enabled = val == "1" || val == "true";
        result.insert(path.to_string(), enabled.to_string());
    } else if path.ends_with("X_OptimACS_DropInvalid") {
        let val = uci_get_raw("firewall.@defaults[0].drop_invalid").unwrap_or_default();
        let enabled = val == "1" || val == "true";
        result.insert(path.to_string(), enabled.to_string());
    } else if path.ends_with("X_OptimACS_Input") {
        let val = uci_get_raw("firewall.@defaults[0].input").unwrap_or_else(|| "REJECT".to_string());
        result.insert(path.to_string(), val);
    } else if path.ends_with("X_OptimACS_Output") {
        let val = uci_get_raw("firewall.@defaults[0].output").unwrap_or_else(|| "ACCEPT".to_string());
        result.insert(path.to_string(), val);
    } else if path.ends_with("X_OptimACS_Forward") {
        let val = uci_get_raw("firewall.@defaults[0].forward").unwrap_or_else(|| "REJECT".to_string());
        result.insert(path.to_string(), val);
    } else if path.ends_with("X_OptimACS_FlowOffloading") {
        let val = uci_get_raw("firewall.@defaults[0].flow_offloading").unwrap_or_default();
        let enabled = val == "1" || val == "true";
        result.insert(path.to_string(), enabled.to_string());
    } else if path.ends_with("ZoneNumberOfEntries") {
        // Count firewall zones
        let output = std::process::Command::new("uci")
            .args(["show", "firewall"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .unwrap_or_default();
        let count = output.lines()
            .filter(|l| l.contains("@zone[") && l.contains(".name="))
            .count();
        result.insert(path.to_string(), count.to_string());
    }

    result
}

/// Helper to read a UCI value and return trimmed Option<String>
fn uci_get_raw(key: &str) -> Option<String> {
    std::process::Command::new("uci")
        .args(["get", key])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout).ok().map(|s| s.trim().trim_matches('\'').to_string())
            } else {
                None
            }
        })
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

    // Count real WireGuard interfaces from `wg show interfaces`
    let wg_ifaces = get_wg_interfaces();

    if path.contains("InterfaceNumberOfEntries") {
        result.insert(path.to_string(), wg_ifaces.len().to_string());
    } else if path.contains("Interface.") {
        let iface_num = extract_index(path, "Interface.").unwrap_or(1);
        let iface_name = wg_ifaces.get(iface_num - 1).cloned()
            .unwrap_or_else(|| format!("wg{}", iface_num - 1));

        if path.ends_with("PeersNumberOfEntries") {
            let count = get_wg_peer_count(&iface_name);
            result.insert(path.to_string(), count.to_string());
        } else if path.ends_with("Status") {
            let exists = std::process::Command::new("ip")
                .args(["link", "show", &iface_name])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            result.insert(path.to_string(), if exists { "Up" } else { "Down" }.to_string());
        } else if path.ends_with("ListenPort") {
            let port = std::process::Command::new("wg")
                .args(["show", &iface_name, "listen-port"])
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
            result.insert(path.to_string(), port);
        } else if path.ends_with("PublicKey") && !path.contains("Peer.") {
            let key = std::process::Command::new("wg")
                .args(["show", &iface_name, "public-key"])
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
            result.insert(path.to_string(), key);
        } else if path.contains("Peer.") {
            // Per-peer params from `wg show <iface> dump`
            let peer_num = extract_index(path, "Peer.").unwrap_or(1);
            let peers = get_wg_peers(&iface_name);
            if let Some(peer) = peers.get(peer_num - 1) {
                if path.ends_with("PublicKey") {
                    result.insert(path.to_string(), peer.public_key.clone());
                } else if path.ends_with("AllowedIPs") {
                    result.insert(path.to_string(), peer.allowed_ips.clone());
                } else if path.ends_with("LastHandshakeTime") {
                    result.insert(path.to_string(), peer.last_handshake.clone());
                } else if path.ends_with("TransferRx") {
                    result.insert(path.to_string(), peer.rx_bytes.clone());
                } else if path.ends_with("TransferTx") {
                    result.insert(path.to_string(), peer.tx_bytes.clone());
                } else if path.ends_with("PersistentKeepalive") {
                    result.insert(path.to_string(), peer.keepalive.clone());
                } else {
                    result.insert(path.to_string(), "".to_string());
                }
            } else {
                result.insert(path.to_string(), "".to_string());
            }
        } else {
            result.insert(path.to_string(), "".to_string());
        }
    }

    result
}

fn get_wg_interfaces() -> Vec<String> {
    std::process::Command::new("wg")
        .args(["show", "interfaces"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.split_whitespace().map(|i| i.to_string()).collect())
        .unwrap_or_default()
}

struct WgPeer {
    public_key: String,
    allowed_ips: String,
    last_handshake: String,
    rx_bytes: String,
    tx_bytes: String,
    keepalive: String,
}

/// Parse `wg show <iface> dump` for per-peer data.
/// Dump format (tab-separated, first line is interface, rest are peers):
///   <private_key> <public_key> <listen-port> <fwmark>
///   <public_key> <preshared_key> <endpoint> <allowed_ips> <latest_handshake> <rx> <tx> <keepalive>
fn get_wg_peers(iface: &str) -> Vec<WgPeer> {
    let output = std::process::Command::new("wg")
        .args(["show", iface, "dump"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    let mut peers = Vec::new();
    for (i, line) in output.lines().enumerate() {
        if i == 0 { continue; } // skip interface line
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() >= 8 {
            let handshake_epoch = fields[4].parse::<u64>().unwrap_or(0);
            let handshake_str = if handshake_epoch == 0 {
                "Never".to_string()
            } else {
                // Seconds ago
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let ago = now.saturating_sub(handshake_epoch);
                if ago < 60 { format!("{}s ago", ago) }
                else if ago < 3600 { format!("{}m ago", ago / 60) }
                else { format!("{}h {}m ago", ago / 3600, (ago % 3600) / 60) }
            };

            peers.push(WgPeer {
                public_key: fields[0].to_string(),
                allowed_ips: fields[3].to_string(),
                last_handshake: handshake_str,
                rx_bytes: fields[5].to_string(),
                tx_bytes: fields[6].to_string(),
                keepalive: if fields[7] == "off" { "0".to_string() } else { fields[7].to_string() },
            });
        }
    }

    peers
}

fn get_wg_peer_count(iface: &str) -> usize {
    std::process::Command::new("wg")
        .args(["show", iface, "peers"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0)
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

    let usb_devices = get_usb_devices();

    if path.contains("USBHosts.HostNumberOfEntries") {
        // Typically 1 host controller
        result.insert(path.to_string(), if usb_devices.is_empty() { "0" } else { "1" }.to_string());
    } else if path.contains("HostNumberOfEntries") || path.contains("DeviceNumberOfEntries") {
        result.insert(path.to_string(), usb_devices.len().to_string());
    } else if path.contains("Host.1.Device.") {
        // Device.USB.USBHosts.Host.1.Device.{d}.* path
        let dev_idx = extract_index(path, "Device.").unwrap_or(1);
        if let Some(dev_path) = usb_devices.get(dev_idx - 1) {
            let value = if path.ends_with("DeviceNumber") {
                dev_idx.to_string()
            } else if path.ends_with("VendorID") {
                read_usb_attr(dev_path, "idVendor")
            } else if path.ends_with("ProductID") {
                read_usb_attr(dev_path, "idProduct")
            } else if path.ends_with("Manufacturer") {
                read_usb_attr(dev_path, "manufacturer")
            } else if path.ends_with("ProductClass") {
                read_usb_attr(dev_path, "bDeviceClass")
            } else if path.ends_with("SerialNumber") {
                read_usb_attr(dev_path, "serial")
            } else if path.ends_with("USBVersion") {
                read_usb_attr(dev_path, "version")
            } else {
                "".to_string()
            };
            result.insert(path.to_string(), value);
        }
    } else if path.contains("Device.1.") {
        // Legacy flat path
        let device_path = usb_devices.first().map(|s| s.as_str()).unwrap_or("/sys/bus/usb/devices/1-1");
        let value = if path.ends_with("DeviceNumber") {
            "1".to_string()
        } else if path.ends_with("VendorID") {
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

    result
}

/// Enumerate real USB device paths from /sys/bus/usb/devices
fn get_usb_devices() -> Vec<String> {
    std::fs::read_dir("/sys/bus/usb/devices")
        .map(|entries| {
            let mut devs: Vec<String> = entries
                .filter_map(|e| e.ok())
                .filter(|e| {
                    let name = e.file_name().to_string_lossy().to_string();
                    // Match actual devices like 1-1, 1-2, 2-1 (not root hubs like usb1, usb2)
                    (name.starts_with("1-") || name.starts_with("2-")) && !name.contains(':')
                })
                .map(|e| e.path().to_string_lossy().to_string())
                .collect();
            devs.sort();
            devs
        })
        .unwrap_or_default()
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
        let has_modem = get_mmcli_modem_index().is_some();
        result.insert(path.to_string(), if has_modem { "1" } else { "0" }.to_string());
    } else if path.contains("Interface.1.") {
        if let Some(modem_idx) = get_mmcli_modem_index() {
            let modem_info = get_mmcli_info(&modem_idx);

            if path.ends_with("Status") {
                let status = modem_info.get("state").cloned().unwrap_or_else(|| "Unknown".to_string());
                result.insert(path.to_string(), status);
            } else if path.ends_with("IMEI") {
                result.insert(path.to_string(), modem_info.get("imei").cloned().unwrap_or_default());
            } else if path.ends_with("SignalStrength") {
                result.insert(path.to_string(), modem_info.get("signal").cloned().unwrap_or_default());
            } else if path.ends_with("Band") {
                result.insert(path.to_string(), modem_info.get("band").cloned().unwrap_or_default());
            } else if path.ends_with("RoamingStatus") {
                result.insert(path.to_string(), modem_info.get("roaming").cloned().unwrap_or_else(|| "Unknown".to_string()));
            } else if path.ends_with("IMSI") {
                let sim_info = get_mmcli_sim_info(&modem_idx);
                result.insert(path.to_string(), sim_info.get("imsi").cloned().unwrap_or_default());
            } else if path.ends_with("ICCID") {
                let sim_info = get_mmcli_sim_info(&modem_idx);
                result.insert(path.to_string(), sim_info.get("iccid").cloned().unwrap_or_default());
            } else if path.ends_with("RSRP") {
                let sig = get_mmcli_signal_info(&modem_idx);
                result.insert(path.to_string(), sig.get("rsrp").cloned().unwrap_or_default());
            } else if path.ends_with("RSRQ") {
                let sig = get_mmcli_signal_info(&modem_idx);
                result.insert(path.to_string(), sig.get("rsrq").cloned().unwrap_or_default());
            } else if path.ends_with("SINR") {
                let sig = get_mmcli_signal_info(&modem_idx);
                result.insert(path.to_string(), sig.get("sinr").cloned().unwrap_or_default());
            } else if path.ends_with("RegisteredNetwork") {
                result.insert(path.to_string(), modem_info.get("operator_name").cloned().unwrap_or_default());
            } else if path.ends_with("BytesSent") {
                let val = read_sysfs_net_stat("tx_bytes");
                result.insert(path.to_string(), val);
            } else if path.ends_with("BytesReceived") {
                let val = read_sysfs_net_stat("rx_bytes");
                result.insert(path.to_string(), val);
            } else if path.ends_with("SignalStrengthLevel") {
                let level = signal_quality_to_level(
                    modem_info.get("signal").and_then(|s| s.parse::<u32>().ok()).unwrap_or(0)
                );
                result.insert(path.to_string(), level.to_string());
            } else {
                result.insert(path.to_string(), "".to_string());
            }
        } else {
            result.insert(path.to_string(), "Not present".to_string());
        }
    }

    result
}

/// Get modem index from mmcli -L (returns first modem index like "0")
fn get_mmcli_modem_index() -> Option<String> {
    let output = std::process::Command::new("mmcli")
        .args(["-L"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())?;

    // Parse line like: "/org/freedesktop/ModemManager1/Modem/0 [Quectel] EC25"
    for line in output.lines() {
        if line.contains("/Modem/") {
            if let Some(idx_start) = line.rfind("/Modem/") {
                let rest = &line[idx_start + 7..];
                let idx: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                if !idx.is_empty() {
                    return Some(idx);
                }
            }
        }
    }
    None
}

/// Get modem info from mmcli -m <idx>
fn get_mmcli_info(modem_idx: &str) -> std::collections::HashMap<String, String> {
    let mut info = std::collections::HashMap::new();

    let output = std::process::Command::new("mmcli")
        .args(["-m", modem_idx])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.contains("imei") || trimmed.contains("IMEI") {
            if let Some(val) = extract_mmcli_value(trimmed) {
                info.insert("imei".to_string(), val);
            }
        }
        if trimmed.contains("signal quality") {
            if let Some(val) = extract_mmcli_value(trimmed) {
                // Extract just the percentage number
                let num: String = val.chars().take_while(|c| c.is_ascii_digit()).collect();
                info.insert("signal".to_string(), num);
            }
        }
        if trimmed.contains("state") && !trimmed.contains("power state") && !trimmed.contains("access") {
            if let Some(val) = extract_mmcli_value(trimmed) {
                info.insert("state".to_string(), val);
            }
        }
        if trimmed.contains("current bands") || trimmed.contains("access technologies") {
            if let Some(val) = extract_mmcli_value(trimmed) {
                info.insert("band".to_string(), val);
            }
        }
        if trimmed.contains("roaming") {
            if let Some(val) = extract_mmcli_value(trimmed) {
                info.insert("roaming".to_string(), val);
            }
        }
        if trimmed.contains("operator name") {
            if let Some(val) = extract_mmcli_value(trimmed) {
                info.insert("operator_name".to_string(), val);
            }
        }
    }

    info
}

/// Get SIM properties (IMSI, ICCID) by finding the SIM path from modem info,
/// then querying it with `mmcli -i <sim_idx>`.
fn get_mmcli_sim_info(modem_idx: &str) -> std::collections::HashMap<String, String> {
    let mut info = std::collections::HashMap::new();

    // First, get the SIM path from `mmcli -m <idx> --output-keyvalue`
    let output = std::process::Command::new("mmcli")
        .args(["-m", modem_idx, "--output-keyvalue"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    // Find the primary SIM path, e.g. "modem.generic.sim : /org/freedesktop/ModemManager1/SIM/0"
    let mut sim_idx = None;
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("modem.generic.sim") && trimmed.contains("/SIM/") {
            if let Some(pos) = trimmed.rfind("/SIM/") {
                let rest = &trimmed[pos + 5..];
                let idx: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                if !idx.is_empty() {
                    sim_idx = Some(idx);
                }
            }
        }
        // Also grab IMSI/ICCID directly if available in keyvalue output
        if trimmed.starts_with("sim.properties.imsi") {
            if let Some(val) = extract_kv_value(trimmed) {
                info.insert("imsi".to_string(), val);
            }
        }
        if trimmed.starts_with("sim.properties.iccid") {
            if let Some(val) = extract_kv_value(trimmed) {
                info.insert("iccid".to_string(), val);
            }
        }
    }

    // If we didn't get them from modem keyvalue, query the SIM directly
    if info.get("imsi").is_none() || info.get("iccid").is_none() {
        if let Some(idx) = sim_idx {
            let sim_output = std::process::Command::new("mmcli")
                .args(["-i", &idx])
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .unwrap_or_default();

            for line in sim_output.lines() {
                let trimmed = line.trim();
                if trimmed.contains("imsi") && info.get("imsi").is_none() {
                    if let Some(val) = extract_mmcli_value(trimmed) {
                        info.insert("imsi".to_string(), val);
                    }
                }
                if trimmed.contains("iccid") && info.get("iccid").is_none() {
                    if let Some(val) = extract_mmcli_value(trimmed) {
                        info.insert("iccid".to_string(), val);
                    }
                }
            }
        }
    }

    info
}

/// Get LTE signal metrics (RSRP, RSRQ, SINR) from `mmcli -m <idx> --signal-get`
fn get_mmcli_signal_info(modem_idx: &str) -> std::collections::HashMap<String, String> {
    let mut info = std::collections::HashMap::new();

    let output = std::process::Command::new("mmcli")
        .args(["-m", modem_idx, "--signal-get"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.contains("rsrp") {
            if let Some(val) = extract_mmcli_value(trimmed) {
                // Value may include unit like "-100.0 dBm", keep just the number
                let num = val.split_whitespace().next().unwrap_or(&val).to_string();
                info.insert("rsrp".to_string(), num);
            }
        }
        if trimmed.contains("rsrq") {
            if let Some(val) = extract_mmcli_value(trimmed) {
                let num = val.split_whitespace().next().unwrap_or(&val).to_string();
                info.insert("rsrq".to_string(), num);
            }
        }
        if trimmed.contains("s/n") || trimmed.contains("snr") || trimmed.contains("sinr") {
            if let Some(val) = extract_mmcli_value(trimmed) {
                let num = val.split_whitespace().next().unwrap_or(&val).to_string();
                info.insert("sinr".to_string(), num);
            }
        }
    }

    info
}

/// Read wwan interface byte counters from sysfs.
/// Tries wwan0 first, then falls back to any wwan* interface.
fn read_sysfs_net_stat(stat: &str) -> String {
    // Try wwan0 first
    let path = format!("/sys/class/net/wwan0/statistics/{stat}");
    if let Ok(val) = std::fs::read_to_string(&path) {
        return val.trim().to_string();
    }
    // Fallback: find any wwan* interface
    if let Ok(entries) = std::fs::read_dir("/sys/class/net") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("wwan") {
                let fallback = format!("/sys/class/net/{name}/statistics/{stat}");
                if let Ok(val) = std::fs::read_to_string(&fallback) {
                    return val.trim().to_string();
                }
            }
        }
    }
    String::new()
}

/// Map signal quality percentage (0-100) to a discrete level (0-5).
fn signal_quality_to_level(percent: u32) -> u32 {
    match percent {
        0 => 0,
        1..=20 => 1,
        21..=40 => 2,
        41..=60 => 3,
        61..=80 => 4,
        _ => 5,
    }
}

/// Extract value from mmcli keyvalue format ("key : value")
fn extract_kv_value(line: &str) -> Option<String> {
    if let Some(colon_idx) = line.find(':') {
        let val = line[colon_idx + 1..].trim().to_string();
        if !val.is_empty() && val != "--" {
            return Some(val);
        }
    }
    None
}

fn extract_mmcli_value(line: &str) -> Option<String> {
    if let Some(colon_idx) = line.find(':') {
        let val = line[colon_idx + 1..].trim().to_string();
        if !val.is_empty() && val != "--" {
            return Some(val);
        }
    }
    None
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

// ── DHCPv4 ──────────────────────────────────────────────────────────────────

fn handle_dhcpv4(path: &str) -> Params {
    let mut result = Params::new();

    if path.contains("Server.Pool.") {
        // Get DHCP pool info from UCI (dnsmasq or dhcp config)
        let pool_idx = extract_index(path, "Pool.").unwrap_or(1);
        // UCI: dhcp.lan (pool 1), dhcp.guest (pool 2), etc.
        let pools = get_dhcp_pools();
        let pool_name = pools.get(pool_idx - 1).cloned().unwrap_or_else(|| "lan".to_string());

        if path.ends_with("Enable") {
            let ignore = uci_get_raw(&format!("dhcp.{pool_name}.ignore")).unwrap_or_default();
            let enabled = ignore != "1";
            result.insert(path.to_string(), enabled.to_string());
        } else if path.ends_with("Status") {
            let ignore = uci_get_raw(&format!("dhcp.{pool_name}.ignore")).unwrap_or_default();
            let status = if ignore == "1" { "Disabled" } else { "Enabled" };
            result.insert(path.to_string(), status.to_string());
        } else if path.ends_with("MinAddress") || path.ends_with("Start") {
            let start = uci_get_raw(&format!("dhcp.{pool_name}.start")).unwrap_or_else(|| "100".to_string());
            result.insert(path.to_string(), start);
        } else if path.ends_with("MaxAddress") || path.ends_with("Limit") {
            let limit = uci_get_raw(&format!("dhcp.{pool_name}.limit")).unwrap_or_else(|| "150".to_string());
            result.insert(path.to_string(), limit);
        } else if path.ends_with("SubnetMask") {
            // Read from network config for this pool's interface
            let iface = uci_get_raw(&format!("dhcp.{pool_name}.interface")).unwrap_or_else(|| pool_name.clone());
            let mask = uci_get_raw(&format!("network.{iface}.netmask")).unwrap_or_else(|| "255.255.255.0".to_string());
            result.insert(path.to_string(), mask);
        } else if path.ends_with("DomainName") {
            let domain = uci_get_raw("dhcp.@dnsmasq[0].domain").unwrap_or_else(|| "lan".to_string());
            result.insert(path.to_string(), domain);
        } else if path.ends_with("LeaseTime") {
            let leasetime = uci_get_raw(&format!("dhcp.{pool_name}.leasetime")).unwrap_or_else(|| "12h".to_string());
            result.insert(path.to_string(), leasetime);
        } else if path.ends_with("LeaseNumberOfEntries") {
            // Count active leases from /tmp/dhcp.leases
            let count = std::fs::read_to_string("/tmp/dhcp.leases")
                .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count())
                .unwrap_or(0);
            result.insert(path.to_string(), count.to_string());
        } else if path.ends_with("StaticAddressNumberOfEntries") {
            // Count static hosts from UCI
            let output = std::process::Command::new("uci")
                .args(["show", "dhcp"])
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .unwrap_or_default();
            let count = output.lines().filter(|l| l.contains("@host[") && l.contains(".mac=")).count();
            result.insert(path.to_string(), count.to_string());
        } else if path.ends_with("Interface") || path.ends_with("X_OptimACS_Interface") {
            let iface = uci_get_raw(&format!("dhcp.{pool_name}.interface")).unwrap_or_else(|| pool_name.clone());
            result.insert(path.to_string(), iface);
        } else if path.ends_with("DNSServers") || path.ends_with("X_OptimACS_DNSServers") {
            let dns = uci_get_raw(&format!("dhcp.{pool_name}.dhcp_option"))
                .unwrap_or_default();
            let dns_servers: String = dns.split_whitespace()
                .filter(|o| o.starts_with("6,"))
                .map(|o| o.trim_start_matches("6,"))
                .collect::<Vec<&str>>()
                .join(",");
            result.insert(path.to_string(), dns_servers);
        }
    } else if path.contains("Server.PoolNumberOfEntries") {
        let count = get_dhcp_pools().len();
        result.insert(path.to_string(), count.to_string());
    }

    result
}

fn get_dhcp_pools() -> Vec<String> {
    let output = std::process::Command::new("uci")
        .args(["show", "dhcp"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    let mut pools = Vec::new();
    for line in output.lines() {
        // Look for sections that have start= (indicating DHCP pool)
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
