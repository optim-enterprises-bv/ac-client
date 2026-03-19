//! TR-181 Device.X_OptimACS_Firmware.* — firmware version and download operation.

use crate::apply;
use crate::config::ClientConfig;
use crate::util;
use std::collections::HashMap;

pub fn get(_cfg: &ClientConfig, path: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();

    // CurrentVersion: the running firmware version (same as Device.DeviceInfo.SoftwareVersion).
    if path.ends_with("CurrentVersion") || path.ends_with("Device.X_OptimACS_Firmware.") {
        m.insert(
            "Device.X_OptimACS_Firmware.CurrentVersion".into(),
            util::read_fw_version(),
        );
    }

    // AvailableVersion: the version of firmware available for upgrade.
    // This is set by the controller via SET before triggering an upgrade;
    // we read it from a well-known file written by the upgrade workflow,
    // or return empty string when no upgrade is staged.
    if path.ends_with("AvailableVersion") || path.ends_with("Device.X_OptimACS_Firmware.") {
        let available = std::fs::read_to_string("/tmp/firmware_available_version")
            .ok()
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        m.insert(
            "Device.X_OptimACS_Firmware.AvailableVersion".into(),
            available,
        );
    }

    m
}

pub async fn operate_download(
    cfg: &ClientConfig,
    _command: &str,
    input_args: &HashMap<String, String>,
) -> Result<HashMap<String, String>, String> {
    let fw_url = input_args.get("url").cloned().unwrap_or_default();
    if fw_url.is_empty() {
        return Err("firmware download requires 'url' input arg".into());
    }
    // Download to fw_dir then apply
    let fw_path = cfg.fw_dir.join("firmware.bin");
    // Use a simple HTTP download via reqwest
    let resp = reqwest::get(&fw_url).await.map_err(|e| e.to_string())?;
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    tokio::fs::write(&fw_path, &bytes)
        .await
        .map_err(|e| e.to_string())?;
    apply::apply_firmware(&fw_path)
        .await
        .map_err(|e| e.to_string())?;
    let mut out = HashMap::new();
    out.insert("status".into(), "applied".into());
    Ok(out)
}
