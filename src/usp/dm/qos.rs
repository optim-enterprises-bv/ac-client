//! TR-181 Device.QoS.* — Quality of Service configuration.
//!
//! Reads QoS queues and classifications from UCI `qos` package and
//! SQM (Smart Queue Management) configuration.

use crate::config::ClientConfig;
use std::collections::HashMap;

pub type Params = HashMap<String, String>;

pub async fn get(_cfg: &ClientConfig, path: &str) -> Params {
    let mut m = Params::new();
    let queues = get_qos_queues();
    let classifications = get_qos_classifications();

    if path == "Device.QoS." || path.contains("QueueNumberOfEntries") {
        m.insert(
            "Device.QoS.QueueNumberOfEntries".to_string(),
            queues.len().to_string(),
        );
    }
    if path == "Device.QoS." || path.contains("ClassificationNumberOfEntries") {
        m.insert(
            "Device.QoS.ClassificationNumberOfEntries".to_string(),
            classifications.len().to_string(),
        );
    }

    if path == "Device.QoS." || path.starts_with("Device.QoS.Queue.") {
        let specific_idx = extract_index(path, "Queue.");
        for (i, q) in queues.iter().enumerate() {
            let idx = i + 1;
            if let Some(si) = specific_idx {
                if si != idx {
                    continue;
                }
            }
            let base = format!("Device.QoS.Queue.{idx}.");
            m.insert(format!("{base}Enable"), q.enable.to_string());
            m.insert(
                format!("{base}Status"),
                if q.enable { "Enabled" } else { "Disabled" }.to_string(),
            );
            m.insert(format!("{base}Interface"), q.interface.clone());
            m.insert(
                format!("{base}ShapingRate"),
                q.shaping_rate.to_string(),
            );
            // Server polls "Bandwidth" — alias for ShapingRate
            m.insert(
                format!("{base}Bandwidth"),
                q.shaping_rate.to_string(),
            );
            m.insert(format!("{base}Alias"), q.name.clone());
        }
    }

    if path == "Device.QoS." || path.starts_with("Device.QoS.Classification.") {
        let specific_idx = extract_index(path, "Classification.");
        for (i, c) in classifications.iter().enumerate() {
            let idx = i + 1;
            if let Some(si) = specific_idx {
                if si != idx {
                    continue;
                }
            }
            let base = format!("Device.QoS.Classification.{idx}.");
            m.insert(format!("{base}Enable"), c.enable.to_string());
            m.insert(format!("{base}Alias"), c.name.clone());
            m.insert(format!("{base}Order"), (i + 1).to_string());
            m.insert(format!("{base}Interface"), c.interface.clone());
            m.insert(format!("{base}Protocol"), c.proto.clone());
            m.insert(format!("{base}DestPort"), c.dest_port.clone());
            m.insert(format!("{base}SourcePort"), c.src_port.clone());
            m.insert(format!("{base}DSCPMark"), c.dscp_mark.clone());
        }
    }

    m
}

struct QosQueue {
    name: String,
    enable: bool,
    interface: String,
    shaping_rate: i32, // kbps, -1 = unlimited
}

struct QosClassification {
    name: String,
    enable: bool,
    interface: String,
    proto: String,
    dest_port: String,
    src_port: String,
    dscp_mark: String,
}

fn get_qos_queues() -> Vec<QosQueue> {
    let mut queues = Vec::new();

    // Try SQM (sqm-scripts) first — more common on modern OpenWrt
    let sqm_out = std::process::Command::new("uci")
        .args(["show", "sqm"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    let mut current = String::new();
    let mut enabled = false;
    let mut iface = String::new();
    let mut download = 0i32;
    let mut upload = 0i32;

    for line in sqm_out.lines() {
        if line.contains(".interface=") {
            if !current.is_empty() && enabled {
                // Two queues per SQM instance: download + upload
                queues.push(QosQueue {
                    name: format!("{current}_download"),
                    enable: true,
                    interface: iface.clone(),
                    shaping_rate: download,
                });
                queues.push(QosQueue {
                    name: format!("{current}_upload"),
                    enable: true,
                    interface: iface.clone(),
                    shaping_rate: upload,
                });
            }
            current = line.split('.').nth(1).unwrap_or("").to_string();
            iface = line.split('=').nth(1).unwrap_or("").trim_matches('\'').to_string();
            enabled = false;
            download = -1;
            upload = -1;
        }
        if line.contains(".enabled=") {
            let val = line.split('=').nth(1).unwrap_or("").trim_matches('\'');
            enabled = val == "1" || val == "true";
        }
        if line.contains(".download=") {
            download = line
                .split('=')
                .nth(1)
                .unwrap_or("")
                .trim_matches('\'')
                .parse()
                .unwrap_or(-1);
        }
        if line.contains(".upload=") {
            upload = line
                .split('=')
                .nth(1)
                .unwrap_or("")
                .trim_matches('\'')
                .parse()
                .unwrap_or(-1);
        }
    }
    if !current.is_empty() && enabled {
        queues.push(QosQueue {
            name: format!("{current}_download"),
            enable: true,
            interface: iface.clone(),
            shaping_rate: download,
        });
        queues.push(QosQueue {
            name: format!("{current}_upload"),
            enable: true,
            interface: iface,
            shaping_rate: upload,
        });
    }

    queues
}

fn get_qos_classifications() -> Vec<QosClassification> {
    let mut classifications = Vec::new();

    let qos_out = std::process::Command::new("uci")
        .args(["show", "qos"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    // Parse qos.@classify[] or qos.@rule[] sections
    use std::collections::BTreeMap;
    let mut by_idx: BTreeMap<usize, QosClassification> = BTreeMap::new();

    for line in qos_out.lines() {
        let is_classify = line.contains("@classify[") || line.contains("@rule[");
        if !is_classify {
            continue;
        }

        let idx_start = line.find('[').map(|p| p + 1);
        let idx_end = line.find(']');
        let idx: usize = match (idx_start, idx_end) {
            (Some(s), Some(e)) => line[s..e].parse().unwrap_or(usize::MAX),
            _ => continue,
        };
        if idx == usize::MAX {
            continue;
        }

        let key_val: Vec<&str> = line.splitn(2, '=').collect();
        if key_val.len() != 2 {
            continue;
        }
        let key = key_val[0].rsplit('.').next().unwrap_or("");
        let val = key_val[1].trim_matches('\'').to_string();

        let c = by_idx.entry(idx).or_insert_with(|| QosClassification {
            name: String::new(),
            enable: true,
            interface: String::new(),
            proto: String::new(),
            dest_port: String::new(),
            src_port: String::new(),
            dscp_mark: String::new(),
        });

        match key {
            "name" | "comment" => c.name = val,
            "proto" => c.proto = val,
            "ports" | "dstport" => c.dest_port = val,
            "srcport" => c.src_port = val,
            "dscp" => c.dscp_mark = val,
            "target" => c.interface = val,
            _ => {}
        }
    }

    classifications.extend(by_idx.into_values());
    classifications
}

fn extract_index(path: &str, key: &str) -> Option<usize> {
    if let Some(pos) = path.find(key) {
        let rest = &path[pos + key.len()..];
        let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        num_str.parse().ok()
    } else {
        None
    }
}

pub async fn set(_cfg: &ClientConfig, path: &str, _value: &str) -> Result<(), String> {
    Err(format!("Device.QoS is read-only: {path}"))
}
