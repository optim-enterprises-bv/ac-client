//! TR-181 Device.Optical.* and Device.XPON.* — GPON/XPON interface objects.
//!
//! Reads optical network terminal data from sysfs and UCI if present.
//! Returns empty on non-fiber hardware.

use crate::config::ClientConfig;
use std::collections::HashMap;

pub type Params = HashMap<String, String>;

pub async fn get(_cfg: &ClientConfig, path: &str) -> Params {
    let mut m = Params::new();
    let ifaces = get_optical_interfaces();

    if path.starts_with("Device.Optical.") || path == "Device.Optical." {
        m.insert(
            "Device.Optical.InterfaceNumberOfEntries".to_string(),
            ifaces.len().to_string(),
        );

        for (i, iface) in ifaces.iter().enumerate() {
            let idx = i + 1;
            let base = format!("Device.Optical.Interface.{idx}.");
            m.insert(format!("{base}Enable"), "true".to_string());
            m.insert(format!("{base}Status"), iface.status.clone());
            m.insert(format!("{base}Name"), iface.name.clone());
            if !iface.optical_signal_level.is_empty() {
                m.insert(
                    format!("{base}OpticalSignalLevel"),
                    iface.optical_signal_level.clone(),
                );
            }
            if !iface.tx_power.is_empty() {
                m.insert(
                    format!("{base}TransmitOpticalLevel"),
                    iface.tx_power.clone(),
                );
            }
            m.insert(
                format!("{base}LowerLayers"),
                String::new(),
            );
        }
    }

    m
}

struct OpticalInterface {
    name: String,
    status: String,
    optical_signal_level: String,
    tx_power: String,
}

fn get_optical_interfaces() -> Vec<OpticalInterface> {
    let mut ifaces = Vec::new();

    // Check for GPON/XPON interfaces in sysfs
    if let Ok(entries) = std::fs::read_dir("/sys/class/net") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            // Common GPON interface names
            if name.starts_with("gpon") || name.starts_with("xpon") || name.starts_with("pon") {
                let status = std::fs::read_to_string(format!("/sys/class/net/{name}/operstate"))
                    .map(|s| {
                        if s.trim() == "up" {
                            "Up"
                        } else {
                            "Down"
                        }
                        .to_string()
                    })
                    .unwrap_or_else(|_| "Down".to_string());

                // Try to read optical levels from driver-specific paths
                let rx_power = std::fs::read_to_string(format!(
                    "/sys/class/net/{name}/device/pon_stats/rx_power"
                ))
                .ok()
                .or_else(|| {
                    std::process::Command::new("uci")
                        .args(["get", "gpon.onu.rx_power"])
                        .output()
                        .ok()
                        .and_then(|o| String::from_utf8(o.stdout).ok())
                })
                .map(|s| s.trim().to_string())
                .unwrap_or_default();

                let tx_power = std::fs::read_to_string(format!(
                    "/sys/class/net/{name}/device/pon_stats/tx_power"
                ))
                .map(|s| s.trim().to_string())
                .unwrap_or_default();

                ifaces.push(OpticalInterface {
                    name,
                    status,
                    optical_signal_level: rx_power,
                    tx_power,
                });
            }
        }
    }

    ifaces
}

pub async fn set(_cfg: &ClientConfig, path: &str, _value: &str) -> Result<(), String> {
    Err(format!("Device.Optical is read-only: {path}"))
}
