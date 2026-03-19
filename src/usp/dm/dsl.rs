//! TR-181 Device.DSL.* — DSL line and channel objects.
//!
//! Reads DSL stats from /sys/class/atm or /proc/driver/dsl if present.
//! Returns empty on non-DSL hardware (most OpenWrt devices).

use crate::config::ClientConfig;
use std::collections::HashMap;

pub type Params = HashMap<String, String>;

pub async fn get(_cfg: &ClientConfig, path: &str) -> Params {
    let mut m = Params::new();
    let lines = get_dsl_lines();

    if path == "Device.DSL." || path.contains("LineNumberOfEntries") {
        m.insert(
            "Device.DSL.LineNumberOfEntries".to_string(),
            lines.len().to_string(),
        );
        m.insert(
            "Device.DSL.ChannelNumberOfEntries".to_string(),
            lines.len().to_string(),
        );
    }

    if path == "Device.DSL." || path.starts_with("Device.DSL.Line.") {
        for (i, line) in lines.iter().enumerate() {
            let idx = i + 1;
            let base = format!("Device.DSL.Line.{idx}.");
            m.insert(format!("{base}Enable"), "true".to_string());
            m.insert(format!("{base}Status"), line.status.clone());
            m.insert(format!("{base}StandardUsed"), line.standard.clone());
            m.insert(
                format!("{base}UpstreamMaxBitRate"),
                line.upstream_max.to_string(),
            );
            m.insert(
                format!("{base}DownstreamMaxBitRate"),
                line.downstream_max.to_string(),
            );
            m.insert(
                format!("{base}UpstreamAttenuation"),
                line.upstream_atten.to_string(),
            );
            m.insert(
                format!("{base}DownstreamAttenuation"),
                line.downstream_atten.to_string(),
            );
            m.insert(
                format!("{base}UpstreamNoiseMargin"),
                line.upstream_snr.to_string(),
            );
            m.insert(
                format!("{base}DownstreamNoiseMargin"),
                line.downstream_snr.to_string(),
            );
            m.insert(
                format!("{base}UpstreamPower"),
                line.upstream_power.to_string(),
            );
            m.insert(
                format!("{base}DownstreamPower"),
                line.downstream_power.to_string(),
            );
        }
    }

    m
}

struct DslLine {
    status: String,
    standard: String,
    upstream_max: u64,
    downstream_max: u64,
    upstream_atten: i32,
    downstream_atten: i32,
    upstream_snr: i32,
    downstream_snr: i32,
    upstream_power: i32,
    downstream_power: i32,
}

fn get_dsl_lines() -> Vec<DslLine> {
    // Try reading from the xDSL driver
    let output = std::process::Command::new("dsl_cpe_pipe")
        .args(["ifx", "lsg"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok());

    // Fallback: try /proc/driver/dsl
    let proc_output = std::fs::read_to_string("/proc/driver/dsl/status").ok();

    let text = output.or(proc_output).unwrap_or_default();
    if text.is_empty() {
        return Vec::new();
    }

    // Parse generic DSL stats output
    let mut line = DslLine {
        status: "Up".to_string(),
        standard: "ADSL2+".to_string(),
        upstream_max: 0,
        downstream_max: 0,
        upstream_atten: 0,
        downstream_atten: 0,
        upstream_snr: 0,
        downstream_snr: 0,
        upstream_power: 0,
        downstream_power: 0,
    };

    for l in text.lines() {
        let lower = l.to_lowercase();
        if lower.contains("status") && lower.contains("showtime") {
            line.status = "Up".to_string();
        } else if lower.contains("status") && lower.contains("idle") {
            line.status = "Down".to_string();
        }
        // Parse key-value pairs from DSL stats
        if let Some((key, val)) = l.split_once(':') {
            let val = val.trim().split_whitespace().next().unwrap_or("");
            let key_lower = key.to_lowercase();
            if key_lower.contains("upstream") && key_lower.contains("max") {
                line.upstream_max = val.parse().unwrap_or(0);
            }
            if key_lower.contains("downstream") && key_lower.contains("max") {
                line.downstream_max = val.parse().unwrap_or(0);
            }
            if key_lower.contains("attenuation") && key_lower.contains("up") {
                line.upstream_atten = parse_dsl_val(val);
            }
            if key_lower.contains("attenuation") && key_lower.contains("down") {
                line.downstream_atten = parse_dsl_val(val);
            }
            if key_lower.contains("snr") && key_lower.contains("up") {
                line.upstream_snr = parse_dsl_val(val);
            }
            if key_lower.contains("snr") && key_lower.contains("down") {
                line.downstream_snr = parse_dsl_val(val);
            }
        }
    }

    if line.status != "Down" || line.downstream_max > 0 {
        vec![line]
    } else {
        Vec::new()
    }
}

fn parse_dsl_val(s: &str) -> i32 {
    // DSL values often in tenths of dB: "12.5" → 125
    s.parse::<f64>().map(|v| (v * 10.0) as i32).unwrap_or(0)
}

pub async fn set(_cfg: &ClientConfig, path: &str, _value: &str) -> Result<(), String> {
    Err(format!("Device.DSL is read-only: {path}"))
}
