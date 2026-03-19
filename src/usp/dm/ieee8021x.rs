//! TR-181 Device.IEEE8021x.* — 802.1x port-based authentication.
//!
//! Reads 802.1x supplicant state from wpa_supplicant or hostapd on OpenWrt.

use crate::config::ClientConfig;
use std::collections::HashMap;

pub type Params = HashMap<String, String>;

pub async fn get(_cfg: &ClientConfig, path: &str) -> Params {
    let mut m = Params::new();
    let supplicants = get_supplicants();

    if path == "Device.IEEE8021x." || path.contains("SupplicantNumberOfEntries") {
        m.insert(
            "Device.IEEE8021x.SupplicantNumberOfEntries".to_string(),
            supplicants.len().to_string(),
        );
    }

    if path == "Device.IEEE8021x." || path.starts_with("Device.IEEE8021x.Supplicant.") {
        for (i, s) in supplicants.iter().enumerate() {
            let idx = i + 1;
            let base = format!("Device.IEEE8021x.Supplicant.{idx}.");
            m.insert(format!("{base}Enable"), "true".to_string());
            m.insert(format!("{base}Status"), s.status.clone());
            m.insert(format!("{base}Interface"), s.interface.clone());
            m.insert(format!("{base}PAEState"), s.pae_state.clone());
            m.insert(format!("{base}EAPIdentity"), s.identity.clone());
        }
    }

    m
}

struct Supplicant {
    interface: String,
    status: String,
    pae_state: String,
    identity: String,
}

fn get_supplicants() -> Vec<Supplicant> {
    // Check if wpa_supplicant is running for any wired 802.1x interfaces
    let output = std::process::Command::new("wpa_cli")
        .args(["-i", "eth0", "status"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    if output.is_empty() || output.contains("Failed") {
        return Vec::new();
    }

    let mut supplicant = Supplicant {
        interface: "eth0".to_string(),
        status: "Disabled".to_string(),
        pae_state: "Disconnected".to_string(),
        identity: String::new(),
    };

    for line in output.lines() {
        if let Some((key, val)) = line.split_once('=') {
            match key {
                "wpa_state" => {
                    supplicant.status = if val == "COMPLETED" {
                        "Enabled".to_string()
                    } else {
                        "Error".to_string()
                    };
                    supplicant.pae_state = match val {
                        "COMPLETED" => "Authenticated",
                        "ASSOCIATING" | "ASSOCIATED" => "Connecting",
                        "DISCONNECTED" => "Disconnected",
                        _ => "Held",
                    }
                    .to_string();
                }
                "identity" => supplicant.identity = val.to_string(),
                _ => {}
            }
        }
    }

    vec![supplicant]
}

pub async fn set(_cfg: &ClientConfig, path: &str, _value: &str) -> Result<(), String> {
    Err(format!("Device.IEEE8021x is read-only: {path}"))
}
