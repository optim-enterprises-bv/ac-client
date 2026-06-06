//! TR-181 Device.X_OptimACS_FP.* — device fingerprinting.
//!
//! Aggregates signals from six passive sniffers:
//!   dhcp_snoop  — option 12 (hostname), 55 (param list), 60 (vendor class)
//!   mdns_snoop  — Bonjour PTR + TXT (Apple model IDs, HomeKit, Cast, printers)
//!   wifi_snoop  — 802.11 association IEs (Apple/Qualcomm/Broadcom vendor IEs)
//!   ssdp_snoop  — UPnP NOTIFY (Roku, Samsung TV, LG TV, media servers)
//!   lldp_snoop  — LLDP system name + capabilities (IP phones, APs, switches)
//!   nbns_snoop  — NetBIOS Name Service (Windows computer names)
//!
//! Identification priority (highest first):
//!   1. mDNS TXT records    (Apple model ID, HomeKit category, Cast model, …)
//!   2. NBNS computer name  (Windows-only protocol → definitive Windows PC)
//!   3. 802.11 vendor IEs   (Apple IE survives MAC randomisation)
//!   4. SSDP NT: header     (Roku, Samsung TV, LG TV, media server, …)
//!   5. LLDP capabilities   (IP phone, WLAN AP, router, station)
//!   6. DHCP VendorClassID  (Android version, Xbox, PlayStation)
//!   7. Hostname heuristics (iPhone, iPad, Galaxy, Nintendo Switch, …)
//!   8. DHCP option 55      (Apple/Windows/Android OS fingerprint)
//!   9. OUI lookup          (NIC vendor, coarse class)
//!
//! Anomaly detection (cross-signal contradiction checks):
//!   • Apple 802.11 IE present but DHCP VendorClassID is Android/Windows
//!   • mDNS identifies Apple device but DHCP VendorClassID is Android
//!   • NBNS computer name and Apple mDNS both present (impossible OS combo)
//!   • Hostname claims Apple but 802.11 IEs show no Apple vendor element
//!   • Detected vendor/class changed from a previously stable fingerprint
//!   • Active device with no fingerprint signal from any source
//!
//! Anomaly output:
//!   Device.X_OptimACS_FP.Host.{i}.AnomalyFlag   (machine-readable code)
//!   Device.X_OptimACS_FP.Host.{i}.AnomalyDetail (human-readable description)
//!
//! Path structure:
//!   Device.X_OptimACS_FP.HostNumberOfEntries
//!   Device.X_OptimACS_FP.Host.{i}.MACAddress
//!   Device.X_OptimACS_FP.Host.{i}.IPAddress
//!   Device.X_OptimACS_FP.Host.{i}.HostName
//!   Device.X_OptimACS_FP.Host.{i}.VendorClassID
//!   Device.X_OptimACS_FP.Host.{i}.ParamRequestList
//!   Device.X_OptimACS_FP.Host.{i}.Vendor
//!   Device.X_OptimACS_FP.Host.{i}.Class
//!   Device.X_OptimACS_FP.Host.{i}.OS
//!   Device.X_OptimACS_FP.Host.{i}.Model
//!   Device.X_OptimACS_FP.Host.{i}.Active

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use crate::config::ClientConfig;

// ── OUI database ──────────────────────────────────────────────────────────────
//
// Format: (oui_6hex_lowercase, vendor, device_class)
// Sources: optimwifi/feeds/ucentral/ufp/data/*.json + IEEE registry.

const OUI_DB: &[(&str, &str, &str)] = &[
    ("000393", "Apple",         "unknown"),
    ("000c29", "VMware",        "pc"),
    ("0000f0", "Samsung",       "phone"),
    ("001317", "Philips Hue",   "iot"),
    ("001788", "Philips Hue",   "iot"),
    ("001a7d", "Mikrotik",      "router"),
    ("001d0f", "ASUS",          "router"),
    ("002402", "Nintendo",      "gaming"),
    ("00224c", "Nintendo",      "gaming"),
    ("002709", "Nintendo",      "gaming"),
    ("002722", "Ubiquiti",      "router"),
    ("0003e9", "Apple",         "unknown"),
    ("0050f2", "Microsoft",     "pc"),
    ("005056", "VMware",        "pc"),
    ("00904c", "Epigram",       "unknown"),
    ("009ec8", "Xiaomi",        "phone"),
    ("00e0fc", "Huawei",        "phone"),
    ("049226", "ASUS",          "router"),
    ("0c47c9", "Amazon",        "iot"),
    ("0cae7d", "Ring",          "doorbell"),
    ("0e022d", "Apple",         "unknown"),
    ("10bf48", "ASUS",          "router"),
    ("14cc20", "TP-Link",       "router"),
    ("141877", "Dell",          "pc"),
    ("18dbf2", "Dell",          "pc"),
    ("18e829", "TP-Link",       "router"),
    ("1c53f9", "Google",        "iot"),
    ("1cbfc4", "Samsung",       "phone"),
    ("1cf29a", "Google",        "iot"),
    ("20df5b", "Google",        "iot"),
    ("24a43c", "Ubiquiti",      "router"),
    ("286ab8", "Apple",         "unknown"),
    ("28cdc1", "Raspberry Pi",  "iot"),
    ("2c4d54", "ASUS",          "router"),
    ("2cc81b", "Mikrotik",      "router"),
    ("2cfda1", "ASUS",          "router"),
    ("30fd38", "Samsung",       "phone"),
    ("34c3ac", "Samsung",       "phone"),
    ("34ce00", "Xiaomi",        "phone"),
    ("3c22fb", "Apple",         "unknown"),
    ("3c5ab4", "Google",        "iot"),
    ("3c6105", "Espressif",     "iot"),
    ("3c0754", "Apple",         "unknown"),
    ("40cbc0", "Apple",         "unknown"),
    ("44650d", "Amazon",        "iot"),
    ("48a6b8", "Sonos",         "speaker"),
    ("48d705", "Google",        "iot"),
    ("4c5e0c", "Mikrotik",      "router"),
    ("50465d", "ASUS",          "router"),
    ("508f4c", "Xiaomi",        "phone"),
    ("50c7bf", "TP-Link",       "router"),
    ("546009", "Google",        "iot"),
    ("548998", "Huawei",        "phone"),
    ("5415ce", "Google",        "tv"),
    ("5447ca", "Google",        "tv"),
    ("5c475e", "Ring",          "doorbell"),
    ("5caafd", "Sonos",         "speaker"),
    ("600194", "Espressif",     "iot"),
    ("646666", "Xiaomi",        "phone"),
    ("68370e", "Amazon",        "iot"),
    ("68d79a", "Ubiquiti",      "router"),
    ("6c2f2c", "Samsung",       "phone"),
    ("6c5650", "Google",        "tv"),
    ("6c8dc1", "Huawei",        "phone"),
    ("78bdbc", "Samsung",       "phone"),
    ("78e103", "Amazon",        "iot"),
    ("788a20", "Ubiquiti",      "router"),
    ("80002d", "Motorola",      "phone"),
    ("802aa8", "Ubiquiti",      "router"),
    ("88e9fe", "Apple",         "unknown"),
    ("8c5765", "Apple",         "unknown"),
    ("8c7712", "Samsung",       "phone"),
    ("8c8590", "Apple",         "unknown"),
    ("8cbebe", "Xiaomi",        "phone"),
    ("94350a", "Samsung",       "phone"),
    ("94eb2c", "Google",        "iot"),
    ("94f6a3", "Sonos",         "speaker"),
    ("98b658", "Nintendo",      "gaming"),
    ("98f4ab", "Espressif",     "iot"),
    ("9cadf1", "Xiaomi",        "phone"),
    ("a036bc", "Intel",         "laptop"),
    ("a4ae11", "Intel",         "pc"),
    ("a036f8", "Samsung",       "phone"),
    ("a0f3c1", "TP-Link",       "router"),
    ("a047d7", "Apple",         "unknown"),
    ("a451ed", "Apple",         "unknown"),
    ("a45046", "Huawei",        "phone"),
    ("a47293", "Google",        "tv"),
    ("a47733", "Google",        "iot"),
    ("a4c3f0", "Apple",         "unknown"),
    ("a4507a", "Apple",         "unknown"),
    ("a8be27", "Apple",         "unknown"),
    ("a851ab", "Apple",         "unknown"),
    ("a002dc", "Amazon",        "iot"),
    ("a4cf12", "Espressif",     "iot"),
    ("b072bf", "Samsung",       "phone"),
    ("b4ae2b", "Raspberry Pi",  "iot"),
    ("b4e62d", "LG",            "tv"),
    ("b469f4", "Ubiquiti",      "router"),
    ("b827eb", "Raspberry Pi",  "iot"),
    ("b86920", "Ubiquiti",      "router"),
    ("b869f4", "Ubiquiti",      "router"),
    ("b8ca3a", "Dell",          "pc"),
    ("b8ce18", "Mikrotik",      "router"),
    ("b8e937", "Sonos",         "speaker"),
    ("bc9fef", "Apple",         "unknown"),
    ("bcce25", "Nintendo",      "gaming"),
    ("bcddc2", "Espressif",     "iot"),
    ("c025e9", "TP-Link",       "router"),
    ("c080f8", "Apple",         "unknown"),
    ("c8bbc8", "Apple",         "unknown"),
    ("c86000", "Apple",         "unknown"),
    ("cc50e3", "Espressif",     "iot"),
    ("d4ca6d", "Mikrotik",      "router"),
    ("d8522d", "Tuya",          "iot"),
    ("d8f15b", "Tuya",          "iot"),
    ("d89695", "Apple",         "unknown"),
    ("dca632", "Raspberry Pi",  "iot"),
    ("dca904", "Apple",         "unknown"),
    ("e063da", "Ubiquiti",      "router"),
    ("e45f01", "Raspberry Pi",  "iot"),
    ("e4e4ab", "Apple",         "unknown"),
    ("e8508b", "Samsung",       "phone"),
    ("e868e7", "Espressif",     "iot"),
    ("e84ecf", "Nintendo",      "gaming"),
    ("ecb5fa", "Philips Hue",   "iot"),
    ("f01898", "Apple",         "unknown"),
    ("f0b479", "Apple",         "unknown"),
    ("f0dcf8", "Apple",         "unknown"),
    ("f4d488", "Apple",         "unknown"),
    ("f4f15a", "Google",        "iot"),
    ("f4f5db", "Google",        "iot"),
    ("f8db88", "Dell",          "pc"),
    ("f8d111", "TP-Link",       "router"),
    ("fc65de", "Amazon",        "iot"),
    ("fcecda", "Ubiquiti",      "router"),
    ("0017f2", "Apple",         "unknown"),
];

/// Look up (vendor, class) for a MAC address using OUI prefix.
fn oui_lookup(mac: &str) -> Option<(&'static str, &'static str)> {
    let raw: String = mac
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .take(6)
        .collect::<String>()
        .to_lowercase();
    if raw.len() < 6 { return None; }
    OUI_DB
        .iter()
        .find(|&&(oui, _, _)| oui == raw.as_str())
        .map(|&(_, vendor, class)| (vendor, class))
}

// ── VendorClassID matching ────────────────────────────────────────────────────

struct VcMatch {
    vendor: &'static str,
    class:  &'static str,
    os:     Option<&'static str>,
    model:  Option<&'static str>,
}

fn match_vendor_class(vc: &str) -> Option<VcMatch> {
    if let Some(ver) = vc.strip_prefix("android-dhcp-") {
        let os: &'static str = match ver {
            "9"  => "Android 9",
            "10" => "Android 10",
            "11" => "Android 11",
            "12" => "Android 12",
            "13" => "Android 13",
            "14" => "Android 14",
            "15" => "Android 15",
            _    => "Android",
        };
        return Some(VcMatch { vendor: "Google", class: "phone", os: Some(os), model: None });
    }
    match vc {
        "MSFT 5.0 XBOX" => Some(VcMatch { vendor: "Microsoft", class: "gaming", os: None,                 model: Some("Xbox") }),
        "MSFT 5.0"      => Some(VcMatch { vendor: "Microsoft", class: "pc",     os: Some("Windows"),      model: None }),
        "PS5"           => Some(VcMatch { vendor: "Sony",      class: "gaming", os: None,                 model: Some("PlayStation 5") }),
        "PS4"           => Some(VcMatch { vendor: "Sony",      class: "gaming", os: None,                 model: Some("PlayStation 4") }),
        _               => None,
    }
}

// ── DHCP option 55 param-list OS fingerprint ──────────────────────────────────
//
// Option 55 is the Parameter Request List — the ordered set of DHCP options the
// client asks for.  Different OS DHCP stacks have recognisable patterns:
//
//   Apple (macOS/iOS):  always requests 121 (classless-static-routes) + 252 (proxy-PAC)
//   Windows:            requests 44 (NBNS) + 46 (NetBIOS node type)
//   Android:            requests 26 (MTU) + 28 (broadcast), never 121 or 252
//
// Returns (os_hint, vendor_hint).

fn match_param_list(pl: &str) -> Option<(&'static str, &'static str)> {
    let opts: std::collections::HashSet<u32> = pl
        .split(',')
        .filter_map(|s| s.trim().parse::<u32>().ok())
        .collect();
    if opts.is_empty() { return None; }

    // Apple: option 121 (classless static routes) AND 252 (proxy autoconfig)
    if opts.contains(&121) && opts.contains(&252) {
        return Some(("macOS/iOS", "Apple"));
    }
    // Windows: NetBIOS name server (44) AND NetBIOS node type (46)
    if opts.contains(&44) && opts.contains(&46) {
        return Some(("Windows", "Microsoft"));
    }
    // Android: MTU (26) AND broadcast (28), but NOT Apple markers
    if opts.contains(&26) && opts.contains(&28)
        && !opts.contains(&121) && !opts.contains(&252)
    {
        return Some(("Android", "Google"));
    }
    None
}

// ── Hostname heuristics ───────────────────────────────────────────────────────

fn class_from_hostname(name: &str) -> Option<(&'static str, &'static str)> {
    let n = name.to_lowercase();
    if n.contains("iphone")   { return Some(("Apple",     "phone")); }
    if n.contains("ipad")     { return Some(("Apple",     "tablet")); }
    if n.contains("macbook")  { return Some(("Apple",     "laptop")); }
    if n.contains("imac")     { return Some(("Apple",     "pc")); }
    if n.contains("mac-mini") || n.contains("macmini") || n.contains("mac_mini") {
                                return Some(("Apple",     "pc")); }
    if n.contains("appletv")  { return Some(("Apple",     "tv")); }
    if n.contains("galaxy")   { return Some(("Samsung",   "phone")); }
    if n == "samsung"         { return Some(("Samsung",   "phone")); }
    if n.contains("samsung")  { return Some(("Samsung",   "unknown")); }
    if n.contains("android")  { return Some(("Google",    "phone")); }
    if n.contains("chromecast") { return Some(("Google",  "tv")); }
    if n.contains("nest")     { return Some(("Google",    "iot")); }
    if n.contains("nintendo") || n.contains("switch") {
                                return Some(("Nintendo",  "gaming")); }
    if n.contains("playstation") || n.contains("-ps4") || n.contains("-ps5") {
                                return Some(("Sony",      "gaming")); }
    if n.contains("xbox")     { return Some(("Microsoft", "gaming")); }
    if n.contains("echo") || n.contains("kindle") || n.contains("fire-") {
                                return Some(("Amazon",    "iot")); }
    if n.contains("ring")     { return Some(("Ring",      "doorbell")); }
    if n.contains("sonos")    { return Some(("Sonos",     "speaker")); }
    if n.contains("raspberry") || n.starts_with("rpi") {
                                return Some(("Raspberry Pi", "iot")); }
    if n.contains("printer") || n.contains("deskjet") || n.contains("laserjet") || n.contains("officejet") {
                                return Some(("",          "printer")); }
    if n.contains("camera") || n.contains("ipcam") {
                                return Some(("",          "camera")); }
    if n == "asus" || n.starts_with("asus-") {
                                return Some(("ASUS",      "unknown")); }
    None
}

// ── Host collection ───────────────────────────────────────────────────────────

struct HostInfo {
    mac:      String,
    ip:       String,
    hostname: String,
    active:   bool,
}

fn collect_hosts() -> Vec<HostInfo> {
    let mut hosts: HashMap<String, HostInfo> = HashMap::new();

    // DHCP leases
    let leases = std::fs::read_to_string("/tmp/dhcp.leases").unwrap_or_default();
    for line in leases.lines() {
        let p: Vec<&str> = line.split_whitespace().collect();
        if p.len() < 4 { continue; }
        let expiry: u64 = p[0].parse().unwrap_or(0);
        let mac      = p[1].to_uppercase();
        let ip       = p[2].to_string();
        let hostname = if p[3] == "*" { String::new() } else { p[3].to_string() };
        let active = expiry == 0 || {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            expiry > now
        };
        hosts.insert(mac.clone(), HostInfo { mac, ip, hostname, active });
    }

    // ARP table for hosts not in DHCP leases
    if let Ok(arp) = std::fs::read_to_string("/proc/net/arp") {
        for line in arp.lines().skip(1) {
            let p: Vec<&str> = line.split_whitespace().collect();
            if p.len() < 6 { continue; }
            let mac_raw = p[3];
            if mac_raw == "00:00:00:00:00:00" || mac_raw == "<incomplete>" { continue; }
            let mac = mac_raw.to_uppercase();
            if hosts.contains_key(&mac) { continue; }
            let flags: u32 =
                u32::from_str_radix(p[2].trim_start_matches("0x"), 16).unwrap_or(0);
            hosts.insert(mac.clone(), HostInfo {
                mac,
                ip:       p[0].to_string(),
                hostname: String::new(),
                active:   flags & 0x2 != 0,
            });
        }
    }

    // Sort by MAC for stable indexing (avoids stale DB rows when indices shift)
    let mut v: Vec<HostInfo> = hosts.into_values().collect();
    v.sort_by(|a, b| a.mac.cmp(&b.mac));
    v
}

// ── DM GET handler ────────────────────────────────────────────────────────────

pub async fn get(_cfg: &ClientConfig, _path: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    let hosts = collect_hosts();

    // Snapshot all fingerprint tables (avoid holding locks during fingerprinting)
    let snoop_snap: HashMap<String, crate::dhcp_snoop::DhcpFp> =
        crate::dhcp_snoop::table()
            .map(|t| t.lock().unwrap().clone())
            .unwrap_or_default();

    let mdns_snap: HashMap<String, crate::mdns_snoop::MdnsFp> =
        crate::mdns_snoop::table()
            .map(|t| t.lock().unwrap().clone())
            .unwrap_or_default();

    let wifi_snap: HashMap<String, crate::wifi_snoop::WifiFp> =
        crate::wifi_snoop::table()
            .map(|t| t.lock().unwrap().clone())
            .unwrap_or_default();

    let ssdp_snap: HashMap<String, crate::ssdp_snoop::SsdpFp> =
        crate::ssdp_snoop::table()
            .map(|t| t.lock().unwrap().clone())
            .unwrap_or_default();

    let lldp_snap: HashMap<String, crate::lldp_snoop::LldpFp> =
        crate::lldp_snoop::table()
            .map(|t| t.lock().unwrap().clone())
            .unwrap_or_default();

    let nbns_snap: HashMap<String, crate::nbns_snoop::NbnsFp> =
        crate::nbns_snoop::table()
            .map(|t| t.lock().unwrap().clone())
            .unwrap_or_default();

    let mut idx = 1u32;
    for h in &hosts {
        let snoop = snoop_snap.get(&h.mac).cloned().unwrap_or_default();
        let mdns  = mdns_snap.get(&h.mac).cloned().unwrap_or_default();
        let wifi  = wifi_snap.get(&h.mac).cloned().unwrap_or_default();
        let ssdp  = ssdp_snap.get(&h.mac).cloned().unwrap_or_default();
        let lldp  = lldp_snap.get(&h.mac).cloned().unwrap_or_default();
        let nbns  = nbns_snap.get(&h.mac).cloned().unwrap_or_default();

        // Prefer DHCP lease hostname over snooped option 12
        let hostname = if !h.hostname.is_empty() {
            h.hostname.clone()
        } else {
            snoop.hostname.clone().unwrap_or_default()
        };

        let (vendor, class, os, model) = fingerprint(
            &h.mac,
            snoop.vendor_class.as_deref(),
            &hostname,
            snoop.param_list.as_deref(),
            &mdns,
            &wifi,
            &ssdp,
            &lldp,
            &nbns,
        );

        // Anomaly detection before any moves
        let anomaly = detect_anomalies(
            &h.mac,
            snoop.vendor_class.as_deref(),
            snoop.param_list.as_deref(),
            &hostname,
            &mdns,
            &wifi,
            &nbns,
            &vendor,
            &class,
            h.active,
        );

        let base = format!("Device.X_OptimACS_FP.Host.{idx}.");
        m.insert(format!("{base}MACAddress"),    h.mac.clone());
        m.insert(format!("{base}IPAddress"),     h.ip.clone());
        m.insert(format!("{base}Active"),        h.active.to_string());

        if !hostname.is_empty() {
            m.insert(format!("{base}HostName"),  hostname);
        }
        if let Some(vc) = &snoop.vendor_class {
            m.insert(format!("{base}VendorClassID"), vc.clone());
        }
        if let Some(pl) = &snoop.param_list {
            m.insert(format!("{base}ParamRequestList"), pl.clone());
        }
        if !vendor.is_empty() {
            m.insert(format!("{base}Vendor"),    vendor);
        }
        if !class.is_empty() && class != "unknown" {
            m.insert(format!("{base}Class"),     class);
        }
        if !os.is_empty() {
            m.insert(format!("{base}OS"),        os);
        }
        if !model.is_empty() {
            m.insert(format!("{base}Model"),     model);
        }
        if let Some((flag, detail)) = anomaly {
            m.insert(format!("{base}AnomalyFlag"),   flag.to_string());
            m.insert(format!("{base}AnomalyDetail"), detail);
        }

        idx += 1;
    }

    m.insert(
        "Device.X_OptimACS_FP.HostNumberOfEntries".to_string(),
        (idx - 1).to_string(),
    );
    m
}

// ── Core fingerprint logic ────────────────────────────────────────────────────

/// Returns `(vendor, class, os, model)` as owned Strings.
///
/// Priority: mDNS → NBNS → WiFi IEs → SSDP → LLDP → VendorClassID → hostname → option-55 → OUI
#[allow(clippy::too_many_arguments)]
fn fingerprint(
    mac:          &str,
    vendor_class: Option<&str>,
    hostname:     &str,
    param_list:   Option<&str>,
    mdns:         &crate::mdns_snoop::MdnsFp,
    wifi:         &crate::wifi_snoop::WifiFp,
    ssdp:         &crate::ssdp_snoop::SsdpFp,
    lldp:         &crate::lldp_snoop::LldpFp,
    nbns:         &crate::nbns_snoop::NbnsFp,
) -> (String, String, String, String) {

    // 1. mDNS — richest signal: Apple model ID, HomeKit category, Cast model, printer make/model
    if let Some((mdns_vendor, mdns_class, mdns_os, mdns_model)) =
        crate::mdns_snoop::classify_fp(mdns)
    {
        let vendor = if !mdns_vendor.is_empty() {
            mdns_vendor
        } else {
            oui_lookup(mac).map(|(v, _)| v.to_string()).unwrap_or_default()
        };
        return (vendor, mdns_class, mdns_os, mdns_model);
    }

    // 2. NBNS — NetBIOS Name Service is Windows-only: any NBNS packet → Windows PC
    if let Some(nbns_name) = &nbns.computer_name {
        let vendor = oui_lookup(mac)
            .map(|(v, _)| v.to_string())
            .unwrap_or_else(|| "Microsoft".to_string());
        return (vendor, "pc".to_string(), "Windows".to_string(), nbns_name.clone());
    }

    // 3. 802.11 vendor IEs — survives MAC randomisation
    if let Some((wifi_vendor, wifi_class)) = crate::wifi_snoop::classify_fp(wifi) {
        if wifi_vendor == "Apple" {
            // Confirmed Apple; refine class with OUI or other signals
            let oui_class = oui_lookup(mac).map(|(_, c)| c).unwrap_or("unknown");
            let class = if oui_class != "unknown" { oui_class } else { wifi_class };
            return ("Apple".to_string(), class.to_string(), String::new(), String::new());
        }
        if !wifi_class.is_empty() && wifi_class != "unknown" {
            let vendor = oui_lookup(mac)
                .map(|(v, _)| v.to_string())
                .unwrap_or_else(|| wifi_vendor.to_string());
            return (vendor, wifi_class.to_string(), String::new(), String::new());
        }
    }

    // 4. SSDP — UPnP device announcements (Roku, Samsung TV, LG TV, media server, …)
    if let Some((ssdp_vendor, ssdp_class, ssdp_model)) = crate::ssdp_snoop::classify_fp(ssdp) {
        let vendor = if !ssdp_vendor.is_empty() {
            ssdp_vendor
        } else {
            oui_lookup(mac).map(|(v, _)| v.to_string()).unwrap_or_default()
        };
        return (vendor, ssdp_class, String::new(), ssdp_model);
    }

    // 5. LLDP — capabilities bitmap and system description
    if let Some((lldp_vendor, lldp_class)) = crate::lldp_snoop::classify_fp(lldp) {
        let vendor = if !lldp_vendor.is_empty() {
            lldp_vendor
        } else {
            oui_lookup(mac).map(|(v, _)| v.to_string()).unwrap_or_default()
        };
        // Use LLDP system name as model if available
        let model = lldp.system_name.clone().unwrap_or_default();
        return (vendor, lldp_class, String::new(), model);
    }

    // 6. VendorClassID — Android version, Xbox, PlayStation, …
    if let Some(vc) = vendor_class {
        if let Some(m) = match_vendor_class(vc) {
            let oui_vendor = oui_lookup(mac).map(|(v, _)| v).unwrap_or("");
            // OUI refines vendor (e.g. Android + Samsung OUI → Samsung)
            let vendor = if !oui_vendor.is_empty() && oui_vendor != "Google" {
                oui_vendor.to_string()
            } else {
                m.vendor.to_string()
            };
            return (
                vendor,
                m.class.to_string(),
                m.os.unwrap_or("").to_string(),
                m.model.unwrap_or("").to_string(),
            );
        }
    }

    let oui_hit = oui_lookup(mac);

    // 7. Hostname heuristics — DHCP option 12 or DHCP lease name
    if !hostname.is_empty() {
        if let Some((hn_vendor, hn_class)) = class_from_hostname(hostname) {
            let vendor = if !hn_vendor.is_empty() {
                hn_vendor.to_string()
            } else {
                oui_hit.map(|(v, _)| v.to_string()).unwrap_or_default()
            };
            return (vendor, hn_class.to_string(), String::new(), String::new());
        }
    }

    // 8. DHCP option 55 OS fingerprint
    if let Some(pl) = param_list {
        if let Some((os_hint, pl_vendor)) = match_param_list(pl) {
            let vendor = oui_hit
                .map(|(v, _)| v.to_string())
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| pl_vendor.to_string());
            let class = match os_hint {
                "Windows" => "pc",
                "Android" => "phone",
                _         => "unknown", // macOS/iOS: can't distinguish phone vs laptop
            };
            return (vendor, class.to_string(), os_hint.to_string(), String::new());
        }
    }

    // 9. OUI only
    if let Some((vendor, class)) = oui_hit {
        return (vendor.to_string(), class.to_string(), String::new(), String::new());
    }

    (String::new(), String::new(), String::new(), String::new())
}

// ── Anomaly detection ─────────────────────────────────────────────────────────

/// Per-MAC record of the last stable fingerprint, used for change detection.
struct PrevFp {
    vendor: String,
    class:  String,
    seen:   Instant,
}

/// Process-wide table of previously seen fingerprints (MAC → PrevFp).
static PREV_FP: OnceLock<Mutex<HashMap<String, PrevFp>>> = OnceLock::new();

fn prev_fp_table() -> &'static Mutex<HashMap<String, PrevFp>> {
    PREV_FP.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Cross-signal anomaly detection.
///
/// Returns `Some((flag, detail))` for the highest-priority anomaly found,
/// or `None` if all signals are consistent.
///
/// Checks (in priority order):
///   1. Apple 802.11 IE + Android DHCP VendorClassID — physically impossible
///   2. Apple 802.11 IE + Windows DHCP option 55    — physically impossible
///   3. Apple mDNS model  + Android DHCP VendorClassID — OS contradiction
///   4. NBNS name present + Apple mDNS model         — OS contradiction
///   5. Hostname claims Apple + WiFi IEs present + no Apple IE — hostname spoof
///   6. Vendor changed from previously stable fingerprint
///   7. Active device with no fingerprint signal at all
#[allow(clippy::too_many_arguments)]
fn detect_anomalies(
    mac:             &str,
    vendor_class:    Option<&str>,
    param_list:      Option<&str>,
    hostname:        &str,
    mdns:            &crate::mdns_snoop::MdnsFp,
    wifi:            &crate::wifi_snoop::WifiFp,
    nbns:            &crate::nbns_snoop::NbnsFp,
    detected_vendor: &str,
    detected_class:  &str,
    active:          bool,
) -> Option<(&'static str, String)> {

    // ── 1. Apple 802.11 vendor IE + Android DHCP VendorClassID ───────────────
    // The Apple vendor IE (OUI 00:17:f2) is injected by the Apple Wi-Fi stack at
    // the hardware level.  An Android DHCP client cannot appear alongside it.
    if wifi.has_apple_ie {
        if let Some(vc) = vendor_class {
            if vc.starts_with("android-dhcp-") {
                return Some(("apple_ie_android_dhcp", format!(
                    "Apple 802.11 vendor IE present but DHCP VendorClassID={vc}"
                )));
            }
        }

        // ── 2. Apple 802.11 vendor IE + Windows DHCP option 55 ───────────────
        // Windows DHCP stack requests options 44 (NBNS) + 46 (node type) and
        // never requests 121 (classless static routes) + 252 (proxy autoconfig),
        // which Apple always requests.
        if let Some(pl) = param_list {
            let opts: std::collections::HashSet<u32> = pl
                .split(',')
                .filter_map(|s| s.trim().parse::<u32>().ok())
                .collect();
            if opts.contains(&44) && opts.contains(&46) && !opts.contains(&121) {
                return Some(("apple_ie_windows_dhcp",
                    "Apple 802.11 vendor IE present but DHCP option 55 matches Windows".into()
                ));
            }
        }
    }

    // ── 3. Apple mDNS model + Android DHCP VendorClassID ─────────────────────
    // Bonjour (mDNS) `_airplay._tcp` TXT `model=iPhoneXX,X` is set by iOS.
    // android-dhcp-* is set by the Android DHCP client.  Mutually exclusive.
    if let Some(apple_model) = &mdns.apple_model {
        if let Some(vc) = vendor_class {
            if vc.starts_with("android-dhcp-") {
                return Some(("os_contradiction", format!(
                    "mDNS identifies Apple ({apple_model}) but DHCP VendorClassID={vc}"
                )));
            }
        }
    }

    // ── 4. NBNS computer name + Apple mDNS ───────────────────────────────────
    // NetBIOS Name Service is a Windows-only protocol.  Apple mDNS model IDs
    // come from iOS/macOS only.  No device can run both.
    if let (Some(nbns_name), Some(apple_model)) =
        (&nbns.computer_name, &mdns.apple_model)
    {
        return Some(("os_contradiction", format!(
            "NBNS ({nbns_name}) and Apple mDNS ({apple_model}) both present — \
             impossible OS combination"
        )));
    }

    // ── 5. Hostname claims Apple but 802.11 IEs contradict ───────────────────
    // Only meaningful when we actually have IE data for this device (i.e., the
    // device is on WiFi and hostapd has returned IEs for it).  A wired device
    // will have no IEs and must not be flagged here.
    let hn_lower = hostname.to_lowercase();
    let hostname_claims_apple = hn_lower.contains("iphone")
        || hn_lower.contains("ipad")
        || hn_lower.contains("macbook")
        || hn_lower.contains("airpods");
    let has_any_ie = wifi.has_ht || wifi.has_vht || wifi.has_he;
    if hostname_claims_apple && has_any_ie && !wifi.has_apple_ie {
        return Some(("hostname_spoof", format!(
            "Hostname '{hostname}' implies Apple but 802.11 IEs contain no Apple vendor element"
        )));
    }

    // ── 6. Fingerprint vendor changed from previously stable value ────────────
    // Protects against: device replacement without IP change, MAC spoofing,
    // or an attacker cloning a known-good device's MAC.
    // Grace window: 120s after first sight (allows all sniffers to populate).
    {
        let mut tbl = prev_fp_table().lock().unwrap();
        let now = Instant::now();
        if !detected_vendor.is_empty() && !detected_class.is_empty() {
            if let Some(prev) = tbl.get(mac) {
                let stable = now.duration_since(prev.seen).as_secs() > 120;
                if stable
                    && !prev.vendor.is_empty()
                    && prev.vendor != detected_vendor
                {
                    let detail = format!(
                        "Was {}/{}, now {detected_vendor}/{detected_class}",
                        prev.vendor, prev.class,
                    );
                    // Update stored fingerprint to new value
                    tbl.insert(mac.to_string(), PrevFp {
                        vendor: detected_vendor.to_string(),
                        class:  detected_class.to_string(),
                        seen:   now,
                    });
                    return Some(("fp_changed", detail));
                }
            } else {
                tbl.insert(mac.to_string(), PrevFp {
                    vendor: detected_vendor.to_string(),
                    class:  detected_class.to_string(),
                    seen:   now,
                });
            }
        }
    }

    // ── 7. Active device with no signal from any source ───────────────────────
    if active && detected_vendor.is_empty() && detected_class.is_empty() {
        return Some(("unidentified",
            "Active device produces no fingerprint signal from any source".into()
        ));
    }

    None
}
