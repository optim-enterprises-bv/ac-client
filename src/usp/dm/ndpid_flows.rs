//! nDPId flow events, outbound — `Device.X_OptimACS_DPI.NdpidEvents`.
//!
//! The fifth courier, alongside `dm::enforcement` (proof out),
//! `dm::reputation` (feed in), `dm::sensing` (drops out) and `dm::dpi_flows`
//! (sensord's own classifications out).
//!
//! Separate from `dm::dpi_flows` on purpose. The two carry different schemas
//! from different engines: sensord emits its own record shape and can enforce,
//! nDPId emits its native event JSON and cannot. Merging them into one
//! parameter would force the controller to sniff which producer wrote each
//! line, and a record that guesses wrong is silently mis-mapped rather than
//! rejected.
//!
//! # Why batches are deleted once read
//!
//! Same reasoning as the other couriers: a USP GET has no acknowledgement, so
//! "shipped" and "received" cannot be told apart from this side. The choice is
//! between re-reporting a batch and losing one. These records accumulate byte
//! counters into per-app rollups, so a replayed batch inflates a subscriber's
//! usage for traffic that happened once. Losing a batch understates it
//! slightly. Delete-on-read.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use log::{debug, info, warn};

use crate::config::ClientConfig;

/// Where `ndpid.rs` writes batches.
const SPOOL: &str = "/var/spool/ac-client/ndpid";

/// Most batches to ship in one GET. A device offline for a day must not try to
/// deliver its whole backlog in one message and have all of it rejected for
/// size; the rest goes next time.
const MAX_BATCHES_PER_GET: usize = 4;

/// Ceiling on what we put in one parameter, in bytes.
///
/// Half what the sensord couriers use: nDPId events are considerably larger
/// than a sensord flow record, and this is the parameter most likely to be the
/// one that makes a USP message too big.
const MAX_BYTES: usize = 256 * 1024;

fn timestamp_of(p: &Path) -> Option<u64> {
    p.file_name()?
        .to_str()?
        .strip_prefix("ndpid-")?
        .strip_suffix(".ndjson")?
        .parse()
        .ok()
}

/// Complete batches, oldest first. `.partial` files are excluded by the suffix
/// test — the collector writes one and renames, so reading a partial would
/// ship truncated NDJSON and the controller counts malformed lines against the
/// device.
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
            warn!("ndpid: cannot read {}, leaving it", path.display());
            continue;
        };
        if !body.is_empty() && body.len() + text.len() > MAX_BYTES {
            debug!("ndpid: size ceiling reached, deferring the rest");
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

    // Removed only now, with the contents already held, so a read error loses
    // nothing.
    for p in &taken {
        if let Err(e) = fs::remove_file(p) {
            warn!(
                "ndpid: shipped {} but could not remove it: {e} -- it will be \
                 reported again, which inflates per-app usage for those clients",
                p.display()
            );
        }
    }
    info!(
        "ndpid: reporting {} batch(es), {} bytes",
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

    // Reported whether or not anything ships. A collector producing faster
    // than the controller collects is worth seeing before the spool starts
    // dropping the oldest batches.
    let waiting = batches(Path::new(SPOOL)).len();
    m.insert(
        "Device.X_OptimACS_DPI.NdpidPendingBatches".into(),
        waiting.to_string(),
    );

    if let Some(body) = take_batches() {
        m.insert("Device.X_OptimACS_DPI.NdpidEvents".into(), body);
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_complete_batches_are_candidates() {
        assert_eq!(
            timestamp_of(Path::new("/x/ndpid-1787461200.ndjson")),
            Some(1787461200)
        );
        // The collector writes .partial then renames.
        assert_eq!(
            timestamp_of(Path::new("/x/ndpid-1787461200.ndjson.partial")),
            None
        );
        // Another producer's spool must not be shipped down this parameter.
        assert_eq!(timestamp_of(Path::new("/x/flows-1787461200.ndjson")), None);
    }

    #[test]
    fn a_missing_spool_reports_nothing_rather_than_failing() {
        assert!(batches(Path::new("/nonexistent/ac-ndpid")).is_empty());
    }
}
