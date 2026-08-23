//! The reputation feed, inbound — `Device.X_OptimACS_Reputation.Feed`.
//!
//! The other half of `dm::enforcement`. That module carries the proof that
//! blocking works *out*; this one carries the thing to block *in*.
//!
//! # Why this exists
//!
//! On the test device the enforcement path was demonstrably working and had
//! nothing to enforce: `nft list set inet fw4 aether_rep4` held zero elements
//! and `/var/spool/aether-sensord/feed` was empty. `aether-sensord` reads feed
//! messages from that directory and opens no network socket of its own — by
//! design, because this agent already holds the device's mTLS identity — but
//! nothing was delivering them. A canary that proves the firewall can drop a
//! packet is not worth much when the set it guards is empty.
//!
//! # Shape
//!
//! The controller SETs one parameter whose value is the feed message the
//! scorer published, verbatim:
//!
//! ```text
//! {"type":"delta","serial":41,"add":["203.0.113.7"],"remove":[]}
//! ```
//!
//! It is written to `<spool>/feed-<serial>.json`, `.partial` then renamed, and
//! the daemon picks it up on its next scan and deletes it.
//!
//! # What this module does NOT do
//!
//! It does not parse addresses, apply anything, or decide what is hostile.
//! `feed.c` treats the payload as attacker-influenced and hands every address
//! to a strict parser; duplicating any of that judgement here would create a
//! second opinion about the same bytes, and the one that matters is the one
//! next to the code that builds the nftables command. The only checks here are
//! the ones a courier is entitled to make: is this JSON, does it carry a
//! serial, and is it small enough to write.

use std::fs;
use std::path::{Path, PathBuf};

use log::{debug, info};

use crate::config::ClientConfig;
use std::collections::HashMap;

/// Where `aether-sensord` reads feed messages. Matches its `spool_dir` default.
const SPOOL: &str = "/var/spool/aether-sensord/feed";

/// Largest feed message accepted, in bytes.
///
/// A full snapshot of a reputation set is thousands of addresses, so this has
/// to be generous — but not unbounded. The value arrives over a channel the
/// controller controls and lands on a device with a few megabytes of writable
/// flash, so an unbounded write is a way to fill the filesystem and take the
/// router down. `feed.c` has its own element cap; this is the byte cap that
/// stops the file being written at all.
const MAX_FEED_BYTES: usize = 1024 * 1024;

/// Extract the message serial, which names the file.
///
/// Deliberately not a full parse. The serial orders the messages and
/// `feed_client_accept` is the thing that decides whether an order is
/// acceptable; all this needs is a stable filename so two messages do not
/// collide and a retried one overwrites cleanly rather than accumulating.
fn serial_of(v: &serde_json::Value) -> Option<u64> {
    v.get("serial").and_then(|s| s.as_u64())
}

fn write_message(dir: &Path, serial: u64, body: &str) -> Result<PathBuf, String> {
    fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;

    let path = dir.join(format!("feed-{serial}.json"));
    let tmp = dir.join(format!("feed-{serial}.json.partial"));

    // Written then renamed, because the daemon scans this directory on a timer
    // and would otherwise read a half-written message. It skips dotfiles and
    // requires a .json suffix, so a .partial is invisible to it -- but only if
    // we never write directly to the final name.
    fs::write(&tmp, body).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, &path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("cannot rename into place: {e}")
    })?;
    Ok(path)
}

/// Accept a feed message from the controller.
pub fn deliver(value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err("empty feed message".into());
    }
    if value.len() > MAX_FEED_BYTES {
        // Refused rather than truncated. A truncated message is invalid JSON,
        // which the daemon would reject as a corrupt file -- and a corrupt file
        // looks like a bug in the scorer rather than a size limit here.
        return Err(format!(
            "feed message is {} bytes, over the {MAX_FEED_BYTES} limit",
            value.len()
        ));
    }

    let parsed: serde_json::Value =
        serde_json::from_str(value).map_err(|e| format!("feed message is not JSON: {e}"))?;

    let Some(serial) = serial_of(&parsed) else {
        // Without a serial the daemon cannot order this against what it has,
        // and an out-of-order apply is exactly what its gap handling exists to
        // prevent. Refusing here means the controller learns; writing it would
        // put a file on disk that the daemon rejects silently.
        return Err("feed message carries no serial".into());
    };

    let path = write_message(Path::new(SPOOL), serial, value)
        .map_err(|e| format!("reputation feed: {e}"))?;

    info!(
        "reputation feed: accepted serial {serial} ({} bytes) -> {}",
        value.len(),
        path.display()
    );
    Ok(())
}

pub fn set(_cfg: &ClientConfig, path: &str, value: &str) -> Result<(), String> {
    match path {
        "Device.X_OptimACS_Reputation.Feed" => deliver(value),
        other => Err(format!("read-only or unknown path: {other}")),
    }
}

/// What the device can say about the feed it has received.
///
/// Only what this agent knows: how many messages are waiting for the daemon.
/// Deliberately not the set contents or the applied serial — the daemon owns
/// those and reporting our guess at them would give the controller a second,
/// staler answer to a question that already has an authoritative one.
pub fn get(_cfg: &ClientConfig, path: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    if !path.starts_with("Device.X_OptimACS_Reputation") {
        return m;
    }
    let pending = fs::read_dir(SPOOL)
        .map(|d| {
            d.flatten()
                .filter(|e| {
                    e.file_name()
                        .to_str()
                        .is_some_and(|n| n.starts_with("feed-") && n.ends_with(".json"))
                })
                .count()
        })
        .unwrap_or(0);
    m.insert(
        "Device.X_OptimACS_Reputation.PendingMessages".into(),
        pending.to_string(),
    );
    if pending > 0 {
        debug!("reputation feed: {pending} message(s) awaiting the daemon");
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("ac-client-rep-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn a_message_lands_under_its_serial() {
        let dir = tempdir("serial");
        let body = r#"{"type":"delta","serial":41,"add":["203.0.113.7"],"remove":[]}"#;
        let p = write_message(&dir, 41, body).unwrap();
        assert_eq!(p.file_name().unwrap(), "feed-41.json");
        assert_eq!(fs::read_to_string(&p).unwrap(), body);
        fs::remove_dir_all(&dir).ok();
    }

    /// The daemon requires a `.json` suffix and skips dotfiles, so a partial
    /// write is invisible to it — but only because we never write directly to
    /// the final name. If that ever changes, it reads half a message.
    #[test]
    fn nothing_is_left_under_a_name_the_daemon_would_read() {
        let dir = tempdir("atomic");
        write_message(&dir, 7, r#"{"serial":7}"#).unwrap();
        let names: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["feed-7.json".to_string()]);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_retried_message_overwrites_rather_than_accumulating() {
        let dir = tempdir("retry");
        write_message(&dir, 9, r#"{"serial":9,"add":[]}"#).unwrap();
        write_message(&dir, 9, r#"{"serial":9,"add":["203.0.113.1"]}"#).unwrap();
        let n = fs::read_dir(&dir).unwrap().count();
        assert_eq!(n, 1, "same serial is the same message, not a second one");
        assert!(fs::read_to_string(dir.join("feed-9.json"))
            .unwrap()
            .contains("203.0.113.1"));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn junk_is_refused_before_it_reaches_the_disk() {
        assert!(deliver("").is_err());
        assert!(deliver("not json at all").is_err());
        // Valid JSON, no serial: the daemon orders on it and cannot apply a
        // message without one, so refusing here tells the controller instead of
        // leaving a file that is silently ignored.
        assert!(deliver(r#"{"type":"delta","add":[]}"#).is_err());
    }

    /// Refused, not truncated. A truncated message is invalid JSON, which the
    /// daemon reports as a corrupt file — making a size limit here look like a
    /// bug in the scorer.
    #[test]
    fn an_oversized_message_is_refused_whole() {
        let big = format!(r#"{{"serial":1,"pad":"{}"}}"#, "a".repeat(MAX_FEED_BYTES));
        let e = deliver(&big).unwrap_err();
        assert!(e.contains("over the"), "got: {e}");
    }

    #[test]
    fn serial_is_read_only_when_it_is_a_number() {
        let v: serde_json::Value = serde_json::from_str(r#"{"serial":41}"#).unwrap();
        assert_eq!(serial_of(&v), Some(41));
        let v: serde_json::Value = serde_json::from_str(r#"{"serial":"41"}"#).unwrap();
        assert_eq!(serial_of(&v), None, "a string is not a serial");
    }

    #[test]
    fn unknown_paths_are_refused() {
        let cfg = ClientConfig::default();
        assert!(set(&cfg, "Device.X_OptimACS_Reputation.Nope", "{}").is_err());
    }
}
