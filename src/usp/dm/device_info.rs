//! TR-181 Device.DeviceInfo.* — reads from /proc and UCI.

use crate::config::ClientConfig;
use crate::util;
use crate::usp::tp469::uci_backend;
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
            insert(&mut m, "HostName", if hostname.is_empty() { cfg.sys_model.clone() } else { hostname });
            insert(&mut m, "SoftwareVersion", util::read_fw_version());
            insert(&mut m, "HardwareVersion", cfg.sys_model.clone());
            insert(&mut m, "SerialNumber", cfg.mac_addr.clone());
            insert(&mut m, "UpTime", util::read_uptime());
            insert(&mut m, "X_OptimACS_LoadAvg", util::read_load_avg());
            insert(&mut m, "X_OptimACS_FreeMem", util::read_free_mem());
            insert(&mut m, "X_OptimACS_MemTotal", util::read_mem_total());
            insert(&mut m, "X_OptimACS_KernelVersion", util::read_kernel_version());
            insert(&mut m, "ModelName", util::read_device_model());
            insert(&mut m, "ProcessorArchitecture", util::read_device_arch());
            insert(&mut m, "Manufacturer", "OpenWrt".to_string());
            insert(&mut m, "ManufacturerOUI", util::read_manufacturer_oui(&cfg.mac_addr));
            insert(&mut m, "Description", util::read_device_description());
            insert(&mut m, "BaseMacAddress", cfg.mac_addr.clone());
            insert(&mut m, "AdditionalSoftwareVersion", util::read_kernel_version());
            insert(&mut m, "ProductClass", "Gateway".to_string());
            insert(&mut m, "DeviceStatus", util::read_device_status());
        }
        "HostName" => {
            let hostname = uci_backend::get_system_hostname();
            insert(&mut m, "HostName", if hostname.is_empty() { cfg.sys_model.clone() } else { hostname });
        }
        "SoftwareVersion" => {
            insert(&mut m, "SoftwareVersion", util::read_fw_version());
        }
        "HardwareVersion" => {
            insert(&mut m, "HardwareVersion", cfg.sys_model.clone());
        }
        "SerialNumber" => {
            insert(&mut m, "SerialNumber", cfg.mac_addr.clone());
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
            insert(&mut m, "X_OptimACS_KernelVersion", util::read_kernel_version());
        }
        "ModelName" => {
            insert(&mut m, "ModelName", util::read_device_model());
        }
        "ProcessorArchitecture" => {
            insert(&mut m, "ProcessorArchitecture", util::read_device_arch());
        }
        "Manufacturer" => {
            insert(&mut m, "Manufacturer", "OpenWrt".to_string());
        }
        "ManufacturerOUI" => {
            insert(&mut m, "ManufacturerOUI", util::read_manufacturer_oui(&cfg.mac_addr));
        }
        "Description" => {
            insert(&mut m, "Description", util::read_device_description());
        }
        "BaseMacAddress" => {
            insert(&mut m, "BaseMacAddress", cfg.mac_addr.clone());
        }
        "AdditionalSoftwareVersion" => {
            insert(&mut m, "AdditionalSoftwareVersion", util::read_kernel_version());
        }
        "ProductClass" => {
            insert(&mut m, "ProductClass", "Gateway".to_string());
        }
        "DeviceStatus" => {
            insert(&mut m, "DeviceStatus", util::read_device_status());
        }
        "VendorConfigFileNumberOfEntries" => {
            m.insert(format!("{base}VendorConfigFileNumberOfEntries"), "0".to_string());
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
                        m.insert(format!("{base}ProcessStatus.ProcessNumberOfEntries"), read_process_count());
                    }
                }
                "ProcessNumberOfEntries" => {
                    m.insert(format!("{base}ProcessStatus.ProcessNumberOfEntries"), read_process_count());
                }
                _ => {}
            }
        }
        // ── TemperatureStatus ────────────────────────────────
        sub if sub.starts_with("TemperatureStatus.") => {
            let leaf = sub.trim_start_matches("TemperatureStatus.");
            if leaf == "TemperatureSensorNumberOfEntries" || leaf.is_empty() {
                let count = count_thermal_zones();
                m.insert(format!("{base}TemperatureStatus.TemperatureSensorNumberOfEntries"), count.to_string());
            }
            if leaf.starts_with("TemperatureSensor.") || leaf.is_empty() {
                // Parse sensor index: TemperatureSensor.1.Value
                let idx: usize = leaf.split('.').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
                if idx > 0 {
                    let zone = idx - 1;
                    let type_path = format!("/sys/class/thermal/thermal_zone{zone}/type");
                    let temp_path = format!("/sys/class/thermal/thermal_zone{zone}/temp");
                    if let Ok(name) = std::fs::read_to_string(&type_path) {
                        m.insert(format!("{base}TemperatureStatus.TemperatureSensor.{idx}.Name"), name.trim().to_string());
                    }
                    if let Ok(temp) = std::fs::read_to_string(&temp_path) {
                        let millideg: i64 = temp.trim().parse().unwrap_or(0);
                        let deg = millideg / 1000;
                        m.insert(format!("{base}TemperatureStatus.TemperatureSensor.{idx}.Value"), deg.to_string());
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
                    || std::path::Path::new("/tmp/log/messages").exists() { 1 } else { 0 };
                m.insert(format!("{base}VendorLogFileNumberOfEntries"), count.to_string());
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
                        m.insert(format!("{base}VendorLogFile.1.Size"), meta.len().to_string());
                        if let Ok(modified) = meta.modified() {
                            let secs = modified.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
                            m.insert(format!("{base}VendorLogFile.1.LastModified"), secs.to_string());
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
                m.insert(format!("{base}X_TP_LEDs.LEDNumberOfEntries"), count.to_string());
            }
            if leaf.starts_with("LED.") || leaf.is_empty() {
                let idx: usize = leaf.split('.').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
                if idx > 0 {
                    if let Some((name, brightness)) = get_led_info(idx - 1) {
                        m.insert(format!("{base}X_TP_LEDs.LED.{idx}.Name"), name);
                        let status = if brightness > 0 { "On" } else { "Off" };
                        m.insert(format!("{base}X_TP_LEDs.LED.{idx}.Status"), status.to_string());
                        m.insert(format!("{base}X_TP_LEDs.LED.{idx}.Enable"), "true".to_string());
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
        let vals: Vec<u64> = cpu_line.split_whitespace().skip(1)
            .filter_map(|s| s.parse().ok()).collect();
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
        .map(|entries| entries.filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_str().map(|s| s.chars().all(|c| c.is_ascii_digit())).unwrap_or(false))
            .count())
        .unwrap_or(0)
        .to_string()
}

fn count_thermal_zones() -> usize {
    (0..10).filter(|i| std::path::Path::new(&format!("/sys/class/thermal/thermal_zone{i}")).exists()).count()
}

fn count_leds() -> usize {
    std::fs::read_dir("/sys/class/leds")
        .map(|entries| entries.filter_map(|e| e.ok()).count())
        .unwrap_or(0)
}

fn get_led_info(idx: usize) -> Option<(String, u32)> {
    let mut leds: Vec<String> = std::fs::read_dir("/sys/class/leds").ok()?
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
