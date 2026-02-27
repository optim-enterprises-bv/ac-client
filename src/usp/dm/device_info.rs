//! TR-181 Device.DeviceInfo.* — reads from /proc and UCI.

use std::collections::HashMap;
use crate::config::ClientConfig;
use crate::util;

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
        "SoftwareVersion" => { insert(&mut m, "SoftwareVersion", util::read_fw_version()); }
        "HardwareVersion" => { insert(&mut m, "HardwareVersion", cfg.sys_model.clone()); }
        "SerialNumber"    => { insert(&mut m, "SerialNumber", cfg.mac_addr.clone()); }
        "UpTime"          => { insert(&mut m, "UpTime", util::read_uptime()); }
        "X_OptimACS_LoadAvg" => { insert(&mut m, "X_OptimACS_LoadAvg", util::read_load_avg()); }
        "X_OptimACS_FreeMem" => { insert(&mut m, "X_OptimACS_FreeMem", util::read_free_mem()); }
        _ => {}
    }
    m
}

pub fn set(_cfg: &ClientConfig, _path: &str, _value: &str) -> Result<(), String> {
    // DeviceInfo is largely read-only; HostName could be set via UCI
    Err("Device.DeviceInfo.* is read-only".into())
}
