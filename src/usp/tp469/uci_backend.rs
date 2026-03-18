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
