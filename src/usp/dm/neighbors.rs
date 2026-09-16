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

/// Wireless interfaces grouped by the radio that actually carries them.
///
/// A scan takes a **radio** off channel. Neither the interface nor the wiphy is
/// that radio:
///
/// ```text
///   D50 (ath11k, 6.12)      phy#1 -> phy1-mesh0, phy1-ap0        one radio
///                           phy#0 -> phy0-ap0                    one radio
///   BPI-R4 (mt7996, 6.18)   phy#0 -> phy0.0-ap0    Radios: 0     2.4 GHz
///                                    phy0.1-ap0    Radios: 1     5 GHz
///                                    phy0.1-mesh0  Radios: 1     5 GHz
///                                    phy0.2-ap0    Radios: 2     6 GHz
/// ```
///
/// Deciding per *interface* scans an idle AP and takes the mesh sharing its
/// radio down with it. Deciding per *wiphy* is right on the D50 and wrong on the
/// BPI, which presents three independent radios as one wiphy under MLO -- it
/// would refuse to scan 2.4 and 6 GHz because the 5 GHz mesh is busy.
///
/// The kernel states the answer: `iw dev <if> info` reports `Radios: N` where
/// multi-radio wiphys are supported. Where it does not (6.12 on the D50), a
/// wiphy is a radio and grouping by wiphy is correct.
fn radios() -> Vec<(String, Vec<String>)> {
    let Some(out) = std::process::Command::new("iw")
        .arg("dev")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
    else {
        return Vec::new();
    };
    let by_wiphy = parse_iw_dev(&out);

    // Subdivide each wiphy by the kernel's radio index where it reports one.
    let mut out_groups: Vec<(String, Vec<String>)> = Vec::new();
    for (phy, ifaces) in by_wiphy {
        let mut grouped: std::collections::BTreeMap<String, Vec<String>> = Default::default();
        for i in ifaces {
            let key = match radio_index(&i) {
                Some(n) => format!("{phy}/radio{n}"),
                // No index reported: this kernel has one radio per wiphy.
                None => phy.clone(),
            };
            grouped.entry(key).or_default().push(i);
        }
        out_groups.extend(grouped);
    }
    out_groups
}

/// The kernel's radio index for an interface, where the kernel reports one.
fn radio_index(ifname: &str) -> Option<u32> {
    let out = std::process::Command::new("iw")
        .args(["dev", ifname, "info"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())?;
    parse_radio_index(&out)
}

/// `Radios: 1` in `iw dev <if> info`. Absent on kernels without multi-radio
/// wiphy support, which is not an error -- it means one radio per wiphy.
pub fn parse_radio_index(info: &str) -> Option<u32> {
    info.lines()
        .map(str::trim)
        .find_map(|l| l.strip_prefix("Radios:"))
        .and_then(|v| v.trim().parse().ok())
}

/// `iw dev` lists each `phy#N` followed by the interfaces on it.
pub fn parse_iw_dev(out: &str) -> Vec<(String, Vec<String>)> {
    let mut radios: Vec<(String, Vec<String>)> = Vec::new();
    for line in out.lines() {
        let t = line.trim();
        if let Some(phy) = t.strip_prefix("phy#") {
            radios.push((format!("phy{phy}"), Vec::new()));
        } else if let Some(ifname) = t.strip_prefix("Interface ") {
            if let Some(last) = radios.last_mut() {
                last.1.push(ifname.to_owned());
            }
        }
    }
    radios
}

/// Every interface, for the cached-table read, which disturbs nothing.
fn wifi_interfaces() -> Vec<String> {
    radios().into_iter().flat_map(|(_, ifs)| ifs).collect()
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
    for (phy, ifaces) in radios() {
        // One busy interface makes the whole radio off limits. On a device whose
        // mesh and AP share a phy -- both platforms here -- scanning the idle one
        // takes the other off channel too.
        if let Some(busy) = ifaces.iter().find(|i| has_stations(i)) {
            info!("neighbors: {phy}: skipping active scan, {busy} has stations associated");
            skipped += ifaces.len();
            continue;
        }
        // One scan per radio, not per interface: the second would repeat the
        // disruption for the same result.
        let Some(ifname) = ifaces.first() else {
            continue;
        };
        match std::process::Command::new("iw")
            .args(["dev", ifname, "scan"])
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

    /// Real `iw dev` output from the BPI-R4: one radio, four interfaces, one of
    /// them the mesh.
    const IW_DEV_BPI: &str = "\
phy#0
\tInterface phy0.2-ap0
\tInterface phy0.1-mesh0
\tInterface phy0.1-ap0
\tInterface phy0.0-ap0
";

    /// And from a D50: two radios, with mesh and AP sharing the second.
    const IW_DEV_D50: &str = "\
phy#1
\tInterface phy1-mesh0
\tInterface phy1-ap0
phy#0
\tInterface phy0-ap0
";

    /// Real `iw dev <if> info` from the BPI-R4 (kernel 6.18): the kernel names
    /// the radio, so three bands on one wiphy are three groups.
    #[test]
    fn the_kernel_radio_index_is_read_when_present() {
        let info =
            "Interface phy0.1-mesh0\n\tifindex 25\n\ttype mesh point\n\twiphy 0\n\tRadios: 1\n";
        assert_eq!(parse_radio_index(info), Some(1));
    }

    /// And from a D50 (kernel 6.12), which has no such field. That is not a
    /// failure: one wiphy is one radio there, and grouping by wiphy is correct.
    #[test]
    fn an_older_kernel_reports_no_radio_index() {
        let info = "Interface phy1-mesh0\n\tifindex 12\n\ttype mesh point\n\twiphy 1\n";
        assert_eq!(parse_radio_index(info), None);
    }

    /// The BPI regression in one assertion: 2.4 and 6 GHz are independent radios
    /// and must stay scannable while the 5 GHz mesh is busy. Grouping by wiphy
    /// made the whole device unscannable; grouping by interface would have taken
    /// the mesh off channel.
    #[test]
    fn independent_bands_on_one_wiphy_are_separate_radios() {
        let by_iface = [
            ("phy0.0-ap0", Some(0u32)),
            ("phy0.1-ap0", Some(1)),
            ("phy0.1-mesh0", Some(1)),
            ("phy0.2-ap0", Some(2)),
        ];
        let mut groups: std::collections::BTreeSet<String> = Default::default();
        for (_, idx) in by_iface {
            groups.insert(match idx {
                Some(n) => format!("phy0/radio{n}"),
                None => "phy0".to_string(),
            });
        }
        assert_eq!(
            groups.len(),
            3,
            "three bands, three radios -- not one because they share a wiphy"
        );
    }

    #[test]
    fn interfaces_are_grouped_by_the_radio_that_carries_them() {
        let r = parse_iw_dev(IW_DEV_BPI);
        assert_eq!(r.len(), 1, "the BPI has one radio carrying everything");
        assert_eq!(r[0].1.len(), 4);

        let r = parse_iw_dev(IW_DEV_D50);
        assert_eq!(r.len(), 2);
        assert!(
            r[0].1.contains(&"phy1-mesh0".to_string()) && r[0].1.contains(&"phy1-ap0".to_string()),
            "mesh and AP share phy1 on the D50, which is the whole point"
        );
    }

    /// The defect this replaced: deciding per interface would scan an idle AP
    /// that shares a radio with a busy mesh, taking the mesh off channel. On the
    /// BPI every interface shares one radio, so a per-interface guard would scan
    /// three of four while the fourth carried four batman peers.
    #[test]
    fn a_busy_interface_protects_every_interface_on_its_radio() {
        let r = parse_iw_dev(IW_DEV_BPI);
        let (_, ifaces) = &r[0];
        assert!(
            ifaces.iter().any(|i| i.contains("mesh")),
            "the mesh is on this radio, so nothing on it may be scanned"
        );
        assert_eq!(
            ifaces.len(),
            4,
            "and skipping it must account for all four, not one"
        );
    }

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
