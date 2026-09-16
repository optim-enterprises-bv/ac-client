//! usteer-ng client/band steering — `Device.X_OptimACS_Usteer.*`.
//!
//! usteer-ng is a device-side roaming/band-steering daemon for the AetherM1
//! APs (802.11k neighbor reports + 802.11v BSS transition management). It is
//! self-contained on the device; Aether only needs to (a) push its UCI config
//! over USP and (b) read back its live state so the dashboard can show which
//! clients are being steered, where, and why.
//!
//! This module surfaces that live state. It reads the `usteer` ubus object
//! (the daemon's own status interface) and reports it as vendor params so the
//! controller can render steering activity without guessing.
//!
//! ## Config push: UCI, never ubus
//!
//! Config is written to UCI (`usteer.@usteer[0].*`), never through the daemon's
//! `set_config`/`update_config` ubus methods. Those apply steering policy at
//! runtime but leave UCI untouched, so the change silently reverts on reboot --
//! the failure is invisible until the device restarts.
//!
//! The param <-> UCI mapping is a two-repo contract with the controller and is
//! specified in aether `docs/adr/ADR-034-usteer-config-push-contract.md`.
//! `SETTABLE` below is that table; keep them in step.
//!
//! `network` is deliberately absent from `SETTABLE`. It is `bat0` because
//! usteer exchanges steering state over the batman-adv backbone, not the LAN
//! bridge. Pointing it elsewhere does not fail loudly: steering keeps running
//! per-AP while silently losing inter-AP coordination, which presents as
//! "roaming got worse" with no error anywhere. It stays GET-only.

use std::collections::HashMap;
use std::process::Command;

use log::debug;

use crate::usp::tp469::uci_backend::{uci_add_list, uci_commit, uci_delete, uci_set};

/// Run `ubus call usteer <method>` and return the JSON output.
fn ubus_call(method: &str) -> Option<serde_json::Value> {
    let out = Command::new("ubus")
        .args(["call", "usteer", method])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

/// Is usteer-ng installed and running on this device?
///
/// Reported unconditionally (like `Mesh.WingCapable`): a property of the
/// firmware, not of the current steering state. The controller needs it
/// before it plans anything.
fn usteer_present() -> bool {
    std::path::Path::new("/sbin/usteerd").exists()
}

/// Report the usteer-ng status.
pub fn get(_cfg: &crate::config::ClientConfig, path: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    if !path.starts_with("Device.X_OptimACS_Usteer") {
        return m;
    }

    m.insert(
        "Device.X_OptimACS_Usteer.Enable".into(),
        if usteer_present() { "1" } else { "0" }.into(),
    );

    // Local node info: which APs/bands usteer-ng is watching and the local
    // node's identity. Lets the controller confirm steering is active on the
    // right interfaces.
    if let Some(v) = ubus_call("local_info") {
        m.insert(
            "Device.X_OptimACS_Usteer.LocalInfo".into(),
            serde_json::to_string(&v).unwrap_or_default(),
        );
    }

    // Connected clients: the live set of STAs associated to this node's APs,
    // with their signal/band. This is the raw material steering acts on.
    if let Some(v) = ubus_call("connected_clients") {
        m.insert(
            "Device.X_OptimACS_Usteer.Clients".into(),
            serde_json::to_string(&v).unwrap_or_default(),
        );
    }

    // The active steering config (thresholds, aggressiveness, ssid_list).
    // Read back so the controller can confirm what the device is actually
    // running, not just what was pushed.
    if let Some(v) = ubus_call("get_config") {
        m.insert(
            "Device.X_OptimACS_Usteer.Config".into(),
            serde_json::to_string(&v).unwrap_or_default(),
        );
    }

    debug!("usteer: reported {} params", m.len());
    m
}

/// UCI section usteer-ng reads. The package ships a single anonymous section.
const SECTION: &str = "usteer.@usteer[0]";

/// `Device.X_OptimACS_Usteer.<Param>` -> `usteer.@usteer[0].<option>`.
///
/// Contract: aether ADR-034. Anything not listed here is rejected -- an
/// unknown leaf must not silently no-op, or the controller believes it pushed
/// a setting the device never applied.
const SETTABLE: &[(&str, &str)] = &[
    ("Syslog", "syslog"),
    ("DebugLevel", "debug_level"),
    ("AssocSteering", "assoc_steering"),
    ("ProbeSteering", "probe_steering"),
    ("BandSteeringInterval", "band_steering_interval"),
    // SsidList is handled separately: it is a UCI list, not an option.
];

/// ac-client's usual USP boolean spelling.
fn usp_bool(v: &str) -> Result<bool, String> {
    match v.trim().to_ascii_lowercase().as_str() {
        "1" | "true" => Ok(true),
        "0" | "false" => Ok(false),
        other => Err(format!("not a boolean: {other}")),
    }
}

/// Write `ssid_list` with REPLACE semantics.
///
/// `uci add_list` appends. A controller pushing desired state repeatedly -- the
/// normal idempotent-reconciliation pattern -- would accumulate duplicates, so
/// the existing list is deleted before the supplied set is written. Pushing the
/// same value twice is then a no-op.
fn set_ssid_list(value: &str) -> Result<(), String> {
    let path = format!("{SECTION}.ssid_list");
    // Deleting a list that does not exist is not an error worth failing on.
    let _ = uci_delete(&path);
    for ssid in value.split_whitespace() {
        uci_add_list(&path, ssid)?;
    }
    Ok(())
}

/// Apply one `Device.X_OptimACS_Usteer.*` parameter.
pub async fn set(_cfg: &crate::config::ClientConfig, path: &str, value: &str) -> Result<(), String> {
    let leaf = path
        .strip_prefix("Device.X_OptimACS_Usteer.")
        .ok_or_else(|| format!("not a usteer path: {path}"))?;

    // Validate BEFORE writing anything, so a rejected SET leaves no partial
    // config behind (the same ordering mesh::set uses, for the same reason).
    match leaf {
        "SsidList" => {
            if value.trim().is_empty() {
                return Err("SsidList must not be empty".into());
            }
            set_ssid_list(value)?;
        }
        "Network" => {
            return Err(
                "Network is read-only: usteer rides the batman-adv backbone (ADR-034)".into(),
            )
        }
        "LocalInfo" | "Clients" | "Config" | "Enable" => {
            return Err(format!("{leaf} is read-only"))
        }
        _ => {
            let opt = SETTABLE
                .iter()
                .find(|(p, _)| *p == leaf)
                .map(|(_, o)| *o)
                .ok_or_else(|| format!("unknown usteer parameter: {leaf}"))?;

            let checked = match leaf {
                "Syslog" | "AssocSteering" | "ProbeSteering" => {
                    if usp_bool(value)? { "1".to_string() } else { "0".to_string() }
                }
                "DebugLevel" => match value.trim() {
                    v @ ("0" | "1" | "2" | "3" | "4" | "5") => v.to_string(),
                    other => return Err(format!("DebugLevel must be 0-5, got {other}")),
                },
                "BandSteeringInterval" => {
                    let v = value.trim();
                    v.parse::<u32>()
                        .map_err(|_| format!("BandSteeringInterval must be a number (ms), got {v}"))?;
                    v.to_string()
                }
                _ => value.to_owned(),
            };
            uci_set(&format!("{SECTION}.{opt}"), &checked)?;
        }
    }

    uci_commit("usteer")?;
    restart_usteer().await;
    Ok(())
}

/// Reload the daemon so the committed UCI takes effect now as well as on boot.
async fn restart_usteer() {
    use tokio::process::Command as AsyncCommand;
    let path = "/etc/init.d/usteer";
    if !std::path::Path::new(path).exists() {
        debug!("{path} not present -- config committed but daemon not reloaded");
        return;
    }
    if let Err(e) = AsyncCommand::new(path).arg("reload").status().await {
        debug!("usteer reload failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ADR-034's table is the contract. If a leaf is added to `SETTABLE`
    /// without updating the ADR (or vice versa), the two repos drift.
    #[test]
    fn settable_matches_adr_034() {
        let expected = [
            ("Syslog", "syslog"),
            ("DebugLevel", "debug_level"),
            ("AssocSteering", "assoc_steering"),
            ("ProbeSteering", "probe_steering"),
            ("BandSteeringInterval", "band_steering_interval"),
        ];
        assert_eq!(SETTABLE, &expected[..], "SETTABLE drifted from ADR-034");
    }

    #[test]
    fn usp_bool_accepts_both_spellings() {
        for v in ["1", "true", "TRUE", " 1 "] {
            assert_eq!(usp_bool(v), Ok(true), "{v}");
        }
        for v in ["0", "false", "FALSE"] {
            assert_eq!(usp_bool(v), Ok(false), "{v}");
        }
        assert!(usp_bool("yes").is_err());
    }

    /// `network` must never become settable: pointing usteer off the batman
    /// backbone silently severs inter-AP steering fleet-wide (ADR-034).
    #[test]
    fn network_is_not_in_the_settable_table() {
        assert!(
            !SETTABLE.iter().any(|(p, o)| *p == "Network" || *o == "network"),
            "Network became settable -- see ADR-034"
        );
    }

    /// Read-only params reported by get() must not be writable.
    #[test]
    fn reported_params_are_not_settable() {
        for leaf in ["LocalInfo", "Clients", "Config", "Enable"] {
            assert!(
                !SETTABLE.iter().any(|(p, _)| *p == leaf),
                "{leaf} is reported by get() and must stay read-only"
            );
        }
    }

    /// SsidList is a UCI list, so it must NOT be in the scalar option table --
    /// writing it with uci_set would replace the list with a single string.
    #[test]
    fn ssid_list_is_not_a_scalar_option() {
        assert!(!SETTABLE.iter().any(|(p, _)| *p == "SsidList"));
    }

    /// Every option maps under the single anonymous section the package ships.
    #[test]
    fn section_path_is_the_anonymous_usteer_section() {
        assert_eq!(SECTION, "usteer.@usteer[0]");
        assert_eq!(format!("{SECTION}.syslog"), "usteer.@usteer[0].syslog");
    }
}
