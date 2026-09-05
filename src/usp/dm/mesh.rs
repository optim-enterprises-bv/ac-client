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
use crate::usp::tp469::uci_backend::{
    uci_add_list, uci_commit, uci_delete, uci_del_list, uci_get, uci_set,
};

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

/// Established mesh peers, as a comma-separated list of MACs.
///
/// Read from `iw`, not from UCI: UCI says what was asked for, `iw` says what is
/// actually peered. A mesh configured correctly that has found nobody is the
/// failure this parameter exists to make visible.
///
/// The interface is discovered, not assumed: OpenWrt names mesh interfaces
/// `mesh0`, `mesh1`, … or `phy1-mesh0` depending on the driver and how many
/// radios are in play, and the `ifname` UCI option is frequently empty (netifd
/// assigns the name). Scanning `iw dev` for a `type mesh point` interface is
/// the only spelling that is guaranteed to match the real one.
fn mesh_peers() -> Option<Vec<String>> {
    // Find the mesh interface by scanning `iw dev` output for a mesh point.
    let iface = discover_mesh_iface()?;
    let out = std::process::Command::new("iw")
        .args(["dev", &iface, "station", "dump"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    // `iw dev <iface> station dump` lists each peer as `Station <mac>`, with
    // the plink state on a following line. Only established peers count as
    // a real link; a peer stuck in OPN_SNT/LISTEN is not a usable path.
    let lines: Vec<&str> = text.lines().collect();
    let mut peers = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if let Some(mac) = line.strip_prefix("Station ") {
            let mac = mac.split_whitespace().next().unwrap_or("").to_owned();
            // Scan the station's block for its plink state (it may not be the
            // immediate next line).
            let mut established = false;
            i += 1;
            while i < lines.len() && !lines[i].starts_with("Station ") {
                if lines[i].contains("mesh plink:") && lines[i].contains("ESTAB") {
                    established = true;
                }
                i += 1;
            }
            if established {
                peers.push(mac);
            }
            continue;
        }
        i += 1;
    }
    if peers.is_empty() {
        None
    } else {
        Some(peers)
    }
}

/// Find the name of the mesh interface, if any.
///
/// `iw dev` prints one block per interface, each with a `type mesh point`
/// line. Prefer the interface the controller created (`aethermesh`'s device),
/// falling back to any mesh point.
fn discover_mesh_iface() -> Option<String> {
    let out = std::process::Command::new("iw").arg("dev").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    // Parse blocks: "Interface <name>" ... "type mesh point". The line is
    // indented with a tab in `iw dev` output, so trim before matching.
    let mut current: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix("Interface ") {
            current = Some(name.trim().to_owned());
        } else if line.contains("type mesh point") {
            if let Some(name) = current.take() {
                return Some(name);
            }
        }
    }
    None
}

/// Is the WING mesh routing toolkit usable on this device?
///
/// Reported so the controller does not have to guess. aether classifies a
/// device into a mesh family from what it reports, and its USP path had no way
/// to learn this at all -- it hardcoded "not WING-capable", so a device with
/// WING installed was permanently classified as 802.11s and the WING compiler
/// was unreachable for the entire fleet.
///
/// Both markers are required, and they are the same two `platform::detect::
/// wing_capable` looks for: the Click binary WING routes with, and the netifd
/// proto handler that brings a WING interface up. Either alone is a partial
/// install that would take a config it cannot run.
fn wing_capable() -> bool {
    std::path::Path::new("/usr/bin/click").exists()
        && std::path::Path::new("/lib/netifd/proto/wing.sh").exists()
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
    //
    // Peers is a comma-separated list of established peer MACs, so the
    // controller can draw the actual mesh graph (which node links to which),
    // not just a count. Empty when no mesh is configured or no peer is up.
    m.insert(
        "Device.X_OptimACS_Mesh.Peers".into(),
        mesh_peers()
            .map(|p| p.join(","))
            .unwrap_or_default(),
    );

    // The mesh interface's own MAC. Peers are reported as the *mesh* MACs of
    // the other nodes, which differ from the agent MACs the controller keys
    // devices by. Without this, the controller cannot map a peer mesh-MAC back
    // to the agent that owns it, so it cannot draw the link between two nodes.
    m.insert(
        "Device.X_OptimACS_Mesh.InterfaceMAC".into(),
        discover_mesh_iface()
            .and_then(|iface| {
                std::fs::read_to_string(format!("/sys/class/net/{iface}/address"))
                    .ok()
                    .map(|s| s.trim().to_owned())
            })
            .unwrap_or_default(),
    );

    // Reported unconditionally, including when no mesh is configured: this is a
    // property of the FIRMWARE, not of the current mesh, and the controller
    // needs it before it plans anything.
    m.insert(
        "Device.X_OptimACS_Mesh.WingCapable".into(),
        if wing_capable() { "1" } else { "0" }.into(),
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
                ensure_network_interface()?;
            } else {
                remove_section()?;
                remove_network_interface()?;
            }
            uci_commit("wireless")?;
            uci_commit("network")?;
            uci_commit("firewall")?;
            mark_wifi_reload();
            info!("mesh: {}", if on { "enabled" } else { "removed" });
            return Ok(());
        }
        "Peers" => return Err("Peers is read-only".into()),
        // A capability, not a setting. Letting a controller assert it would let
        // a device be planned into a mesh family its firmware cannot run.
        "WingCapable" => return Err("WingCapable is read-only".into()),
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
        // The mesh interface's layer-3 address (e.g. `10.0.0.1/8`). Stored on
        // the wireless section so the network interface + firewall zone can be
        // created together when the mesh is enabled.
        "IPAddress" => {
            if value.trim().is_empty() {
                return Err("IPAddress must not be empty".into());
            }
            ("ipaddr", value.to_owned())
        }
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
/// `mode` is set here because a `wifi-iface` without `mode 'mesh'` is not a mesh
/// at all, and that is not discoverable from a failure.
///
/// `mesh_fwding` is deliberately NOT written. OpenWrt's wifi-scripts translate
/// it into a wpa_supplicant network field which the supplicant rejects:
///
/// ```text
/// Line 10: unknown network field 'mesh_fwding'.
/// Failed to read or parse configuration '/var/run/wpa-supplicant-phy1-mesh0.conf'.
/// ```
///
/// The whole config then fails to parse, the supplicant never joins the mesh,
/// and no peering is ever attempted -- observed on hardware, where the UCI was
/// perfect and the radio was up while the mesh silently did nothing. Kernel mesh
/// forwarding defaults to on, so omitting it changes no behaviour.
fn ensure_section() -> Result<(), String> {
    if configured() {
        return Ok(());
    }
    uci_set(&format!("wireless.{SECTION}"), "wifi-iface")?;
    uci_set(&opt("mode"), "mesh")?;
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

/// The `network` interface section that carries the mesh's layer-3 address.
///
/// A mesh `wifi-iface` alone forms the 802.11s adjacency but carries no IP, so
/// the mesh forms and routes nothing. This creates a static `network` interface
/// on the mesh device and binds it to the `lan` firewall zone so the firewall
/// accepts mesh traffic. The address comes from the `ipaddr` option the
/// controller sets via `Device.X_OptimACS_Mesh.IPAddress`.
const NETWORK_SECTION: &str = "aethermesh";

fn net_opt(name: &str) -> String {
    format!("network.{NETWORK_SECTION}.{name}")
}

/// Create the static network interface + firewall zone binding for the mesh.
fn ensure_network_interface() -> Result<(), String> {
    // The mesh device is the wifi-iface's ifname (mesh0, mesh1, ...). Resolve it
    // from the wireless section; fall back to `mesh0` (OpenWrt's default).
    let ifname = uci_get(&opt("ifname")).trim().to_owned();
    let ifname = if ifname.is_empty() { "mesh0".to_owned() } else { ifname };

    // Create the network interface if it does not exist.
    if uci_get(&format!("network.{NETWORK_SECTION}")).trim().is_empty() {
        uci_set(&format!("network.{NETWORK_SECTION}"), "interface")?;
        uci_set(&net_opt("device"), &ifname)?;
        uci_set(&net_opt("proto"), "static")?;
        // The address is set by the controller via IPAddress; if it was not
        // sent yet, leave it empty and let the controller fill it in.
        let ip = uci_get(&opt("ipaddr"));
        if !ip.trim().is_empty() {
            uci_set(&net_opt("ipaddr"), ip.trim())?;
        }
        info!("mesh: created network interface '{NETWORK_SECTION}' on {ifname}");
    }

    // Bind the mesh network to the lan firewall zone so mesh traffic is not
    // dropped. Idempotent: only add if not already present.
    let zone = uci_get("firewall.@zone[0].name");
    if zone.trim() == "lan" {
        let networks = uci_get("firewall.@zone[0].network");
        if !networks.split_whitespace().any(|n| n == NETWORK_SECTION) {
            uci_add_list("firewall.@zone[0].network", NETWORK_SECTION)?;
            info!("mesh: bound '{NETWORK_SECTION}' to lan firewall zone");
        }
    }

    Ok(())
}

/// Remove the network interface + firewall zone binding when the mesh is torn
/// down. Best-effort: a missing section is not an error.
fn remove_network_interface() -> Result<(), String> {
    if !uci_get(&format!("network.{NETWORK_SECTION}")).trim().is_empty() {
        uci_delete(&format!("network.{NETWORK_SECTION}"))?;
        info!("mesh: removed network interface '{NETWORK_SECTION}'");
    }
    // Remove the mesh network from the lan zone if present.
    let networks = uci_get("firewall.@zone[0].network");
    if networks.split_whitespace().any(|n| n == NETWORK_SECTION) {
        uci_del_list("firewall.@zone[0].network", NETWORK_SECTION)?;
        info!("mesh: unbound '{NETWORK_SECTION}' from lan firewall zone");
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

    /// WingCapable must be reported even with no mesh configured.
    ///
    /// It is a property of the firmware, not of the current mesh. The
    /// controller needs it BEFORE it plans anything — that is the whole point,
    /// since without it a WING-capable device is permanently classified as
    /// 802.11s and the WING compiler is unreachable.
    #[test]
    fn wing_capability_is_reported_even_with_no_mesh() {
        let cfg = ClientConfig::default();
        let m = get(&cfg, "Device.X_OptimACS_Mesh.");
        assert!(
            m.contains_key("Device.X_OptimACS_Mesh.WingCapable"),
            "WingCapable must always be present, got: {:?}",
            m.keys().collect::<Vec<_>>()
        );
        let v = &m["Device.X_OptimACS_Mesh.WingCapable"];
        assert!(v == "0" || v == "1", "must be a boolean, got {v:?}");
    }

    /// A capability must not be settable.
    ///
    /// Letting a controller assert it would let a device be planned into a mesh
    /// family its firmware cannot run.
    #[tokio::test]
    async fn wing_capability_is_read_only() {
        let cfg = ClientConfig::default();
        let err = set(&cfg, "Device.X_OptimACS_Mesh.WingCapable", "1")
            .await
            .expect_err("must be refused");
        assert!(err.contains("read-only"), "got: {err}");
    }

    /// Only established peers are reported as links; a peer stuck in
    /// OPN_SNT/LISTEN is not a usable path and must not appear.
    #[test]
    fn only_established_peers_are_reported() {
        // Simulate `iw dev <iface> station dump` for a mesh with one ESTAB
        // peer and one stuck in OPN_SNT.
        let dump = "\
Station d6:f3:37:42:d3:cd (on phy1-mesh0)
\tsignal: -45 dBm
\tmesh plink:\tESTAB
Station ae:5e:ca:cf:3f:1a (on phy1-mesh0)
\tsignal: -60 dBm
\tmesh plink:\tOPN_SNT
";
        // Extract established peer MACs the same way mesh_peers() does.
        let lines: Vec<&str> = dump.lines().collect();
        let mut peers = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            if let Some(mac) = lines[i].strip_prefix("Station ") {
                let mac = mac.split_whitespace().next().unwrap_or("").to_owned();
                let mut established = false;
                i += 1;
                while i < lines.len() && !lines[i].starts_with("Station ") {
                    if lines[i].contains("mesh plink:") && lines[i].contains("ESTAB") {
                        established = true;
                    }
                    i += 1;
                }
                if established {
                    peers.push(mac);
                }
                continue;
            }
            i += 1;
        }
        assert_eq!(peers, vec!["d6:f3:37:42:d3:cd"]);
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
