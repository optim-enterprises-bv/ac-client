//! TR-181 Device.LocalAgent.* — TR-369 mandatory agent identity objects.
//!
//! Exposes the agent's endpoint ID, supported protocols, MTP configuration,
//! controller information, and subscription management so that TR-369
//! controllers can properly discover and manage this agent.

use crate::config::{ClientConfig, MtpType};
use crate::util;
use std::collections::HashMap;
use std::sync::Mutex;

pub type Params = HashMap<String, String>;

// ── In-memory subscription store ─────────────────────────────────────────────

#[derive(Clone, Default)]
struct Subscription {
    enable: bool,
    notif_type: String,
    reference_list: String,
}

static SUBSCRIPTIONS: Mutex<Option<Vec<Subscription>>> = Mutex::new(None);

fn get_subscriptions() -> Vec<Subscription> {
    SUBSCRIPTIONS
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_default()
}

/// Add a subscription and return its 1-based instance number.
pub fn add_subscription(
    enable: bool,
    notif_type: &str,
    reference_list: &str,
) -> u32 {
    let mut guard = SUBSCRIPTIONS.lock().unwrap();
    let subs = guard.get_or_insert_with(Vec::new);
    subs.push(Subscription {
        enable,
        notif_type: notif_type.to_string(),
        reference_list: reference_list.to_string(),
    });
    subs.len() as u32
}

/// Delete a subscription by 1-based instance number. Returns true on success.
pub fn delete_subscription(instance: u32) -> bool {
    let mut guard = SUBSCRIPTIONS.lock().unwrap();
    let subs = guard.get_or_insert_with(Vec::new);
    let idx = (instance as usize).wrapping_sub(1);
    if idx < subs.len() {
        subs.remove(idx);
        true
    } else {
        false
    }
}

// ── GET handler ──────────────────────────────────────────────────────────────

pub fn get(cfg: &ClientConfig, path: &str) -> Params {
    let mut m = Params::new();
    let base = "Device.LocalAgent.";

    let sub = path.strip_prefix(base).unwrap_or("");

    // Top-level Device.LocalAgent. params
    if sub.is_empty() || sub == "EndpointID" || !sub.contains('.') {
        if sub.is_empty() || sub == "EndpointID" {
            m.insert(
                format!("{base}EndpointID"),
                cfg.usp_endpoint_id.clone(),
            );
        }
        if sub.is_empty() || sub == "SoftwareVersion" {
            m.insert(
                format!("{base}SoftwareVersion"),
                util::read_fw_version(),
            );
        }
        if sub.is_empty() || sub == "SupportedProtocols" {
            let protocols = match cfg.mtp {
                MtpType::WebSocket => "WebSocket",
                MtpType::Mqtt => "MQTT",
                MtpType::Stomp => "STOMP",
                MtpType::CoAP => "CoAP",
                MtpType::Both => "WebSocket,MQTT",
                MtpType::All => "WebSocket,MQTT,STOMP,CoAP",
            };
            m.insert(format!("{base}SupportedProtocols"), protocols.to_string());
        }
        if sub.is_empty() || sub == "UpTime" {
            m.insert(format!("{base}UpTime"), util::read_uptime());
        }

        // NumberOfEntries
        if sub.is_empty() || sub == "MTPNumberOfEntries" {
            let count = match cfg.mtp {
                MtpType::Both => 2,
                MtpType::All => {
                    let mut c = 0u32;
                    if cfg.ws_url.is_some() { c += 1; }
                    if cfg.mqtt_url.is_some() { c += 1; }
                    if cfg.stomp_url.is_some() { c += 1; }
                    if cfg.coap_url.is_some() { c += 1; }
                    c.max(1)
                }
                _ => 1,
            };
            m.insert(format!("{base}MTPNumberOfEntries"), count.to_string());
        }
        if sub.is_empty() || sub == "ControllerNumberOfEntries" {
            let mut count: u32 = if cfg.controller_id.is_empty() { 0 } else { 1 };
            count += cfg.secondary_controllers.len() as u32;
            m.insert(
                format!("{base}ControllerNumberOfEntries"),
                count.to_string(),
            );
        }
        if sub.is_empty() || sub == "SubscriptionNumberOfEntries" {
            let subs = get_subscriptions();
            m.insert(
                format!("{base}SubscriptionNumberOfEntries"),
                subs.len().to_string(),
            );
        }
    }

    // Device.LocalAgent.MTP.{i}.
    if sub.is_empty() || sub.starts_with("MTP.") {
        populate_mtp(cfg, &mut m);
    }

    // Device.LocalAgent.Controller.{i}.
    if sub.is_empty() || sub.starts_with("Controller.") {
        populate_controller(cfg, &mut m);
    }

    // Device.LocalAgent.Subscription.{i}.
    if sub.is_empty() || sub.starts_with("Subscription.") {
        let subs = get_subscriptions();
        for (i, s) in subs.iter().enumerate() {
            let idx = i + 1;
            let sb = format!("{base}Subscription.{idx}.");
            m.insert(format!("{sb}Enable"), s.enable.to_string());
            m.insert(format!("{sb}NotifType"), s.notif_type.clone());
            m.insert(format!("{sb}ReferenceList"), s.reference_list.clone());
        }
    }

    m
}

fn populate_mtp(cfg: &ClientConfig, m: &mut Params) {
    let base = "Device.LocalAgent.MTP.";
    let mut idx: u32 = 1;

    // WebSocket MTP
    if cfg.mtp == MtpType::WebSocket || cfg.mtp == MtpType::Both {
        let b = format!("{base}{idx}.");
        m.insert(format!("{b}Enable"), "true".to_string());
        m.insert(format!("{b}Protocol"), "WebSocket".to_string());
        m.insert(format!("{b}Status"), "Up".to_string());

        if let Some(ref url) = cfg.ws_url {
            m.insert(format!("{b}WebSocket.URL"), url.clone());
        }
        m.insert(
            format!("{b}WebSocket.CertFile"),
            cfg.cert_file.display().to_string(),
        );
        idx += 1;
    }

    // MQTT MTP
    if cfg.mtp == MtpType::Mqtt || cfg.mtp == MtpType::Both || cfg.mtp == MtpType::All {
        if cfg.mqtt_url.is_some() {
            let b = format!("{base}{idx}.");
            m.insert(format!("{b}Enable"), "true".to_string());
            m.insert(format!("{b}Protocol"), "MQTT".to_string());
            m.insert(format!("{b}Status"), "Up".to_string());

            if let Some(ref url) = cfg.mqtt_url {
                m.insert(format!("{b}MQTT.BrokerAddress"), url.clone());
            }
            if let Some(ref client_id) = cfg.mqtt_client_id {
                m.insert(format!("{b}MQTT.ClientID"), client_id.clone());
            }
            idx += 1;
        }
    }

    // STOMP MTP
    if cfg.mtp == MtpType::Stomp || cfg.mtp == MtpType::All {
        if cfg.stomp_url.is_some() {
            let b = format!("{base}{idx}.");
            m.insert(format!("{b}Enable"), "true".to_string());
            m.insert(format!("{b}Protocol"), "STOMP".to_string());
            m.insert(format!("{b}Status"), "Up".to_string());
            if let Some(ref url) = cfg.stomp_url {
                m.insert(format!("{b}STOMP.Destination"), url.clone());
            }
            idx += 1;
        }
    }

    // CoAP MTP
    if cfg.mtp == MtpType::CoAP || cfg.mtp == MtpType::All {
        if cfg.coap_url.is_some() {
            let b = format!("{base}{idx}.");
            m.insert(format!("{b}Enable"), "true".to_string());
            m.insert(format!("{b}Protocol"), "CoAP".to_string());
            m.insert(format!("{b}Status"), "Up".to_string());
            if let Some(ref url) = cfg.coap_url {
                m.insert(format!("{b}CoAP.URL"), url.clone());
            }
        }
    }
}

fn populate_controller(cfg: &ClientConfig, m: &mut Params) {
    // Primary controller
    if !cfg.controller_id.is_empty() {
        let base = "Device.LocalAgent.Controller.1.";
        m.insert(format!("{base}Enable"), "true".to_string());
        m.insert(format!("{base}EndpointID"), cfg.controller_id.clone());
        m.insert(
            format!("{base}PeriodicNotifInterval"),
            cfg.status_interval.to_string(),
        );
        // TR-369 trust role
        m.insert(format!("{base}AssignedRole"), "Device.LocalAgent.ControllerTrust.Role.1.".to_string());
    }

    // Secondary controllers (multi-controller support)
    for (i, ctrl_id) in cfg.secondary_controllers.iter().enumerate() {
        let idx = i + 2; // Primary is 1, secondary starts at 2
        let base = format!("Device.LocalAgent.Controller.{idx}.");
        m.insert(format!("{base}Enable"), "true".to_string());
        m.insert(format!("{base}EndpointID"), ctrl_id.clone());
        m.insert(
            format!("{base}PeriodicNotifInterval"),
            cfg.status_interval.to_string(),
        );
        m.insert(format!("{base}AssignedRole"), "Device.LocalAgent.ControllerTrust.Role.2.".to_string());
    }
}

// ── SET handler ──────────────────────────────────────────────────────────────

pub fn set(_cfg: &ClientConfig, path: &str, value: &str) -> Result<(), String> {
    // Subscription params are writable
    if path.contains("Subscription.") {
        let subs_count = get_subscriptions().len();
        let idx = extract_sub_index(path).ok_or("Invalid Subscription index")?;
        if idx == 0 || idx > subs_count {
            return Err(format!("Subscription index {idx} out of range"));
        }
        let mut guard = SUBSCRIPTIONS.lock().unwrap();
        let subs = guard.get_or_insert_with(Vec::new);
        let s = &mut subs[idx - 1];
        if path.ends_with("Enable") {
            s.enable = value == "true" || value == "1";
        } else if path.ends_with("NotifType") {
            s.notif_type = value.to_string();
        } else if path.ends_with("ReferenceList") {
            s.reference_list = value.to_string();
        } else {
            return Err(format!("Unknown Subscription param: {path}"));
        }
        return Ok(());
    }

    // Controller.1.PeriodicNotifInterval — accept but cannot persist to config
    // (would require rewriting ClientConfig which is loaded at startup)
    Err(format!("Device.LocalAgent path is read-only: {path}"))
}

fn extract_sub_index(path: &str) -> Option<usize> {
    if let Some(pos) = path.find("Subscription.") {
        let rest = &path[pos + 13..];
        let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        num_str.parse().ok()
    } else {
        None
    }
}
