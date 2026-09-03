//! Consent-gated capture switches — `Device.X_OptimACS_{Sensing,DPI}.Enable`.
//!
//! `aether-sensord` ships with sensing and classification OFF, and that is
//! deliberate rather than an oversight: sensing REPORTS attacker addresses,
//! which are personal data under GDPR, and classification copies packet payload
//! (a TLS ClientHello, a QUIC Initial) into userspace. `/etc/config/aether-sensord`
//! states both, citing ADR-019 §9 and ADR-020 decision 1.
//!
//! That makes them a per-subscriber decision, not a packaging default -- which
//! is exactly why they belong here. The controller holds the consent record and
//! pushes it; the device applies it and reports back what it is ACTUALLY doing.
//!
//! ## Enable vs DaemonRunning
//!
//! Two parameters, not one, because they answer different questions and a
//! single "enabled" would let them disagree silently. `Enable` is the committed
//! intent in UCI. `DaemonRunning` is whether `aether-sensord` is actually up. A
//! console showing "Sensing: on" while the daemon is dead is the failure mode
//! this whole area keeps producing -- the operator reads an empty flow list as
//! "no traffic" when the truth is "nothing is looking".
//!
//! Neither is inferred from the other. If UCI says 1 and the process is absent,
//! that is reported, not smoothed over.

use std::collections::HashMap;

use log::{info, warn};

use crate::config::ClientConfig;
use crate::usp::tp469::uci_backend::{uci_commit, uci_get, uci_set};

/// UCI package owning both switches.
const PKG: &str = "aether-sensord";
/// Section within it. Single `main` section; the daemon reads no other.
const SECTION: &str = "main";

/// Sensing: firewall-drop reporting. Personal data (attacker addresses).
const OPT_SENSE: &str = "sense_enabled";
/// Classification: nDPI payload inspection. Stronger consent than sensing.
const OPT_DPI: &str = "dpi_enabled";

/// Is `aether-sensord` actually running?
///
/// Checked by pidfile-free process scan rather than `service ... status`, whose
/// exit codes vary between procd versions. A false here with `Enable=1` is the
/// disagreement the controller most needs to see.
fn daemon_running() -> bool {
    std::path::Path::new("/proc")
        .read_dir()
        .map(|entries| {
            entries.flatten().any(|e| {
                let comm = e.path().join("comm");
                std::fs::read_to_string(comm)
                    .map(|s| s.trim() == "aether-sensord")
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

/// UCI truthiness. OpenWrt accepts several spellings; treat them all.
fn uci_truthy(v: &str) -> bool {
    matches!(v.trim(), "1" | "on" | "true" | "yes" | "enabled")
}

/// A USP boolean. The controller sends "1"/"0" or "true"/"false".
fn usp_truthy(v: &str) -> Result<bool, String> {
    match v.trim().to_ascii_lowercase().as_str() {
        "1" | "true" => Ok(true),
        "0" | "false" => Ok(false),
        other => Err(format!("not a boolean: {other}")),
    }
}

fn read_flag(opt: &str) -> bool {
    uci_truthy(&uci_get(&format!("{PKG}.{SECTION}.{opt}")))
}

/// Report both switches and whether the daemon backing them is alive.
pub fn get(_cfg: &ClientConfig, path: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    let running = daemon_running();

    if path.starts_with("Device.X_OptimACS_Sensing") {
        m.insert(
            "Device.X_OptimACS_Sensing.Enable".into(),
            if read_flag(OPT_SENSE) { "1" } else { "0" }.into(),
        );
        m.insert(
            "Device.X_OptimACS_Sensing.DaemonRunning".into(),
            if running { "1" } else { "0" }.into(),
        );
    }
    if path.starts_with("Device.X_OptimACS_DPI") {
        m.insert(
            "Device.X_OptimACS_DPI.Enable".into(),
            if read_flag(OPT_DPI) { "1" } else { "0" }.into(),
        );
        m.insert(
            "Device.X_OptimACS_DPI.DaemonRunning".into(),
            if running { "1" } else { "0" }.into(),
        );
    }
    m
}

/// Apply a consent decision from the controller.
///
/// Committed to UCI and the daemon restarted, in that order. A restart is
/// required rather than a reload: `aether-sensord` installs its NFLOG rules at
/// start and decides then whether to open the sensing and classification
/// groups, so a live process will not begin (or stop) capturing on a config
/// change alone. Skipping it would leave UCI saying "on" while nothing is
/// captured -- the config applied, the behaviour unchanged.
pub async fn set(_cfg: &ClientConfig, path: &str, value: &str) -> Result<(), String> {
    let opt = match path {
        "Device.X_OptimACS_Sensing.Enable" => OPT_SENSE,
        "Device.X_OptimACS_DPI.Enable" => OPT_DPI,
        other => return Err(format!("read-only or unknown path: {other}")),
    };

    let on = usp_truthy(value)?;
    uci_set(
        &format!("{PKG}.{SECTION}.{opt}"),
        if on { "1" } else { "0" },
    )?;
    uci_commit(PKG)?;

    // Logged at INFO with the decision spelled out: this is a consent change,
    // and "who turned capture on for this device, and when" must be answerable
    // from the device's own log, not only from the controller that sent it.
    info!(
        "consent: {opt} set to {} by controller; restarting aether-sensord",
        if on {
            "1 (capture ON)"
        } else {
            "0 (capture OFF)"
        }
    );

    restart_sensord().await;
    Ok(())
}

async fn restart_sensord() {
    use tokio::process::Command;
    let path = "/etc/init.d/aether-sensord";
    if !std::path::Path::new(path).exists() {
        warn!("{path} not present -- UCI committed but capture state unchanged");
        return;
    }
    match Command::new(path).arg("restart").status().await {
        Ok(s) if s.success() => info!("aether-sensord: restarted"),
        Ok(s) => warn!("aether-sensord: restart exited with {s}"),
        Err(e) => warn!("aether-sensord: restart failed: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `get` must answer a PREFIX query, not only the exact leaf.
    ///
    /// The controller polls `Device.X_OptimACS_Sensing.` and never
    /// `...Sensing.Enable`. Matching only the leaf makes the switch present in
    /// the data model and invisible to the sole caller -- shipped once already
    /// in this change before the prefix was checked.
    #[test]
    fn a_prefix_query_returns_the_switch() {
        let cfg = ClientConfig::default();
        let m = get(&cfg, "Device.X_OptimACS_Sensing.");
        assert!(
            m.contains_key("Device.X_OptimACS_Sensing.Enable"),
            "prefix GET must include Enable, got: {:?}",
            m.keys().collect::<Vec<_>>()
        );
        let d = get(&cfg, "Device.X_OptimACS_DPI.");
        assert!(d.contains_key("Device.X_OptimACS_DPI.Enable"));
    }

    #[test]
    fn usp_booleans_accept_both_spellings() {
        assert_eq!(usp_truthy("1"), Ok(true));
        assert_eq!(usp_truthy("true"), Ok(true));
        assert_eq!(usp_truthy("0"), Ok(false));
        assert_eq!(usp_truthy("FALSE"), Ok(false));
        assert!(usp_truthy("maybe").is_err());
    }

    /// A malformed value must not be treated as "off".
    ///
    /// Silently coercing junk to false would turn a controller bug into a quiet
    /// consent withdrawal -- capture stops, nothing errors, and the console
    /// still shows whatever it last believed.
    #[test]
    fn a_bad_boolean_is_rejected_not_coerced_to_off() {
        assert!(usp_truthy("").is_err());
        assert!(usp_truthy("off").is_err());
    }

    #[test]
    fn uci_truthiness_covers_openwrt_spellings() {
        for v in ["1", "on", "true", "yes", "enabled"] {
            assert!(uci_truthy(v), "{v} should be truthy");
        }
        for v in ["0", "off", "false", "no", ""] {
            assert!(!uci_truthy(v), "{v} should be falsy");
        }
    }

    /// The two switches must map to DIFFERENT UCI options.
    ///
    /// They are separate consents -- sensing reports addresses, classification
    /// copies payload -- and collapsing them onto one option would let enabling
    /// the weaker one silently enable the stronger.
    #[test]
    fn sensing_and_dpi_are_not_the_same_switch() {
        assert_ne!(OPT_SENSE, OPT_DPI);
    }
}
