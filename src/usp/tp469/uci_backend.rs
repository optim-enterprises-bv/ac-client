//! UCI Backend for TP-469 ADD/DELETE Operations
//!
//! Implements actual UCI operations to create/delete objects in OpenWrt configuration.
//! Handles instance number management, rollback on failure, and service restarts.

use super::error_codes::ErrorCode;
use log::{info, warn};
use std::process::Command;

/// Result of a UCI backend operation
#[derive(Debug)]
pub struct UciResult {
    pub success: bool,
    pub instance: u32,
    pub err_code: Option<ErrorCode>,
    pub err_msg: Option<String>,
}

impl UciResult {
    pub fn success(instance: u32) -> Self {
        UciResult {
            success: true,
            instance,
            err_code: None,
            err_msg: None,
        }
    }

    pub fn error(code: ErrorCode, msg: &str) -> Self {
        UciResult {
            success: false,
            instance: 0,
            err_code: Some(code),
            err_msg: Some(msg.to_string()),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// DHCP Static Lease Operations
// ─────────────────────────────────────────────────────────────────────────────

/// Add a new DHCP static lease via UCI
pub fn add_dhcp_lease(mac: &str, ip: &str, hostname: Option<&str>) -> UciResult {
    info!(
        "Adding DHCP static lease: MAC={}, IP={}, Host={:?}",
        mac, ip, hostname
    );

    // Find next available host section index
    let next_idx = find_next_dhcp_host_index();
    if next_idx == 0 {
        return UciResult::error(
            ErrorCode::ResourcesExceeded,
            "Could not find available host section",
        );
    }

    let section = format!("@host[{}]", next_idx);

    // Add the host section
    if let Err(e) = uci_add("dhcp", "host") {
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to add host section: {}", e),
        );
    }

    // Set MAC address
    if let Err(e) = uci_set(&format!("dhcp.{}.mac", section), mac) {
        // Rollback
        let _ = uci_delete(&format!("dhcp.{}", section));
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to set MAC: {}", e),
        );
    }

    // Set IP address
    if let Err(e) = uci_set(&format!("dhcp.{}.ip", section), ip) {
        // Rollback
        let _ = uci_delete(&format!("dhcp.{}", section));
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to set IP: {}", e),
        );
    }

    // Set hostname if provided
    if let Some(name) = hostname {
        if !name.is_empty() {
            if let Err(e) = uci_set(&format!("dhcp.{}.name", section), name) {
                warn!("Failed to set hostname, continuing: {}", e);
            }
        }
    }

    // Commit changes
    if let Err(e) = uci_commit("dhcp") {
        // Rollback
        let _ = uci_delete(&format!("dhcp.{}", section));
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to commit: {}", e),
        );
    }

    // Restart dnsmasq
    if let Err(e) = restart_dnsmasq() {
        warn!("Failed to restart dnsmasq: {}", e);
    }

    info!("Successfully added DHCP lease instance {}", next_idx);
    UciResult::success(next_idx as u32)
}

/// Delete a DHCP static lease by instance number
pub fn delete_dhcp_lease(instance: u32) -> UciResult {
    info!("Deleting DHCP static lease instance {}", instance);

    let section = format!("@host[{}]", instance);
    let full_path = format!("dhcp.{}", section);

    // Check if section exists
    let out = Command::new("uci")
        .args(["get", &format!("{}.mac", full_path)])
        .output();

    match out {
        Ok(result) if result.status.success() => {
            // Section exists, delete it
            if let Err(e) = uci_delete(&full_path) {
                return UciResult::error(
                    ErrorCode::InternalError,
                    &format!("Failed to delete: {}", e),
                );
            }

            if let Err(e) = uci_commit("dhcp") {
                return UciResult::error(
                    ErrorCode::InternalError,
                    &format!("Failed to commit: {}", e),
                );
            }

            // Restart dnsmasq
            if let Err(e) = restart_dnsmasq() {
                warn!("Failed to restart dnsmasq: {}", e);
            }

            info!("Successfully deleted DHCP lease instance {}", instance);
            UciResult::success(instance)
        }
        _ => UciResult::error(
            ErrorCode::ObjectNotFound,
            &format!("DHCP lease instance {} not found", instance),
        ),
    }
}

/// Find the next available DHCP host section index
fn find_next_dhcp_host_index() -> usize {
    let mut idx = 1;

    loop {
        let section = format!("@host[{}]", idx);
        let out = Command::new("uci")
            .args(["get", &format!("dhcp.{}.mac", section)])
            .output();

        match out {
            Ok(result) if result.status.success() => {
                // Section exists, try next
                idx += 1;
            }
            _ => {
                // Section doesn't exist, this is our slot
                return idx;
            }
        }

        // Safety limit
        if idx > 100 {
            return 0;
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// WiFi Interface Operations
// ─────────────────────────────────────────────────────────────────────────────

/// Add a new WiFi interface (SSID)
pub fn add_wifi_interface(
    ssid: &str,
    encryption: Option<&str>,
    key: Option<&str>,
    device: Option<&str>,
) -> UciResult {
    info!("Adding WiFi interface: SSID={}", ssid);

    // Find next available wifi-iface index
    let next_idx = find_next_wifi_iface_index();
    if next_idx == 0 {
        return UciResult::error(
            ErrorCode::ResourcesExceeded,
            "Could not find available wifi-iface section",
        );
    }

    let section = format!("@wifi-iface[{}]", next_idx);

    // Add the wifi-iface section
    if let Err(e) = uci_add("wireless", "wifi-iface") {
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to add wifi-iface: {}", e),
        );
    }

    // Set SSID
    if let Err(e) = uci_set(&format!("wireless.{}.ssid", section), ssid) {
        let _ = uci_delete(&format!("wireless.{}", section));
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to set SSID: {}", e),
        );
    }

    // Set device (radio) - default to radio0 if not specified
    let radio_device = device.unwrap_or("radio0");
    if let Err(e) = uci_set(&format!("wireless.{}.device", section), radio_device) {
        let _ = uci_delete(&format!("wireless.{}", section));
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to set device: {}", e),
        );
    }

    // Set mode (default to ap)
    if let Err(e) = uci_set(&format!("wireless.{}.mode", section), "ap") {
        let _ = uci_delete(&format!("wireless.{}", section));
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to set mode: {}", e),
        );
    }

    // Set network (default to lan)
    if let Err(e) = uci_set(&format!("wireless.{}.network", section), "lan") {
        let _ = uci_delete(&format!("wireless.{}", section));
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to set network: {}", e),
        );
    }

    // Set encryption if provided
    if let Some(enc) = encryption {
        if !enc.is_empty() && enc != "none" {
            if let Err(e) = uci_set(&format!("wireless.{}.encryption", section), enc) {
                warn!("Failed to set encryption, continuing: {}", e);
            }

            // Set key if provided
            if let Some(k) = key {
                if !k.is_empty() {
                    if let Err(e) = uci_set(&format!("wireless.{}.key", section), k) {
                        warn!("Failed to set key, continuing: {}", e);
                    }
                }
            }
        }
    }

    // Commit changes
    if let Err(e) = uci_commit("wireless") {
        let _ = uci_delete(&format!("wireless.{}", section));
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to commit: {}", e),
        );
    }

    // Reload WiFi
    if let Err(e) = wifi_reload() {
        warn!("Failed to reload WiFi: {}", e);
    }

    info!("Successfully added WiFi interface instance {}", next_idx);
    UciResult::success(next_idx as u32)
}

/// Delete a WiFi interface by instance number
pub fn delete_wifi_interface(instance: u32) -> UciResult {
    info!("Deleting WiFi interface instance {}", instance);

    let section = format!("@wifi-iface[{}]", instance);
    let full_path = format!("wireless.{}", section);

    // Check if section exists
    let out = Command::new("uci")
        .args(["get", &format!("{}.ssid", full_path)])
        .output();

    match out {
        Ok(result) if result.status.success() => {
            // Section exists, delete it
            if let Err(e) = uci_delete(&full_path) {
                return UciResult::error(
                    ErrorCode::InternalError,
                    &format!("Failed to delete: {}", e),
                );
            }

            if let Err(e) = uci_commit("wireless") {
                return UciResult::error(
                    ErrorCode::InternalError,
                    &format!("Failed to commit: {}", e),
                );
            }

            // Reload WiFi
            if let Err(e) = wifi_reload() {
                warn!("Failed to reload WiFi: {}", e);
            }

            info!("Successfully deleted WiFi interface instance {}", instance);
            UciResult::success(instance)
        }
        _ => UciResult::error(
            ErrorCode::ObjectNotFound,
            &format!("WiFi interface instance {} not found", instance),
        ),
    }
}

/// Find the next available wifi-iface index
fn find_next_wifi_iface_index() -> usize {
    let mut idx = 0;

    loop {
        let section = format!("@wifi-iface[{}]", idx);
        let out = Command::new("uci")
            .args(["get", &format!("wireless.{}.ssid", section)])
            .output();

        match out {
            Ok(result) if result.status.success() => {
                // Section exists, try next
                idx += 1;
            }
            _ => {
                // Section doesn't exist, this is our slot
                return if idx == 0 { 1 } else { idx };
            }
        }

        // Safety limit
        if idx > 50 {
            return 0;
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Static Host Operations
// ─────────────────────────────────────────────────────────────────────────────

/// Add a static host entry (DNS/hosts)
pub fn add_static_host(ip: &str, hostname: &str) -> UciResult {
    info!("Adding static host: {} -> {}", hostname, ip);

    // Method 1: Add to dnsmasq address list (preferred for DHCP-managed hosts)
    let address_entry = format!("/{}/{}", hostname, ip);

    if let Err(e) = uci_add_list("dhcp.@dnsmasq[0].address", &address_entry) {
        // Try method 2: add to /etc/hosts
        if let Err(e2) = add_to_hosts_file(ip, hostname) {
            return UciResult::error(
                ErrorCode::InternalError,
                &format!("Failed to add host via UCI: {} and hosts file: {}", e, e2),
            );
        }

        // Method 2 succeeded
        info!("Added host to /etc/hosts: {} -> {}", hostname, ip);
    } else {
        // Method 1 succeeded
        if let Err(e) = uci_commit("dhcp") {
            return UciResult::error(
                ErrorCode::InternalError,
                &format!("Failed to commit: {}", e),
            );
        }
        info!("Added host to dnsmasq: {} -> {}", hostname, ip);
    }

    // Restart dnsmasq
    if let Err(e) = restart_dnsmasq() {
        warn!("Failed to restart dnsmasq: {}", e);
    }

    // Return a generated instance number (based on line number in hosts file or dnsmasq index)
    let instance = find_host_instance_number(hostname);
    UciResult::success(instance)
}

/// Delete a static host entry
pub fn delete_static_host(instance: u32) -> UciResult {
    info!("Deleting static host instance {}", instance);

    // Find and remove from dnsmasq address list
    let out = Command::new("uci")
        .args(["get", "dhcp.@dnsmasq[0].address"])
        .output();

    if let Ok(result) = out {
        if result.status.success() {
            let stdout = String::from_utf8_lossy(&result.stdout);
            let addresses: Vec<&str> = stdout.lines().collect();

            if (instance as usize) < addresses.len() {
                let to_remove = addresses[instance as usize].trim();

                // Remove this specific address entry
                if let Err(e) = uci_del_list("dhcp.@dnsmasq[0].address", to_remove) {
                    warn!("Failed to remove from dnsmasq: {}", e);
                } else {
                    let _ = uci_commit("dhcp");
                    let _ = restart_dnsmasq();
                    return UciResult::success(instance);
                }
            }
        }
    }

    // Try removing from /etc/hosts
    if let Err(e) = remove_from_hosts_file(instance) {
        return UciResult::error(
            ErrorCode::ObjectNotFound,
            &format!("Host instance {} not found: {}", instance, e),
        );
    }

    UciResult::success(instance)
}

/// Add entry to /etc/hosts file
fn add_to_hosts_file(ip: &str, hostname: &str) -> Result<(), String> {
    use std::fs::OpenOptions;
    use std::io::Write;

    let line = format!("{} {}\n", ip, hostname);

    let mut file = OpenOptions::new()
        .append(true)
        .open("/etc/hosts")
        .map_err(|e| format!("Failed to open /etc/hosts: {}", e))?;

    file.write_all(line.as_bytes())
        .map_err(|e| format!("Failed to write to /etc/hosts: {}", e))?;

    Ok(())
}

/// Remove entry from /etc/hosts file by instance number (line index)
fn remove_from_hosts_file(instance: u32) -> Result<(), String> {
    use std::fs;

    let content = fs::read_to_string("/etc/hosts")
        .map_err(|e| format!("Failed to read /etc/hosts: {}", e))?;

    let lines: Vec<&str> = content.lines().collect();
    let mut host_entries = Vec::new();

    // Find host entries (non-empty, non-comment lines)
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if !trimmed.is_empty() && !trimmed.starts_with('#') {
            host_entries.push(i);
        }
    }

    if (instance as usize) >= host_entries.len() {
        return Err(format!("Host instance {} not found", instance));
    }

    let line_idx = host_entries[instance as usize];

    // Comment out the line instead of removing (safer)
    let mut new_lines: Vec<String> = lines.iter().map(|&s| s.to_string()).collect();
    new_lines[line_idx] = format!("# Removed by USP DELETE: {}", new_lines[line_idx]);

    fs::write("/etc/hosts", new_lines.join("\n"))
        .map_err(|e| format!("Failed to write /etc/hosts: {}", e))?;

    Ok(())
}

/// Find instance number for a hostname
fn find_host_instance_number(hostname: &str) -> u32 {
    // Try dnsmasq addresses first
    let out = Command::new("uci")
        .args(["get", "dhcp.@dnsmasq[0].address"])
        .output();

    if let Ok(result) = out {
        if result.status.success() {
            let stdout = String::from_utf8_lossy(&result.stdout);
            for (i, line) in stdout.lines().enumerate() {
                if line.contains(hostname) {
                    return i as u32;
                }
            }
        }
    }

    // Try /etc/hosts
    if let Ok(content) = std::fs::read_to_string("/etc/hosts") {
        let mut host_count = 0;
        for line in content.lines() {
            let trimmed = line.trim();
            if !trimmed.is_empty() && !trimmed.starts_with('#') {
                if trimmed.contains(hostname) {
                    return host_count;
                }
                host_count += 1;
            }
        }
    }

    0 // Default
}

// ─────────────────────────────────────────────────────────────────────────────
// UCI Helper Functions
// ─────────────────────────────────────────────────────────────────────────────

fn uci_add(config: &str, section_type: &str) -> Result<(), String> {
    let status = Command::new("uci")
        .args(["add", config, section_type])
        .status()
        .map_err(|e| format!("Failed to execute uci add: {}", e))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("uci add {} {} failed", config, section_type))
    }
}

/// Get a UCI value (generic getter with error handling)
pub fn uci_get_value(config: &str, section: &str, option: &str) -> Result<String, String> {
    let path = format!("{}.{}.{}", config, section, option);
    let out = Command::new("uci")
        .args(["get", &path])
        .output()
        .map_err(|e| format!("Failed to execute uci get: {}", e))?;

    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(format!("uci get {} failed", path))
    }
}

/// Set a UCI value (generic setter with commit option)
pub fn uci_set_value(
    config: &str,
    section: &str,
    option: &str,
    value: &str,
    auto_commit: bool,
) -> Result<(), String> {
    let path = format!("{}.{}.{}", config, section, option);
    uci_set(&path, value)?;

    if auto_commit {
        uci_commit(config)?;
    }

    Ok(())
}

/// Get a UCI list value (returns all entries)
pub fn uci_get_list(config: &str, section: &str, option: &str) -> Result<Vec<String>, String> {
    let path = format!("{}.{}.{}", config, section, option);
    let out = Command::new("uci")
        .args(["get", &path])
        .output()
        .map_err(|e| format!("Failed to execute uci get: {}", e))?;

    if out.status.success() {
        let stdout = String::from_utf8_lossy(&out.stdout);
        Ok(stdout
            .lines()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect())
    } else {
        Err(format!("uci get {} failed", path))
    }
}

/// Add value to UCI list
pub fn uci_add_to_list(
    config: &str,
    section: &str,
    option: &str,
    value: &str,
    auto_commit: bool,
) -> Result<(), String> {
    let path = format!("{}.{}.{}", config, section, option);
    uci_add_list(&path, value)?;

    if auto_commit {
        uci_commit(config)?;
    }

    Ok(())
}

/// Remove value from UCI list
pub fn uci_remove_from_list(
    config: &str,
    section: &str,
    option: &str,
    value: &str,
    auto_commit: bool,
) -> Result<(), String> {
    let path = format!("{}.{}.{}", config, section, option);
    uci_del_list(&path, value)?;

    if auto_commit {
        uci_commit(config)?;
    }

    Ok(())
}

/// Reload network service
pub fn reload_network() -> Result<(), String> {
    let methods: Vec<Vec<&str>> = vec![
        vec!["/etc/init.d/network", "reload"],
        vec!["/etc/init.d/network", "restart"],
        vec!["killall", "-HUP", "netifd"],
    ];

    for args in &methods {
        let status = Command::new(args[0]).args(&args[1..]).status();

        if let Ok(s) = status {
            if s.success() {
                info!("Network reloaded successfully");
                return Ok(());
            }
        }
    }

    warn!("Could not reload network");
    Ok(()) // Don't fail the operation
}

/// Set a UCI option value at the given path (config.section.option)
pub fn uci_set(path: &str, value: &str) -> Result<(), String> {
    let status = Command::new("uci")
        .args(["set", &format!("{}={}", path, value)])
        .status()
        .map_err(|e| format!("Failed to execute uci set: {}", e))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("uci set {} failed", path))
    }
}

fn uci_delete(path: &str) -> Result<(), String> {
    let status = Command::new("uci")
        .args(["delete", path])
        .status()
        .map_err(|e| format!("Failed to execute uci delete: {}", e))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("uci delete {} failed", path))
    }
}

fn uci_add_list(path: &str, value: &str) -> Result<(), String> {
    let status = Command::new("uci")
        .args(["add_list", &format!("{}={}", path, value)])
        .status()
        .map_err(|e| format!("Failed to execute uci add_list: {}", e))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("uci add_list {} failed", path))
    }
}

/// Legacy UCI get function (wrapper for backward compatibility)
pub fn uci_get(path: &str) -> String {
    let parts: Vec<&str> = path.split('.').collect();
    if parts.len() >= 3 {
        let config = parts[0];
        let section = parts[1];
        let option = parts[2];
        uci_get_value(config, section, option).unwrap_or_default()
    } else {
        // Fallback to direct command
        Command::new("uci")
            .args(["get", path])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .unwrap_or_default()
            .trim()
            .to_string()
    }
}

fn uci_del_list(path: &str, value: &str) -> Result<(), String> {
    let status = Command::new("uci")
        .args(["del_list", &format!("{}={}", path, value)])
        .status()
        .map_err(|e| format!("Failed to execute uci del_list: {}", e))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("uci del_list {} failed", path))
    }
}

/// Commit UCI configuration changes for a config package
pub fn uci_commit(config: &str) -> Result<(), String> {
    let status = Command::new("uci")
        .args(["commit", config])
        .status()
        .map_err(|e| format!("Failed to execute uci commit: {}", e))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("uci commit {} failed", config))
    }
}

fn restart_dnsmasq() -> Result<(), String> {
    let methods: Vec<Vec<&str>> = vec![
        vec!["/etc/init.d/dnsmasq", "restart"],
        vec!["/etc/init.d/dnsmasq", "reload"],
        vec!["killall", "-HUP", "dnsmasq"],
    ];

    for args in &methods {
        let status = Command::new(args[0]).args(&args[1..]).status();

        if let Ok(s) = status {
            if s.success() {
                info!("dnsmasq restarted successfully");
                return Ok(());
            }
        }
    }

    warn!("Could not restart dnsmasq");
    Ok(()) // Don't fail the operation
}

fn wifi_reload() -> Result<(), String> {
    let status = Command::new("wifi").status();

    if let Ok(s) = status {
        if s.success() {
            info!("WiFi reloaded successfully");
            return Ok(());
        }
    }

    // Fallback
    let status = Command::new("/sbin/wifi").status();

    if let Ok(s) = status {
        if s.success() {
            info!("WiFi reloaded via /sbin/wifi");
            return Ok(());
        }
    }

    warn!("Could not reload WiFi");
    Ok(()) // Don't fail the operation
}

// ─────────────────────────────────────────────────────────────────────────────
// WiFi Device (Radio) Operations
// ─────────────────────────────────────────────────────────────────────────────

/// Get WiFi radio device configuration
pub fn get_wifi_device(radio_name: &str) -> Result<WifiDeviceConfig, String> {
    let _section = format!("wireless.{}", radio_name);

    Ok(WifiDeviceConfig {
        name: radio_name.to_string(),
        type_: uci_get_value("wireless", radio_name, "type").unwrap_or_default(),
        path: uci_get_value("wireless", radio_name, "path").unwrap_or_default(),
        channel: uci_get_value("wireless", radio_name, "channel")
            .unwrap_or_else(|_| "auto".to_string()),
        band: uci_get_value("wireless", radio_name, "band").unwrap_or_else(|_| "2g".to_string()),
        htmode: uci_get_value("wireless", radio_name, "htmode")
            .unwrap_or_else(|_| "HT20".to_string()),
        cell_density: uci_get_value("wireless", radio_name, "cell_density")
            .ok()
            .and_then(|v| v.parse().ok()),
        country: uci_get_value("wireless", radio_name, "country").ok(),
    })
}

/// Update WiFi radio device parameter
pub fn update_wifi_device_param(radio_name: &str, param: &str, value: &str) -> UciResult {
    info!("Updating WiFi device {}: {} = {}", radio_name, param, value);

    // Check if device exists
    let test = uci_get_value("wireless", radio_name, "type");
    if test.is_err() || test.unwrap().is_empty() {
        return UciResult::error(
            ErrorCode::ObjectNotFound,
            &format!("WiFi radio {} not found", radio_name),
        );
    }

    // Update parameter
    if let Err(e) = uci_set_value("wireless", radio_name, param, value, true) {
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to set WiFi device {}: {}", param, e),
        );
    }

    // Reload WiFi to apply changes
    if let Err(e) = wifi_reload() {
        warn!("Failed to reload WiFi after device update: {}", e);
    }

    info!(
        "Successfully updated WiFi device {}: {} = {}",
        radio_name, param, value
    );
    UciResult::success(1)
}

/// Set WiFi radio channel
pub fn set_wifi_channel(radio_name: &str, channel: &str) -> UciResult {
    update_wifi_device_param(radio_name, "channel", channel)
}

/// Set WiFi radio bandwidth (htmode)
pub fn set_wifi_htmode(radio_name: &str, htmode: &str) -> UciResult {
    update_wifi_device_param(radio_name, "htmode", htmode)
}

/// Set WiFi cell density
pub fn set_wifi_cell_density(radio_name: &str, density: i32) -> UciResult {
    update_wifi_device_param(radio_name, "cell_density", &density.to_string())
}

/// WiFi device configuration struct
#[derive(Debug, Clone)]
pub struct WifiDeviceConfig {
    pub name: String,
    pub type_: String,
    pub path: String,
    pub channel: String,
    pub band: String,
    pub htmode: String,
    pub cell_density: Option<i32>,
    pub country: Option<String>,
}

/// List all WiFi devices (radios)
pub fn list_wifi_devices() -> Vec<String> {
    let mut devices = Vec::new();

    // Try to find all wifi-device sections
    for i in 0..10 {
        let name = format!("radio{}", i);
        let test = uci_get(&format!("wireless.{}.type", name));
        if !test.is_empty() {
            devices.push(name);
        }
    }

    devices
}

// ─────────────────────────────────────────────────────────────────────────────
// WiFi Interface (SSID/AP) Extended Operations
// ─────────────────────────────────────────────────────────────────────────────

/// Update WiFi interface parameter
pub fn update_wifi_interface_param(iface_name: &str, param: &str, value: &str) -> UciResult {
    info!(
        "Updating WiFi interface {}: {} = {}",
        iface_name, param, value
    );

    // Check if interface exists
    let test = uci_get(&format!("wireless.{}.ssid", iface_name));
    if test.is_empty() {
        return UciResult::error(
            ErrorCode::ObjectNotFound,
            &format!("WiFi interface {} not found", iface_name),
        );
    }

    // Update parameter
    if let Err(e) = uci_set_value("wireless", iface_name, param, value, true) {
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to set WiFi interface {}: {}", param, e),
        );
    }

    // Reload WiFi
    if let Err(e) = wifi_reload() {
        warn!("Failed to reload WiFi after interface update: {}", e);
    }

    info!(
        "Successfully updated WiFi interface {}: {} = {}",
        iface_name, param, value
    );
    UciResult::success(1)
}

/// Set WiFi SSID
pub fn set_wifi_ssid(iface_name: &str, ssid: &str) -> UciResult {
    update_wifi_interface_param(iface_name, "ssid", ssid)
}

/// Set WiFi encryption mode
pub fn set_wifi_encryption(iface_name: &str, encryption: &str) -> UciResult {
    update_wifi_interface_param(iface_name, "encryption", encryption)
}

/// Set WiFi key/password
pub fn set_wifi_key(iface_name: &str, key: &str) -> UciResult {
    update_wifi_interface_param(iface_name, "key", key)
}

/// Set WiFi OCV (Operating Channel Validation)
pub fn set_wifi_ocv(iface_name: &str, ocv: i32) -> UciResult {
    update_wifi_interface_param(iface_name, "ocv", &ocv.to_string())
}

// ─────────────────────────────────────────────────────────────────────────────
// System Hostname Operations
// ─────────────────────────────────────────────────────────────────────────────

/// Set system hostname via UCI
pub fn set_system_hostname(hostname: &str) -> UciResult {
    info!("Setting system hostname to: {}", hostname);

    // OpenWrt UCI format: system.@system[0].hostname
    if let Err(e) = uci_set("system.@system[0].hostname", hostname) {
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to set hostname: {}", e),
        );
    }

    if let Err(e) = uci_commit("system") {
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to commit system config: {}", e),
        );
    }

    // Optionally apply hostname to system immediately
    let _ = Command::new("hostname").arg(hostname).status();

    info!("System hostname set to: {}", hostname);
    UciResult::success(1)
}

/// Get system hostname from UCI
pub fn get_system_hostname() -> String {
    uci_get("system.@system[0].hostname")
}

// ─────────────────────────────────────────────────────────────────────────────
// Network Interface Operations
// ─────────────────────────────────────────────────────────────────────────────

/// Add a new network interface
pub fn add_network_interface(
    name: &str,
    proto: &str,
    ipaddr: Option<&str>,
    netmask: Option<&str>,
    gateway: Option<&str>,
    dns: Option<&str>,
) -> UciResult {
    info!("Adding network interface: {} with proto {}", name, proto);

    // Check if interface already exists
    let test = uci_get(&format!("network.{}.proto", name));
    if !test.is_empty() {
        return UciResult::error(
            ErrorCode::DuplicateUniqueKey,
            &format!("Network interface {} already exists", name),
        );
    }

    // Create interface section (named, not anonymous)
    if let Err(e) = uci_set(&format!("network.{}=interface", name), "") {
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to create interface: {}", e),
        );
    }

    // Set protocol
    if let Err(e) = uci_set(&format!("network.{}.proto", name), proto) {
        let _ = uci_delete(&format!("network.{}", name));
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to set proto: {}", e),
        );
    }

    // Set static IP configuration if provided
    if proto == "static" {
        if let Some(ip) = ipaddr {
            if let Err(e) = uci_set(&format!("network.{}.ipaddr", name), ip) {
                let _ = uci_delete(&format!("network.{}", name));
                return UciResult::error(
                    ErrorCode::InternalError,
                    &format!("Failed to set ipaddr: {}", e),
                );
            }
        }

        if let Some(mask) = netmask {
            if let Err(e) = uci_set(&format!("network.{}.netmask", name), mask) {
                let _ = uci_delete(&format!("network.{}", name));
                return UciResult::error(
                    ErrorCode::InternalError,
                    &format!("Failed to set netmask: {}", e),
                );
            }
        }
    }

    // Set gateway if provided
    if let Some(gw) = gateway {
        if !gw.is_empty() {
            if let Err(e) = uci_set(&format!("network.{}.gateway", name), gw) {
                warn!("Failed to set gateway: {}", e);
            }
        }
    }

    // Set DNS if provided
    if let Some(dns_str) = dns {
        if !dns_str.is_empty() {
            // DNS can be a space-separated list in UCI
            if let Err(e) = uci_set(&format!("network.{}.dns", name), dns_str) {
                warn!("Failed to set dns: {}", e);
            }
        }
    }

    // Commit changes
    if let Err(e) = uci_commit("network") {
        let _ = uci_delete(&format!("network.{}", name));
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to commit: {}", e),
        );
    }

    // Reload network
    if let Err(e) = reload_network() {
        warn!("Failed to reload network: {}", e);
    }

    info!("Successfully added network interface: {}", name);
    UciResult::success(1)
}

/// Delete a network interface
pub fn delete_network_interface(name: &str) -> UciResult {
    info!("Deleting network interface: {}", name);

    // Check if interface exists
    let test = uci_get(&format!("network.{}.proto", name));
    if test.is_empty() {
        return UciResult::error(
            ErrorCode::ObjectNotFound,
            &format!("Network interface {} not found", name),
        );
    }

    // Delete the interface section
    if let Err(e) = uci_delete(&format!("network.{}", name)) {
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to delete interface: {}", e),
        );
    }

    // Commit changes
    if let Err(e) = uci_commit("network") {
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to commit: {}", e),
        );
    }

    // Reload network
    if let Err(e) = reload_network() {
        warn!("Failed to reload network: {}", e);
    }

    info!("Successfully deleted network interface: {}", name);
    UciResult::success(1)
}

/// Update network interface parameter
pub fn update_network_interface_param(name: &str, param: &str, value: &str) -> UciResult {
    info!("Updating network interface {}: {} = {}", name, param, value);

    // Check if interface exists
    let test = uci_get(&format!("network.{}.proto", name));
    if test.is_empty() {
        return UciResult::error(
            ErrorCode::ObjectNotFound,
            &format!("Network interface {} not found", name),
        );
    }

    // Update parameter
    let path = format!("network.{}.{}", name, param);
    if let Err(e) = uci_set(&path, value) {
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to set {}: {}", param, e),
        );
    }

    // Commit changes
    if let Err(e) = uci_commit("network") {
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to commit: {}", e),
        );
    }

    // Reload network
    if let Err(e) = reload_network() {
        warn!("Failed to reload network: {}", e);
    }

    info!(
        "Successfully updated network interface {}: {} = {}",
        name, param, value
    );
    UciResult::success(1)
}

/// Add bridge port to network device
pub fn add_bridge_port(device_name: &str, port: &str) -> UciResult {
    info!("Adding bridge port {} to {}", port, device_name);

    // Check if device exists
    let test = uci_get(&format!("network.{}.name", device_name));
    if test.is_empty() {
        return UciResult::error(
            ErrorCode::ObjectNotFound,
            &format!("Network device {} not found", device_name),
        );
    }

    // Verify it's a bridge
    let dev_type = uci_get(&format!("network.{}.type", device_name));
    if dev_type != "bridge" {
        return UciResult::error(
            ErrorCode::InvalidValue,
            &format!("Device {} is not a bridge", device_name),
        );
    }

    // Add port to list
    if let Err(e) = uci_add_to_list("network", device_name, "ports", port, true) {
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to add bridge port: {}", e),
        );
    }

    // Reload network
    if let Err(e) = reload_network() {
        warn!("Failed to reload network after adding bridge port: {}", e);
    }

    info!("Successfully added bridge port {} to {}", port, device_name);
    UciResult::success(1)
}

/// Remove bridge port from network device
pub fn remove_bridge_port(device_name: &str, port: &str) -> UciResult {
    info!("Removing bridge port {} from {}", port, device_name);

    // Check if device exists
    let test = uci_get(&format!("network.{}.name", device_name));
    if test.is_empty() {
        return UciResult::error(
            ErrorCode::ObjectNotFound,
            &format!("Network device {} not found", device_name),
        );
    }

    // Remove port from list
    if let Err(e) = uci_remove_from_list("network", device_name, "ports", port, true) {
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to remove bridge port: {}", e),
        );
    }

    // Reload network
    if let Err(e) = reload_network() {
        warn!("Failed to reload network after removing bridge port: {}", e);
    }

    info!(
        "Successfully removed bridge port {} from {}",
        port, device_name
    );
    UciResult::success(1)
}

/// Set network device MAC address
pub fn set_device_mac(device_name: &str, mac: &str) -> UciResult {
    info!("Setting MAC address for device {}: {}", device_name, mac);

    // Check if device exists
    let test = uci_get(&format!("network.{}.name", device_name));
    if test.is_empty() {
        return UciResult::error(
            ErrorCode::ObjectNotFound,
            &format!("Network device {} not found", device_name),
        );
    }

    // Set MAC address
    if let Err(e) = uci_set_value("network", device_name, "macaddr", mac, true) {
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to set MAC address: {}", e),
        );
    }

    // Reload network
    if let Err(e) = reload_network() {
        warn!("Failed to reload network after setting MAC: {}", e);
    }

    info!(
        "Successfully set MAC address for device {}: {}",
        device_name, mac
    );
    UciResult::success(1)
}

/// Get bridge ports for a device
pub fn get_bridge_ports(device_name: &str) -> Result<Vec<String>, String> {
    uci_get_list("network", device_name, "ports")
}

/// Add DNS server to interface
pub fn add_dns_server(iface_name: &str, dns: &str) -> UciResult {
    info!("Adding DNS server {} to {}", dns, iface_name);

    // Check if interface exists
    let test = uci_get(&format!("network.{}.proto", iface_name));
    if test.is_empty() {
        return UciResult::error(
            ErrorCode::ObjectNotFound,
            &format!("Network interface {} not found", iface_name),
        );
    }

    // Add DNS to list
    if let Err(e) = uci_add_to_list("network", iface_name, "dns", dns, true) {
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to add DNS server: {}", e),
        );
    }

    // Reload network
    if let Err(e) = reload_network() {
        warn!("Failed to reload network after adding DNS: {}", e);
    }

    info!("Successfully added DNS server {} to {}", dns, iface_name);
    UciResult::success(1)
}

/// Remove DNS server from interface
pub fn remove_dns_server(iface_name: &str, dns: &str) -> UciResult {
    info!("Removing DNS server {} from {}", dns, iface_name);

    // Check if interface exists
    let test = uci_get(&format!("network.{}.proto", iface_name));
    if test.is_empty() {
        return UciResult::error(
            ErrorCode::ObjectNotFound,
            &format!("Network interface {} not found", iface_name),
        );
    }

    // Remove DNS from list
    if let Err(e) = uci_remove_from_list("network", iface_name, "dns", dns, true) {
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to remove DNS server: {}", e),
        );
    }

    // Reload network
    if let Err(e) = reload_network() {
        warn!("Failed to reload network after removing DNS: {}", e);
    }

    info!(
        "Successfully removed DNS server {} from {}",
        dns, iface_name
    );
    UciResult::success(1)
}

/// Get DNS servers for an interface
pub fn get_dns_servers(iface_name: &str) -> Result<Vec<String>, String> {
    uci_get_list("network", iface_name, "dns")
}

/// Set IPv6 ULA prefix
pub fn set_ipv6_prefix(iface_name: &str, prefix: &str) -> UciResult {
    info!("Setting IPv6 prefix for {}: {}", iface_name, prefix);

    // Check if interface exists
    let test = uci_get(&format!("network.{}.proto", iface_name));
    if test.is_empty() {
        return UciResult::error(
            ErrorCode::ObjectNotFound,
            &format!("Network interface {} not found", iface_name),
        );
    }

    // Set IPv6 prefix
    if let Err(e) = uci_set_value("network", iface_name, "ip6prefix", prefix, true) {
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to set IPv6 prefix: {}", e),
        );
    }

    // Reload network
    if let Err(e) = reload_network() {
        warn!("Failed to reload network after setting IPv6 prefix: {}", e);
    }

    info!(
        "Successfully set IPv6 prefix for {}: {}",
        iface_name, prefix
    );
    UciResult::success(1)
}

// ─────────────────────────────────────────────────────────────────────────────
// System Time/Zone Operations
// ─────────────────────────────────────────────────────────────────────────────

/// Set system timezone
pub fn set_system_timezone(tz: &str) -> UciResult {
    info!("Setting system timezone to: {}", tz);

    if let Err(e) = uci_set("system.@system[0].timezone", tz) {
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to set timezone: {}", e),
        );
    }

    if let Err(e) = uci_commit("system") {
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to commit: {}", e),
        );
    }

    info!("System timezone set to: {}", tz);
    UciResult::success(1)
}

/// Set system log size
pub fn set_system_log_size(size: &str) -> UciResult {
    info!("Setting system log size to: {}", size);

    if let Err(e) = uci_set("system.@system[0].log_size", size) {
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to set log_size: {}", e),
        );
    }

    if let Err(e) = uci_commit("system") {
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to commit: {}", e),
        );
    }

    // Restart logd to apply
    let _ = Command::new("/etc/init.d/log").arg("restart").status();

    info!("System log size set to: {}", size);
    UciResult::success(1)
}

/// Set system zonename (timezone name)
pub fn set_system_zonename(zonename: &str) -> UciResult {
    info!("Setting system zonename to: {}", zonename);

    if let Err(e) = uci_set("system.@system[0].zonename", zonename) {
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to set zonename: {}", e),
        );
    }

    if let Err(e) = uci_commit("system") {
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to commit: {}", e),
        );
    }

    info!("System zonename set to: {}", zonename);
    UciResult::success(1)
}

/// Get system zonename
pub fn get_system_zonename() -> String {
    uci_get("system.@system[0].zonename")
}

/// Set system TTY login
pub fn set_system_ttylogin(enable: bool) -> UciResult {
    let value = if enable { "1" } else { "0" };
    info!("Setting system TTY login to: {}", value);

    if let Err(e) = uci_set("system.@system[0].ttylogin", value) {
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to set ttylogin: {}", e),
        );
    }

    if let Err(e) = uci_commit("system") {
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to commit: {}", e),
        );
    }

    info!("System TTY login set to: {}", value);
    UciResult::success(1)
}

/// Get system TTY login setting
pub fn get_system_ttylogin() -> bool {
    uci_get("system.@system[0].ttylogin") == "1"
}

/// Get system compatibility version
pub fn get_system_compat_version() -> String {
    uci_get("system.@system[0].compat_version")
}

// ─────────────────────────────────────────────────────────────────────────────
// DHCP Dnsmasq Operations
// ─────────────────────────────────────────────────────────────────────────────

/// Set dnsmasq option
pub fn set_dnsmasq_option(option: &str, value: &str) -> UciResult {
    info!("Setting dnsmasq option {}: {}", option, value);

    if let Err(e) = uci_set(&format!("dhcp.@dnsmasq[0].{}", option), value) {
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to set dnsmasq option {}: {}", option, e),
        );
    }

    if let Err(e) = uci_commit("dhcp") {
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to commit: {}", e),
        );
    }

    // Restart dnsmasq
    if let Err(e) = restart_dnsmasq() {
        warn!("Failed to restart dnsmasq: {}", e);
    }

    info!("Successfully set dnsmasq option {}: {}", option, value);
    UciResult::success(1)
}

/// Get dnsmasq option
pub fn get_dnsmasq_option(option: &str) -> String {
    uci_get(&format!("dhcp.@dnsmasq[0].{}", option))
}

/// Add DNS server to dnsmasq
pub fn add_dnsmasq_server(server: &str) -> UciResult {
    info!("Adding DNS server to dnsmasq: {}", server);

    if let Err(e) = uci_add_list("dhcp.@dnsmasq[0].server", server) {
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to add DNS server: {}", e),
        );
    }

    if let Err(e) = uci_commit("dhcp") {
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to commit: {}", e),
        );
    }

    // Restart dnsmasq
    if let Err(e) = restart_dnsmasq() {
        warn!("Failed to restart dnsmasq: {}", e);
    }

    info!("Successfully added DNS server: {}", server);
    UciResult::success(1)
}

/// Set DHCP pool configuration
pub fn set_dhcp_pool(iface_name: &str, start: u32, limit: u32, leasetime: &str) -> UciResult {
    info!(
        "Setting DHCP pool for {}: start={}, limit={}, leasetime={}",
        iface_name, start, limit, leasetime
    );

    // Check if DHCP section exists
    let section = format!("dhcp.{}", iface_name);
    let test = uci_get(&format!("{}.interface", section));

    if test.is_empty() {
        // Create new DHCP section
        if let Err(e) = uci_add("dhcp", "dhcp") {
            return UciResult::error(
                ErrorCode::InternalError,
                &format!("Failed to add DHCP section: {}", e),
            );
        }

        // Set interface name
        if let Err(e) = uci_set(&format!("dhcp.@dhcp[-1].interface={}", iface_name), "") {
            return UciResult::error(
                ErrorCode::InternalError,
                &format!("Failed to set interface: {}", e),
            );
        }
    }

    // Set pool parameters
    if let Err(e) = uci_set(&format!("dhcp.{}.start", iface_name), &start.to_string()) {
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to set start: {}", e),
        );
    }

    if let Err(e) = uci_set(&format!("dhcp.{}.limit", iface_name), &limit.to_string()) {
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to set limit: {}", e),
        );
    }

    if let Err(e) = uci_set(&format!("dhcp.{}.leasetime", iface_name), leasetime) {
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to set leasetime: {}", e),
        );
    }

    if let Err(e) = uci_commit("dhcp") {
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to commit: {}", e),
        );
    }

    // Restart dnsmasq
    if let Err(e) = restart_dnsmasq() {
        warn!("Failed to restart dnsmasq: {}", e);
    }

    info!("Successfully set DHCP pool for {}", iface_name);
    UciResult::success(1)
}

// ─────────────────────────────────────────────────────────────────────────────
// LED Operations
// ─────────────────────────────────────────────────────────────────────────────

/// Add LED configuration
pub fn add_led_config(
    name: &str,
    sysfs: &str,
    trigger: &str,
    mode: Option<&str>,
    dev: Option<&str>,
) -> UciResult {
    info!("Adding LED config: {} -> {}", name, sysfs);

    // Find next LED index
    let mut idx = 0;
    loop {
        let test = uci_get(&format!("system.@led[{}].name", idx));
        if test.is_empty() {
            break;
        }
        idx += 1;
        if idx > 20 {
            return UciResult::error(ErrorCode::ResourcesExceeded, "Too many LED configurations");
        }
    }

    // Add LED section
    if let Err(e) = uci_add("system", "led") {
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to add LED section: {}", e),
        );
    }

    let section = format!("@led[{}]", idx);

    // Set LED parameters
    if let Err(e) = uci_set(&format!("system.{}.name", section), name) {
        let _ = uci_delete(&format!("system.{}", section));
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to set LED name: {}", e),
        );
    }

    if let Err(e) = uci_set(&format!("system.{}.sysfs", section), sysfs) {
        let _ = uci_delete(&format!("system.{}", section));
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to set LED sysfs: {}", e),
        );
    }

    if let Err(e) = uci_set(&format!("system.{}.trigger", section), trigger) {
        let _ = uci_delete(&format!("system.{}", section));
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to set LED trigger: {}", e),
        );
    }

    // Optional parameters
    if let Some(m) = mode {
        let _ = uci_set(&format!("system.{}.mode", section), m);
    }

    if let Some(d) = dev {
        let _ = uci_set(&format!("system.{}.dev", section), d);
    }

    if let Err(e) = uci_commit("system") {
        let _ = uci_delete(&format!("system.{}", section));
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to commit: {}", e),
        );
    }

    info!("Successfully added LED config: {}", name);
    UciResult::success(idx as u32)
}

/// Delete LED configuration
pub fn delete_led_config(idx: u32) -> UciResult {
    info!("Deleting LED config instance {}", idx);

    let section = format!("system.@led[{}]", idx);

    // Check if exists
    let test = uci_get(&format!("{}.name", section));
    if test.is_empty() {
        return UciResult::error(
            ErrorCode::ObjectNotFound,
            &format!("LED config {} not found", idx),
        );
    }

    // Delete section
    if let Err(e) = uci_delete(&section) {
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to delete LED config: {}", e),
        );
    }

    if let Err(e) = uci_commit("system") {
        return UciResult::error(
            ErrorCode::InternalError,
            &format!("Failed to commit: {}", e),
        );
    }

    info!("Successfully deleted LED config {}", idx);
    UciResult::success(idx)
}
