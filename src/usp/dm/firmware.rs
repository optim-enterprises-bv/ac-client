//! TR-181 Device.X_OptimACS_Firmware.* — firmware version and download operation.

use std::collections::HashMap;
use crate::apply;
use crate::config::ClientConfig;
use crate::util;

pub fn get(_cfg: &ClientConfig, path: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    if path.ends_with("AvailableVersion") || path.ends_with("Device.X_OptimACS_Firmware.") {
        m.insert("Device.X_OptimACS_Firmware.AvailableVersion".into(), util::read_fw_version());
    }
    m
}

pub async fn operate_download(
    cfg:        &ClientConfig,
    _command:    &str,
    input_args:  &HashMap<String, String>,
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
    tokio::fs::write(&fw_path, &bytes).await.map_err(|e| e.to_string())?;
    apply::apply_firmware(&fw_path).await.map_err(|e| e.to_string())?;
    let mut out = HashMap::new();
    out.insert("status".into(), "applied".into());
    Ok(out)
}
