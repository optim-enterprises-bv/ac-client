//! Status heartbeat data collection.
//!
//! Collects system metrics from `/proc`, wireless tools, and the shared GNSS
//! position, then builds a `StatusRequest` protobuf message ready to send.

use std::sync::{Arc, Mutex};

use crate::config::ClientConfig;
use crate::gnss::GnssPosition;
use crate::proto::StatusRequest;
use crate::util;

/// Build a [`StatusRequest`] populated with current system metrics.
///
/// `gnss` — shared GNSS position (may be `None` if no fix or no receiver).
/// `mac`  — device MAC address (always included for server-side identification).
pub fn build_status(
    cfg:  &ClientConfig,
    gnss: &Arc<Mutex<Option<GnssPosition>>>,
) -> StatusRequest {
    let pos = gnss.lock().ok().and_then(|g| g.clone());

    StatusRequest {
        uptime:          util::read_uptime(),
        load_avg:        util::read_load_avg(),
        free_mem:        util::read_free_mem(),
        gw:              util::get_default_gateway(),
        ip:              util::get_own_ip(),
        mac:             cfg.mac_addr.clone(),
        ssid:            util::read_ssid(),
        orion_ver:       util::read_fw_version(),
        nbs:             String::new(),   // neighbour count — populated separately if available
        rank:            String::new(),
        tot_kb_up:       String::new(),
        tot_kb_down:     String::new(),
        users:           String::new(),
        latitude:        pos.as_ref().map(|p| p.latitude.clone()).unwrap_or_default(),
        longitude:       pos.as_ref().map(|p| p.longitude.clone()).unwrap_or_default(),
        modem_status:    read_modem_status(),
        wireless_status: read_wireless_status(),
    }
}

/// Return modem status code.  0 = not present / unknown, 1 = connected.
/// Checks for a usb-connected modem via `/sys/class/net`.
fn read_modem_status() -> i32 {
    // If wwan0 or usb0 interface exists, assume modem is present
    for iface in &["wwan0", "usb0", "ppp0"] {
        if std::path::Path::new(&format!("/sys/class/net/{iface}")).exists() {
            return 1;
        }
    }
    0
}

/// Return wireless status code.  0 = down, 1 = up.
/// Checks for wlan0 carrier.
fn read_wireless_status() -> i32 {
    let carrier_path = "/sys/class/net/wlan0/carrier";
    if let Ok(v) = std::fs::read_to_string(carrier_path) {
        if v.trim() == "1" {
            return 1;
        }
    }
    0
}
