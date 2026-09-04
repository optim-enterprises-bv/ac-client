//! 802.11s mesh — `Device.X_OptimACS_Mesh.*`.
//!
//! TR-181 has no mesh. Its WiFi model covers `Radio`, `SSID` and `AccessPoint`,
//! and there is no standard object for an 802.11s mesh point: no mesh id, no
//! forwarding flag, no `mode 'mesh'`. So a controller that speaks USP had no way
//! to build a mesh at all, while the same controller could do it over uCentral
//! by sending a UCI document directly.
//!
//! That asymmetry is what this module removes. It is a vendor extension because
//! it has to be — writing these into `Device.WiFi.SSID.{i}.*` would be inventing
//! standard paths that no other agent implements and no spec defines.
//!
//! ## One section, named
//!
//! The mesh interface is always the `wifi-iface` named `aethermesh`. A fixed
//! name is deliberate: the controller must be able to find, update and REMOVE
//! exactly the interface it created, without touching a mesh the operator
//! configured by hand. An index-based section would drift as other interfaces
//! come and go.
//!
//! ## Enable is create/destroy, not up/down
//!
//! `Enable=0` deletes the section rather than setting `disabled=1`. A disabled
//! mesh iface still occupies the radio's interface budget on some drivers, and
//! leaves a stale mesh id in the config that a later reader would report as
//! configured. Removing it means "no mesh" reads as no mesh.

use std::collections::HashMap;

use log::{info, warn};

use crate::config::ClientConfig;
use crate::usp::dm::wifi::mark_wifi_reload;
use crate::usp::tp469::uci_backend::{uci_commit, uci_get, uci_set};

/// UCI `wifi-iface` section owned by the controller.
const SECTION: &str = "aethermesh";

/// Encryption values hostapd accepts for an 802.11s mesh.
///
/// Mesh peering is SAE or nothing: there is no PSK handshake for 802.11s in
/// hostapd, so `psk2` and friends are rejected at interface bring-up rather than
/// at config time. Validating here turns a radio that silently fails to come up
/// into an error the controller can show.
const VALID_ENCRYPTION: [&str; 3] = ["sae", "sae-mixed", "none"];

fn opt(name: &str) -> String {
    format!("wireless.{SECTION}.{name}")
}

/// Does the controller-owned mesh section exist?
fn configured() -> bool {
    !uci_get(&format!("wireless.{SECTION}")).trim().is_empty()
}

/// Number of established mesh peers, if the interface is up.
///
/// Read from `iw`, not from UCI: UCI says what was asked for, `iw` says what is
/// actually peered. A mesh configured correctly that has found nobody is the
/// failure this parameter exists to make visible.
fn peer_count() -> Option<usize> {
    let iface = uci_get(&opt("ifname"));
    let iface = iface.trim();
    let candidates: Vec<String> = if iface.is_empty() {
        // OpenWrt names mesh interfaces `mesh0`, `mesh1`, … unless overridden.
        (0..4).map(|i| format!("mesh{i}")).collect()
    } else {
        vec![iface.to_owned()]
    };
    for name in candidates {
        if let Ok(out) = std::process::Command::new("iw")
            .args(["dev", &name, "station", "dump"])
            .output()
        {
            if out.status.success() {
                let text = String::from_utf8_lossy(&out.stdout);
                return Some(text.matches("Station ").count());
            }
        }
    }
    None
}

/// Report the mesh configuration and what it is actually doing.
pub fn get(_cfg: &ClientConfig, path: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    if !path.starts_with("Device.X_OptimACS_Mesh") {
        return m;
    }
    let present = configured();

    m.insert(
        "Device.X_OptimACS_Mesh.Enable".into(),
        if present { "1" } else { "0" }.into(),
    );
    // Reported even when absent, so a controller reading back after a failed SET
    // sees empty values rather than the previous mesh's settings.
    for (param, uci) in [
        ("MeshId", "mesh_id"),
        ("Encryption", "encryption"),
        ("Radio", "device"),
        ("Forwarding", "mesh_fwding"),
        ("MFP", "ieee80211w"),
    ] {
        let v = if present {
            uci_get(&opt(uci)).trim().to_owned()
        } else {
            String::new()
        };
        m.insert(format!("Device.X_OptimACS_Mesh.{param}"), v);
    }

    // The passphrase is never reported. A controller that can read back the
    // key it set gains nothing, and anything that can read the data model
    // gains a credential.
    m.insert(
        "Device.X_OptimACS_Mesh.Peers".into(),
        peer_count().map(|n| n.to_string()).unwrap_or_default(),
    );
    m
}

/// Apply a mesh parameter.
///
/// Every write marks a radio reload owed rather than reloading immediately;
/// `dm::set_params` flushes once after the whole SET. Reloading per parameter
/// would bounce the radios five times for one mesh and drop clients each time.
pub async fn set(_cfg: &ClientConfig, path: &str, value: &str) -> Result<(), String> {
    let leaf = path
        .strip_prefix("Device.X_OptimACS_Mesh.")
        .ok_or_else(|| format!("not a mesh path: {path}"))?;

    match leaf {
        "Enable" => {
            let on = usp_bool(value)?;
            if on {
                ensure_section()?;
            } else {
                remove_section()?;
            }
            uci_commit("wireless")?;
            mark_wifi_reload();
            info!("mesh: {}", if on { "enabled" } else { "removed" });
            return Ok(());
        }
        "Peers" => return Err("Peers is read-only".into()),
        _ => {}
    }

    // Validate the parameter BEFORE touching UCI.
    //
    // This used to call `ensure_section()` first, so a misspelled parameter
    // created a mesh interface as a side effect and then failed -- leaving a
    // half-built mesh behind for a SET that reported an error. Nothing should
    // be written until the value is known to be one this device accepts.
    let (uci_name, checked) = match leaf {
        "MeshId" => {
            if value.trim().is_empty() {
                return Err("MeshId must not be empty".into());
            }
            ("mesh_id", value.to_owned())
        }
        "Passphrase" => {
            // WPA/SAE minimum. hostapd rejects a shorter key at bring-up, which
            // presents as a radio that simply never appears.
            if value.len() < 8 {
                return Err("Passphrase must be at least 8 characters".into());
            }
            ("key", value.to_owned())
        }
        "Encryption" => {
            let v = value.trim().to_ascii_lowercase();
            if !VALID_ENCRYPTION.contains(&v.as_str()) {
                return Err(format!(
                    "Encryption must be one of {} (802.11s peering is SAE or open)",
                    VALID_ENCRYPTION.join(", ")
                ));
            }
            ("encryption", v)
        }
        "Radio" => {
            let v = value.trim();
            if uci_get(&format!("wireless.{v}")).trim().is_empty() {
                return Err(format!("no such radio: {v}"));
            }
            ("device", v.to_owned())
        }
        "Forwarding" => (
            "mesh_fwding",
            if usp_bool(value)? { "1" } else { "0" }.into(),
        ),
        "MFP" => match value.trim() {
            v @ ("0" | "1" | "2") => ("ieee80211w", v.to_owned()),
            other => return Err(format!("MFP must be 0, 1 or 2, got {other}")),
        },
        other => return Err(format!("unknown mesh parameter: {other}")),
    };

    // Only now, with a validated parameter in hand, create the section if
    // needed. Creating it lazily means the controller can send MeshId before
    // Enable without the order mattering -- a USP SET carries no guaranteed
    // parameter ordering.
    ensure_section()?;
    uci_set(&opt(uci_name), &checked)?;
    uci_commit("wireless")?;
    mark_wifi_reload();
    Ok(())
}

/// Create the section with safe defaults if it does not exist.
///
/// `mode` and `mesh_fwding` are set here rather than left to the controller:
/// a `wifi-iface` without `mode 'mesh'` is not a mesh at all, and forwarding off
/// yields a mesh that peers and carries no traffic. Both are what every caller
/// wants and neither is discoverable from a failure.
fn ensure_section() -> Result<(), String> {
    if configured() {
        return Ok(());
    }
    uci_set(&format!("wireless.{SECTION}"), "wifi-iface")?;
    uci_set(&opt("mode"), "mesh")?;
    uci_set(&opt("mesh_fwding"), "1")?;
    // Default to the first radio only so the section is valid; the controller
    // is expected to set Radio explicitly.
    if uci_get(&opt("device")).trim().is_empty() {
        uci_set(&opt("device"), "radio0")?;
    }
    info!("mesh: created wifi-iface '{SECTION}'");
    Ok(())
}

fn remove_section() -> Result<(), String> {
    if !configured() {
        return Ok(());
    }
    let status = std::process::Command::new("uci")
        .args(["delete", &format!("wireless.{SECTION}")])
        .status()
        .map_err(|e| format!("uci delete failed: {e}"))?;
    if !status.success() {
        warn!("mesh: uci delete wireless.{SECTION} returned {status}");
        return Err("failed to remove mesh interface".into());
    }
    Ok(())
}

/// A USP boolean. Rejected rather than coerced: treating junk as `false` would
/// turn a controller bug into a silently dismantled mesh.
fn usp_bool(v: &str) -> Result<bool, String> {
    match v.trim().to_ascii_lowercase().as_str() {
        "1" | "true" => Ok(true),
        "0" | "false" => Ok(false),
        other => Err(format!("not a boolean: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A prefix GET must return the switch, not only an exact leaf.
    ///
    /// The controller polls `Device.X_OptimACS_Mesh.` and never the individual
    /// leaves. Matching only the leaf makes the whole object present in the data
    /// model and invisible to its only caller — a mistake already shipped once
    /// in this codebase, in the capture-consent module.
    #[test]
    fn a_prefix_query_returns_the_object() {
        let cfg = ClientConfig::default();
        let m = get(&cfg, "Device.X_OptimACS_Mesh.");
        assert!(m.contains_key("Device.X_OptimACS_Mesh.Enable"));
        assert!(m.contains_key("Device.X_OptimACS_Mesh.MeshId"));
    }

    /// The passphrase must never be readable.
    #[test]
    fn the_passphrase_is_not_reported() {
        let cfg = ClientConfig::default();
        let m = get(&cfg, "Device.X_OptimACS_Mesh.");
        assert!(
            !m.contains_key("Device.X_OptimACS_Mesh.Passphrase"),
            "the mesh key must not be readable through the data model"
        );
    }

    #[test]
    fn booleans_are_rejected_not_coerced() {
        assert_eq!(usp_bool("1"), Ok(true));
        assert_eq!(usp_bool("FALSE"), Ok(false));
        assert!(usp_bool("").is_err());
        assert!(usp_bool("off").is_err());
    }

    /// 802.11s peering is SAE or open. A PSK value must be refused here rather
    /// than accepted and then rejected by hostapd at bring-up, where it presents
    /// as a radio that never appears.
    #[test]
    fn only_mesh_capable_encryption_is_accepted() {
        assert!(VALID_ENCRYPTION.contains(&"sae"));
        assert!(VALID_ENCRYPTION.contains(&"none"));
        assert!(!VALID_ENCRYPTION.contains(&"psk2"));
        assert!(!VALID_ENCRYPTION.contains(&"wpa2"));
    }
}
