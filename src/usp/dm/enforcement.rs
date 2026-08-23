//! Proof of enforcement — `Device.X_OptimACS_Enforcement.*`.
//!
//! `aether-sensord` runs a canary against the live nftables sets: it puts a
//! documentation address in the set, checks the kernel holds it, sends one
//! packet, and requires the packet to be refused. The verdict says whether
//! anything is actually being blocked, as distinct from whether blocking has
//! been *configured* — a distinction that cost a day on 2026-08-22, when the
//! daemon logged "Reputation enforcement is live" against nftables sets that
//! did not exist.
//!
//! The daemon writes each verdict to a spool directory and has no network of
//! its own. This module is the courier: it reads the newest record and exposes
//! it as a parameter, so it reaches the controller over the WebSocket that
//! already carries this device's mTLS identity. No new transport, no second
//! credential.
//!
//! # The serial is deliberately not sent
//!
//! The daemon omits its own identity because it does not reliably know it. The
//! controller takes the serial from the authenticated connection instead, which
//! is the only source that cannot be spoofed by whatever wrote the file — a
//! device that could name itself in the body could report another device's
//! enforcement as healthy.
//!
//! # Freshness is the signal
//!
//! The controller treats a verdict older than six hours as an alarm, because a
//! device that quietly stops checking looks exactly like a healthy one. That
//! only works if the newest record is what gets served, so this reads the
//! highest-numbered file rather than the first one `readdir` happens to return.

use std::fs;
use std::path::{Path, PathBuf};

use log::{debug, trace, warn};

use crate::config::ClientConfig;
use std::collections::HashMap;

/// Where `aether-sensord` writes verdicts. Matches its `canary_spool` default.
const SPOOL: &str = "/var/spool/aether-sensord/canary";

/// Records older than this are not worth sending.
///
/// Slightly longer than the controller's own six-hour freshness window, so a
/// device with a stale record still sends it and is judged stale by the
/// controller — rather than sending nothing and being judged as never having
/// reported. The two states are different faults: one is "the canary stopped
/// running", the other is "this device has never proved anything".
const MAX_AGE_SECS: u64 = 7 * 3600;

/// Filenames are `canary-<unix seconds>.ndjson`.
fn timestamp_of(p: &Path) -> Option<u64> {
    p.file_name()?
        .to_str()?
        .strip_prefix("canary-")?
        .strip_suffix(".ndjson")?
        .parse()
        .ok()
}

/// The newest complete verdict file, with its timestamp.
///
/// `.partial` files are skipped by the suffix check above: the daemon writes to
/// one and renames, so a half-written record can exist and must never be read.
fn newest(dir: &Path) -> Option<(PathBuf, u64)> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            trace!("enforcement: no spool at {}: {e}", dir.display());
            return None;
        }
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter_map(|p| timestamp_of(&p).map(|t| (p, t)))
        .max_by_key(|(_, t)| *t)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The newest verdict record, as the single line the daemon wrote.
///
/// `None` when there is no spool, no record, or nothing recent enough. Each of
/// those is reported as an absent parameter rather than an empty string: the
/// controller distinguishes "no verdict" from a verdict it cannot parse, and an
/// empty value would land in the second bucket.
pub fn last_verdict() -> Option<String> {
    let dir = Path::new(SPOOL);
    let (path, ts) = newest(dir)?;

    let age = now_secs().saturating_sub(ts);
    if age > MAX_AGE_SECS {
        debug!(
            "enforcement: newest verdict is {age}s old ({}), not reporting it",
            path.display()
        );
        return None;
    }

    let body = match fs::read_to_string(&path) {
        Ok(b) => b,
        Err(e) => {
            warn!("enforcement: cannot read {}: {e}", path.display());
            return None;
        }
    };
    let line = body.lines().find(|l| !l.trim().is_empty())?;

    // Sanity-check rather than reformat. Re-encoding here would put a second
    // writer of this record in the system and let the two drift; the controller
    // parses what the daemon wrote, and this only refuses to forward something
    // that is not a record at all.
    match serde_json::from_str::<serde_json::Value>(line) {
        Ok(v) if v.get("result").and_then(|r| r.as_str()).is_some() => {
            trace!("enforcement: forwarding verdict from {}", path.display());
            Some(line.to_owned())
        }
        Ok(_) => {
            warn!(
                "enforcement: {} has no result field -- not forwarding",
                path.display()
            );
            None
        }
        Err(e) => {
            warn!("enforcement: {} is not JSON: {e}", path.display());
            None
        }
    }
}

pub fn get(_cfg: &ClientConfig, path: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    if !path.starts_with("Device.X_OptimACS_Enforcement") {
        return m;
    }
    if let Some(v) = last_verdict() {
        m.insert("Device.X_OptimACS_Enforcement.LastVerdict".into(), v);
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_complete_records_are_candidates() {
        assert_eq!(
            timestamp_of(Path::new("/x/canary-1787463777.ndjson")),
            Some(1787463777)
        );
        // The daemon writes to .partial and renames. Reading a partial would
        // forward a truncated record, which the controller rejects -- and a
        // rejected verdict counts as silence, which is its alarm state.
        assert_eq!(
            timestamp_of(Path::new("/x/canary-1787463777.ndjson.partial")),
            None
        );
        assert_eq!(timestamp_of(Path::new("/x/batch-1787463777.ndjson")), None);
        assert_eq!(timestamp_of(Path::new("/x/canary-notanumber.ndjson")), None);
    }

    #[test]
    fn the_newest_record_wins() {
        let dir = tempdir();
        for ts in [1787463000u64, 1787463777, 1787463500] {
            fs::write(
                dir.join(format!("canary-{ts}.ndjson")),
                format!("{{\"result\":\"enforced\",\"reported_at\":{ts}}}\n"),
            )
            .unwrap();
        }
        // Not whichever one readdir returned first: the controller's alarm is
        // based on age, so serving an older record makes a working device look
        // like it stopped checking.
        let (_, ts) = newest(&dir).unwrap();
        assert_eq!(ts, 1787463777);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_spool_is_not_an_error() {
        assert!(newest(Path::new("/nonexistent/aether-canary")).is_none());
    }

    fn tempdir() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "ac-client-enf-{}-{}",
            std::process::id(),
            now_secs()
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }
}
