//! mDNS/Bonjour passive sniffer — service-type + TXT record fingerprinting.
//!
//! Listens on the LAN bridge for mDNS responses (UDP 224.0.0.251:5353) and
//! extracts both PTR records (service types) and TXT records (device metadata).
//!
//! TXT records are where the real intelligence lives:
//!   _airplay._tcp / _companion-link._tcp  → `model=iPhone15,2` (Apple model ID)
//!   _googlecast._tcp                       → `model=Chromecast HD`, `fn=Living Room TV`
//!   _hap._tcp / _hap._udp                 → `ci=9` (HomeKit category), `md=...`
//!   _ipp._tcp                              → `usb_MFG=HP`, `usb_MDL=LaserJet Pro`
//!   _hue._tcp                              → Philips Hue
//!   _fbox._tcp / _avmnexus._tcp            → AVM FRITZ!Box

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use log::{debug, warn};

// ── Public types ──────────────────────────────────────────────────────────────

/// Per-MAC mDNS fingerprint, accumulated from PTR + TXT records.
#[derive(Debug, Clone, Default)]
pub struct MdnsFp {
    /// Bonjour service types observed (e.g. `["_airplay._tcp", "_raop._tcp"]`).
    pub services: Vec<String>,
    /// Apple internal model identifier from TXT `model`/`rpMd`/`am` fields.
    /// Examples: `"iPhone15,2"`, `"MacBookAir10,1"`, `"AppleTV14,1"`.
    pub apple_model: Option<String>,
    /// Google Cast model string (e.g. `"Chromecast HD"`, `"Google Nest Hub"`).
    pub cast_model: Option<String>,
    /// Google Cast friendly name set by the user (e.g. `"Living Room TV"`).
    pub cast_name: Option<String>,
    /// HomeKit accessory category ID from TXT `ci` field.
    /// See `homekit_category_class()` for the mapping.
    pub homekit_category: Option<String>,
    /// HomeKit accessory model string from TXT `md` field.
    pub homekit_model: Option<String>,
    /// Printer/scanner vendor from TXT `usb_MFG` / `mfg` field.
    pub printer_vendor: Option<String>,
    /// Printer/scanner model from TXT `usb_MDL` / `mdl` / `ty` field.
    pub printer_model: Option<String>,
    /// Fixed vendor override from a specific service (Philips Hue, FRITZ!Box…).
    pub fixed_vendor: Option<String>,
    /// Fixed device/model override from a specific service.
    pub fixed_device: Option<String>,
}

type MdnsTable = Arc<Mutex<HashMap<String, MdnsFp>>>;
static TABLE: OnceLock<MdnsTable> = OnceLock::new();

/// Initialise the global mDNS table (idempotent).
pub fn init() {
    TABLE.get_or_init(|| Arc::new(Mutex::new(HashMap::new())));
}

pub fn table() -> Option<&'static MdnsTable> {
    TABLE.get()
}


/// Spawn the background sniffer thread on `iface` (e.g. `"br-lan"`).
pub fn spawn(iface: &str) {
    let iface = iface.to_string();
    std::thread::spawn(move || {
        if let Err(e) = run_sniffer(&iface) {
            warn!("mDNS sniffer on {iface} exited: {e}");
        }
    });
}

// ── Classification ────────────────────────────────────────────────────────────

/// Map a HomeKit category ID (`ci` TXT field) to a device class string.
pub fn homekit_category_class(ci: &str) -> &'static str {
    match ci {
        "5"  => "iot",      // Lightbulb
        "6"  => "iot",      // Door Lock
        "7"  => "iot",      // Outlet / Smart Plug
        "8"  => "iot",      // Switch
        "9"  => "iot",      // Thermostat
        "10" => "iot",      // Sensor
        "11" => "iot",      // Security System
        "17" => "camera",   // IP Camera
        "18" => "camera",   // Video Doorbell
        "31" => "tv",       // Television
        "33" => "router",   // WiFi Router
        "34" => "speaker",  // Audio Receiver
        "35" => "tv",       // TV Set Top Box
        "36" => "tv",       // TV Stick
        _    => "iot",
    }
}

/// Apple model ID prefix → (device_class, human_name_prefix).
/// Used when the exact model is not in the lookup table.
pub fn apple_model_class(model_id: &str) -> (&'static str, &'static str) {
    if model_id.starts_with("iPhone")            { return ("phone",   "iPhone"); }
    if model_id.starts_with("iPad")              { return ("tablet",  "iPad"); }
    if model_id.starts_with("MacBookAir")        { return ("laptop",  "MacBook Air"); }
    if model_id.starts_with("MacBookPro")        { return ("laptop",  "MacBook Pro"); }
    if model_id.starts_with("MacBook")           { return ("laptop",  "MacBook"); }
    if model_id.starts_with("iMac")              { return ("pc",      "iMac"); }
    if model_id.starts_with("MacPro")            { return ("pc",      "Mac Pro"); }
    if model_id.starts_with("Mac")               { return ("pc",      "Mac"); }
    if model_id.starts_with("AppleTV")           { return ("tv",      "Apple TV"); }
    if model_id.starts_with("AudioAccessory")    { return ("speaker", "HomePod"); }
    if model_id.starts_with("Watch")             { return ("watch",   "Apple Watch"); }
    ("unknown", "Apple device")
}

/// Exact Apple model ID → human-readable device name.
/// Sourced from optimwifi/feeds/ucentral/ufp/data/apple.json.
pub fn apple_model_name(model_id: &str) -> Option<&'static str> {
    // (model_id, human_name)
    const MODELS: &[(&str, &str)] = &[
        // iPhone 13 family
        ("iPhone14,4",     "iPhone 13 mini"),
        ("iPhone14,5",     "iPhone 13"),
        ("iPhone14,2",     "iPhone 13 Pro"),
        ("iPhone14,3",     "iPhone 13 Pro Max"),
        // iPhone 14 family
        ("iPhone14,7",     "iPhone 14"),
        ("iPhone14,8",     "iPhone 14 Plus"),
        ("iPhone15,2",     "iPhone 14 Pro"),
        ("iPhone15,3",     "iPhone 14 Pro Max"),
        // iPhone 15 family
        ("iPhone15,4",     "iPhone 15"),
        ("iPhone15,5",     "iPhone 15 Plus"),
        ("iPhone16,1",     "iPhone 15 Pro"),
        ("iPhone16,2",     "iPhone 15 Pro Max"),
        // iPad (standard)
        ("iPad12,1",       "iPad (9th gen)"),
        ("iPad12,2",       "iPad (9th gen)"),
        ("iPad13,18",      "iPad (10th gen)"),
        ("iPad13,19",      "iPad (10th gen)"),
        // iPad Air
        ("iPad13,1",       "iPad Air"),
        ("iPad13,2",       "iPad Air"),
        ("iPad13,16",      "iPad Air"),
        ("iPad13,17",      "iPad Air"),
        // iPad mini
        ("iPad11,1",       "iPad mini"),
        ("iPad11,2",       "iPad mini"),
        ("iPad14,1",       "iPad mini"),
        ("iPad14,2",       "iPad mini"),
        // iPad Pro
        ("iPad13,4",       "iPad Pro"),
        ("iPad13,5",       "iPad Pro"),
        ("iPad13,6",       "iPad Pro"),
        ("iPad13,7",       "iPad Pro"),
        ("iPad13,8",       "iPad Pro"),
        ("iPad13,9",       "iPad Pro"),
        ("iPad13,10",      "iPad Pro"),
        ("iPad13,11",      "iPad Pro"),
        // MacBook Air
        ("MacBookAir10,1", "MacBook Air M1"),
        ("Mac14,2",        "MacBook Air M2"),
        ("Mac15,12",       "MacBook Air M3"),
        // MacBook Pro
        ("MacBookPro17,1", "MacBook Pro M1"),
        ("MacBookPro18,1", "MacBook Pro M1 Pro"),
        ("MacBookPro18,2", "MacBook Pro M1 Max"),
        ("MacBookPro18,3", "MacBook Pro M1 Pro"),
        ("MacBookPro18,4", "MacBook Pro M1 Max"),
        ("Mac14,7",        "MacBook Pro M2"),
        ("Mac14,9",        "MacBook Pro M2 Pro"),
        ("Mac14,10",       "MacBook Pro M2 Pro"),
        // Mac (desktop)
        ("Mac13,1",        "Mac Studio M1"),
        ("Mac13,2",        "Mac Studio M1 Max"),
        ("iMac21,1",       "iMac M1"),
        ("iMac21,2",       "iMac M1"),
        // Apple TV
        ("AppleTV5,3",     "Apple TV HD"),
        ("AppleTV6,2",     "Apple TV 4K"),
        ("AppleTV11,1",    "Apple TV 4K"),
        ("AppleTV14,1",    "Apple TV 4K"),
        // HomePod
        ("AudioAccessory1,1", "HomePod"),
        ("AudioAccessory1,2", "HomePod"),
        ("AudioAccessory5,1", "HomePod mini"),
        ("AudioAccessory6,1", "HomePod (2nd gen)"),
        // Apple Watch
        ("Watch6,18",      "Apple Watch Ultra"),
        ("Watch6,16",      "Apple Watch Series 8"),
        ("Watch6,17",      "Apple Watch Series 8"),
        ("Watch7,1",       "Apple Watch Series 9"),
        ("Watch7,2",       "Apple Watch Series 9"),
        ("Watch7,5",       "Apple Watch Ultra 2"),
    ];
    MODELS.iter().find(|&&(id, _)| id == model_id).map(|&(_, name)| name)
}

/// Classify a full `MdnsFp` into `(vendor, class, os, model)`.
/// Returns `None` if no useful signal is present.
pub fn classify_fp(fp: &MdnsFp) -> Option<(String, String, String, String)> {
    // 1. Apple model ID — most specific signal (gives exact class + model name)
    if let Some(model_id) = &fp.apple_model {
        let name = apple_model_name(model_id)
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                let (_, prefix) = apple_model_class(model_id);
                prefix.to_string()
            });
        let (class, _) = apple_model_class(model_id);
        return Some(("Apple".to_string(), class.to_string(), String::new(), name));
    }

    // 2. HomeKit category — identifies specific IoT/smart home device type
    if let Some(ci) = &fp.homekit_category {
        let class = homekit_category_class(ci);
        let model = fp.homekit_model.clone().unwrap_or_default();
        return Some(("Apple".to_string(), class.to_string(), String::new(), model));
    }

    // 3. Google Cast — identified by TXT model or name
    if let Some(cast_model) = &fp.cast_model {
        let display = if let Some(name) = &fp.cast_name {
            name.clone()
        } else {
            cast_model.clone()
        };
        return Some(("Google".to_string(), "tv".to_string(), String::new(), display));
    }
    if let Some(cast_name) = &fp.cast_name {
        return Some(("Google".to_string(), "tv".to_string(), String::new(), cast_name.clone()));
    }

    // 4. Printer / scanner
    if fp.printer_vendor.is_some() || fp.printer_model.is_some() {
        let vendor = fp.printer_vendor.clone().unwrap_or_default();
        let model  = fp.printer_model.clone().unwrap_or_default();
        return Some((vendor, "printer".to_string(), String::new(), model));
    }

    // 5. Fixed vendor/device (Philips Hue, FRITZ!Box, …)
    if let Some(vendor) = &fp.fixed_vendor {
        let device = fp.fixed_device.clone().unwrap_or_default();
        return Some((vendor.clone(), "iot".to_string(), String::new(), device));
    }

    // 6. Fall back to service-type classification
    classify_services(&fp.services)
        .map(|(v, c, m)| (v.to_string(), c.to_string(), String::new(), m.to_string()))
}

/// Coarse classification from service type list alone (no TXT data).
pub fn classify_services(services: &[String]) -> Option<(&'static str, &'static str, &'static str)> {
    for svc in services {
        match svc.as_str() {
            "_googlecast._tcp"      => return Some(("Google",    "tv",      "Chromecast")),
            "_airplay._tcp"         => return Some(("Apple",     "tv",      "")),
            "_apple-mobdev2._tcp"   => return Some(("Apple",     "phone",   "")),
            "_raop._tcp"            => return Some(("Apple",     "speaker", "")),
            "_sonos._tcp"           => return Some(("Sonos",     "speaker", "")),
            "_spotify-connect._tcp" => return Some(("",          "speaker", "")),
            "_hap._tcp"             => return Some(("Apple",     "iot",     "")),
            "_homekit._tcp"         => return Some(("Apple",     "iot",     "")),
            "_xbox._tcp"            => return Some(("Microsoft", "gaming",  "Xbox")),
            "_psn._tcp"             => return Some(("Sony",      "gaming",  "")),
            "_printer._tcp"         => return Some(("",          "printer", "")),
            "_pdl-datastream._tcp"  => return Some(("",          "printer", "")),
            "_ipp._tcp"             => return Some(("",          "printer", "")),
            "_ipps._tcp"            => return Some(("",          "printer", "")),
            "_afpovertcp._tcp"      => return Some(("Apple",     "pc",      "")),
            "_smb._tcp"             => return Some(("",          "pc",      "")),
            "_ssh._tcp"             => return Some(("",          "pc",      "")),
            _ => {}
        }
    }
    None
}

// ── Raw-socket sniffer ────────────────────────────────────────────────────────

fn run_sniffer(iface: &str) -> std::io::Result<()> {
    let proto_be = (libc::ETH_P_IP as u16).to_be() as i32;
    let fd = unsafe { libc::socket(libc::AF_PACKET, libc::SOCK_RAW, proto_be) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }

    let iface_idx = match iface_index(iface) {
        Ok(i)  => i,
        Err(e) => { unsafe { libc::close(fd); } return Err(e); }
    };

    let mut sll: libc::sockaddr_ll = unsafe { std::mem::zeroed() };
    sll.sll_family   = libc::AF_PACKET as u16;
    sll.sll_protocol = proto_be as u16;
    sll.sll_ifindex  = iface_idx as i32;

    let rc = unsafe {
        libc::bind(
            fd,
            &sll as *const libc::sockaddr_ll as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_ll>() as u32,
        )
    };
    if rc < 0 {
        let e = std::io::Error::last_os_error();
        unsafe { libc::close(fd); }
        return Err(e);
    }

    debug!("mDNS sniffer listening on {iface} (fd={fd})");
    let mut buf = vec![0u8; 65536];
    loop {
        let n = unsafe {
            libc::recv(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0)
        };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted { continue; }
            unsafe { libc::close(fd); }
            return Err(e);
        }
        if n > 0 {
            process_packet(&buf[..n as usize]);
        }
    }
}

fn iface_index(name: &str) -> std::io::Result<u32> {
    let c = std::ffi::CString::new(name).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "non-NUL iface name required")
    })?;
    let idx = unsafe { libc::if_nametoindex(c.as_ptr()) };
    if idx == 0 { Err(std::io::Error::last_os_error()) } else { Ok(idx) }
}

// ── Packet parsing ────────────────────────────────────────────────────────────

fn process_packet(pkt: &[u8]) {
    let _ = try_process(pkt);
}

fn try_process(pkt: &[u8]) -> Option<()> {
    // ── Ethernet ──
    if pkt.len() < 14 { return None; }
    if u16::from_be_bytes([pkt[12], pkt[13]]) != 0x0800 { return None; }

    let src_mac = format!(
        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        pkt[6], pkt[7], pkt[8], pkt[9], pkt[10], pkt[11]
    );

    // ── IPv4 ──
    let ip = pkt.get(14..)?;
    if ip.len() < 20 { return None; }
    let ihl   = ((ip[0] & 0x0f) as usize) * 4;
    let proto = ip[9];
    if proto != 17 { return None; }
    if ip.get(16..20)? != [224, 0, 0, 251] { return None; }   // mDNS multicast only

    // ── UDP ──
    let udp = ip.get(ihl..)?;
    if udp.len() < 8 { return None; }
    if u16::from_be_bytes([udp[0], udp[1]]) != 5353 { return None; }
    if u16::from_be_bytes([udp[2], udp[3]]) != 5353 { return None; }

    // ── DNS ──
    let dns = udp.get(8..)?;
    if dns.len() < 12 { return None; }
    // Must be a response
    if u16::from_be_bytes([dns[2], dns[3]]) & 0x8000 == 0 { return None; }

    let qdcount = u16::from_be_bytes([dns[4],  dns[5]])  as usize;
    let ancount = u16::from_be_bytes([dns[6],  dns[7]])  as usize;
    let nscount = u16::from_be_bytes([dns[8],  dns[9]])  as usize;
    let arcount = u16::from_be_bytes([dns[10], dns[11]]) as usize;

    // Skip question section
    let mut pos = 12usize;
    for _ in 0..qdcount {
        pos = skip_name(dns, pos)?;
        pos = pos.checked_add(4)?;
        if pos > dns.len() { return None; }
    }

    // Collect all RRs, then apply service handlers
    let total = ancount + nscount + arcount;
    // service_type → list of TXT record key-value maps seen for that service
    let mut txt_by_service: HashMap<String, Vec<HashMap<String, String>>> = HashMap::new();
    // Service types from PTR records (for service-list tracking)
    let mut ptr_services: Vec<String> = Vec::new();

    for _ in 0..total {
        let (owner, new_pos) = parse_name(dns, pos)?;
        pos = new_pos;
        if pos + 10 > dns.len() { return None; }

        let rr_type = u16::from_be_bytes([dns[pos],     dns[pos + 1]]);
        let rdlen   = u16::from_be_bytes([dns[pos + 8], dns[pos + 9]]) as usize;
        pos += 10;
        if pos + rdlen > dns.len() { return None; }

        match rr_type {
            12 => {
                // PTR: owner = service type, RDATA = instance name
                if let Some(svc) = extract_service_type(&owner) {
                    if !ptr_services.contains(&svc) {
                        ptr_services.push(svc);
                    }
                }
            }
            16 => {
                // TXT: owner = instance name (contains service type in it)
                if let Some(svc) = extract_service_type(&owner) {
                    let txt = parse_txt_rdata(&dns[pos..pos + rdlen]);
                    if !txt.is_empty() {
                        txt_by_service.entry(svc).or_default().push(txt);
                    }
                }
            }
            _ => {}
        }

        pos += rdlen;
    }

    if ptr_services.is_empty() && txt_by_service.is_empty() {
        return Some(());
    }

    // Apply all collected data to the global table
    if let Some(t) = TABLE.get() {
        if let Ok(mut lock) = t.lock() {
            let entry = lock.entry(src_mac.clone()).or_default();

            // Record new service types from PTR records
            for svc in &ptr_services {
                if !entry.services.contains(svc) {
                    debug!("mDNS {src_mac} +{svc}");
                    entry.services.push(svc.clone());
                }
            }

            // Apply TXT data per service type
            for (svc, txt_list) in &txt_by_service {
                for txt in txt_list {
                    apply_txt(entry, svc, txt, &src_mac);
                }
            }
        }
    }

    Some(())
}

/// Apply TXT record data to `MdnsFp` based on the service type.
fn apply_txt(fp: &mut MdnsFp, service: &str, txt: &HashMap<String, String>, mac: &str) {
    match service {
        // Apple AirPlay / Companion Link — TXT has `model`, `rpMd`, or `am`
        "_airplay._tcp" | "_companion-link._tcp" => {
            let model = txt.get("model")
                .or_else(|| txt.get("rpMd"))
                .or_else(|| txt.get("am"));
            if let Some(m) = model {
                if fp.apple_model.is_none() {
                    debug!("mDNS {mac} apple_model={m}");
                    fp.apple_model = Some(m.clone());
                }
            }
        }

        // AirTunes / AirPlay audio (HomePod, speakers) — also has `model`
        "_raop._tcp" => {
            let model = txt.get("model").or_else(|| txt.get("am"));
            if let Some(m) = model {
                if fp.apple_model.is_none() {
                    debug!("mDNS {mac} apple_model(raop)={m}");
                    fp.apple_model = Some(m.clone());
                }
            }
        }

        // Google Cast — `model` is device model, `fn` is user-set name
        "_googlecast._tcp" => {
            let model = txt.get("model").or_else(|| txt.get("md"));
            if let Some(m) = model {
                if fp.cast_model.is_none() {
                    debug!("mDNS {mac} cast_model={m}");
                    fp.cast_model = Some(m.clone());
                }
            }
            if let Some(name) = txt.get("fn") {
                if fp.cast_name.is_none() {
                    debug!("mDNS {mac} cast_name={name}");
                    fp.cast_name = Some(name.clone());
                }
            }
        }

        // HomeKit — `ci` = category ID, `md` = model string
        "_hap._tcp" | "_hap._udp" => {
            if let Some(ci) = txt.get("ci") {
                if fp.homekit_category.is_none() {
                    debug!("mDNS {mac} homekit_ci={ci}");
                    fp.homekit_category = Some(ci.clone());
                }
            }
            if let Some(md) = txt.get("md") {
                if fp.homekit_model.is_none() {
                    fp.homekit_model = Some(md.clone());
                }
            }
        }

        // Printers via IPP / PDL
        "_ipp._tcp" | "_ipps._tcp" | "_printer._tcp" | "_pdl-datastream._tcp" => {
            if let Some(vendor) = txt.get("usb_MFG") {
                fp.printer_vendor.get_or_insert(vendor.clone());
            }
            let model = txt.get("usb_MDL").or_else(|| txt.get("ty"));
            if let Some(m) = model {
                fp.printer_model.get_or_insert(m.clone());
            }
        }

        // Scanners
        "_scanner._tcp" => {
            if let Some(vendor) = txt.get("mfg") {
                fp.printer_vendor.get_or_insert(vendor.clone());
            }
            let model = txt.get("mdl").or_else(|| txt.get("ty"));
            if let Some(m) = model {
                fp.printer_model.get_or_insert(m.clone());
            }
        }

        // Philips Hue bridge
        "_hue._tcp" => {
            fp.fixed_vendor.get_or_insert("Philips".to_string());
            fp.fixed_device.get_or_insert("Hue".to_string());
        }

        // AVM FRITZ!Box
        "_fbox._tcp" | "_avmnexus._tcp" => {
            fp.fixed_vendor.get_or_insert("AVM".to_string());
            fp.fixed_device.get_or_insert("FRITZ!Box".to_string());
        }

        _ => {}
    }
}

// ── DNS parsing helpers ───────────────────────────────────────────────────────

fn skip_name(pkt: &[u8], mut pos: usize) -> Option<usize> {
    loop {
        let &b = pkt.get(pos)?;
        if b == 0             { return Some(pos + 1); }
        if b & 0xc0 == 0xc0  { return Some(pos + 2); }
        pos = pos.checked_add(1 + b as usize)?;
    }
}

fn parse_name(pkt: &[u8], start: usize) -> Option<(String, usize)> {
    let mut labels: Vec<String> = Vec::new();
    let mut pos        = start;
    let mut consumed   = None::<usize>;
    let mut ptr_hops   = 0u8;

    loop {
        let &b = pkt.get(pos)?;
        if b == 0 {
            consumed.get_or_insert(pos + 1);
            break;
        }
        if b & 0xc0 == 0xc0 {
            consumed.get_or_insert(pos + 2);
            ptr_hops += 1;
            if ptr_hops > 4 { return None; }
            let hi = (b & 0x3f) as usize;
            let lo = *pkt.get(pos + 1)? as usize;
            pos = (hi << 8) | lo;
            continue;
        }
        pos += 1;
        let end = pos.checked_add(b as usize)?;
        let label = std::str::from_utf8(pkt.get(pos..end)?).ok()?.to_string();
        labels.push(label);
        pos = end;
    }

    Some((labels.join("."), consumed?))
}

/// Parse TXT RDATA into a key→value map.
/// Each entry is a length-prefixed string, typically `"key=value"`.
fn parse_txt_rdata(data: &[u8]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut pos = 0;
    while pos < data.len() {
        let len = data[pos] as usize;
        pos += 1;
        if len == 0 { continue; }
        if pos + len > data.len() { break; }
        if let Ok(s) = std::str::from_utf8(&data[pos..pos + len]) {
            if let Some((k, v)) = s.split_once('=') {
                map.insert(k.to_string(), v.to_string());
            }
        }
        pos += len;
    }
    map
}

/// Extract `_service._proto` from a DNS owner name.
/// Works for both PTR owners (`_airplay._tcp.local`) and
/// TXT owners (`MyDevice._airplay._tcp.local`).
fn extract_service_type(name: &str) -> Option<String> {
    let n = name.to_lowercase();
    let n = n.strip_suffix(".local").unwrap_or(&n);

    for proto in ["._tcp", "._udp"] {
        if let Some(idx) = n.find(proto) {
            let before = &n[..idx];
            let svc = before.rsplit('.').next().unwrap_or(before);
            if svc.starts_with('_') {
                return Some(format!("{svc}{proto}"));
            }
        }
    }
    None
}
