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
