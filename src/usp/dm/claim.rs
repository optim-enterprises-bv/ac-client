//! Device claiming, inbound — `Device.X_OptimACS_Claim.Code` (ADR-023).
//!
//! # Why this exists
//!
//! A subscriber could sign up, verify their email and log in, and then never
//! see their router: `subscribers.device_serials` was read by the portal and
//! written by nothing. This is the device's half of the write.
//!
//! # Why the code comes here rather than to the person asking for it
//!
//! A serial is not a secret. It is on the box, it is in DHCP logs, and it is
//! derivable from a MAC address the radio broadcasts in every beacon. If the
//! controller accepted a serial as proof of ownership, anyone within WiFi
//! range could bind someone else's router to their own account and inherit the
//! WiFi configuration surface, the client list, and the household's per-flow
//! history.
//!
//! So the controller sends the code *here*, and the claimant has to read it
//! off the device. Reading it needs SSH or LuCI access to the router, which is
//! the thing being proven. The controller never returns the code to the caller
//! who started the claim, and never writes it to its own logs -- either would
//! put the proof somewhere that does not require access to the device.
//!
//! # What this module does NOT do
//!
//! It does not decide anything. It does not check who is claiming, whether the
//! device is already claimed, or whether the code is still valid -- all of
//! that is the controller's, next to the database that can answer it. This is
//! a display: it takes a short string and puts it where a local administrator
//! can see it.

use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use log::info;

use crate::config::ClientConfig;
use std::collections::HashMap;

/// Where the code is left for a local administrator to read.
///
/// `/var/run` is tmpfs: the code does not survive a reboot, which is correct
/// for something that expires in ten minutes anyway, and it never touches
/// flash.
const CLAIM_FILE: &str = "/var/run/aether-claim-code";

/// Longest code accepted.
///
/// The controller sends eight characters. This is a bound on what arrives over
/// a channel the controller controls, not a format check -- validating the
/// alphabet here would be a second opinion about a value only the controller
/// can judge, and the failure mode of getting that wrong is a device that
/// refuses a legitimate claim.
const MAX_CODE_LEN: usize = 64;

/// Accept a claim code and make it visible locally.
pub fn deliver(value: &str) -> Result<(), String> {
    let code = value.trim();
    if code.is_empty() {
        return Err("empty claim code".into());
    }
    if code.len() > MAX_CODE_LEN {
        return Err(format!(
            "claim code is {} bytes, over the {MAX_CODE_LEN} limit",
            code.len()
        ));
    }
    // Refused rather than sanitised. A code containing a newline would break
    // the syslog line into two and could forge a second one; a code containing
    // a control character is not something the controller sends. Either means
    // this is not a claim code and should not be displayed as one.
    if code.chars().any(|c| c.is_control()) {
        return Err("claim code contains control characters".into());
    }

    // 0600: the file is proof of physical access, so it should be readable by
    // root only. Anyone who can read it can already read everything on the
    // device, which is exactly the population entitled to claim it.
    let path = Path::new(CLAIM_FILE);
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("cannot write {CLAIM_FILE}: {e}"))?;
    writeln!(f, "{code}").map_err(|e| format!("cannot write {CLAIM_FILE}: {e}"))?;

    // Also to syslog, because `logread` is the first place an OpenWrt admin
    // looks and does not require knowing this file exists. The tag is
    // greppable and named in the controller's own instructions to the user.
    info!(
        "aether-claim: claim code is {code} -- enter it in the Aether portal to claim this device"
    );

    Ok(())
}

pub fn set(_cfg: &ClientConfig, path: &str, value: &str) -> Result<(), String> {
    match path {
        "Device.X_OptimACS_Claim.Code" => deliver(value),
        other => Err(format!("read-only or unknown path: {other}")),
    }
}

/// What the device will say about claiming.
///
/// Whether a code is currently pending locally, and nothing else. Deliberately
/// not the code itself: a GET is answered over the controller's channel, and
/// returning it there would hand the proof to whoever asked the controller --
/// undoing the entire point of delivering it to the device.
pub fn get(_cfg: &ClientConfig, path: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    if !path.starts_with("Device.X_OptimACS_Claim") {
        return m;
    }
    m.insert(
        "Device.X_OptimACS_Claim.CodePending".into(),
        Path::new(CLAIM_FILE).exists().to_string(),
    );
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The code must never be returned over the controller's channel.
    ///
    /// Delivering it to the device and then handing it back on a GET would
    /// make the whole exchange prove nothing -- the claimant would never need
    /// to touch the router.
    #[test]
    fn get_never_returns_the_code() {
        let cfg = ClientConfig::default();
        let m = get(&cfg, "Device.X_OptimACS_Claim.");
        for (k, v) in &m {
            assert!(
                !k.ends_with(".Code"),
                "the code itself must not be readable over USP"
            );
            assert!(
                v == "true" || v == "false",
                "only a pending flag may be reported, got {v}"
            );
        }
    }

    #[test]
    fn junk_is_refused_before_it_is_displayed() {
        assert!(deliver("").is_err());
        assert!(deliver("   ").is_err());
        // A newline would split the syslog line and could forge a second one.
        assert!(deliver("ABCD\nEFGH").is_err());
        assert!(deliver(&"A".repeat(MAX_CODE_LEN + 1)).is_err());
    }

    #[test]
    fn unknown_paths_are_refused() {
        let cfg = ClientConfig::default();
        assert!(set(&cfg, "Device.X_OptimACS_Claim.Nope", "ABCD2345").is_err());
    }
}
