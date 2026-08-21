//! DPI telemetry composition — `Device.X_OptimACS_*.Telemetry`.
//!
//! Open App Filter exposes its classification results across several ubus
//! methods, all but one of them per-MAC:
//!
//!   dev_list                        -> the client set (mac, ip, hostname)
//!   dev_visit_list {mac}            -> per-app records: id, name, act, ft, lt, tt
//!   app_class_visit_time {mac}      -> per-category dwell: type, visit_time
//!   get_app_filter_user {mac}       -> configured block intent: mode, list
//!
//! This module merges them into the single envelope the controller ingests
//! (`aether.dpi.v1`), so the controller does not have to know the engine's
//! call structure or make N round trips of its own.
//!
//! Cadence: composition is cached for `REFRESH_SECS`. The controller polls
//! parameters roughly every minute, and this payload grows with clients x
//! apps — running the merge on every GET would tie an unbounded amount of
//! work to the poll interval, which is also what keeps the WebSocket under
//! the path's idle timeout. Compose slowly, serve from cache.

use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use log::{debug, trace, warn};
use serde_json::{json, Value};

/// How long a composed payload stays fresh.
const REFRESH_SECS: u64 = 300;

/// Total app records across all clients. Matches the controller's own guard;
/// exceeding it truncates rather than dropping the payload, so a busy client
/// degrades to partial data instead of silence.
const MAX_RECORDS: usize = 4096;

const SCHEMA: &str = "aether.dpi.v1";

type Cache = Mutex<Option<(Instant, String)>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// Call a ubus method and parse its reply as JSON.
///
/// `args` is the JSON argument object, or None for a bare call. A method that
/// needs arguments returns success with an empty body when called without
/// them, so an empty reply is treated as no data rather than an error.
fn ubus(method: &str, args: Option<&str>) -> Option<Value> {
    let mut cmd = Command::new("ubus");
    cmd.args(["call", "appfilter", method]);
    if let Some(a) = args {
        cmd.arg(a);
    }
    let out = match cmd.output() {
        Ok(o) => o,
        Err(e) => {
            debug!("dpi: ubus {method} failed to spawn: {e}");
            return None;
        }
    };
    if !out.status.success() {
        debug!("dpi: ubus {method} exited {:?}", out.status.code());
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    if text.trim().is_empty() {
        trace!("dpi: ubus {method} returned an empty body");
        return None;
    }
    match serde_json::from_str(&text) {
        Ok(v) => Some(v),
        Err(e) => {
            warn!("dpi: ubus {method} returned unparseable JSON: {e}");
            None
        }
    }
}

/// `act` is the engine's enforcement verdict for a record.
///
/// Absent means the engine does not report one, which is a different claim
/// from "not blocked" — it is returned as null so the controller can tell
/// unknown from negative rather than inferring enforcement that never happened.
fn verdict(rec: &Value) -> Value {
    match rec.get("act").and_then(Value::as_i64) {
        Some(a) => json!(a != 0),
        None => Value::Null,
    }
}

/// Seconds a client was active on an app (`tt`). Null when not collected.
fn active_seconds(rec: &Value) -> Value {
    match rec.get("tt").and_then(Value::as_i64) {
        Some(t) => json!(t),
        None => Value::Null,
    }
}

/// The signature database the engine has actually loaded: entry count and
/// SHA-256.
///
/// Read through `/tmp/feature.cfg`, which is the symlink `appfilter.init`
/// creates and the one oafd opens — deliberately NOT `/etc/appfilter/*.cfg`,
/// which is the file a reader would assume is in use. Those two disagreed:
/// the curated 1,349-signature `feature_en.cfg` was installed and never
/// loaded, while the engine ran on upstream's 228-entry `feature.cfg`, and
/// nothing anywhere reported the difference.
///
/// App ids are only meaningful relative to the database that defines them.
/// The same id means Samba in one of these files and YouTube in the other, so
/// a controller mapping ids to names or categories needs to know which one
/// produced them. Reporting both lets a mismatch be detected instead of
/// silently mis-labelling; the count alone is a weak fingerprint, since two
/// databases can agree on size and disagree on content.
fn signature_db_fingerprint() -> (usize, String) {
    const LOADED_DB: &str = "/tmp/feature.cfg";
    let Ok(data) = std::fs::read(LOADED_DB) else {
        debug!("dpi: {LOADED_DB} not readable; engine may not be running");
        return (0, String::new());
    };
    let count = data
        .split(|b| *b == b'\n')
        .filter(|l| {
            let t = l
                .iter()
                .position(|c| !c.is_ascii_whitespace())
                .map(|i| &l[i..])
                .unwrap_or(&[]);
            !t.is_empty() && t[0] != b'#'
        })
        .count();
    (count, sha256_hex(&data))
}

/// SHA-256, implemented here rather than pulled in as a dependency: this is
/// the only hash the agent needs and it is not on any hot path.
fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bitlen = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bitlen.to_be_bytes());

    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (i, v) in [a, b, c, d, e, f, g, hh].iter().enumerate() {
            h[i] = h[i].wrapping_add(*v);
        }
    }
    h.iter().map(|w| format!("{w:08x}")).collect()
}

/// Compose the OAF telemetry envelope. None when the engine is not present.
fn compose_oaf() -> Option<String> {
    let devices = ubus("dev_list", None)?;
    let devlist = devices.get("devlist").and_then(Value::as_array)?;

    // Engine identity and global config, reported once rather than per client.
    let mut engine = json!({ "name": "open-app-filter" });
    if let Some(v) = ubus("get_oaf_status", None) {
        if let Some(ver) = v.pointer("/data/version").and_then(Value::as_str) {
            engine["version"] = json!(ver);
        }
        if let Some(ev) = v.pointer("/data/engine_version").and_then(Value::as_str) {
            engine["engine_version"] = json!(ev);
        }
    }
    if let Some(v) = ubus("get_app_filter_base", None) {
        if let Some(d) = v.get("data") {
            engine["config"] = d.clone();
        }
    }
    let (count, hash) = signature_db_fingerprint();
    engine["signatures"] = json!(count);
    engine["signature_db_sha256"] = json!(hash);

    let mut clients = Vec::new();
    let mut records = 0usize;
    let mut truncated = false;

    for dev in devlist {
        let mac = match dev.get("mac").and_then(Value::as_str) {
            Some(m) if !m.is_empty() => m,
            _ => continue,
        };
        let arg = json!({ "mac": mac }).to_string();

        // Per-app records: id, name, act (verdict), tt (dwell).
        let mut apps = Vec::new();
        if let Some(v) = ubus("dev_visit_list", Some(&arg)) {
            if let Some(list) = v.get("list").and_then(Value::as_array) {
                for rec in list {
                    if records >= MAX_RECORDS {
                        truncated = true;
                        break;
                    }
                    let Some(id) = rec.get("id").and_then(Value::as_i64) else {
                        continue;
                    };
                    apps.push(json!({
                        "id": id,
                        "name": rec.get("name").and_then(Value::as_str).unwrap_or(""),
                        "active_seconds": active_seconds(rec),
                        "blocked": verdict(rec),
                        "first_seen": rec.get("ft").and_then(Value::as_i64),
                        "last_seen": rec.get("lt").and_then(Value::as_i64),
                    }));
                    records += 1;
                }
            }
        }

        // Per-category dwell. `type` is the index into class_list and is NOT
        // derivable from the app id (verified: id/1000 disagrees for 228 of
        // them). The localised category name is deliberately not forwarded —
        // the controller maps the numeric id into its own taxonomy so nothing
        // language-specific reaches the UI.
        let mut categories = Vec::new();
        if let Some(v) = ubus("app_class_visit_time", Some(&arg)) {
            if let Some(list) = v.get("class_list").and_then(Value::as_array) {
                for c in list {
                    if let Some(t) = c.get("type").and_then(Value::as_i64) {
                        categories.push(json!({
                            "id": t,
                            "active_seconds": c.get("visit_time").and_then(Value::as_i64),
                        }));
                    }
                }
            }
        }

        // Configured block intent. Not a verdict — comparing it against the
        // observed `blocked` above is what surfaces enforcement drift.
        let mut blocked_ids = Vec::new();
        let mut filter_mode = Value::Null;
        if let Some(v) = ubus("get_app_filter_user", Some(&arg)) {
            if let Some(m) = v.pointer("/data/mode").and_then(Value::as_i64) {
                filter_mode = json!(m);
            }
            if let Some(list) = v.pointer("/data/list").and_then(Value::as_array) {
                for a in list {
                    if let Some(id) = a.as_i64() {
                        blocked_ids.push(id);
                    } else if let Some(id) = a.get("id").and_then(Value::as_i64) {
                        blocked_ids.push(id);
                    }
                }
            }
        }

        clients.push(json!({
            "mac": mac,
            "ip": dev.get("ip").and_then(Value::as_str).unwrap_or(""),
            "hostname": dev.get("hostname").and_then(Value::as_str).unwrap_or(""),
            "apps": apps,
            "categories": categories,
            "blocked_app_ids": blocked_ids,
            "filter_mode": filter_mode,
        }));

        if truncated {
            break;
        }
    }

    let envelope = json!({
        "schema": SCHEMA,
        "engine": engine,
        "collected_at": chrono::Utc::now().to_rfc3339(),
        "truncated": truncated,
        "clients": clients,
    });

    debug!(
        "dpi: composed OAF telemetry — {} client(s), {} app record(s){}",
        envelope["clients"].as_array().map_or(0, Vec::len),
        records,
        if truncated { " (truncated)" } else { "" }
    );
    Some(envelope.to_string())
}

/// Composed OAF telemetry, recomposed at most every `REFRESH_SECS`.
pub fn oaf_telemetry() -> Option<String> {
    let mut guard = match cache().lock() {
        Ok(g) => g,
        Err(e) => {
            warn!("dpi: telemetry cache poisoned: {e}");
            return None;
        }
    };
    if let Some((at, payload)) = guard.as_ref() {
        if at.elapsed() < Duration::from_secs(REFRESH_SECS) {
            trace!("dpi: serving cached telemetry");
            return Some(payload.clone());
        }
    }
    let fresh = compose_oaf()?;
    *guard = Some((Instant::now(), fresh.clone()));
    Some(fresh)
}

#[cfg(test)]
mod tests {
    use super::sha256_hex;

    /// Known-answer tests. A hand-rolled hash that is subtly wrong would make
    /// the fingerprint worse than useless: it would report a mismatch as a
    /// match, or churn on identical input.
    #[test]
    fn sha256_matches_known_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"hello world"),
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    /// Exercises the multi-block path and the length-encoding edge cases
    /// around the 56/64-byte padding boundary.
    #[test]
    fn sha256_spans_block_boundaries() {
        assert_eq!(
            sha256_hex(&[b'a'; 55]),
            "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318"
        );
        assert_eq!(
            sha256_hex(&[b'a'; 56]),
            "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a"
        );
        assert_eq!(
            sha256_hex(&[b'a'; 64]),
            "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb"
        );
    }
}
