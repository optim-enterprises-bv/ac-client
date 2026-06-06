//! 802.11 IE fingerprinting via hostapd ubus.
//!
//! Every 30 seconds polls `ubus call hostapd.<iface> get_clients` to get the
//! list of associated STAs, then calls `get_sta_ies` for each to retrieve
//! the binary association IEs from the (Re)Association Request frame.
//!
//! Key signals extracted:
//!   Vendor IEs (tag 221) — OUI identifies the device's chip/OS stack:
//!     00:17:f2  → Apple   (always present on iOS/macOS, even with random MAC)
//!     00:10:18  → Broadcom (Android flagship, macOS, some Windows)
//!     8c:fd:f0  → Qualcomm Atheros (Android flagship phones)
//!     00:0c:43  → Ralink/MediaTek (budget Android, IoT)
//!     50:6f:9a  → Wi-Fi Alliance (WPA supplicant marker)
//!   Tag  45  — HT Capabilities  → WiFi 4
//!   Tag 191  — VHT Capabilities → WiFi 5
//!   Tag 255 (ext 35/36) → HE Capabilities → WiFi 6
//!
//! On non-OpenWrt systems `ubus` won't exist; all calls fail silently and the
//! table stays empty.  The module compiles and runs everywhere.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use log::{debug, info};

// ── Public types ──────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default)]
pub struct WifiFp {
    /// Vendor IE OUI 00:17:f2 — Apple device (iOS, macOS, tvOS, watchOS)
    pub has_apple_ie:    bool,
    /// Vendor IE OUI 00:10:18 — Broadcom chip (Android, macOS, some Windows)
    pub has_broadcom_ie: bool,
    /// Vendor IE OUI 8c:fd:f0 — Qualcomm Atheros (Android flagships)
    pub has_qualcomm_ie: bool,
    /// Vendor IE OUI 00:0c:43 — Ralink/MediaTek (budget Android, IoT)
    pub has_ralink_ie:   bool,
    /// HE capabilities present (802.11ax / WiFi 6)
    pub has_he:  bool,
    /// VHT capabilities present (802.11ac / WiFi 5)
    pub has_vht: bool,
    /// HT capabilities present (802.11n / WiFi 4)
    pub has_ht:  bool,
}

pub type FpTable = Arc<Mutex<HashMap<String, WifiFp>>>;
static FP_TABLE: OnceLock<FpTable> = OnceLock::new();

pub fn init() -> FpTable {
    let table: FpTable = Arc::new(Mutex::new(HashMap::new()));
    let _ = FP_TABLE.set(table.clone());
    table
}

pub fn table() -> Option<&'static FpTable> {
    FP_TABLE.get()
}

/// Spawn the polling loop as an async background task.
pub fn spawn() {
    tokio::task::spawn(async move {
        info!("wifi_snoop: starting hostapd IE polling (30s interval)");
        loop {
            poll_all_ifaces().await;
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
    });
}

// ── Polling logic ─────────────────────────────────────────────────────────────

async fn poll_all_ifaces() {
    let out = match tokio::process::Command::new("ubus")
        .args(["list", "hostapd.*"])
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => { debug!("wifi_snoop: ubus list: {e}"); return; }
    };

    let ifaces: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();

    for iface in &ifaces {
        poll_iface(iface).await;
    }
}

async fn poll_iface(iface: &str) {
    let out = match tokio::process::Command::new("ubus")
        .args(["call", iface, "get_clients"])
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => { debug!("wifi_snoop: {iface} get_clients: {e}"); return; }
    };

    let json: serde_json::Value = match serde_json::from_slice(&out.stdout) {
        Ok(v) => v,
        Err(_) => return,
    };

    let macs: Vec<String> = match json.get("clients").and_then(|c| c.as_object()) {
        Some(c) => c.keys().cloned().collect(),
        None    => return,
    };

    for mac in macs {
        if let Some(fp) = get_sta_ies(iface, &mac).await {
            debug!(
                "wifi_snoop: {} apple={} qualcomm={} broadcom={} he={} vht={} ht={}",
                mac, fp.has_apple_ie, fp.has_qualcomm_ie, fp.has_broadcom_ie,
                fp.has_he, fp.has_vht, fp.has_ht,
            );
            if let Some(t) = FP_TABLE.get() {
                // hostapd returns lowercase colon MACs; normalize to uppercase
                let key = mac.to_uppercase();
                t.lock().unwrap().insert(key, fp);
            }
        }
    }
}

async fn get_sta_ies(iface: &str, mac: &str) -> Option<WifiFp> {
    let arg = format!(r#"{{"address":"{mac}"}}"#);
    let out = tokio::process::Command::new("ubus")
        .args(["call", iface, "get_sta_ies", &arg])
        .output()
        .await
        .ok()?;

    let json: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let b64 = json.get("ies")?.as_str()?;

    use base64::Engine;
    let ies = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    Some(parse_ies(&ies))
}

// ── IE parser ─────────────────────────────────────────────────────────────────

fn parse_ies(data: &[u8]) -> WifiFp {
    let mut fp = WifiFp::default();
    let mut i = 0usize;

    while i + 2 <= data.len() {
        let tag = data[i];
        let len = data[i + 1] as usize;
        i += 2;
        if i + len > data.len() { break; }
        let val = &data[i..i + len];

        match tag {
            45  => { fp.has_ht  = true; }
            191 => { fp.has_vht = true; }
            221 => {
                // Vendor IE: bytes 0-2 = OUI, byte 3 = OUI type
                if val.len() >= 3 {
                    match (val[0], val[1], val[2]) {
                        (0x00, 0x17, 0xf2) => fp.has_apple_ie    = true,
                        (0x00, 0x10, 0x18) => fp.has_broadcom_ie = true,
                        (0x8c, 0xfd, 0xf0) => fp.has_qualcomm_ie = true,
                        (0x00, 0x0c, 0x43) => fp.has_ralink_ie   = true,
                        _ => {}
                    }
                }
            }
            255 => {
                // Extension element — first byte is the element ID extension
                if !val.is_empty() {
                    match val[0] {
                        35 | 36 => fp.has_he = true, // HE MAC (35) / HE PHY (36)
                        _ => {}
                    }
                }
            }
            _ => {}
        }

        i += len;
    }

    fp
}

// ── Classification helper ─────────────────────────────────────────────────────

/// Returns `(vendor, class)` if the WiFi IE fingerprint is conclusive.
///
/// The Apple vendor IE is present on *all* Apple devices even when they use
/// randomized MACs, making this the most reliable Apple confirmation signal.
pub fn classify_fp(fp: &WifiFp) -> Option<(&'static str, &'static str)> {
    if fp.has_apple_ie {
        return Some(("Apple", "unknown")); // class refined by other signals
    }
    if fp.has_qualcomm_ie && !fp.has_broadcom_ie {
        // Qualcomm without Broadcom → strongly suggests Android flagship
        return Some(("", "phone"));
    }
    if fp.has_ralink_ie {
        return Some(("", "iot"));
    }
    None
}
