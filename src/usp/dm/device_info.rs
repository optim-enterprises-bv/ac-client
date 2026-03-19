//! TR-181 Device.DeviceInfo.* — reads from /proc and UCI.

use crate::config::ClientConfig;
use crate::usp::tp469::uci_backend;
use crate::util;
use std::collections::HashMap;

pub fn get(cfg: &ClientConfig, path: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    let base = "Device.DeviceInfo.";
    let insert = |m: &mut HashMap<String, String>, suffix: &str, val: String| {
        m.insert(format!("{base}{suffix}"), val);
    };
    match path.trim_start_matches(base) {
        "" => {
            // Return ALL parameters
            let hostname = uci_backend::get_system_hostname();
            insert(
                &mut m,
                "HostName",
                if hostname.is_empty() {
                    cfg.sys_model.clone()
                } else {
                    hostname
                },
            );
            insert(&mut m, "SoftwareVersion", util::read_fw_version());
            insert(&mut m, "HardwareVersion", cfg.sys_model.clone());
            // SerialNumber must be the manufacturer serial, not the MAC.
            insert(&mut m, "SerialNumber", read_serial_number());
            insert(&mut m, "UpTime", util::read_uptime());
            insert(&mut m, "X_OptimACS_LoadAvg", util::read_load_avg());
            insert(&mut m, "X_OptimACS_FreeMem", util::read_free_mem());
            insert(&mut m, "X_OptimACS_MemTotal", util::read_mem_total());
            insert(
                &mut m,
                "X_OptimACS_KernelVersion",
                util::read_kernel_version(),
            );
            insert(&mut m, "ModelName", util::read_device_model());
            insert(&mut m, "ProcessorArchitecture", util::read_device_arch());
            insert(&mut m, "Manufacturer", read_manufacturer());
            insert(
                &mut m,
                "ManufacturerOUI",
                util::read_manufacturer_oui(&cfg.mac_addr),
            );
            insert(&mut m, "Description", util::read_device_description());
            insert(&mut m, "BaseMacAddress", cfg.mac_addr.clone());
            // AdditionalSoftwareVersion: space-separated list of additional
            // firmware component versions.  Kernel version is a legitimate entry.
            insert(
                &mut m,
                "AdditionalSoftwareVersion",
                format!("kernel:{}", util::read_kernel_version()),
            );
            insert(&mut m, "ProductClass", read_product_class());
            insert(&mut m, "DeviceStatus", util::read_device_status());
            // Sub-objects returned on full query
            m.insert(format!("{base}MemoryStatus.Total"), util::read_mem_total());
            m.insert(format!("{base}MemoryStatus.Free"), util::read_free_mem());
            let tcp_win = std::fs::read_to_string("/proc/sys/net/core/rmem_max")
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|_| "65535".to_string());
            m.insert(format!("{base}NetworkProperties.MaxTCPWindowSize"), tcp_win);
            let tcp_impl = std::fs::read_to_string("/proc/sys/net/ipv4/tcp_congestion_control")
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|_| "cubic".to_string());
            m.insert(format!("{base}NetworkProperties.TCPImplementation"), tcp_impl);
            m.insert(format!("{base}FirmwareImage.1.Name"), "current".to_string());
            m.insert(format!("{base}FirmwareImage.1.Version"), util::read_fw_version());
            m.insert(format!("{base}FirmwareImage.1.Available"), "true".to_string());
            m.insert(format!("{base}FirmwareImage.1.Status"), "Active".to_string());
        }
        "HostName" => {
            let hostname = uci_backend::get_system_hostname();
            insert(
                &mut m,
                "HostName",
                if hostname.is_empty() {
                    cfg.sys_model.clone()
                } else {
                    hostname
                },
            );
        }
        "SoftwareVersion" => {
            insert(&mut m, "SoftwareVersion", util::read_fw_version());
        }
        "HardwareVersion" => {
            insert(&mut m, "HardwareVersion", cfg.sys_model.clone());
        }
        "SerialNumber" => {
            insert(&mut m, "SerialNumber", read_serial_number());
        }
        "UpTime" => {
            insert(&mut m, "UpTime", util::read_uptime());
        }
        "X_OptimACS_LoadAvg" => {
            insert(&mut m, "X_OptimACS_LoadAvg", util::read_load_avg());
        }
        "X_OptimACS_FreeMem" => {
            insert(&mut m, "X_OptimACS_FreeMem", util::read_free_mem());
        }
        "X_OptimACS_MemTotal" => {
            insert(&mut m, "X_OptimACS_MemTotal", util::read_mem_total());
        }
        "X_OptimACS_KernelVersion" => {
            insert(
                &mut m,
                "X_OptimACS_KernelVersion",
                util::read_kernel_version(),
            );
        }
        "ModelName" => {
            insert(&mut m, "ModelName", util::read_device_model());
        }
        "ProcessorArchitecture" => {
            insert(&mut m, "ProcessorArchitecture", util::read_device_arch());
        }
        "Manufacturer" => {
            insert(&mut m, "Manufacturer", read_manufacturer());
        }
        "ManufacturerOUI" => {
            insert(
                &mut m,
                "ManufacturerOUI",
                util::read_manufacturer_oui(&cfg.mac_addr),
            );
        }
        "Description" => {
            insert(&mut m, "Description", util::read_device_description());
        }
        "BaseMacAddress" => {
            insert(&mut m, "BaseMacAddress", cfg.mac_addr.clone());
        }
        "AdditionalSoftwareVersion" => {
            insert(
                &mut m,
                "AdditionalSoftwareVersion",
                format!("kernel:{}", util::read_kernel_version()),
            );
        }
        "ProductClass" => {
            insert(&mut m, "ProductClass", read_product_class());
        }
        "DeviceStatus" => {
            insert(&mut m, "DeviceStatus", util::read_device_status());
        }
        // ── MemoryStatus ────────────────────────────────────
        sub if sub == "MemoryStatus." || sub.starts_with("MemoryStatus.") => {
            let leaf = sub.strip_prefix("MemoryStatus.").unwrap_or("");
            if leaf.is_empty() || leaf == "Total" {
                m.insert(
                    format!("{base}MemoryStatus.Total"),
                    util::read_mem_total(),
                );
            }
            if leaf.is_empty() || leaf == "Free" {
                m.insert(
                    format!("{base}MemoryStatus.Free"),
                    util::read_free_mem(),
                );
            }
        }
        // ── NetworkProperties ────────────────────────────────
        sub if sub == "NetworkProperties." || sub.starts_with("NetworkProperties.") => {
            let leaf = sub.strip_prefix("NetworkProperties.").unwrap_or("");
            if leaf.is_empty() || leaf == "MaxTCPWindowSize" {
                let val = std::fs::read_to_string("/proc/sys/net/core/rmem_max")
                    .map(|s| s.trim().to_string())
                    .unwrap_or_else(|_| "65535".to_string());
                m.insert(format!("{base}NetworkProperties.MaxTCPWindowSize"), val);
            }
            if leaf.is_empty() || leaf == "TCPImplementation" {
                let val = std::fs::read_to_string(
                    "/proc/sys/net/ipv4/tcp_congestion_control",
                )
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|_| "cubic".to_string());
                m.insert(
                    format!("{base}NetworkProperties.TCPImplementation"),
                    val,
                );
            }
        }
        // ── FirmwareImage ────────────────────────────────────
        sub if sub == "FirmwareImage." || sub.starts_with("FirmwareImage.") => {
            // Single firmware image entry (slot 1)
            m.insert(
                format!("{base}FirmwareImage.1.Name"),
                "current".to_string(),
            );
            m.insert(
                format!("{base}FirmwareImage.1.Version"),
                util::read_fw_version(),
            );
            m.insert(
                format!("{base}FirmwareImage.1.Available"),
                "true".to_string(),
            );
            m.insert(
                format!("{base}FirmwareImage.1.Status"),
                "Active".to_string(),
            );
        }
        "VendorConfigFileNumberOfEntries" => {
            let count = std::fs::read_dir("/etc/config")
                .map(|e| e.filter_map(|f| f.ok()).count())
                .unwrap_or(0);
            m.insert(
                format!("{base}VendorConfigFileNumberOfEntries"),
                count.to_string(),
            );
        }
        // ── ProcessStatus ────────────────────────────────────
        sub if sub.starts_with("ProcessStatus.") => {
            let leaf = sub.trim_start_matches("ProcessStatus.");
            match leaf {
                "CPUUsage" | "" => {
                    // Read CPU usage from /proc/stat (simplified: 1 - idle%)
                    let usage = read_cpu_usage();
                    m.insert(format!("{base}ProcessStatus.CPUUsage"), usage);
                    if leaf.is_empty() {
                        m.insert(
                            format!("{base}ProcessStatus.ProcessNumberOfEntries"),
                            read_process_count(),
                        );
                    }
                }
                "ProcessNumberOfEntries" => {
                    m.insert(
                        format!("{base}ProcessStatus.ProcessNumberOfEntries"),
                        read_process_count(),
                    );
                }
                _ => {}
            }
        }
        // ── TemperatureStatus ────────────────────────────────
        sub if sub.starts_with("TemperatureStatus.") => {
            let leaf = sub.trim_start_matches("TemperatureStatus.");
            if leaf == "TemperatureSensorNumberOfEntries" || leaf.is_empty() {
                let count = count_thermal_zones();
                m.insert(
                    format!("{base}TemperatureStatus.TemperatureSensorNumberOfEntries"),
                    count.to_string(),
                );
            }
            if leaf.starts_with("TemperatureSensor.") || leaf.is_empty() {
                // Parse sensor index: TemperatureSensor.1.Value
                let idx: usize = leaf
                    .split('.')
                    .nth(1)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                if idx > 0 {
                    let zone = idx - 1;
                    let type_path = format!("/sys/class/thermal/thermal_zone{zone}/type");
                    let temp_path = format!("/sys/class/thermal/thermal_zone{zone}/temp");
                    let zone_exists =
                        std::path::Path::new(&format!("/sys/class/thermal/thermal_zone{zone}"))
                            .exists();

                    // TR-181 requires Enable, Status, LastUpdate, Name, Value.
                    m.insert(
                        format!("{base}TemperatureStatus.TemperatureSensor.{idx}.Enable"),
                        "true".to_string(),
                    );
                    m.insert(
                        format!("{base}TemperatureStatus.TemperatureSensor.{idx}.Status"),
                        if zone_exists { "Enabled" } else { "Error" }.to_string(),
                    );
                    // LastUpdate: current time in ISO 8601 (we always just polled it)
                    let now_secs = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    m.insert(
                        format!("{base}TemperatureStatus.TemperatureSensor.{idx}.LastUpdate"),
                        epoch_to_iso8601(now_secs),
                    );

                    if let Ok(name) = std::fs::read_to_string(&type_path) {
                        m.insert(
                            format!("{base}TemperatureStatus.TemperatureSensor.{idx}.Name"),
                            name.trim().to_string(),
                        );
                    }
                    if let Ok(temp) = std::fs::read_to_string(&temp_path) {
                        let millideg: i64 = temp.trim().parse().unwrap_or(0);
                        let deg = millideg / 1000;
                        m.insert(
                            format!("{base}TemperatureStatus.TemperatureSensor.{idx}.Value"),
                            deg.to_string(),
                        );
                    }
                }
            }
        }
        // ── VendorLogFile ────────────────────────────────────
        sub if sub.starts_with("VendorLogFile") => {
            let leaf = sub.trim_start_matches("VendorLogFile");
            if leaf == "NumberOfEntries" || leaf.is_empty() {
                // Check if syslog exists
                let count = if std::path::Path::new("/var/log/syslog").exists()
                    || std::path::Path::new("/tmp/log/messages").exists()
                {
                    1
                } else {
                    0
                };
                m.insert(
                    format!("{base}VendorLogFileNumberOfEntries"),
                    count.to_string(),
                );
            }
            if leaf.starts_with(".1.") || leaf.is_empty() {
                let log_path = if std::path::Path::new("/var/log/syslog").exists() {
                    "/var/log/syslog"
                } else if std::path::Path::new("/tmp/log/messages").exists() {
                    "/tmp/log/messages"
                } else {
                    ""
                };
                if !log_path.is_empty() {
                    m.insert(format!("{base}VendorLogFile.1.Name"), "syslog".to_string());
                    if let Ok(meta) = std::fs::metadata(log_path) {
                        m.insert(
                            format!("{base}VendorLogFile.1.Size"),
                            meta.len().to_string(),
                        );
                        if let Ok(modified) = meta.modified() {
                            // TR-181 dateTime must be ISO 8601 / TR-106 format.
                            let secs = modified
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs();
                            m.insert(
                                format!("{base}VendorLogFile.1.LastModified"),
                                epoch_to_iso8601(secs),
                            );
                        }
                    }
                }
            }
        }
        // ── X_TP_LEDs ────────────────────────────────────────
        sub if sub.starts_with("X_TP_LEDs.") => {
            let leaf = sub.trim_start_matches("X_TP_LEDs.");
            if leaf == "LEDNumberOfEntries" || leaf.is_empty() {
                let count = count_leds();
                m.insert(
                    format!("{base}X_TP_LEDs.LEDNumberOfEntries"),
                    count.to_string(),
                );
            }
            if leaf.starts_with("LED.") || leaf.is_empty() {
                let idx: usize = leaf
                    .split('.')
                    .nth(1)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                if idx > 0 {
                    if let Some((name, brightness)) = get_led_info(idx - 1) {
                        m.insert(format!("{base}X_TP_LEDs.LED.{idx}.Name"), name);
                        let status = if brightness > 0 { "On" } else { "Off" };
                        m.insert(
                            format!("{base}X_TP_LEDs.LED.{idx}.Status"),
                            status.to_string(),
                        );
                        m.insert(
                            format!("{base}X_TP_LEDs.LED.{idx}.Enable"),
                            "true".to_string(),
                        );
                    }
                }
            }
        }
        _ => {}
    }
    m
}

fn read_cpu_usage() -> String {
    // Simple: read /proc/loadavg and estimate CPU% from 1-min avg
    // Or read /proc/stat for more accurate measure
    let content = std::fs::read_to_string("/proc/stat").unwrap_or_default();
    if let Some(cpu_line) = content.lines().next() {
        let vals: Vec<u64> = cpu_line
            .split_whitespace()
            .skip(1)
            .filter_map(|s| s.parse().ok())
            .collect();
        if vals.len() >= 4 {
            let total: u64 = vals.iter().sum();
            let idle = vals.get(3).copied().unwrap_or(0);
            if total > 0 {
                let usage = 100 - (idle * 100 / total);
                return usage.to_string();
            }
        }
    }
    "0".to_string()
}

fn read_process_count() -> String {
    std::fs::read_dir("/proc")
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.file_name()
                        .to_str()
                        .map(|s| s.chars().all(|c| c.is_ascii_digit()))
                        .unwrap_or(false)
                })
                .count()
        })
        .unwrap_or(0)
        .to_string()
}

/// Read the hardware serial number from /proc/cpuinfo "Serial" field.
/// Falls back to the board name if absent.
fn read_serial_number() -> String {
    if let Ok(cpuinfo) = std::fs::read_to_string("/proc/cpuinfo") {
        for line in cpuinfo.lines() {
            if let Some(rest) = line.strip_prefix("Serial") {
                if let Some(val) = rest
                    .trim_start_matches(':')
                    .trim()
                    .split_whitespace()
                    .next()
                {
                    if val != "0000000000000000" && !val.is_empty() {
                        return val.to_string();
                    }
                }
            }
        }
    }
    // Fallback: board name from OpenWrt sysinfo
    if let Ok(board) = std::fs::read_to_string("/tmp/sysinfo/board_name") {
        let b = board.trim().to_string();
        if !b.is_empty() {
            return b;
        }
    }
    String::new()
}

/// Read hardware manufacturer from `ubus call system board`.
/// Falls back to "OpenWrt" (the OS brand) only if no hardware manufacturer
/// is discoverable, which is expected on development/emulated environments.
fn read_manufacturer() -> String {
    if let Ok(output) = std::process::Command::new("ubus")
        .args(["call", "system", "board"])
        .output()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        // The "model" field often contains "Manufacturer Model" (e.g. "TP-Link TL-WR841N")
        if let Some(pos) = text.find("\"model\"") {
            let chunk = &text[pos..];
            if let Some(start) = chunk.find('"').and_then(|i| chunk.get(i + 1..)) {
                // skip the "model" key quotes, find next quoted string value
                if let Some(val_start) = start.find('"') {
                    let rest = &start[val_start + 1..];
                    if let Some(val_end) = rest.find('"') {
                        let model = &rest[..val_end];
                        // Extract the first word as manufacturer
                        if let Some(first) = model.split_whitespace().next() {
                            if !first.is_empty() {
                                return first.to_string();
                            }
                        }
                    }
                }
            }
        }
    }
    "OpenWrt".to_string()
}

/// Determine the product class from the device model.
/// TR-181 ProductClass is a string that classifies the device type.
fn read_product_class() -> String {
    if let Ok(output) = std::process::Command::new("ubus")
        .args(["call", "system", "board"])
        .output()
    {
        let text = String::from_utf8_lossy(&output.stdout).to_lowercase();
        if text.contains("router") || text.contains("gateway") {
            return "Gateway".to_string();
        }
        if text.contains("ap") || text.contains("access point") {
            return "AccessPoint".to_string();
        }
        if text.contains("switch") {
            return "Switch".to_string();
        }
    }
    "Gateway".to_string()
}

/// Convert a Unix epoch timestamp to an ISO 8601 / TR-106 dateTime string.
/// Format: "YYYY-MM-DDTHH:MM:SSZ"
fn epoch_to_iso8601(secs: u64) -> String {
    // Manual conversion without external crates.
    // Gregorian calendar calculation from epoch seconds.
    let mut remaining = secs;
    let ss = remaining % 60;
    remaining /= 60;
    let mm = remaining % 60;
    remaining /= 60;
    let hh = remaining % 24;
    let mut days = remaining / 24;

    // Calculate year, month, day from days since 1970-01-01
    let mut year: u64 = 1970;
    loop {
        let days_in_year = if is_leap_year(year) { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }
    let leap = is_leap_year(year);
    let month_days: [u64; 12] = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month: u64 = 1;
    for &md in &month_days {
        if days < md {
            break;
        }
        days -= md;
        month += 1;
    }
    let day = days + 1;
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hh, mm, ss
    )
}

fn is_leap_year(y: u64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn count_thermal_zones() -> usize {
    (0..10)
        .filter(|i| std::path::Path::new(&format!("/sys/class/thermal/thermal_zone{i}")).exists())
        .count()
}

fn count_leds() -> usize {
    std::fs::read_dir("/sys/class/leds")
        .map(|entries| entries.filter_map(|e| e.ok()).count())
        .unwrap_or(0)
}

fn get_led_info(idx: usize) -> Option<(String, u32)> {
    let mut leds: Vec<String> = std::fs::read_dir("/sys/class/leds")
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    leds.sort();
    let name = leds.get(idx)?;
    let brightness: u32 = std::fs::read_to_string(format!("/sys/class/leds/{name}/brightness"))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    Some((name.clone(), brightness))
}

pub fn set(_cfg: &ClientConfig, path: &str, value: &str) -> Result<(), String> {
    use crate::usp::tp469::uci_backend;

    match path {
        "Device.DeviceInfo.HostName" => {
            let result = uci_backend::set_system_hostname(value);
            if result.success {
                Ok(())
            } else {
                Err(result
                    .err_msg
                    .unwrap_or_else(|| "Failed to set hostname".to_string()))
            }
        }
        _ => Err(format!(
            "Device.DeviceInfo.{} is read-only",
            path.trim_start_matches("Device.DeviceInfo.")
        )),
    }
}
