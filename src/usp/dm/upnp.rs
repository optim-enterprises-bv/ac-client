//! TR-181 Device.UPnP.* — UPnP/IGD state.
//!
//! Reports whether miniupnpd is running and its configuration from UCI.

use crate::config::ClientConfig;
use std::collections::HashMap;

pub type Params = HashMap<String, String>;

pub async fn get(_cfg: &ClientConfig, path: &str) -> Params {
    let mut m = Params::new();
    let base = "Device.UPnP.Device.";

    let enabled = is_upnp_enabled();
    let running = is_upnp_running();

    if path == "Device.UPnP." || path.starts_with("Device.UPnP.Device.") {
        m.insert(format!("{base}Enable"), enabled.to_string());
        m.insert(
            format!("{base}Status"),
            if running { "Up" } else { "Down" }.to_string(),
        );
        m.insert(
            format!("{base}UPnPMediaServer"),
            "false".to_string(),
        );
        m.insert(
            format!("{base}UPnPMediaRenderer"),
            "false".to_string(),
        );
        m.insert(
            format!("{base}UPnPWLANAccessPoint"),
            "false".to_string(),
        );
        m.insert(
            format!("{base}UPnPIGD"),
            running.to_string(),
        );
    }

    m
}

fn is_upnp_enabled() -> bool {
    std::process::Command::new("uci")
        .args(["get", "upnpd.config.enabled"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| {
            let v = s.trim();
            v == "1" || v == "true"
        })
        .unwrap_or(false)
}

fn is_upnp_running() -> bool {
    std::process::Command::new("pgrep")
        .args(["-x", "miniupnpd"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub async fn set(_cfg: &ClientConfig, path: &str, _value: &str) -> Result<(), String> {
    Err(format!("Device.UPnP is read-only: {path}"))
}
