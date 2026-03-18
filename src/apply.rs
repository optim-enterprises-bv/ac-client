//! Apply firmware upgrades to the device.
//!
//! Firmware is applied via `/sbin/sysupgrade`.

#![allow(dead_code)]

use std::path::Path;

use log::info;
use tokio::process::Command;

use crate::error::{AcError, Result};

// ── Firmware upgrade ──────────────────────────────────────────────────────────

/// Apply a firmware image stored at `fw_path` using `sysupgrade`.
///
/// This function does not return under normal circumstances — sysupgrade
/// reboots the device.  It only returns if sysupgrade fails.
pub async fn apply_firmware(fw_path: &Path) -> Result<()> {
    info!("running sysupgrade on {}", fw_path.display());

    // -n: don't preserve config (server will re-provision), -q: quiet
    let status = Command::new("/sbin/sysupgrade")
        .args(["-q", fw_path.to_str().unwrap_or("")])
        .status()
        .await?;

    if !status.success() {
        return Err(AcError::Protocol(format!(
            "sysupgrade failed with status {status}"
        )));
    }
    Ok(())
}
