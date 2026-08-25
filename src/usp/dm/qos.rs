//! `Device.X_OptimACS_QoS.` — queue discipline and shaper state.
//!
//! Bufferbloat is latency that appears only under load, and nothing in the
//! existing data model could see it. The controller received byte counters and
//! interface state; neither says whether a queue is holding packets long enough
//! to ruin a call. There was no latency, queue or shaper parameter reported
//! anywhere, so a subscriber complaining that "video calls break when someone
//! uploads" produced no evidence at all.
//!
//! What this reports comes from `tc -s qdisc`, which is where the kernel
//! already keeps it. CAKE in particular reports per-tin `pk_delay` and
//! `av_delay` — measured sojourn time through the queue. That is the
//! bufferbloat signal, directly, without probing anything.
//!
//! ## Why a vendor object
//!
//! TR-181 has `Device.QoS.`, but it models classification policy — queues,
//! schedulers, classification rules — not an AQM's measured behaviour. There is
//! no standard place for "the 99th percentile sojourn time through this
//! queue". Rather than bend `Device.QoS.Queue.{i}.` into a shape it does not
//! mean, this is a vendor extension that says what it is.
//!
//! ## Requires
//!
//! `tc` (package `tc-tiny` or `tc-full`), and for anything beyond the default
//! `fq_codel`, `sqm-scripts` with `kmod-sched-cake`. On an image without them
//! every parameter here is simply absent, which is the honest answer: no shaper
//! configured means no shaper statistics.

use crate::usp::tp469::uci_backend::uci_get;
use std::collections::HashMap;

pub type Params = HashMap<String, String>;

/// The interface the shaper runs on.
///
/// SQM is configured against a named device, so that is preferred; the WAN
/// device is the fallback for an unshaped link, where the qdisc is whatever the
/// kernel defaulted to.
fn wan_device() -> Option<String> {
    for key in [
        "sqm.@queue[0].interface",
        "network.wan.device",
        "network.wan.ifname",
    ] {
        let v = uci_get(key);
        if !v.is_empty() {
            return Some(v);
        }
    }
    None
}

/// Parse a `tc` duration such as `1.2ms`, `300us`, `5s` into microseconds.
///
/// `tc` picks the unit per value, so the same field is `0us` on an idle link
/// and `1.2ms` on a loaded one. Parsing only one unit would silently drop every
/// reading that matters.
fn duration_us(tok: &str) -> Option<f64> {
    let t = tok.trim();

    // Longest suffix first: `strip_suffix('s')` also matches "1.2ms" and would
    // yield "1.2m", which fails to parse and silently drops the reading — and
    // the readings carrying `ms` are exactly the ones that indicate
    // bufferbloat. Covered by `milliseconds_are_not_mistaken_for_seconds`.
    let (num, mult) = t
        .strip_suffix("us")
        .map(|n| (n, 1.0))
        .or_else(|| t.strip_suffix("ms").map(|n| (n, 1_000.0)))
        .or_else(|| t.strip_suffix('s').map(|n| (n, 1_000_000.0)))?;

    num.parse::<f64>().ok().map(|v| v * mult)
}

/// Read the root qdisc's statistics for `dev`.
fn qdisc_stats(dev: &str) -> Params {
    let mut m = Params::new();

    let Some(out) = std::process::Command::new("tc")
        .args(["-s", "qdisc", "show", "dev", dev])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
    else {
        return m;
    };

    let base = "Device.X_OptimACS_QoS.";
    m.insert(format!("{base}Interface"), dev.to_string());

    // Worst case across tins, not an average of them. CAKE splits traffic into
    // priority tins and a bulk transfer parked in one of them is exactly the
    // case being looked for — averaging it against three idle tins hides it.
    let mut peak_delay_us: f64 = 0.0;
    let mut avg_delay_us: f64 = 0.0;

    for line in out.lines() {
        let t = line.trim();

        // `qdisc cake 8001: root refcnt 2 bandwidth 20Mbit ...`
        if t.starts_with("qdisc ") {
            if let Some(kind) = t.split_whitespace().nth(1) {
                m.insert(format!("{base}Kind"), kind.to_string());
            }
            if let Some(pos) = t.find("bandwidth ") {
                if let Some(bw) = t[pos + "bandwidth ".len()..].split_whitespace().next() {
                    // Reported verbatim ("20Mbit", "unlimited"). Normalising to
                    // bits here would turn "unlimited" into a number, and
                    // "unlimited" is the single most useful value to see: it
                    // means no shaping, which means bufferbloat is expected.
                    m.insert(format!("{base}ShaperRate"), bw.to_string());
                }
            }
        }

        // ` Sent 1234 bytes 10 pkt (dropped 0, overlimits 5 requeues 0)`
        if t.starts_with("Sent ") {
            if let Some(pos) = t.find("dropped ") {
                if let Some(v) = t[pos + "dropped ".len()..]
                    .split(|c: char| c == ',' || c.is_whitespace())
                    .next()
                {
                    m.insert(format!("{base}Dropped"), v.to_string());
                }
            }
            if let Some(pos) = t.find("overlimits ") {
                if let Some(v) = t[pos + "overlimits ".len()..].split_whitespace().next() {
                    m.insert(format!("{base}Overlimits"), v.to_string());
                }
            }
        }

        // ` backlog 0b 0p requeues 0`  — the standing queue right now.
        if t.starts_with("backlog ") {
            let mut it = t.split_whitespace().skip(1);
            if let Some(bytes) = it.next() {
                m.insert(
                    format!("{base}BacklogBytes"),
                    bytes.trim_end_matches('b').to_string(),
                );
            }
            if let Some(pkts) = it.next() {
                m.insert(
                    format!("{base}BacklogPackets"),
                    pkts.trim_end_matches('p').to_string(),
                );
            }
        }

        // `  pk_delay        0us      1.2ms        0us`
        if t.starts_with("pk_delay") {
            for tok in t.split_whitespace().skip(1) {
                if let Some(v) = duration_us(tok) {
                    peak_delay_us = peak_delay_us.max(v);
                }
            }
        }
        if t.starts_with("av_delay") {
            for tok in t.split_whitespace().skip(1) {
                if let Some(v) = duration_us(tok) {
                    avg_delay_us = avg_delay_us.max(v);
                }
            }
        }
    }

    // Only emitted when the qdisc actually reports them. fq_codel does not, and
    // publishing a hardcoded 0 would say "no bufferbloat" about a link nobody
    // measured.
    if peak_delay_us > 0.0 {
        m.insert(format!("{base}PeakDelayUs"), format!("{peak_delay_us:.0}"));
    }
    if avg_delay_us > 0.0 {
        m.insert(format!("{base}AvgDelayUs"), format!("{avg_delay_us:.0}"));
    }

    m
}

/// Whether SQM is configured and enabled for this link.
fn sqm_state(m: &mut Params) {
    let base = "Device.X_OptimACS_QoS.";
    let enabled = uci_get("sqm.@queue[0].enabled");
    if !enabled.is_empty() {
        m.insert(format!("{base}SqmEnabled"), enabled);
    }
    for (key, param) in [
        ("sqm.@queue[0].download", "SqmDownloadKbps"),
        ("sqm.@queue[0].upload", "SqmUploadKbps"),
        ("sqm.@queue[0].qdisc", "SqmQdisc"),
    ] {
        let v = uci_get(key);
        if !v.is_empty() {
            m.insert(format!("{base}{param}"), v);
        }
    }
}

pub async fn get(_cfg: &crate::config::ClientConfig, path: &str) -> Params {
    let mut m = Params::new();

    if !path.starts_with("Device.X_OptimACS_QoS") {
        return m;
    }

    let Some(dev) = wan_device() else {
        return m;
    };

    m.extend(qdisc_stats(&dev));
    sqm_state(&mut m);
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_parse_in_every_unit_tc_emits() {
        assert_eq!(duration_us("0us"), Some(0.0));
        assert_eq!(duration_us("300us"), Some(300.0));
        assert_eq!(duration_us("1.2ms"), Some(1200.0));
        assert_eq!(duration_us("2s"), Some(2_000_000.0));
    }

    #[test]
    fn a_unitless_or_unknown_token_is_not_guessed() {
        // Column headers and tin names appear on the same lines as the values.
        assert_eq!(duration_us("Bulk"), None);
        assert_eq!(duration_us("12"), None);
        assert_eq!(duration_us(""), None);
    }

    /// `ms` must not be read as `s`.
    ///
    /// The suffix checks run longest-first for exactly this reason: a naive
    /// `strip_suffix('s')` matches "1.2ms" and yields "1.2m", which fails to
    /// parse and silently drops the reading — and the readings that carry `ms`
    /// are the ones that indicate bufferbloat.
    #[test]
    fn milliseconds_are_not_mistaken_for_seconds() {
        assert_eq!(duration_us("1.2ms"), Some(1200.0));
        assert_ne!(duration_us("1.2ms"), Some(1_200_000.0));
    }
}
