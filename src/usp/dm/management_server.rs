//! TR-181 Device.ManagementServer.* — management server connection parameters.
//!
//! Exposes the controller connection URL, connection status, and periodic
//! inform interval. Mirrors relevant fields from ClientConfig.

use crate::config::{ClientConfig, MtpType};
use std::collections::HashMap;

pub type Params = HashMap<String, String>;

pub fn get(cfg: &ClientConfig, path: &str) -> Params {
    let mut m = Params::new();
    let base = "Device.ManagementServer.";

    let sub = path.strip_prefix(base).unwrap_or("");

    if sub.is_empty() || sub == "URL" {
        let url = match cfg.mtp {
            MtpType::WebSocket | MtpType::Both | MtpType::All => cfg
                .ws_url
                .clone()
                .unwrap_or_else(|| format!("wss://{}:{}/usp", cfg.server_host, cfg.server_port)),
            MtpType::Mqtt => cfg.mqtt_url.clone().unwrap_or_default(),
            MtpType::Stomp => cfg.stomp_url.clone().unwrap_or_default(),
            MtpType::CoAP => cfg.coap_url.clone().unwrap_or_default(),
        };
        m.insert(format!("{base}URL"), url);
    }
    if sub.is_empty() || sub == "EnableCWMP" {
        m.insert(format!("{base}EnableCWMP"), "false".to_string());
    }
    if sub.is_empty() || sub == "ConnectionRequestURL" {
        // USP agents don't have a connection request URL in the TR-069 sense
        m.insert(format!("{base}ConnectionRequestURL"), String::new());
    }
    if sub.is_empty() || sub == "PeriodicInformEnable" {
        m.insert(format!("{base}PeriodicInformEnable"), "true".to_string());
    }
    if sub.is_empty() || sub == "PeriodicInformInterval" {
        m.insert(
            format!("{base}PeriodicInformInterval"),
            cfg.status_interval.to_string(),
        );
    }
    if sub.is_empty() || sub == "PeriodicInformTime" {
        m.insert(format!("{base}PeriodicInformTime"), "0001-01-01T00:00:00Z".to_string());
    }
    if sub.is_empty() || sub == "ParameterKey" {
        m.insert(format!("{base}ParameterKey"), String::new());
    }
    if sub.is_empty() || sub == "ConnectionRequestUsername" {
        m.insert(format!("{base}ConnectionRequestUsername"), String::new());
    }
    if sub.is_empty() || sub == "UpgradesManaged" {
        m.insert(format!("{base}UpgradesManaged"), "true".to_string());
    }

    m
}

pub fn set(_cfg: &ClientConfig, path: &str, _value: &str) -> Result<(), String> {
    Err(format!("Device.ManagementServer path is read-only: {path}"))
}
