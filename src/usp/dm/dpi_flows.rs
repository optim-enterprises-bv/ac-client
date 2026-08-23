//! Classified flows, outbound — `Device.X_OptimACS_DPI.*`.
//!
//! The fourth courier, alongside `dm::enforcement` (proof out),
//! `dm::reputation` (feed in) and `dm::sensing` (drops out). This one carries
//! what the DPI engine classified.
//!
//! Until this existed, `aether-sensord` named the protocol, recovered the SNI
//! and made the policy decision — and the only record was a rate-limited syslog
//! line capped at 25 rows per run. The controller could not show an operator a
//! single classified flow.
//!
//! # Consent
//!
//! Classification reads packet **payload** and is consented separately from
//! everything else the daemon does (ADR-020 §3) — a materially stronger promise
//! than the header-only sensing path. Nothing here can switch it on. A device
//! that has not opted in has no spool, this module reports nothing, and the
//! controller sees a device doing no classification, which is true.
//!
//! # Why batches are deleted once read
//!
//! A USP GET has no acknowledgement, so "shipped" and "received" cannot be
//! distinguished from this side. The choice is therefore between re-reporting a
//! batch and losing one, and it is not a close call:
//!
//! Duplicates are *scored*. The controller counts observations per address and
//! lists above a threshold, so replaying one batch inflates a score and can
//! block an address on evidence that was only ever seen once. Losing a batch
//! weakens the set slightly and blocks nobody.
//!
//! Delete-on-read. A GET lost in flight loses one interval of drop reports,
//! which is the cheap failure.

use std::fs;
use std::path::{Path, PathBuf};

use log::{debug, info, warn};

use crate::config::ClientConfig;
use std::collections::HashMap;

/// Where `aether-sensord` writes flow batches. Matches its `dpi_spool` default.
const SPOOL: &str = "/var/spool/aether-sensord/flows";

/// Most batches to ship in one GET.
///
/// The value lands in a USP parameter and crosses a WebSocket; a device that
/// has been offline for a day should not try to deliver its whole backlog in
/// one message and have the whole thing rejected for size. The rest goes next
/// time.
const MAX_BATCHES_PER_GET: usize = 8;

/// Ceiling on what we will put in one parameter, in bytes.
const MAX_BYTES: usize = 512 * 1024;

/// Batch files are `flows-<unix seconds>.ndjson`. `.partial` files are excluded
/// by the suffix test — the daemon writes one and renames, so reading a partial
/// would ship a truncated record.
fn timestamp_of(p: &Path) -> Option<u64> {
    p.file_name()?
        .to_str()?
        .strip_prefix("flows-")?
        .strip_suffix(".ndjson")?
        .parse()
        .ok()
}

/// Complete batches, oldest first.
///
/// Oldest first because observations decay: if only some of a backlog fits, the
/// older ones are the ones closest to expiring and least likely to survive
/// another round trip.
fn batches(dir: &Path) -> Vec<(PathBuf, u64)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<(PathBuf, u64)> = entries
        .flatten()
        .map(|e| e.path())
        .filter_map(|p| timestamp_of(&p).map(|t| (p, t)))
        .collect();
    v.sort_by_key(|(_, t)| *t);
    v
}

/// Read and consume up to `MAX_BATCHES_PER_GET` batches.
///
/// Returns the concatenated NDJSON, or `None` when there is nothing to report.
/// Files are removed only after their contents are in the returned buffer, so a
/// read error loses nothing.
pub fn take_batches() -> Option<String> {
    let dir = Path::new(SPOOL);
    let found = batches(dir);
    if found.is_empty() {
        return None;
    }

    let mut body = String::new();
    let mut taken: Vec<PathBuf> = Vec::new();

    for (path, _) in found.into_iter().take(MAX_BATCHES_PER_GET) {
        let Ok(text) = fs::read_to_string(&path) else {
            warn!("dpi flows: cannot read {}, leaving it", path.display());
            continue;
        };
        if !body.is_empty() && body.len() + text.len() > MAX_BYTES {
            // Stop at the ceiling rather than truncating: half a batch is
            // malformed NDJSON, and the controller counts malformed lines
            // against the sensor.
            debug!("dpi flows: size ceiling reached, deferring the rest");
            break;
        }
        body.push_str(&text);
        if !body.ends_with('\n') {
            body.push('\n');
        }
        taken.push(path);
    }

    if body.is_empty() {
        return None;
    }

    // Deleted only now, with the contents already held. See the module note on
    // why losing a batch beats replaying one.
    for p in &taken {
        if let Err(e) = fs::remove_file(p) {
            warn!(
                "dpi flows: shipped {} but could not remove it: {e} -- it will be \
                 reported again, which inflates the score for those addresses",
                p.display()
            );
        }
    }
    info!(
        "dpi flows: reporting {} batch(es), {} bytes",
        taken.len(),
        body.len()
    );
    Some(body)
}

pub fn get(_cfg: &ClientConfig, path: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    if !path.starts_with("Device.X_OptimACS_DPI") {
        return m;
    }

    // How many batches are waiting, reported whether or not any are shipped.
    // A sensor that is producing faster than the controller collects is worth
    // seeing before the spool starts dropping the oldest.
    let waiting = batches(Path::new(SPOOL)).len();
    m.insert(
        "Device.X_OptimACS_DPI.PendingBatches".into(),
        waiting.to_string(),
    );

    if let Some(body) = take_batches() {
        m.insert("Device.X_OptimACS_DPI.Observations".into(), body);
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("ac-dpiflows-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn only_complete_batches_are_candidates() {
        assert_eq!(
            timestamp_of(Path::new("/x/flows-1787461200.ndjson")),
            Some(1787461200)
        );
        // The daemon writes .partial then renames. Shipping a partial sends
        // truncated NDJSON, which the controller counts as malformed against
        // the sensor.
        assert_eq!(
            timestamp_of(Path::new("/x/flows-1787461200.ndjson.partial")),
            None
        );
        assert_eq!(timestamp_of(Path::new("/x/batch-1787461200.ndjson")), None);
    }

    /// Oldest first: observations decay, so if only part of a backlog fits, the
    /// oldest are the ones least likely to survive another round trip.
    #[test]
    fn batches_come_out_oldest_first() {
        let dir = tempdir("order");
        for t in [1787461300u64, 1787461100, 1787461200] {
            fs::write(dir.join(format!("flows-{t}.ndjson")), "{}\n").unwrap();
        }
        let order: Vec<u64> = batches(&dir).into_iter().map(|(_, t)| t).collect();
        assert_eq!(order, vec![1787461100, 1787461200, 1787461300]);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_spool_reports_nothing_rather_than_failing() {
        assert!(batches(Path::new("/nonexistent/aether-sense")).is_empty());
    }
}
