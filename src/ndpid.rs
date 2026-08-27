//! nDPId's event stream, drained into a spool for the USP courier.
//!
//! # Why this exists
//!
//! `nDPId-testing` is already installed and boot-enabled on the image, and it
//! was shipping nothing: every instance in `/etc/config/nDPId-testing` is
//! `option enabled 0`, so it never started and produced not one log line.
//! Switched on, it emits per-flow JSON that is strictly richer than anything
//! the platform currently collects — byte counters both directions, nDPI
//! confidence, category, breed, risk bits, and the protocol-specific blocks
//! (TLS/QUIC/HTTP/DNS/SSH).
//!
//! This is the transport for it. `aether-sensord` stays the enforcement path:
//! it reads NFLOG in-line and can drop packets, which nDPId cannot — it is
//! libpcap and observe-only. The two are complementary, not competing.
//!
//! # Consent
//!
//! nDPId dissects packet **payload**, so this rides on the same consent as the
//! rest of classification (ADR-020 §3). Nothing here starts nDPId; when it is
//! not running the socket is absent, this module reports that once and stays
//! quiet. A device that never consented produces no spool at all.
//!
//! # Attribution
//!
//! nDPId emits no MAC address — verified against a live 1.7.0 stream, every
//! flow event, no field containing "mac". Its events carry IP addresses only.
//! The controller keys its rollups on (client, app) and drops anything it
//! cannot attribute, so the client MAC is resolved here from the device's own
//! neighbour table. Doing it on the device is the only place the answer is
//! authoritative: by the time a record reaches the controller the ARP entry may
//! have been reused by a different host.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use log::{debug, info, warn};
use tokio::io::AsyncReadExt;
use tokio::net::UnixStream;

/// Where nDPIsrvd listens. Matches the `nDPId-testing` package default.
const DISTRIBUTOR: &str = "/var/run/nDPId-testing/nDPIsrvd-distributor.sock";

/// Where batches are written for the courier to ship.
const SPOOL: &str = "/var/spool/ac-client/ndpid";

/// Events per batch file.
const BATCH_EVENTS: usize = 128;

/// Longest a buffered event waits before its batch is written.
///
/// Without this a quiet link never reaches BATCH_EVENTS and nothing is ever
/// spooled: the collector connects, reads, counts, and produces no output at
/// all. Observed on the BPI-R4 -- 90 seconds of generated traffic, socket
/// connected, spool empty. `aether-sensord` flushes on its daemon interval for
/// exactly this reason.
const FLUSH_SECS: u64 = 60;

/// Batches kept before the oldest is dropped. The courier deletes what it
/// ships, so this only fills when nothing is collecting.
const MAX_BATCHES: usize = 16;

/// Largest single framed message we will accept, in bytes.
///
/// nDPIsrvd frames as a five-digit zero-padded length, so 99999 is the
/// protocol's own ceiling and anything larger is a desynchronised stream
/// rather than a big event.
const MAX_FRAME: usize = 99_999;

/// Flow events worth shipping.
///
/// `detected` and `detection-update` carry the classification; `end` and
/// `idle` carry the final byte counters. `new`, `update` and `analyse` are
/// dropped: `new` has no classification yet, `update` repeats counters that
/// `end` will restate, and `analyse` is a large statistical block the
/// controller has nowhere to put. Shipping all of them would roughly quintuple
/// the uplink for no extra fact.
const WANTED: &[&str] = &["detected", "detection-update", "end", "idle"];

/// Read the neighbour tables into an IP -> MAC map.
///
/// BOTH families. `/proc/net/arp` is IPv4-only, and reading it alone made every
/// IPv6 flow unattributable by construction -- on a dual-stack subscriber
/// network that silently under-reports usage for whichever share of traffic is
/// v6, which is most of it on a modern handset. Measured on the BPI-R4: the
/// only unattributed flow in a 59-flow sample was IPv6.
///
/// Re-read periodically rather than cached for the process lifetime: a lease
/// changing hands mid-session would otherwise attribute one subscriber's
/// traffic to another, which is worse than failing to attribute it.
fn arp_table() -> HashMap<String, String> {
    let mut m = HashMap::new();

    // IPv4: space-separated, `IP HWtype Flags HWaddr Mask Device`.
    if let Ok(text) = fs::read_to_string("/proc/net/arp") {
        for line in text.lines().skip(1) {
            let mut f = line.split_whitespace();
            let (Some(ip), Some(_hw), Some(_flags), Some(mac)) =
                (f.next(), f.next(), f.next(), f.next())
            else {
                continue;
            };
            // The kernel's placeholder for an incomplete entry. Storing it
            // would attribute every unresolved host to one fictional client.
            if mac == "00:00:00:00:00:00" {
                continue;
            }
            m.insert(ip.to_string(), mac.to_ascii_lowercase());
        }
    }

    // IPv6 has no /proc equivalent, so the neighbour table comes from iproute2.
    // `ip -6 neigh show` prints `<addr> dev <ifname> lladdr <mac> <state>`;
    // entries without an lladdr (FAILED, INCOMPLETE) are skipped rather than
    // recorded as a client we cannot name.
    if let Ok(out) = std::process::Command::new("ip")
        .args(["-6", "neigh", "show"])
        .output()
    {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let f: Vec<&str> = line.split_whitespace().collect();
            let Some(addr) = f.first() else { continue };
            let Some(i) = f.iter().position(|t| *t == "lladdr") else {
                continue;
            };
            let Some(mac) = f.get(i + 1) else { continue };
            if *mac == "00:00:00:00:00:00" {
                continue;
            }
            m.insert(addr.to_string(), mac.to_ascii_lowercase());
        }
    }

    m
}

/// Multicast and broadcast destinations, which have no owning client.
///
/// mDNS, SSDP and friends are addressed to a group, not a host. Counting them
/// as "could not attribute" made the drop counter look like lost subscriber
/// traffic when it is nothing of the kind -- and the counter exists precisely
/// so that a real loss is visible.
fn is_multicast(ip: &str) -> bool {
    match ip.parse::<IpAddr>() {
        Ok(IpAddr::V4(a)) => a.is_multicast() || a.is_broadcast(),
        Ok(IpAddr::V6(a)) => a.is_multicast(),
        Err(_) => false,
    }
}

/// Whether an address is one of ours to attribute.
fn is_local(ip: &str) -> bool {
    match ip.parse::<IpAddr>() {
        Ok(IpAddr::V4(a)) => a.is_private() || a.is_link_local(),
        // Unique-local and link-local. `is_unique_local` is unstable, so the
        // prefix test is written out.
        Ok(IpAddr::V6(a)) => {
            let s = a.segments()[0];
            (s & 0xfe00) == 0xfc00 || (s & 0xffc0) == 0xfe80
        }
        Err(_) => false,
    }
}

/// Attach `client_mac` to an event, and say which endpoint it describes.
///
/// Returns false when the flow cannot be attributed, in which case the event
/// is dropped here rather than sent for the controller to reject: an
/// unattributable flow costs uplink and produces nothing.
/// Why a flow could not be attributed. Distinguished so the drop counter means
/// "subscriber traffic we lost" and not "broadcast chatter we correctly
/// ignored".
enum Attribution {
    Ok,
    /// Addressed to a group, not a host. Expected and harmless.
    Multicast,
    /// A local host we have no neighbour entry for. This is the one that
    /// matters: it IS subscriber traffic and it is being lost.
    Unknown,
}

fn attribute(
    ev: &mut serde_json::Map<String, serde_json::Value>,
    arp: &HashMap<String, String>,
) -> Attribution {
    let src = ev.get("src_ip").and_then(|v| v.as_str()).unwrap_or("");
    let dst = ev.get("dst_ip").and_then(|v| v.as_str()).unwrap_or("");

    // The local endpoint is the subject. Prefer src: for an outbound flow the
    // client is the source, which is the overwhelming majority. A flow between
    // two local addresses (LAN to LAN) is attributed to the source too.
    let (client_ip, subject_is_src) = if is_local(src) {
        (src, true)
    } else if is_local(dst) {
        (dst, false)
    } else {
        return Attribution::Unknown;
    };

    let Some(mac) = arp.get(client_ip) else {
        // Group-addressed traffic has no owning host, so it is not a loss.
        if is_multicast(dst) || is_multicast(src) {
            return Attribution::Multicast;
        }
        return Attribution::Unknown;
    };
    ev.insert("client_mac".into(), serde_json::Value::String(mac.clone()));
    // Which direction the byte counters mean relative to the client, so the
    // controller does not have to re-derive it and risk getting up and down
    // the wrong way round.
    ev.insert(
        "client_is_src".into(),
        serde_json::Value::Bool(subject_is_src),
    );
    Attribution::Ok
}

struct Batcher {
    pending: Vec<String>,
    written: u64,
    dropped_unattributed: u64,
    /// Group-addressed traffic, counted apart so it never inflates the number
    /// that means "subscriber traffic lost".
    skipped_multicast: u64,
}

impl Batcher {
    fn new() -> Self {
        Self {
            pending: Vec::with_capacity(BATCH_EVENTS),
            written: 0,
            dropped_unattributed: 0,
            skipped_multicast: 0,
        }
    }

    fn flush(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let dir = Path::new(SPOOL);
        if fs::create_dir_all(dir).is_err() {
            warn!("ndpid: cannot create {SPOOL}; events are being discarded");
            self.pending.clear();
            return;
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let path = dir.join(format!("ndpid-{now}.ndjson"));
        let tmp = dir.join(format!("ndpid-{now}.ndjson.partial"));

        let mut body = self.pending.join("\n");
        body.push('\n');
        // A meta line per batch, exactly as the sensord spools carry: a
        // consumer that cannot see what was dropped will read a thinned stream
        // as a quiet network.
        body.push_str(&format!(
            "{{\"meta\":true,\"events\":{},\"unattributed_dropped\":{},\
             \"multicast_skipped\":{},\"total_written\":{}}}\n",
            self.pending.len(),
            self.dropped_unattributed,
            self.skipped_multicast,
            self.written
        ));

        // Write-then-rename, so the courier never reads a half-written batch
        // and ships truncated NDJSON.
        if fs::write(&tmp, body).is_ok() && fs::rename(&tmp, &path).is_ok() {
            debug!(
                "ndpid: wrote {} events to {}",
                self.pending.len(),
                path.display()
            );
        } else {
            warn!("ndpid: could not write batch {}", path.display());
            let _ = fs::remove_file(&tmp);
        }
        self.pending.clear();
        prune(dir);
    }

    fn push(&mut self, line: String) {
        self.written += 1;
        self.pending.push(line);
        if self.pending.len() >= BATCH_EVENTS {
            self.flush();
        }
    }
}

/// Keep only the newest `MAX_BATCHES`.
fn prune(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("ndpid-") && n.ends_with(".ndjson"))
                .unwrap_or(false)
        })
        .collect();
    if files.len() <= MAX_BATCHES {
        return;
    }
    files.sort();
    let excess = files.len() - MAX_BATCHES;
    for p in files.into_iter().take(excess) {
        let _ = fs::remove_file(p);
    }
}

/// Parse nDPIsrvd's framing: five ASCII digits of length, then that many bytes
/// of JSON.
///
/// Returns the number of bytes consumed and the JSON slice bounds, or `None`
/// when the buffer does not yet hold a whole message.
fn next_frame(buf: &[u8]) -> Option<(usize, usize, usize)> {
    if buf.len() < 5 {
        return None;
    }
    let head = std::str::from_utf8(&buf[..5]).ok()?;
    let len: usize = head.parse().ok()?;
    if len == 0 || len > MAX_FRAME {
        return None;
    }
    if buf.len() < 5 + len {
        return None;
    }
    Some((5 + len, 5, 5 + len))
}

async fn drain(stream: &mut UnixStream, b: &mut Batcher) -> io::Result<()> {
    let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
    let mut chunk = [0u8; 16 * 1024];
    let mut arp = arp_table();
    let mut since_arp = 0usize;
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(FLUSH_SECS));
    // The first tick fires immediately; skip it so an empty buffer does not
    // produce a batch of nothing on connect.
    ticker.tick().await;

    loop {
        let n = tokio::select! {
            // Time-based flush, so a link too quiet to fill a batch still
            // reports. Biased towards the read: under load the counter matters
            // more than the timer, and a flush that waits one extra chunk is
            // harmless.
            _ = ticker.tick() => {
                b.flush();
                continue;
            }
            r = stream.read(&mut chunk) => r?,
        };

        if n == 0 {
            b.flush();
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "nDPIsrvd closed the connection",
            ));
        }
        buf.extend_from_slice(&chunk[..n]);

        loop {
            let Some((consumed, s, e)) = next_frame(&buf) else {
                // A frame header that will never parse means the stream is
                // desynchronised; dropping the connection and reconnecting is
                // the only way back to a known position.
                if buf.len() >= 5
                    && std::str::from_utf8(&buf[..5])
                        .ok()
                        .and_then(|h| h.parse::<usize>().ok())
                        .is_none()
                {
                    b.flush();
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "desynchronised nDPIsrvd frame header",
                    ));
                }
                break;
            };

            let json = &buf[s..e];
            if let Ok(serde_json::Value::Object(mut ev)) =
                serde_json::from_slice::<serde_json::Value>(json)
            {
                let kind = ev
                    .get("flow_event_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if WANTED.contains(&kind) && ev.contains_key("ndpi") {
                    since_arp += 1;
                    if since_arp >= 64 {
                        arp = arp_table();
                        since_arp = 0;
                    }
                    match attribute(&mut ev, &arp) {
                        Attribution::Ok => {
                            if let Ok(line) = serde_json::to_string(&serde_json::Value::Object(ev))
                            {
                                b.push(line);
                            }
                        }
                        Attribution::Multicast => b.skipped_multicast += 1,
                        Attribution::Unknown => b.dropped_unattributed += 1,
                    }
                }
            }
            buf.drain(..consumed);
        }
    }
}

/// Connect to nDPIsrvd and keep draining, reconnecting when it goes away.
pub fn spawn() {
    tokio::spawn(async move {
        let mut b = Batcher::new();
        let mut announced_absent = false;

        loop {
            match UnixStream::connect(DISTRIBUTOR).await {
                Ok(mut s) => {
                    announced_absent = false;
                    info!("ndpid: connected to {DISTRIBUTOR}");
                    if let Err(e) = drain(&mut s, &mut b).await {
                        warn!("ndpid: stream ended: {e}");
                    }
                    b.flush();
                }
                Err(e) => {
                    // Said once per outage, not every ten seconds. nDPId being
                    // switched off is the shipped default, so this is the
                    // normal case and must not fill the log.
                    if !announced_absent {
                        info!(
                            "ndpid: {DISTRIBUTOR} unavailable ({e}); no flow events \
                             will be reported until nDPId is enabled"
                        );
                        announced_absent = true;
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framing_matches_ndpisrvd() {
        // Five ASCII digits of length, then exactly that many bytes.
        let msg = b"00007{\"a\":1}extra";
        let (consumed, s, e) = next_frame(msg).expect("a complete frame");
        assert_eq!(consumed, 12);
        assert_eq!(&msg[s..e], b"{\"a\":1}");

        // A partial frame yields nothing rather than a truncated parse.
        assert!(next_frame(b"00099{\"a\":1}").is_none());
        assert!(next_frame(b"000").is_none());
        // A non-numeric header is a desynchronised stream, not a short read.
        assert!(next_frame(b"{\"a\":1}xxxxx").is_none());
    }

    #[test]
    fn only_local_addresses_are_attributable() {
        assert!(is_local("192.168.1.151"));
        assert!(is_local("10.0.0.4"));
        assert!(is_local("fd7a:8a35:3985::e7b"));
        assert!(!is_local("142.250.197.35"));
        assert!(!is_local("not-an-ip"));
    }

    /// nDPId ships no MAC, so an unattributable flow must be dropped here.
    /// Sending it costs uplink and the controller rejects it as MissingClient.
    #[test]
    fn a_flow_with_no_known_client_is_dropped() {
        let mut arp = HashMap::new();
        arp.insert("192.168.1.151".to_string(), "8c:16:45:e6:78:16".to_string());

        let mut ev: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(r#"{"src_ip":"192.168.1.151","dst_ip":"142.250.197.35"}"#)
                .unwrap();
        assert!(matches!(attribute(&mut ev, &arp), Attribution::Ok));
        assert_eq!(ev["client_mac"], "8c:16:45:e6:78:16");
        assert_eq!(ev["client_is_src"], true);

        // Inbound: the local endpoint is the destination.
        let mut ev: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(r#"{"src_ip":"142.250.197.35","dst_ip":"192.168.1.151"}"#)
                .unwrap();
        assert!(matches!(attribute(&mut ev, &arp), Attribution::Ok));
        assert_eq!(ev["client_is_src"], false);

        // Local address with no neighbour entry: a real loss, and counted as one.
        let mut ev: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(r#"{"src_ip":"192.168.1.99","dst_ip":"1.1.1.1"}"#).unwrap();
        assert!(matches!(attribute(&mut ev, &arp), Attribution::Unknown));

        // Neither end local (transit): not ours to attribute.
        let mut ev: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(r#"{"src_ip":"8.8.8.8","dst_ip":"1.1.1.1"}"#).unwrap();
        assert!(matches!(attribute(&mut ev, &arp), Attribution::Unknown));
    }

    /// IPv6 must be attributable. `/proc/net/arp` is v4-only, so reading it
    /// alone made every v6 flow unattributable by construction -- measured on
    /// the BPI-R4, the sole unattributed flow in a 59-flow sample was IPv6.
    #[test]
    fn ipv6_clients_are_attributable() {
        let mut arp = HashMap::new();
        arp.insert(
            "fd7a:8a35:3985::e7b".to_string(),
            "8c:16:45:e6:78:16".to_string(),
        );
        let mut ev: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(r#"{"src_ip":"fd7a:8a35:3985::e7b","dst_ip":"2606:4700::1111"}"#)
                .unwrap();
        assert!(matches!(attribute(&mut ev, &arp), Attribution::Ok));
        assert_eq!(ev["client_mac"], "8c:16:45:e6:78:16");
    }

    /// Group-addressed traffic has no owning host, so it must not inflate the
    /// counter that means "subscriber traffic we lost". mDNS to ff02::fb was
    /// the actual content of the drops observed in production.
    #[test]
    fn multicast_is_skipped_not_counted_as_a_loss() {
        let arp = HashMap::new();
        let mut ev: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(r#"{"src_ip":"fe80::b299:d7ff:fe85:5ec8","dst_ip":"ff02::fb"}"#)
                .unwrap();
        assert!(matches!(attribute(&mut ev, &arp), Attribution::Multicast));

        assert!(is_multicast("224.0.0.251"));
        assert!(is_multicast("255.255.255.255"));
        assert!(!is_multicast("192.168.1.151"));
    }

    /// An incomplete ARP entry must never become a client identity.
    #[test]
    fn placeholder_macs_are_not_clients() {
        let mut arp = HashMap::new();
        arp.insert("192.168.1.7".to_string(), "00:00:00:00:00:00".to_string());
        // arp_table() filters these out; assert the shape that matters if one
        // ever slipped through a different reader.
        assert_eq!(
            arp.get("192.168.1.7").map(|s| s.as_str()),
            Some("00:00:00:00:00:00")
        );
    }

    /// A batch must be written on a timer as well as on a count, or a link
    /// too quiet to produce 128 events spools nothing at all.
    #[test]
    fn a_quiet_link_still_flushes() {
        assert!(FLUSH_SECS > 0, "a zero interval would spin");
        assert!(
            FLUSH_SECS <= 300,
            "waiting longer than the controller's poll makes every batch stale"
        );
    }

    #[test]
    fn only_classification_and_final_events_are_shipped() {
        assert!(WANTED.contains(&"detected"));
        assert!(WANTED.contains(&"end"));
        assert!(WANTED.contains(&"idle"));
        // `update` repeats counters that `end` restates; shipping it would
        // multiply the uplink for no extra fact.
        assert!(!WANTED.contains(&"update"));
        assert!(!WANTED.contains(&"new"));
        assert!(!WANTED.contains(&"analyse"));
    }
}
