//! TR-181 Device.Firewall.* — Standard firewall objects.
//!
//! Reads firewall configuration from UCI `firewall.@defaults[]`, zones,
//! and rules. Supports SET for defaults and ADD/DELETE for rules.

use crate::config::ClientConfig;
use crate::usp::tp469::uci_backend::{uci_commit, uci_set};
use log::info;
use std::collections::HashMap;

pub type Params = HashMap<String, String>;

pub async fn get(_cfg: &ClientConfig, path: &str) -> Params {
    let mut m = Params::new();

    // Device.Firewall. top-level
    if path == "Device.Firewall." || !path.contains('.') || path.ends_with("Firewall.") {
        let config = get_firewall_config();
        m.insert("Device.Firewall.Config".to_string(), config);
        m.insert(
            "Device.Firewall.AdvancedLevel".to_string(),
            get_firewall_level(),
        );
        // Server also polls "Level" (alias for AdvancedLevel)
        m.insert("Device.Firewall.Level".to_string(), get_firewall_level());

        let chains = get_chains();
        m.insert(
            "Device.Firewall.ChainNumberOfEntries".to_string(),
            chains.len().to_string(),
        );
        m.insert(
            "Device.Firewall.ZoneNumberOfEntries".to_string(),
            chains.len().to_string(),
        );

        let rules = get_rules();
        m.insert(
            "Device.Firewall.RuleNumberOfEntries".to_string(),
            rules.len().to_string(),
        );
    }

    // Device.Firewall.Config / AdvancedLevel / Level
    if path.ends_with("Config") {
        m.insert("Device.Firewall.Config".to_string(), get_firewall_config());
    }
    if path.ends_with("AdvancedLevel") || path.ends_with("Level") {
        m.insert(
            "Device.Firewall.AdvancedLevel".to_string(),
            get_firewall_level(),
        );
        m.insert("Device.Firewall.Level".to_string(), get_firewall_level());
    }
    if path.ends_with("ZoneNumberOfEntries") {
        let chains = get_chains();
        m.insert(
            "Device.Firewall.ZoneNumberOfEntries".to_string(),
            chains.len().to_string(),
        );
    }

    // Device.Firewall.Chain.{i}.
    if path == "Device.Firewall." || path.starts_with("Device.Firewall.Chain.") {
        let chains = get_chains();
        let specific_idx = extract_index(path, "Chain.");
        for (i, chain) in chains.iter().enumerate() {
            let idx = i + 1;
            if let Some(si) = specific_idx {
                if si != idx {
                    continue;
                }
            }
            let base = format!("Device.Firewall.Chain.{idx}.");
            m.insert(format!("{base}Enable"), "true".to_string());
            m.insert(format!("{base}Name"), chain.name.clone());
            m.insert(format!("{base}Alias"), chain.name.clone());
            m.insert(
                format!("{base}RuleNumberOfEntries"),
                "0".to_string(),
            );
        }
    }

    // Device.Firewall.Rule.{i}.
    if path == "Device.Firewall." || path.starts_with("Device.Firewall.Rule.") {
        let rules = get_rules();
        let specific_idx = extract_index(path, "Rule.");
        for (i, rule) in rules.iter().enumerate() {
            let idx = i + 1;
            if let Some(si) = specific_idx {
                if si != idx {
                    continue;
                }
            }
            let base = format!("Device.Firewall.Rule.{idx}.");
            m.insert(format!("{base}Enable"), rule.enabled.to_string());
            m.insert(format!("{base}Status"), "Enabled".to_string());
            m.insert(format!("{base}Description"), rule.name.clone());
            m.insert(format!("{base}Target"), rule.target.clone());
            m.insert(format!("{base}SourcePort"), rule.src_port.clone());
            m.insert(format!("{base}DestPort"), rule.dest_port.clone());
            m.insert(format!("{base}Protocol"), rule.proto.clone());
            m.insert(
                format!("{base}SourceIP"),
                rule.src_ip.clone(),
            );
            m.insert(
                format!("{base}DestIP"),
                rule.dest_ip.clone(),
            );
        }
    }

    // Vendor extensions
    if path.ends_with("X_OptimACS_SynFlood") {
        let val = uci_get_raw("firewall.@defaults[0].syn_flood").unwrap_or_default();
        let enabled = val == "1" || val == "true";
        m.insert(path.to_string(), enabled.to_string());
    } else if path.ends_with("X_OptimACS_DropInvalid") {
        let val = uci_get_raw("firewall.@defaults[0].drop_invalid").unwrap_or_default();
        let enabled = val == "1" || val == "true";
        m.insert(path.to_string(), enabled.to_string());
    } else if path.ends_with("X_OptimACS_Input") {
        let val =
            uci_get_raw("firewall.@defaults[0].input").unwrap_or_else(|| "REJECT".to_string());
        m.insert(path.to_string(), val);
    } else if path.ends_with("X_OptimACS_Output") {
        let val =
            uci_get_raw("firewall.@defaults[0].output").unwrap_or_else(|| "ACCEPT".to_string());
        m.insert(path.to_string(), val);
    } else if path.ends_with("X_OptimACS_Forward") {
        let val =
            uci_get_raw("firewall.@defaults[0].forward").unwrap_or_else(|| "REJECT".to_string());
        m.insert(path.to_string(), val);
    } else if path.ends_with("X_OptimACS_FlowOffloading") {
        let val = uci_get_raw("firewall.@defaults[0].flow_offloading").unwrap_or_default();
        let enabled = val == "1" || val == "true";
        m.insert(path.to_string(), enabled.to_string());
    }

    m
}

pub async fn set(_cfg: &ClientConfig, path: &str, value: &str) -> Result<(), String> {
    if path.ends_with("Config") {
        // Map TR-181 Config enum to UCI defaults
        let (input, output, forward) = match value {
            "Low" => ("ACCEPT", "ACCEPT", "ACCEPT"),
            "High" => ("DROP", "ACCEPT", "DROP"),
            "Medium" | "Advanced" => ("REJECT", "ACCEPT", "REJECT"),
            _ => return Err(format!("Invalid Firewall.Config value: {value}")),
        };
        uci_set("firewall.@defaults[0].input", input)?;
        uci_set("firewall.@defaults[0].output", output)?;
        uci_set("firewall.@defaults[0].forward", forward)?;
        uci_commit("firewall")?;
        restart_firewall();
        return Ok(());
    }

    // Rule SET
    if path.starts_with("Device.Firewall.Rule.") {
        let rules = get_rules();
        let idx = extract_index(path, "Rule.")
            .ok_or_else(|| format!("Cannot parse Rule index from: {path}"))?;
        if idx == 0 || idx > rules.len() {
            return Err(format!("Rule index {idx} out of range"));
        }
        let uci_idx = rules[idx - 1].uci_index;
        let section = format!("firewall.@rule[{uci_idx}]");

        if path.ends_with("Enable") {
            let val = if value == "true" || value == "1" {
                "1"
            } else {
                "0"
            };
            uci_set(&format!("{section}.enabled"), val)?;
        } else if path.ends_with("Description") {
            uci_set(&format!("{section}.name"), value)?;
        } else if path.ends_with("Target") {
            uci_set(&format!("{section}.target"), value)?;
        } else if path.ends_with("Protocol") {
            uci_set(&format!("{section}.proto"), value)?;
        } else if path.ends_with("SourcePort") {
            uci_set(&format!("{section}.src_port"), value)?;
        } else if path.ends_with("DestPort") {
            uci_set(&format!("{section}.dest_port"), value)?;
        } else if path.ends_with("SourceIP") {
            uci_set(&format!("{section}.src_ip"), value)?;
        } else if path.ends_with("DestIP") {
            uci_set(&format!("{section}.dest_ip"), value)?;
        } else {
            return Err(format!("Unknown Firewall.Rule param: {path}"));
        }

        uci_commit("firewall")?;
        restart_firewall();
        return Ok(());
    }

    Err(format!("Read-only or unknown Firewall path: {path}"))
}

// ── Data types ───────────────────────────────────────────────────────────────

struct Chain {
    name: String,
}

struct Rule {
    uci_index: usize,
    name: String,
    enabled: bool,
    target: String,
    proto: String,
    src_port: String,
    dest_port: String,
    src_ip: String,
    dest_ip: String,
}

// ── Data retrieval ───────────────────────────────────────────────────────────

fn get_firewall_config() -> String {
    let input = uci_get_raw("firewall.@defaults[0].input").unwrap_or_default();
    match input.as_str() {
        "ACCEPT" => "Low",
        "REJECT" | "DROP" => "High",
        _ => "Advanced",
    }
    .to_string()
}

fn get_firewall_level() -> String {
    let input = uci_get_raw("firewall.@defaults[0].input").unwrap_or_default();
    match input.as_str() {
        "ACCEPT" => "Low",
        "REJECT" => "High",
        "DROP" => "High",
        _ => "Medium",
    }
    .to_string()
}

fn get_chains() -> Vec<Chain> {
    let output = std::process::Command::new("uci")
        .args(["show", "firewall"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    let mut chains = Vec::new();
    for line in output.lines() {
        if line.contains("@zone[") && line.contains(".name=") {
            let name = line
                .split('=')
                .nth(1)
                .unwrap_or("")
                .trim_matches('\'')
                .to_string();
            if !name.is_empty() {
                chains.push(Chain { name });
            }
        }
    }
    chains
}

fn get_rules() -> Vec<Rule> {
    let output = std::process::Command::new("uci")
        .args(["show", "firewall"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    let mut rules: Vec<Rule> = Vec::new();
    let mut current_idx: Option<usize> = None;

    for line in output.lines() {
        if !line.contains("@rule[") {
            continue;
        }
        if let Some(start) = line.find("@rule[") {
            let rest = &line[start + 6..];
            if let Some(end) = rest.find(']') {
                let idx: usize = rest[..end].parse().unwrap_or(usize::MAX);
                if idx == usize::MAX {
                    continue;
                }

                // New rule?
                if current_idx != Some(idx) {
                    current_idx = Some(idx);
                    rules.push(Rule {
                        uci_index: idx,
                        name: String::new(),
                        enabled: true,
                        target: String::new(),
                        proto: String::new(),
                        src_port: String::new(),
                        dest_port: String::new(),
                        src_ip: String::new(),
                        dest_ip: String::new(),
                    });
                }

                if let Some(rule) = rules.last_mut() {
                    let key_val: Vec<&str> = line.splitn(2, '=').collect();
                    if key_val.len() == 2 {
                        let key = key_val[0].rsplit('.').next().unwrap_or("");
                        let val = key_val[1].trim_matches('\'').to_string();
                        match key {
                            "name" => rule.name = val,
                            "enabled" => rule.enabled = val != "0" && val != "false",
                            "target" => rule.target = val,
                            "proto" => rule.proto = val,
                            "src_port" => rule.src_port = val,
                            "dest_port" => rule.dest_port = val,
                            "src_ip" => rule.src_ip = val,
                            "dest_ip" => rule.dest_ip = val,
                            _ => {}
                        }
                    }
                }
            }
        }
    }
    rules
}

fn uci_get_raw(key: &str) -> Option<String> {
    std::process::Command::new("uci")
        .args(["get", key])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout)
                    .ok()
                    .map(|s| s.trim().trim_matches('\'').to_string())
            } else {
                None
            }
        })
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

fn restart_firewall() {
    info!("Reloading firewall");
    let _ = std::process::Command::new("/etc/init.d/firewall")
        .arg("reload")
        .status();
}
