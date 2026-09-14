//! TR-143 throughput diagnostics — `Device.IP.Diagnostics.*`.
//!
//! A throughput figure only means something when the link is deliberately
//! saturated, which passive counters can never tell you: a client pulling
//! 0.4 Mbps over a 4804 Mbps radio is idle, not starved. TR-143 specifies how
//! to run that saturating transfer, and TR-181 exposes it as ordinary
//! parameters, so the controller drives it with the Set and Get it already
//! uses. No bespoke endpoint, no callback, no second control path beside USP.
//!
//! ## Flow
//!
//! The controller sets `DownloadURL`, then `DiagnosticsState = Requested`. The
//! transfer runs in the background; the controller sees `Complete` (or an
//! `Error_*`) and the timings on its next poll, and derives the rate itself:
//!
//! ```text
//! mbps = TestBytesReceived * 8 / (EOMTime - BOMTime)
//! ```
//!
//! `BOMTime`..`EOMTime` spans payload only, so DNS and connection setup do not
//! depress the figure -- this measures transfer rate, not round-trip latency.
//!
//! ## What is deliberately not reported
//!
//! `TotalBytes*` are reported equal to `TestBytes*`. TR-143 defines the total
//! as including protocol overhead, but a userspace agent cannot observe TCP/IP
//! framing, and inventing a plausible-looking difference would be worse than
//! reporting what was actually counted.

use std::collections::HashMap;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use log::{info, warn};

use crate::config::ClientConfig;

/// Largest transfer the agent will run, whatever the controller asks for.
///
/// Test traffic competes with subscriber traffic on the same line, so an
/// unbounded `TestFileLength` would let a misconfigured controller saturate a
/// household's connection indefinitely.
const MAX_TRANSFER_BYTES: u64 = 100 * 1024 * 1024;

/// Give up rather than hold the diagnostic open forever on a stalled link.
const TRANSFER_TIMEOUT_SECS: u64 = 120;

#[derive(Debug, Clone, Default)]
struct Test {
    state: String,
    url: String,
    protocol_version: String,
    number_of_connections: u32,
    test_file_length: u64,
    dscp: u32,
    ethernet_priority: u32,
    rom_time: Option<DateTime<Utc>>,
    bom_time: Option<DateTime<Utc>>,
    eom_time: Option<DateTime<Utc>>,
    test_bytes: u64,
    total_bytes_received: u64,
    total_bytes_sent: u64,
    running: bool,
}

impl Test {
    fn new() -> Self {
        Test {
            state: "None".into(),
            protocol_version: "Any".into(),
            number_of_connections: 1,
            test_file_length: 10 * 1024 * 1024,
            ..Default::default()
        }
    }
}

static DOWNLOAD: Mutex<Option<Test>> = Mutex::new(None);
static UPLOAD: Mutex<Option<Test>> = Mutex::new(None);

fn with<T>(which: Which, f: impl FnOnce(&mut Test) -> T) -> T {
    let cell = match which {
        Which::Download => &DOWNLOAD,
        Which::Upload => &UPLOAD,
    };
    let mut guard = cell.lock().expect("diagnostics state poisoned");
    f(guard.get_or_insert_with(Test::new))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Which {
    Download,
    Upload,
}

impl Which {
    fn object(self) -> &'static str {
        match self {
            Which::Download => "Device.IP.Diagnostics.DownloadDiagnostics",
            Which::Upload => "Device.IP.Diagnostics.UploadDiagnostics",
        }
    }
    fn url_param(self) -> &'static str {
        match self {
            Which::Download => "DownloadURL",
            Which::Upload => "UploadURL",
        }
    }
}

/// TR-181 dateTime. The spec uses 0001-01-01T00:00:00Z for "not yet known",
/// which distinguishes "this test has not reached that point" from a zero
/// duration.
fn dt(v: Option<DateTime<Utc>>) -> String {
    match v {
        Some(t) => t.to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
        None => "0001-01-01T00:00:00Z".into(),
    }
}

/// Report both diagnostics objects.
pub fn get(_cfg: &ClientConfig, path: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    if !path.starts_with("Device.IP.Diagnostics") {
        return m;
    }

    for which in [Which::Download, Which::Upload] {
        let base = which.object();
        let t = with(which, |t| t.clone());

        m.insert(format!("{base}.DiagnosticsState"), t.state.clone());
        m.insert(format!("{base}.{}", which.url_param()), t.url.clone());
        m.insert(
            format!("{base}.ProtocolVersion"),
            t.protocol_version.clone(),
        );
        m.insert(
            format!("{base}.NumberOfConnections"),
            t.number_of_connections.to_string(),
        );
        m.insert(format!("{base}.DSCP"), t.dscp.to_string());
        m.insert(
            format!("{base}.EthernetPriority"),
            t.ethernet_priority.to_string(),
        );
        m.insert(format!("{base}.ROMTime"), dt(t.rom_time));
        m.insert(format!("{base}.BOMTime"), dt(t.bom_time));
        m.insert(format!("{base}.EOMTime"), dt(t.eom_time));
        m.insert(
            format!("{base}.TotalBytesReceived"),
            t.total_bytes_received.to_string(),
        );
        m.insert(
            format!("{base}.TotalBytesSent"),
            t.total_bytes_sent.to_string(),
        );

        match which {
            Which::Download => {
                m.insert(
                    format!("{base}.TestBytesReceived"),
                    t.test_bytes.to_string(),
                );
            }
            Which::Upload => {
                m.insert(format!("{base}.TestBytesSent"), t.test_bytes.to_string());
                m.insert(
                    format!("{base}.TestFileLength"),
                    t.test_file_length.to_string(),
                );
            }
        }
    }
    m
}

/// Apply a controller write.
pub fn set(_cfg: &ClientConfig, path: &str, value: &str) -> Result<(), String> {
    let (which, leaf) = split(path)?;

    match leaf {
        "DiagnosticsState" => return set_state(which, value),
        "DownloadURL" if which == Which::Download => {
            with(which, |t| t.url = value.to_string());
        }
        "UploadURL" if which == Which::Upload => {
            with(which, |t| t.url = value.to_string());
        }
        "TestFileLength" if which == Which::Upload => {
            let n: u64 = value
                .trim()
                .parse()
                .map_err(|_| "not a number".to_string())?;
            with(which, |t| t.test_file_length = n.min(MAX_TRANSFER_BYTES));
        }
        "ProtocolVersion" => {
            if !matches!(value, "Any" | "IPv4" | "IPv6") {
                return Err(format!(
                    "ProtocolVersion must be Any, IPv4 or IPv6: {value}"
                ));
            }
            with(which, |t| t.protocol_version = value.to_string());
        }
        // Accepted and stored so a controller can read back what it set, but
        // not yet honoured. See ADR-033 Limitations -- a single TCP flow, and
        // unmarked test traffic. Reporting these as rejected would be wrong
        // (the write took effect at the data-model level); silently claiming
        // the behaviour changed would be worse.
        "NumberOfConnections" => {
            let n: u32 = value
                .trim()
                .parse()
                .map_err(|_| "not a number".to_string())?;
            if n != 1 {
                warn!("diagnostics: NumberOfConnections={n} stored but only 1 is honoured");
            }
            with(which, |t| t.number_of_connections = n);
        }
        "DSCP" => {
            let n: u32 = value
                .trim()
                .parse()
                .map_err(|_| "not a number".to_string())?;
            with(which, |t| t.dscp = n);
        }
        "EthernetPriority" => {
            let n: u32 = value
                .trim()
                .parse()
                .map_err(|_| "not a number".to_string())?;
            with(which, |t| t.ethernet_priority = n);
        }
        other => return Err(format!("read-only or unknown path: {other}")),
    }
    Ok(())
}

fn split(path: &str) -> Result<(Which, &str), String> {
    for which in [Which::Download, Which::Upload] {
        let prefix = format!("{}.", which.object());
        if let Some(leaf) = path.strip_prefix(&prefix) {
            return Ok((which, leaf));
        }
    }
    Err(format!("not a diagnostics path: {path}"))
}

/// `DiagnosticsState` is the trigger, and the only value a controller may write
/// is `Requested`.
fn set_state(which: Which, value: &str) -> Result<(), String> {
    match value {
        "Requested" => {}
        "None" => {
            with(which, |t| {
                if !t.running {
                    *t = Test::new();
                }
            });
            return Ok(());
        }
        other => {
            return Err(format!(
                "DiagnosticsState may be set to Requested or None, not {other}"
            ))
        }
    }

    let url = with(which, |t| {
        // A second Requested while a transfer is in flight is ignored rather
        // than restarting it. Restarting would let a controller loop generate
        // unbounded WAN traffic on a subscriber's line.
        if t.running {
            return None;
        }
        if t.url.trim().is_empty() {
            return Some(String::new());
        }
        let url = t.url.clone();
        let length = t.test_file_length;
        *t = Test {
            state: "Requested".into(),
            url: url.clone(),
            protocol_version: t.protocol_version.clone(),
            number_of_connections: t.number_of_connections,
            test_file_length: length,
            dscp: t.dscp,
            ethernet_priority: t.ethernet_priority,
            running: true,
            ..Default::default()
        };
        Some(url)
    });

    let Some(url) = url else {
        info!("diagnostics: {:?} already running, ignoring request", which);
        return Ok(());
    };
    if url.is_empty() {
        finish(which, "Error_InitConnectionFailed", None);
        return Err("no URL configured".into());
    }

    tokio::spawn(async move {
        run(which, url).await;
    });
    Ok(())
}

fn finish(which: Which, state: &str, timings: Option<(DateTime<Utc>, DateTime<Utc>, u64)>) {
    with(which, |t| {
        t.state = state.to_string();
        t.running = false;
        if let Some((bom, eom, bytes)) = timings {
            t.bom_time = Some(bom);
            t.eom_time = Some(eom);
            t.test_bytes = bytes;
            match which {
                // Total is reported equal to test bytes: a userspace agent
                // cannot see TCP/IP framing, and a fabricated overhead figure
                // would read as measured.
                Which::Download => t.total_bytes_received = bytes,
                Which::Upload => t.total_bytes_sent = bytes,
            }
        }
    });
    info!("diagnostics: {:?} finished as {state}", which);
}

async fn run(which: Which, url: String) {
    let rom = Utc::now();
    with(which, |t| t.rom_time = Some(rom));

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(TRANSFER_TIMEOUT_SECS))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!("diagnostics: client build failed: {e}");
            finish(which, "Error_InitConnectionFailed", None);
            return;
        }
    };

    let result = match which {
        Which::Download => download(&client, &url).await,
        Which::Upload => {
            let len = with(which, |t| t.test_file_length).min(MAX_TRANSFER_BYTES);
            upload(&client, &url, len).await
        }
    };

    match result {
        Ok((bom, eom, bytes)) => finish(which, "Complete", Some((bom, eom, bytes))),
        Err(state) => finish(which, state, None),
    }
}

/// Returns (BOMTime, EOMTime, payload bytes).
async fn download(
    client: &reqwest::Client,
    url: &str,
) -> Result<(DateTime<Utc>, DateTime<Utc>, u64), &'static str> {
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|_| "Error_InitConnectionFailed")?;
    if !resp.status().is_success() {
        return Err("Error_TransferFailed");
    }

    // `send()` resolves once the response headers have arrived, so this is the
    // moment before the first payload byte -- DNS, the TCP handshake and the
    // request round trip are all already behind us and stay outside the
    // measured window, which is what BOMTime means.
    //
    // Reading chunk by chunk would time the first byte exactly, but that needs
    // reqwest's `stream` feature, and this binary ships to devices where the
    // size of an added feature is a real cost. The difference is one network
    // read on a transfer measured in seconds.
    let bom = Utc::now();

    let body = resp.bytes().await.map_err(|_| "Error_TransferFailed")?;
    let eom = Utc::now();

    let bytes = body.len() as u64;
    if bytes == 0 {
        // A success status with no body: nothing was transferred, so there is
        // no rate to report.
        return Err("Error_NoResponse");
    }
    Ok((bom, eom, bytes.min(MAX_TRANSFER_BYTES)))
}

async fn upload(
    client: &reqwest::Client,
    url: &str,
    length: u64,
) -> Result<(DateTime<Utc>, DateTime<Utc>, u64), &'static str> {
    let body = vec![0u8; length as usize];
    let bom = Utc::now();
    let resp = client
        .post(url)
        .body(body)
        .send()
        .await
        .map_err(|_| "Error_InitConnectionFailed")?;
    let eom = Utc::now();

    if !resp.status().is_success() {
        return Err("Error_TransferFailed");
    }
    Ok((bom, eom, length))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The module keeps its state in statics, so tests that touch it cannot run
    /// concurrently. Without this the suite passes or fails depending on
    /// thread scheduling -- which is worse than failing, because a green run
    /// proves nothing.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Serialise, and clear the shared state. Held for the duration of the
    /// test via the returned guard.
    fn reset() -> std::sync::MutexGuard<'static, ()> {
        // A panicking test poisons the lock; the state is reset here anyway,
        // so recovering keeps one failure from cascading into all the others.
        let guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        *DOWNLOAD.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *UPLOAD.lock().unwrap_or_else(|e| e.into_inner()) = None;
        guard
    }

    fn cfg() -> ClientConfig {
        ClientConfig::default()
    }

    #[test]
    fn both_objects_are_reported() {
        let _g = reset();
        let m = get(&cfg(), "Device.IP.Diagnostics.");
        assert!(m.contains_key("Device.IP.Diagnostics.DownloadDiagnostics.DiagnosticsState"));
        assert!(m.contains_key("Device.IP.Diagnostics.UploadDiagnostics.DiagnosticsState"));
    }

    #[test]
    fn an_untouched_test_reports_none_not_an_error() {
        let _g = reset();
        let m = get(&cfg(), "Device.IP.Diagnostics.");
        assert_eq!(
            m.get("Device.IP.Diagnostics.DownloadDiagnostics.DiagnosticsState")
                .map(String::as_str),
            Some("None")
        );
    }

    /// Unset timestamps must be distinguishable from a zero-length interval, or
    /// a controller dividing by (EOM - BOM) cannot tell "not run" from
    /// "instantaneous".
    #[test]
    fn unset_times_use_the_tr181_zero_date() {
        let _g = reset();
        let m = get(&cfg(), "Device.IP.Diagnostics.");
        assert_eq!(
            m.get("Device.IP.Diagnostics.DownloadDiagnostics.BOMTime")
                .map(String::as_str),
            Some("0001-01-01T00:00:00Z")
        );
    }

    #[test]
    fn the_url_round_trips() {
        let _g = reset();
        set(
            &cfg(),
            "Device.IP.Diagnostics.DownloadDiagnostics.DownloadURL",
            "https://example.test/blob",
        )
        .expect("set");
        let m = get(&cfg(), "Device.IP.Diagnostics.");
        assert_eq!(
            m.get("Device.IP.Diagnostics.DownloadDiagnostics.DownloadURL")
                .map(String::as_str),
            Some("https://example.test/blob")
        );
    }

    #[test]
    fn requesting_without_a_url_is_an_error_not_a_silent_no_op() {
        let _g = reset();
        let r = set(
            &cfg(),
            "Device.IP.Diagnostics.DownloadDiagnostics.DiagnosticsState",
            "Requested",
        );
        assert!(r.is_err(), "a test with no target must be rejected");
    }

    /// A controller may only request. Letting it write Complete would let it
    /// fabricate a result the agent never measured.
    #[test]
    fn a_controller_cannot_write_a_result_state() {
        let _g = reset();
        for bad in ["Complete", "Error_TransferFailed", "Canceled"] {
            assert!(
                set(
                    &cfg(),
                    "Device.IP.Diagnostics.DownloadDiagnostics.DiagnosticsState",
                    bad
                )
                .is_err(),
                "{bad} must be rejected"
            );
        }
    }

    #[test]
    fn test_file_length_is_capped() {
        let _g = reset();
        set(
            &cfg(),
            "Device.IP.Diagnostics.UploadDiagnostics.TestFileLength",
            "999999999999",
        )
        .expect("set");
        let m = get(&cfg(), "Device.IP.Diagnostics.");
        let got: u64 = m
            .get("Device.IP.Diagnostics.UploadDiagnostics.TestFileLength")
            .expect("present")
            .parse()
            .expect("number");
        assert_eq!(
            got, MAX_TRANSFER_BYTES,
            "an unbounded transfer would let a controller saturate a \
             subscriber's line indefinitely"
        );
    }

    #[test]
    fn an_invalid_protocol_version_is_rejected() {
        let _g = reset();
        assert!(set(
            &cfg(),
            "Device.IP.Diagnostics.DownloadDiagnostics.ProtocolVersion",
            "IPv7"
        )
        .is_err());
    }

    #[test]
    fn download_url_is_not_settable_on_the_upload_object() {
        let _g = reset();
        assert!(set(
            &cfg(),
            "Device.IP.Diagnostics.UploadDiagnostics.DownloadURL",
            "https://example.test/x"
        )
        .is_err());
    }

    #[test]
    fn unknown_leaves_are_rejected() {
        let _g = reset();
        assert!(set(
            &cfg(),
            "Device.IP.Diagnostics.DownloadDiagnostics.NotAThing",
            "1"
        )
        .is_err());
    }
}
