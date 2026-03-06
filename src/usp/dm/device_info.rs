//! TR-181 Device.DeviceInfo.* — reads from /proc and UCI.

use crate::config::ClientConfig;
use crate::util;
use std::collections::HashMap;

pub fn get(cfg: &ClientConfig, path: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    let base = "Device.DeviceInfo.";
    let insert = |m: &mut HashMap<String, String>, suffix: &str, val: String| {
        m.insert(format!("{base}{suffix}"), val);
    };
    match path.trim_start_matches(base) {
        "HostName" | "" => {
            insert(&mut m, "HostName", cfg.sys_model.clone());
            if path.trim_start_matches(base).is_empty() {
                insert(&mut m, "SoftwareVersion", util::read_fw_version());
                insert(&mut m, "HardwareVersion", cfg.sys_model.clone());
                insert(&mut m, "SerialNumber", cfg.mac_addr.clone());
                insert(&mut m, "UpTime", util::read_uptime());
                insert(&mut m, "X_OptimACS_LoadAvg", util::read_load_avg());
                insert(&mut m, "X_OptimACS_FreeMem", util::read_free_mem());
            }
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
        _ => {}
    }
    m
}

pub fn set(_cfg: &ClientConfig, path: &str, value: &str) -> Result<(), String> {
    use crate::usp::tp469::uci_backend;

    match path {
        "Device.DeviceInfo.HostName" => {
            // Set system hostname via UCI
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
