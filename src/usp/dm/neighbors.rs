//! Neighbouring-AP visibility — `Device.WiFi.NeighboringWiFiDiagnostic.*`.
//!
//! ## Two different questions
//!
//! *"Is my channel crowded?"* and *"which channel should I move to?"* look like
//! one question and are answered by completely different mechanisms.
//!
//! An AP hears its neighbours' beacons continuously, for free, because it is
//! already sitting on that channel listening. The kernel keeps those in a BSS
//! table that `iw dev <if> scan dump` returns in **0 ms without touching the
//! radio**. Measured on a D50: 28 neighbours on the operating channel, `last
//! seen` between 0 and 90 ms. That answers the first question and costs nothing.
//!
//! It cannot answer the second. A radio cannot hear a channel it is not on, so
//! the same table held exactly one entry for any other channel, stale from some
//! earlier scan. Comparing channels requires *going* to them, which is an active
//! scan: the radio leaves its channel and associated clients stop being served
//! for the duration. TR-181 models that as an on-demand diagnostic for this
//! reason, and so does uCentral's `wifiscan`. There is no passive shortcut.
//!
//! A passive channel survey is not one either: `iw survey dump` reports every
//! channel, but off-channel counters are frozen at whatever the association-time
//! scan collected. Sampled 120 s apart on a live AP, the operating channel
//! advanced by 119,958 ms and every other channel advanced by **zero**. A busy
//! percentage from a 149 ms sample taken hours ago is not a measurement.
//!
//! ## What this does
//!
//! - Reports the cached BSS table on every poll. Free, live, no disruption.
//! - Runs one active scan shortly after start, *if no client is associated*.
//!   At boot that is true and the scan costs nothing; on a mere agent restart it
//!   usually is not, and a scan would drop live clients for an upgrade.
//! - Accepts `DiagnosticsState = Requested` to force a scan, which is the
//!   controller's existing scan button.

use std::collections::HashMap;
use std::sync::Mutex;

use log::{info, warn};

use crate::config::ClientConfig;

/// One neighbouring BSS as the radio last heard it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Neighbor {
    pub bssid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ssid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal_dbm: Option<f64>,
    /// Milliseconds since the radio last heard this BSS. A few hundred means a
    /// beacon on the operating channel; minutes or hours means it is a leftover
    /// from the last active scan and says nothing about now.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen_ms: Option<u64>,
    /// The interface that heard it, so a two-radio device does not merge bands.
    pub radio: String,
}

/// `Requested` -> `Complete` / `Error_*`, per TR-181.
static STATE: Mutex<Option<String>> = Mutex::new(None);

fn state() -> String {
    STATE
        .lock()
        .ok()
        .and_then(|s| s.clone())
        .unwrap_or_else(|| "None".to_string())
}

fn set_state_value(v: &str) {
    if let Ok(mut s) = STATE.lock() {
        *s = Some(v.to_string());
    }
}

/// Wireless interfaces, from `iw dev`.
fn wifi_interfaces() -> Vec<String> {
    let Some(out) = std::process::Command::new("iw")
        .arg("dev")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
    else {
        return Vec::new();
    };
    out.lines()
        .filter_map(|l| {
            let t = l.trim();
            t.strip_prefix("Interface ").map(str::to_owned)
        })
        .collect()
}

/// Is any client associated to this interface?
///
/// The guard on the start-up scan. `iw station dump` lists associated stations;
/// a mesh interface lists its peers, which count -- taking the backhaul off
/// channel is as disruptive as dropping a client, and rather harder to notice.
fn has_stations(ifname: &str) -> bool {
    std::process::Command::new("iw")
        .args(["dev", ifname, "station", "dump"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.lines().any(|l| l.starts_with("Station ")))
        .unwrap_or(false)
}

/// Parse `iw scan dump` output into neighbours.
pub fn parse_scan(out: &str, radio: &str) -> Vec<Neighbor> {
    let mut found = Vec::new();
    let mut cur: Option<Neighbor> = None;

    for line in out.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("BSS ") {
            if let Some(n) = cur.take() {
                found.push(n);
            }
            // "BSS aa:bb:cc:dd:ee:ff(on phy0-ap0)" -- the suffix is not the MAC.
            let bssid = rest
                .split(['(', ' '])
                .next()
                .unwrap_or(rest)
                .trim()
                .to_lowercase();
            cur = Some(Neighbor {
                bssid,
                ssid: None,
                channel: None,
                signal_dbm: None,
                last_seen_ms: None,
                radio: radio.to_owned(),
            });
            continue;
        }
        let Some(n) = cur.as_mut() else { continue };
        if let Some(v) = t.strip_prefix("SSID: ") {
            // A hidden SSID is reported as empty; None says "not advertised"
            // rather than inventing a blank network name.
            let v = v.trim();
            if !v.is_empty() {
                n.ssid = Some(v.to_owned());
            }
        } else if let Some(v) = t.strip_prefix("signal: ") {
            n.signal_dbm = v.split_whitespace().next().and_then(|x| x.parse().ok());
        } else if let Some(v) = t.strip_prefix("last seen: ") {
            n.last_seen_ms = v.split_whitespace().next().and_then(|x| x.parse().ok());
        } else if let Some(v) = t.strip_prefix("DS Parameter set: channel ") {
            n.channel = v.trim().parse().ok();
        } else if let Some(v) = t.strip_prefix("* primary channel: ") {
            n.channel = v.trim().parse().ok();
        }
    }
    if let Some(n) = cur.take() {
        found.push(n);
    }
    found
}

/// Read the cached BSS table. Never touches the radio.
fn cached_neighbors() -> Vec<Neighbor> {
    let mut all = Vec::new();
    for ifname in wifi_interfaces() {
        let Some(out) = std::process::Command::new("iw")
            .args(["dev", &ifname, "scan", "dump"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
        else {
            continue;
        };
        all.extend(parse_scan(&out, &ifname));
    }
    all
}

/// Run an active scan on every interface that has nothing associated.
///
/// Interfaces with stations are skipped, not scanned-and-apologised-for: the
/// whole point of the guard is that an operator pressing "scan" on a busy AP
/// should not drop its clients without having asked for that.
fn active_scan() -> (usize, usize) {
    let (mut scanned, mut skipped) = (0, 0);
    for ifname in wifi_interfaces() {
        if has_stations(&ifname) {
            info!("neighbors: {ifname}: skipping active scan, stations associated");
            skipped += 1;
            continue;
        }
        match std::process::Command::new("iw")
            .args(["dev", &ifname, "scan"])
            .output()
        {
            Ok(_) => scanned += 1,
            Err(e) => warn!("neighbors: {ifname}: scan failed: {e}"),
        }
    }
    (scanned, skipped)
}

/// TR-181 parameters for the neighbouring-AP diagnostic.
pub fn get(_cfg: &ClientConfig, path: &str) -> HashMap<String, String> {
    let base = "Device.WiFi.NeighboringWiFiDiagnostic";
    let mut m = HashMap::new();
    if !path.starts_with(base) && !path.starts_with("Device.WiFi.") && path != "Device." {
        return m;
    }

    let found = cached_neighbors();
    m.insert(format!("{base}.DiagnosticsState"), state());
    m.insert(
        format!("{base}.ResultNumberOfEntries"),
        found.len().to_string(),
    );
    // One JSON parameter rather than five leaves across forty entries, matching
    // how X_OptimACS_Mesh.Originators and .Stats already carry structured data.
    if let Ok(js) = serde_json::to_string(&found) {
        m.insert(format!("{base}.X_OptimACS_Result"), js);
    }
    m
}

/// `DiagnosticsState` is the trigger; `Requested` is the only value accepted.
pub fn set(_cfg: &ClientConfig, path: &str, value: &str) -> Result<(), String> {
    let leaf = path.rsplit('.').next().unwrap_or("");
    if leaf != "DiagnosticsState" {
        return Err(format!("unknown neighboring-wifi parameter: {leaf}"));
    }
    match value.trim() {
        "Requested" => {
            set_state_value("Requested");
            std::thread::spawn(|| {
                let (scanned, skipped) = active_scan();
                if scanned == 0 && skipped > 0 {
                    // Every radio was busy. Not an error -- the cached table is
                    // still returned -- but the controller must not read this as
                    // a fresh cross-channel picture.
                    set_state_value("Error_Other");
                } else {
                    set_state_value("Complete");
                }
            });
            Ok(())
        }
        "None" => {
            set_state_value("None");
            Ok(())
        }
        other => Err(format!(
            "DiagnosticsState may be set to Requested or None, not {other}"
        )),
    }
}

/// One active scan shortly after start, on radios with nothing associated.
///
/// At device boot no client has associated yet, so this is free and gives the
/// controller a cross-channel picture it can otherwise never have. After a mere
/// agent restart -- a package upgrade, say -- clients usually *are* associated,
/// the guard skips those radios, and the upgrade does not cost anyone their
/// connection.
pub fn scan_on_start() {
    std::thread::spawn(|| {
        // Let the radios finish coming up; scanning a half-initialised phy
        // returns nothing and wastes the one free opportunity.
        std::thread::sleep(std::time::Duration::from_secs(20));
        let (scanned, skipped) = active_scan();
        info!("neighbors: start-up scan finished (scanned={scanned} skipped={skipped})");
        if scanned > 0 {
            set_state_value("Complete");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real `iw scan dump` output from a D50.
    const SAMPLE: &str = "\
BSS c4:27:28:5d:43:94(on phy0-ap0)
\tlast seen: 20 ms ago
\tsignal: -75.00 dBm
\tSSID: Be
\tDS Parameter set: channel 1
BSS d4:f3:37:42:ca:3e(on phy0-ap0)
\tlast seen: 0 ms ago
\tsignal: -26.00 dBm
\tSSID: AetherM
\tDS Parameter set: channel 1
BSS 00:5c:c2:8e:9b:15(on phy0-ap0)
\tlast seen: 4000 ms ago
\tsignal: -75.00 dBm
\tSSID: \n\tDS Parameter set: channel 11
";

    #[test]
    fn a_bssid_is_not_the_interface_suffix() {
        let n = parse_scan(SAMPLE, "phy0-ap0");
        assert_eq!(
            n[0].bssid, "c4:27:28:5d:43:94",
            "`(on phy0-ap0)` is not MAC"
        );
    }

    #[test]
    fn signal_channel_and_age_are_carried() {
        let n = parse_scan(SAMPLE, "phy0-ap0");
        assert_eq!(n[0].signal_dbm, Some(-75.0));
        assert_eq!(n[0].channel, Some(1));
        assert_eq!(n[0].last_seen_ms, Some(20));
        assert_eq!(n[0].ssid.as_deref(), Some("Be"));
    }

    /// Age is the difference between "my channel, right now" and "some other
    /// channel, at some point in the past". Dropping it would present both as
    /// equally current.
    #[test]
    fn age_distinguishes_a_live_beacon_from_a_stale_scan_entry() {
        let n = parse_scan(SAMPLE, "phy0-ap0");
        assert_eq!(
            n[1].last_seen_ms,
            Some(0),
            "beacon on the operating channel"
        );
        assert_eq!(n[2].last_seen_ms, Some(4000), "left over from a scan");
    }

    /// A hidden network advertises an empty SSID. Reporting `Some("")` would put
    /// a blank-named network in the list as though it had been identified.
    #[test]
    fn a_hidden_ssid_is_absent_not_empty() {
        let n = parse_scan(SAMPLE, "phy0-ap0");
        assert_eq!(n[2].ssid, None);
    }

    #[test]
    fn every_entry_records_which_radio_heard_it() {
        let n = parse_scan(SAMPLE, "phy0-ap0");
        assert!(n.iter().all(|x| x.radio == "phy0-ap0"));
    }

    #[test]
    fn only_diagnostics_state_is_settable() {
        let cfg = ClientConfig::default();
        let e = set(&cfg, "Device.WiFi.NeighboringWiFiDiagnostic.Result", "x").unwrap_err();
        assert!(e.contains("unknown neighboring-wifi parameter"), "{e}");
    }

    #[test]
    fn diagnostics_state_rejects_values_other_than_requested() {
        let cfg = ClientConfig::default();
        let e = set(
            &cfg,
            "Device.WiFi.NeighboringWiFiDiagnostic.DiagnosticsState",
            "Complete",
        )
        .unwrap_err();
        assert!(e.contains("Requested"), "{e}");
    }
}
