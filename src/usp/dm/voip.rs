//! TR-181 Device.Services.VoIPProfile.* — VoIP/SIP service objects.
//!
//! Reads SIP configuration from UCI `voice_client` or `asterisk` package if present.
//! Returns empty on devices without VoIP.

use crate::config::ClientConfig;
use std::collections::HashMap;

pub type Params = HashMap<String, String>;

pub async fn get(_cfg: &ClientConfig, path: &str) -> Params {
    let mut m = Params::new();
    let profiles = get_sip_profiles();

    if path.starts_with("Device.Services.VoIPProfile.")
        || path == "Device.Services."
        || path.contains("VoIPProfileNumberOfEntries")
    {
        m.insert(
            "Device.Services.VoIPProfileNumberOfEntries".to_string(),
            profiles.len().to_string(),
        );
    }

    if path == "Device.Services."
        || path.starts_with("Device.Services.VoIPProfile.")
    {
        for (i, p) in profiles.iter().enumerate() {
            let idx = i + 1;
            let base = format!("Device.Services.VoIPProfile.{idx}.");
            m.insert(format!("{base}Enable"), p.enabled.to_string());
            m.insert(format!("{base}Name"), p.name.clone());
            m.insert(format!("{base}SignallingProtocol"), "SIP".to_string());

            let sb = format!("{base}SIP.");
            m.insert(format!("{sb}ProxyServer"), p.proxy.clone());
            m.insert(format!("{sb}RegistrarServer"), p.registrar.clone());
            m.insert(format!("{sb}UserAgentDomain"), p.domain.clone());
            m.insert(format!("{sb}OutboundProxy"), p.outbound_proxy.clone());

            let lb = format!("{base}Line.1.");
            m.insert(format!("{lb}Enable"), p.enabled.to_string());
            m.insert(
                format!("{lb}Status"),
                if p.registered { "Up" } else { "Disabled" }.to_string(),
            );
            m.insert(format!("{lb}SIP.AuthUserName"), p.auth_user.clone());
            m.insert(format!("{lb}SIP.URI"), p.sip_uri.clone());
        }
    }

    m
}

struct SipProfile {
    name: String,
    enabled: bool,
    registered: bool,
    proxy: String,
    registrar: String,
    domain: String,
    outbound_proxy: String,
    auth_user: String,
    sip_uri: String,
}

fn get_sip_profiles() -> Vec<SipProfile> {
    // Try UCI voice_client package (common on OpenWrt VoIP gateways)
    let output = std::process::Command::new("uci")
        .args(["show", "voice_client"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    if output.is_empty() {
        // Try asterisk
        let ast_output = std::process::Command::new("uci")
            .args(["show", "asterisk"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .unwrap_or_default();

        if ast_output.is_empty() {
            return Vec::new();
        }
    }

    // Parse SIP accounts from UCI
    let mut profiles = Vec::new();
    let mut current = SipProfile {
        name: String::new(),
        enabled: false,
        registered: false,
        proxy: String::new(),
        registrar: String::new(),
        domain: String::new(),
        outbound_proxy: String::new(),
        auth_user: String::new(),
        sip_uri: String::new(),
    };
    let mut found_sip = false;

    for line in output.lines() {
        if line.contains(".sip_") || line.contains("sip.") {
            found_sip = true;
            if let Some((key, val)) = line.rsplit_once('=') {
                let val = val.trim_matches('\'');
                let key_name = key.rsplit('.').next().unwrap_or("");
                match key_name {
                    "enabled" => current.enabled = val == "1",
                    "name" | "displayname" => current.name = val.to_string(),
                    "host" | "proxy" => current.proxy = val.to_string(),
                    "registrar" => current.registrar = val.to_string(),
                    "domain" => current.domain = val.to_string(),
                    "outboundproxy" => current.outbound_proxy = val.to_string(),
                    "authuser" | "username" => current.auth_user = val.to_string(),
                    "uri" => current.sip_uri = val.to_string(),
                    _ => {}
                }
            }
        }
    }

    if found_sip && !current.proxy.is_empty() {
        if current.registrar.is_empty() {
            current.registrar = current.proxy.clone();
        }
        profiles.push(current);
    }

    profiles
}

pub async fn set(_cfg: &ClientConfig, path: &str, _value: &str) -> Result<(), String> {
    Err(format!("Device.Services.VoIPProfile is read-only: {path}"))
}
