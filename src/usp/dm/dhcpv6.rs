//! TR-181 Device.DHCPv6.Client.* — DHCPv6 client information.
//!
//! Reads DHCPv6 client state from UCI interfaces with `proto=dhcpv6`
//! and runtime data from `ubus call network.interface.<name> status`.

use crate::config::ClientConfig;
use std::collections::HashMap;

pub type Params = HashMap<String, String>;

pub async fn get(_cfg: &ClientConfig, path: &str) -> Params {
    let mut m = Params::new();
    let clients = get_dhcpv6_clients();

    if path == "Device.DHCPv6."
        || path == "Device.DHCPv6.Client."
        || path.contains("ClientNumberOfEntries")
    {
        m.insert(
            "Device.DHCPv6.Client.NumberOfEntries".to_string(),
            clients.len().to_string(),
        );
    }

    if path == "Device.DHCPv6."
        || path == "Device.DHCPv6.Client."
        || path.starts_with("Device.DHCPv6.Client.")
    {
        let specific_idx = extract_index(path);
        for (i, client) in clients.iter().enumerate() {
            let idx = i + 1;
            if let Some(si) = specific_idx {
                if si != idx {
                    continue;
                }
            }
            let base = format!("Device.DHCPv6.Client.{idx}.");
            populate_client(&base, client, &mut m);
        }
    }

    m
}

struct Dhcpv6Client {
    interface: String,
    status: String,
    ipv6_address: String,
    prefix_length: String,
    dns_servers: String,
}

fn get_dhcpv6_clients() -> Vec<Dhcpv6Client> {
    let mut clients = Vec::new();

    // Find UCI interfaces with proto=dhcpv6
    let uci_out = std::process::Command::new("uci")
        .args(["show", "network"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    let mut dhcpv6_ifaces = Vec::new();
    for line in uci_out.lines() {
        if line.contains(".proto=") {
            let val = line.split('=').nth(1).unwrap_or("").trim_matches('\'');
            if val == "dhcpv6" {
                // Extract section name: network.wan6.proto -> wan6
                let section = line
                    .split('.')
                    .nth(1)
                    .unwrap_or("")
                    .to_string();
                if !section.is_empty() {
                    dhcpv6_ifaces.push(section);
                }
            }
        }
    }

    for iface in dhcpv6_ifaces {
        let ubus_out = std::process::Command::new("ubus")
            .args(["call", &format!("network.interface.{iface}"), "status"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .unwrap_or_default();

        let mut client = Dhcpv6Client {
            interface: iface.clone(),
            status: "Enabled".to_string(),
            ipv6_address: String::new(),
            prefix_length: String::new(),
            dns_servers: String::new(),
        };

        // Check if up
        if ubus_out.contains("\"up\": true") || ubus_out.contains("\"up\":true") {
            client.status = "Enabled".to_string();
        } else {
            client.status = "Disabled".to_string();
        }

        // Parse ipv6-address
        if let Some(pos) = ubus_out.find("\"ipv6-address\"") {
            let chunk = &ubus_out[pos..];
            if let Some(addr_pos) = chunk.find("\"address\"") {
                let after = &chunk[addr_pos + 9..];
                if let Some(start) = after.find('"') {
                    let rest = &after[start + 1..];
                    if let Some(end) = rest.find('"') {
                        client.ipv6_address = rest[..end].to_string();
                    }
                }
            }
            if let Some(mask_pos) = chunk.find("\"mask\"") {
                let after = &chunk[mask_pos + 5..];
                let after = after.trim_start_matches(|c: char| !c.is_ascii_digit());
                if let Some(end) = after.find(|c: char| !c.is_ascii_digit()) {
                    client.prefix_length = after[..end].to_string();
                }
            }
        }

        // Parse dns-server
        if let Some(pos) = ubus_out.find("\"dns-server\"") {
            let chunk = &ubus_out[pos..];
            if let Some(arr_start) = chunk.find('[') {
                if let Some(arr_end) = chunk[arr_start..].find(']') {
                    let arr = &chunk[arr_start..arr_start + arr_end];
                    let servers: Vec<&str> = arr
                        .split('"')
                        .filter(|s| !s.is_empty() && (s.contains(':') || s.contains('.')))
                        .collect();
                    client.dns_servers = servers.join(",");
                }
            }
        }

        clients.push(client);
    }

    clients
}

fn populate_client(base: &str, client: &Dhcpv6Client, m: &mut Params) {
    m.insert(format!("{base}Enable"), "true".to_string());
    m.insert(format!("{base}Status"), client.status.clone());
    m.insert(
        format!("{base}Interface"),
        format!("Device.IP.Interface.{}", client.interface),
    );

    if !client.ipv6_address.is_empty() {
        m.insert(
            format!("{base}Server.1.SourceAddress"),
            client.ipv6_address.clone(),
        );
    }
    if !client.prefix_length.is_empty() {
        m.insert(
            format!("{base}Server.1.PrefixLength"),
            client.prefix_length.clone(),
        );
    }
    if !client.dns_servers.is_empty() {
        m.insert(
            format!("{base}Server.1.DNSServers"),
            client.dns_servers.clone(),
        );
    }
}

fn extract_index(path: &str) -> Option<usize> {
    if let Some(pos) = path.find("Client.") {
        let rest = &path[pos + 7..];
        let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        num_str.parse().ok()
    } else {
        None
    }
}

pub async fn set(_cfg: &ClientConfig, path: &str, _value: &str) -> Result<(), String> {
    Err(format!("Device.DHCPv6 path is read-only: {path}"))
}
