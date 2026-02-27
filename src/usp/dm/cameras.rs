//! TR-181 Device.X_OptimACS_Camera.* — discovers cameras and captures images.

use std::collections::HashMap;
use crate::cam;
use crate::config::ClientConfig;

pub async fn get(_cfg: &ClientConfig, _path: &str) -> HashMap<String, String> {
    let http = match cam::build_camera_http_client() {
        Ok(h) => h,
        Err(_) => return HashMap::new(),
    };
    let cameras = cam::discover_cameras(&http).await;
    let mut m = HashMap::new();
    for (i, cam) in cameras.iter().enumerate() {
        let idx = i + 1;
        let base = format!("Device.X_OptimACS_Camera.{idx}.");
        m.insert(format!("{base}IPAddress"),   cam.ip.clone());
        m.insert(format!("{base}MACAddress"), cam.mac.clone());
    }
    m
}

pub async fn operate_capture(
    _cfg:        &ClientConfig,
    command:     &str,
    _input_args: &HashMap<String, String>,
) -> Result<HashMap<String, String>, String> {
    // Extract camera IP from command path — e.g. Device.X_OptimACS_Camera.1.Capture()
    // For now, discover cameras and capture from the first one
    let http = cam::build_camera_http_client().map_err(|e| e.to_string())?;
    let cameras = cam::discover_cameras(&http).await;
    // Extract index from command path
    let idx: usize = command
        .split('.')
        .find(|s| s.chars().all(|c| c.is_ascii_digit()))
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let cam = cameras.get(idx.saturating_sub(1))
        .ok_or_else(|| format!("camera {idx} not found"))?;
    let image = cam::capture_image(&http, &cam.ip).await
        .ok_or_else(|| "capture failed".to_string())?;
    let mut out = HashMap::new();
    out.insert("image_size".into(), image.len().to_string());
    out.insert("camera_ip".into(),  cam.ip.clone());
    Ok(out)
}
