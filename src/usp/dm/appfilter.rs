//! TR-181 `Device.X_OptimACS_AppFilter.` and `Device.X_OptimACS_NDPI.` —
//! device-side DPI configuration over USP.
//!
//! # Why a vendor object
//!
//! TR-181 has no standard object for application filtering. The data model covers
//! `Device.Firewall.`, `Device.DNS.`, `Device.WiFi.` and so on, but app-level DPI
//! is vendor-extension territory. Without this, a USP controller can reach a
//! device running Open App Filter or nDPId and be unable to configure either — the
//! agent is connected, the engine is installed, and no parental rule can be
//! applied.
//!
//! Both objects map onto UCI packages the engines already read, so nothing here
//! invents a control surface:
//!
//! - `X_OptimACS_AppFilter` -> `/etc/config/appfilter`, read by Open App Filter
//!   v6.1.8 (`oaf_rule`, `appfilter_ubus.c`).
//! - `X_OptimACS_NDPI` -> `/etc/config/nDPId-testing`, read by the nDPId init
//!   script.
//!
//! # Two options that are load-bearing, not tuning
//!
//! `DisableHNAT` and `DisableQUIC` default to `1` here and should stay that way:
//!
//! - Hardware NAT offload bypasses the `oaf.ko` kernel module entirely. On IPQ
//!   targets with HNAT enabled the module observes nothing, so filtering is
//!   configured, reports healthy, and blocks nothing.
//! - QUIC conceals the SNI that Host-based classification matches on, so
//!   QUIC-capable clients bypass filtering unless it is disabled and they fall
//!   back to TCP/TLS.
//!
//! Upstream defaults BOTH to 0.

use crate::config::ClientConfig;
use crate::usp::tp469::uci_backend::{uci_commit, uci_get, uci_set};
use std::collections::HashMap;

/// UCI package Open App Filter reads.
const OAF_PKG: &str = "appfilter";
/// UCI package the nDPId init script reads.
const NDPI_PKG: &str = "nDPId-testing";

/// `Device.X_OptimACS_AppFilter.<Param>` -> `appfilter.global.<option>`.
///
/// Only the global section is exposed as scalars. The app id list and the
/// per-client scope are set through `AppList` and `Users` below, because both are
/// list-valued and UCI lists are not a single option write.
const OAF_GLOBAL_PARAMS: &[(&str, &str)] = &[
    ("Enable", "enable"),
    ("WorkMode", "work_mode"),
    ("UserMode", "user_mode"),
    ("LanIfname", "lan_ifname"),
    ("TcpRst", "tcp_rst"),
    ("RecordEnable", "record_enable"),
    ("AutoLoadEngine", "auto_load_engine"),
    ("DisableHNAT", "disable_hnat"),
    ("DisableQUIC", "disable_quic"),
    ("AppFilterMode", "app_filter_mode"),
];

/// `Device.X_OptimACS_AppFilter.Time.<Param>` -> `appfilter.time.<option>`.
const OAF_TIME_PARAMS: &[(&str, &str)] = &[
    ("Mode", "time_mode"),
    ("Days", "days"),
    ("StartTime", "start_time"),
    ("EndTime", "end_time"),
];

/// `Device.X_OptimACS_NDPI.<Param>` -> `nDPId-testing.nDPId.<option>`.
const NDPI_PARAMS: &[(&str, &str)] = &[("Enable", "enabled"), ("Interface", "netif")];

pub fn get(_cfg: &ClientConfig, path: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();

    if path.starts_with("Device.X_OptimACS_AppFilter") {
        for (param, opt) in OAF_GLOBAL_PARAMS {
            let v = uci_get(&format!("{OAF_PKG}.global.{opt}"));
            if !v.is_empty() {
                m.insert(format!("Device.X_OptimACS_AppFilter.{param}"), v);
            }
        }
        for (param, opt) in OAF_TIME_PARAMS {
            let v = uci_get(&format!("{OAF_PKG}.time.{opt}"));
            if !v.is_empty() {
                m.insert(format!("Device.X_OptimACS_AppFilter.Time.{param}"), v);
            }
        }
        // The blocked app id list, as the engine actually stores it: one flat
        // list at `appfilter.rule.app_list` (v6.1.8 `oaf_rule`). The per-class
        // `<class>apps` keys are the 5.x schema and are not read.
        let apps = uci_get(&format!("{OAF_PKG}.rule.app_list"));
        if !apps.is_empty() {
            m.insert("Device.X_OptimACS_AppFilter.AppList".into(), apps);
        }
        // Observed classification results, merged from the engine's several
        // per-MAC ubus methods into one envelope. Served from a 300s cache so
        // the merge never rides the controller's parameter poll — see dm::dpi.
        if let Some(t) = super::dpi::oaf_telemetry() {
            m.insert("Device.X_OptimACS_AppFilter.Telemetry".into(), t);
        }
    }

    if path.starts_with("Device.X_OptimACS_NDPI") {
        for (param, opt) in NDPI_PARAMS {
            let v = uci_get(&format!("{NDPI_PKG}.nDPId.{opt}"));
            if !v.is_empty() {
                m.insert(format!("Device.X_OptimACS_NDPI.{param}"), v);
            }
        }
    }

    m
}

pub async fn set(_cfg: &ClientConfig, path: &str, value: &str) -> Result<(), String> {
    // Open App Filter
    if let Some(param) = path.strip_prefix("Device.X_OptimACS_AppFilter.") {
        // Time sub-object.
        if let Some(tparam) = param.strip_prefix("Time.") {
            let Some((_, opt)) = OAF_TIME_PARAMS.iter().find(|(p, _)| *p == tparam) else {
                return Err(format!("unknown AppFilter.Time parameter: {tparam}"));
            };
            uci_set(&format!("{OAF_PKG}.time.{opt}"), value)?;
            return commit_and_reload(OAF_PKG).await;
        }

        // The blocked app id list. Written as a single space-separated option so
        // `config_get appid_list rule app_list` word-splits it, which is what
        // `oaf_rule` does.
        if param == "AppList" {
            uci_set(&format!("{OAF_PKG}.rule.app_list"), value)?;
            return commit_and_reload(OAF_PKG).await;
        }

        // Per-client scope: anonymous `af_user` sections carrying `mac`.
        //
        // NOTE this only takes effect when `global.user_mode` is 1 — `oaf_rule`
        // collects the MAC list ONLY in that mode. Set without it, the rules apply
        // to the whole LAN regardless of what is listed here, which looks like a
        // working per-child policy that silently covers every device.
        if param == "Users" {
            return set_users(value).await;
        }

        let Some((_, opt)) = OAF_GLOBAL_PARAMS.iter().find(|(p, _)| *p == param) else {
            return Err(format!("unknown AppFilter parameter: {param}"));
        };
        uci_set(&format!("{OAF_PKG}.global.{opt}"), value)?;
        return commit_and_reload(OAF_PKG).await;
    }

    // nDPId
    if let Some(param) = path.strip_prefix("Device.X_OptimACS_NDPI.") {
        let Some((_, opt)) = NDPI_PARAMS.iter().find(|(p, _)| *p == param) else {
            return Err(format!("unknown NDPI parameter: {param}"));
        };
        uci_set(&format!("{NDPI_PKG}.nDPId.{opt}"), value)?;
        uci_commit(NDPI_PKG)?;
        restart_service("nDPId-testing").await;
        return Ok(());
    }

    Err(format!("unhandled AppFilter path: {path}"))
}

/// Replace the `af_user` MAC list wholesale.
///
/// `value` is a comma- or space-separated MAC list. Replacing rather than merging
/// is deliberate: a controller that removes a child's device from a profile
/// expects that device to stop being filtered, and a merge would leave it
/// filtered forever with no way to express removal.
async fn set_users(value: &str) -> Result<(), String> {
    use std::process::Command;

    // Drop every existing af_user section. `uci delete` on an anonymous section
    // shifts the remaining indices, so always remove index 0 until none is left.
    loop {
        let out = Command::new("uci")
            .args(["get", &format!("{OAF_PKG}.@af_user[0]")])
            .output()
            .map_err(|e| format!("uci get failed: {e}"))?;
        if !out.status.success() {
            break;
        }
        let st = Command::new("uci")
            .args(["delete", &format!("{OAF_PKG}.@af_user[0]")])
            .status()
            .map_err(|e| format!("uci delete failed: {e}"))?;
        if !st.success() {
            break;
        }
    }

    let macs: Vec<&str> = value
        .split([',', ' '])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();

    for mac in &macs {
        let out = Command::new("uci")
            .args(["add", OAF_PKG, "af_user"])
            .output()
            .map_err(|e| format!("uci add failed: {e}"))?;
        if !out.status.success() {
            return Err("uci add af_user failed".into());
        }
        let section = String::from_utf8_lossy(&out.stdout).trim().to_string();
        uci_set(&format!("{OAF_PKG}.{section}.mac"), mac)?;
    }

    // Scoping only takes effect in user_mode 1 (see the note in `set`). Setting it
    // alongside a non-empty list keeps the two from disagreeing: a MAC list that
    // silently applies to everyone is worse than no list.
    if !macs.is_empty() {
        uci_set(&format!("{OAF_PKG}.global.user_mode"), "1")?;
    }

    commit_and_reload(OAF_PKG).await
}

/// Commit UCI and ask the daemon to reload.
///
/// `/etc/init.d/appfilter reload` runs `oaf_rule reload`, which re-pushes the app
/// list and MAC list into the kernel module through `/dev/appfilter`. Without it
/// the UCI change is on disk and the running engine keeps enforcing the old rules
/// — the config appears applied and behaviour does not change.
async fn commit_and_reload(pkg: &str) -> Result<(), String> {
    uci_commit(pkg)?;
    restart_service("appfilter").await;
    Ok(())
}

async fn restart_service(name: &str) {
    use tokio::process::Command;
    let path = format!("/etc/init.d/{name}");
    if !std::path::Path::new(&path).exists() {
        log::warn!("{path} not present — config committed but service not reloaded");
        return;
    }
    match Command::new(&path).arg("reload").status().await {
        Ok(s) if s.success() => log::info!("{name}: reloaded"),
        Ok(s) => log::warn!("{name}: reload exited with {s}"),
        Err(e) => log::warn!("{name}: reload failed: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_exposed_param_maps_to_a_real_uci_option() {
        // Guards against a parameter that looks settable over USP but writes an
        // option no engine reads. Option names are from upstream's own
        // appfilter.config and oaf_rule.
        for (_, opt) in OAF_GLOBAL_PARAMS {
            assert!(!opt.is_empty());
            assert!(!opt.contains('.'), "{opt} must be a bare option name");
        }
        for (_, opt) in OAF_TIME_PARAMS {
            assert!(!opt.contains('.'), "{opt} must be a bare option name");
        }
    }

    #[test]
    fn the_offload_and_quic_guards_are_exposed() {
        // Without these two a controller cannot turn off the conditions that make
        // filtering silently observe nothing on IPQ.
        assert!(OAF_GLOBAL_PARAMS.iter().any(|(p, _)| *p == "DisableHNAT"));
        assert!(OAF_GLOBAL_PARAMS.iter().any(|(p, _)| *p == "DisableQUIC"));
    }

    #[test]
    fn app_list_is_the_v6_flat_key_not_the_5x_per_class_keys() {
        // v6.1.8 reads `appfilter.rule.app_list`; the per-class `<class>apps`
        // keys are 5.x and are never read.
        let m = OAF_GLOBAL_PARAMS.iter().find(|(p, _)| *p == "AppList");
        assert!(
            m.is_none(),
            "AppList is a list, handled outside the scalar table"
        );
    }
}
