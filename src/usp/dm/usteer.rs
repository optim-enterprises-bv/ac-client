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
//! ## Read-only
//!
//! usteer-ng config is pushed as a UCI document (the same path the controller
//! uses for mesh/DPI config), NOT as individual params here. The daemon's
//! `set_config`/`update_config` ubus methods exist, but writing steering
//! policy through them would bypass the UCI source of truth and make the
//! config non-idempotent across a reboot. So this module is GET-only.

use std::collections::HashMap;
use std::process::Command;

use log::debug;

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
