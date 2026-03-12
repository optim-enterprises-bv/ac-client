//! ONVIF WS-Discovery for automatic camera detection.
//!
//! Sends WS-Discovery Probe multicast messages on the local network and
//! parses ONVIF device responses to extract RTSP stream URLs and device
//! metadata. Discovered cameras can be auto-added to the UCI config.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::time::Duration;

use log::{debug, info, warn};

/// Multicast address and port for WS-Discovery (ONVIF uses this).
const WS_DISCOVERY_ADDR: &str = "239.255.255.250:3702";

/// A discovered ONVIF device.
#[derive(Debug, Clone)]
pub struct OnvifDevice {
    /// Device service URL (XAddr).
    pub xaddr: String,
    /// Device IP address.
    pub ip: String,
    /// Device manufacturer (if available from probe response).
    pub manufacturer: Option<String>,
    /// Device model (if available).
    pub model: Option<String>,
}

/// Run a single ONVIF WS-Discovery probe and return discovered devices.
///
/// This sends a SOAP Probe message to the WS-Discovery multicast group and
/// collects responses for `timeout` duration.
pub fn discover(timeout: Duration) -> Vec<OnvifDevice> {
    let socket = match UdpSocket::bind("0.0.0.0:0") {
        Ok(s) => s,
        Err(e) => {
            warn!("Cannot bind UDP socket for ONVIF discovery: {e}");
            return Vec::new();
        }
    };

    if let Err(e) = socket.set_read_timeout(Some(Duration::from_secs(2))) {
        warn!("Cannot set socket timeout: {e}");
    }

    // Join multicast group
    let multicast_addr: Ipv4Addr = "239.255.255.250".parse().unwrap();
    if let Err(e) = socket.join_multicast_v4(&multicast_addr, &Ipv4Addr::UNSPECIFIED) {
        debug!("Multicast join failed (may still work): {e}");
    }

    // Send WS-Discovery Probe
    let probe = build_probe_message();
    let dest: SocketAddr = WS_DISCOVERY_ADDR.parse().unwrap();

    if let Err(e) = socket.send_to(probe.as_bytes(), dest) {
        warn!("Failed to send WS-Discovery probe: {e}");
        return Vec::new();
    }

    info!("ONVIF: sent WS-Discovery probe to {}, waiting {}s for responses...",
        WS_DISCOVERY_ADDR, timeout.as_secs());

    let mut devices = Vec::new();
    let deadline = std::time::Instant::now() + timeout;

    let mut buf = [0u8; 8192];

    while std::time::Instant::now() < deadline {
        match socket.recv_from(&mut buf) {
            Ok((len, src)) => {
                let response = String::from_utf8_lossy(&buf[..len]);
                debug!("WS-Discovery response from {src}");

                if let Some(device) = parse_probe_response(&response, &src.ip().to_string()) {
                    // Deduplicate by XAddr
                    if !devices.iter().any(|d: &OnvifDevice| d.xaddr == device.xaddr) {
                        info!("Discovered ONVIF device: {} at {}", device.xaddr, device.ip);
                        devices.push(device);
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // Timeout on this recv, loop and check deadline
                continue;
            }
            Err(e) => {
                debug!("WS-Discovery recv error: {e}");
                break;
            }
        }
    }

    info!("ONVIF discovery found {} device(s)", devices.len());
    devices
}

/// Periodic discovery task — standalone version (without camera cross-referencing).
///
/// Prefer `CameraManager::start_discovery_loop()` which cross-references
/// discovered devices against configured cameras. This function is kept for
/// standalone/HTTP endpoint use.
pub async fn discovery_loop(interval: Duration) {
    let probe_timeout = Duration::from_secs(5);

    loop {
        info!("Running ONVIF discovery scan...");

        // Run blocking discovery in a thread
        let devices = tokio::task::spawn_blocking(move || discover(probe_timeout))
            .await
            .unwrap_or_default();

        if devices.is_empty() {
            info!("ONVIF scan complete: no devices found on network");
        } else {
            info!("ONVIF scan complete: {} device(s) found", devices.len());
            for dev in &devices {
                info!(
                    "  ONVIF: {} ({} {}) xaddr={}",
                    dev.ip,
                    dev.manufacturer.as_deref().unwrap_or("unknown"),
                    dev.model.as_deref().unwrap_or(""),
                    dev.xaddr,
                );
            }
        }

        tokio::time::sleep(interval).await;
    }
}

/// Build a WS-Discovery Probe SOAP envelope targeting ONVIF devices.
fn build_probe_message() -> String {
    let message_id = uuid::Uuid::new_v4();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope"
            xmlns:a="http://schemas.xmlsoap.org/ws/2004/08/addressing"
            xmlns:d="http://schemas.xmlsoap.org/ws/2005/04/discovery"
            xmlns:dn="http://www.onvif.org/ver10/network/wsdl">
  <s:Header>
    <a:Action s:mustUnderstand="1">http://schemas.xmlsoap.org/ws/2005/04/discovery/Probe</a:Action>
    <a:MessageID>uuid:{message_id}</a:MessageID>
    <a:ReplyTo>
      <a:Address>http://schemas.xmlsoap.org/ws/2004/08/addressing/role/anonymous</a:Address>
    </a:ReplyTo>
    <a:To s:mustUnderstand="1">urn:schemas-xmlsoap-org:ws:2005:04:discovery</a:To>
  </s:Header>
  <s:Body>
    <d:Probe>
      <d:Types>dn:NetworkVideoTransmitter</d:Types>
    </d:Probe>
  </s:Body>
</s:Envelope>"#
    )
}

/// Extract device info from a WS-Discovery ProbeMatch response.
fn parse_probe_response(xml: &str, src_ip: &str) -> Option<OnvifDevice> {
    // Simple string-based parsing — avoids pulling in a full XML parser.
    // Look for XAddrs element which contains the device service URL.
    let xaddr = extract_between(xml, "<d:XAddrs>", "</d:XAddrs>")
        .or_else(|| extract_between(xml, "<XAddrs>", "</XAddrs>"))?;

    // XAddrs may contain multiple URLs separated by spaces — take the first
    let xaddr = xaddr.split_whitespace().next()?.to_string();

    Some(OnvifDevice {
        xaddr,
        ip: src_ip.to_string(),
        manufacturer: extract_between(xml, "<Manufacturer>", "</Manufacturer>")
            .map(|s| s.to_string()),
        model: extract_between(xml, "<Model>", "</Model>").map(|s| s.to_string()),
    })
}

/// Extract text between two markers in a string.
fn extract_between<'a>(text: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let start_idx = text.find(start)? + start.len();
    let end_idx = text[start_idx..].find(end)? + start_idx;
    Some(text[start_idx..end_idx].trim())
}
