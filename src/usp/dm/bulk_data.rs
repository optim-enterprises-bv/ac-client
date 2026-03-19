//! TR-181 Device.BulkData.* — Bulk Data Collection (TR-232).
//!
//! Implements Device.BulkData.Profile.{i}. for periodic bulk parameter
//! collection and HTTP/HTTPS upload per TR-232.

use crate::config::ClientConfig;
use std::collections::HashMap;
use std::sync::Mutex;

pub type Params = HashMap<String, String>;

// ── In-memory profile store ──────────────────────────────────────────────────

#[derive(Clone, Default)]
struct BulkDataProfile {
    enable: bool,
    protocol: String,       // "HTTP" or "USPEventNotif"
    reporting_interval: u32, // seconds
    url: String,
    parameter_refs: Vec<String>, // TR-181 paths to collect
}

static PROFILES: Mutex<Option<Vec<BulkDataProfile>>> = Mutex::new(None);

fn get_profiles() -> Vec<BulkDataProfile> {
    PROFILES.lock().unwrap().clone().unwrap_or_default()
}

pub fn add_profile(
    enable: bool,
    protocol: &str,
    interval: u32,
    url: &str,
) -> u32 {
    let mut guard = PROFILES.lock().unwrap();
    let profiles = guard.get_or_insert_with(Vec::new);
    profiles.push(BulkDataProfile {
        enable,
        protocol: protocol.to_string(),
        reporting_interval: interval,
        url: url.to_string(),
        parameter_refs: Vec::new(),
    });
    profiles.len() as u32
}

pub fn delete_profile(instance: u32) -> bool {
    let mut guard = PROFILES.lock().unwrap();
    let profiles = guard.get_or_insert_with(Vec::new);
    let idx = (instance as usize).wrapping_sub(1);
    if idx < profiles.len() {
        profiles.remove(idx);
        true
    } else {
        false
    }
}

// ── GET handler ──────────────────────────────────────────────────────────────

pub async fn get(cfg: &ClientConfig, path: &str) -> Params {
    let mut m = Params::new();
    let profiles = get_profiles();

    if path == "Device.BulkData." || path.contains("Enable") && !path.contains("Profile") {
        m.insert(
            "Device.BulkData.Enable".to_string(),
            (cfg.bulk_data_interval > 0).to_string(),
        );
        m.insert(
            "Device.BulkData.Status".to_string(),
            if cfg.bulk_data_interval > 0 { "Enabled" } else { "Disabled" }.to_string(),
        );
        m.insert(
            "Device.BulkData.MinReportingInterval".to_string(),
            "60".to_string(),
        );
        m.insert(
            "Device.BulkData.Protocols".to_string(),
            "HTTP,USPEventNotif".to_string(),
        );
        m.insert(
            "Device.BulkData.EncodingTypes".to_string(),
            "JSON,CSV".to_string(),
        );
        m.insert(
            "Device.BulkData.ProfileNumberOfEntries".to_string(),
            profiles.len().to_string(),
        );
    }

    if path == "Device.BulkData." || path.starts_with("Device.BulkData.Profile.") {
        for (i, p) in profiles.iter().enumerate() {
            let idx = i + 1;
            let base = format!("Device.BulkData.Profile.{idx}.");
            m.insert(format!("{base}Enable"), p.enable.to_string());
            m.insert(format!("{base}Protocol"), p.protocol.clone());
            m.insert(format!("{base}ReportingInterval"), p.reporting_interval.to_string());
            m.insert(format!("{base}HTTP.URL"), p.url.clone());
            m.insert(
                format!("{base}Parameter.NumberOfEntries"),
                p.parameter_refs.len().to_string(),
            );
            for (pi, pr) in p.parameter_refs.iter().enumerate() {
                let pidx = pi + 1;
                m.insert(
                    format!("{base}Parameter.{pidx}.Reference"),
                    pr.clone(),
                );
            }
        }
    }

    m
}

pub async fn set(_cfg: &ClientConfig, path: &str, value: &str) -> Result<(), String> {
    let profiles_count = get_profiles().len();
    if path.starts_with("Device.BulkData.Profile.") {
        let idx = extract_index(path, "Profile.")
            .ok_or("Invalid Profile index")?;
        if idx == 0 || idx > profiles_count {
            return Err(format!("Profile index {idx} out of range"));
        }
        let mut guard = PROFILES.lock().unwrap();
        let profiles = guard.get_or_insert_with(Vec::new);
        let p = &mut profiles[idx - 1];

        if path.ends_with("Enable") {
            p.enable = value == "true" || value == "1";
        } else if path.ends_with("Protocol") {
            p.protocol = value.to_string();
        } else if path.ends_with("ReportingInterval") {
            p.reporting_interval = value.parse().unwrap_or(300);
        } else if path.contains("HTTP.URL") {
            p.url = value.to_string();
        } else if path.contains("Parameter.") && path.ends_with("Reference") {
            p.parameter_refs.push(value.to_string());
        } else {
            return Err(format!("Unknown BulkData param: {path}"));
        }
        return Ok(());
    }
    Err(format!("Read-only BulkData param: {path}"))
}

/// Collect bulk data for all enabled profiles and return collected data.
/// Called periodically by the agent's bulk data loop.
pub async fn collect_bulk_data(cfg: &ClientConfig) -> Vec<(String, HashMap<String, String>)> {
    let profiles = get_profiles();
    let mut results = Vec::new();

    for (_i, p) in profiles.iter().enumerate() {
        if !p.enable {
            continue;
        }
        let paths: Vec<String> = p.parameter_refs.clone();
        if paths.is_empty() {
            continue;
        }
        let params = crate::usp::dm::get_params(cfg, &paths, 0).await;
        if !params.is_empty() {
            results.push((p.url.clone(), params));
        }
    }

    results
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
