//! Axis IP camera discovery and image capture.
//!
//! Discovers cameras by scanning the ARP table for devices that respond to the
//! Axis CGI system-ready endpoint.  Captures JPEG snapshots and uploads them to
//! the ACP server as CAM_IMG / CAM_IMG_DATA / CAM_IMG_END packet streams.
//!
//! Camera HTTP calls use a permissive TLS client (all cert errors accepted) to
//! match the C client's `CURLOPT_SSL_VERIFYPEER = 0` behaviour.

use std::path::PathBuf;
use std::time::Duration;

use log::{debug, info, warn};
use reqwest::Client;

use crate::config::ClientConfig;
use crate::error::{AcError, Result};
use crate::util;

/// Maximum number of cameras to track.
const MAX_CAMERAS: usize = 8;

/// A discovered camera entry.
#[derive(Debug, Clone)]
pub struct Camera {
    pub idx: u32,
    pub ip:  String,
    pub mac: String,
}

/// Discover cameras on the local network via ARP scan and Axis CGI probing.
/// Returns at most `MAX_CAMERAS` entries.
pub async fn discover_cameras(http: &Client) -> Vec<Camera> {
    let arp = util::read_arp_table();
    let mut cameras = Vec::new();

    for entry in arp {
        if cameras.len() >= MAX_CAMERAS {
            break;
        }
        if is_axis_camera(http, &entry.ip).await {
            let idx = cameras.len() as u32;
            info!("discovered Axis camera {} at {}", entry.mac, entry.ip);
            cameras.push(Camera {
                idx,
                ip:  entry.ip,
                mac: entry.mac,
            });
        }
    }
    cameras
}

/// Check if an IP host responds to the Axis CGI systemready endpoint.
async fn is_axis_camera(http: &Client, ip: &str) -> bool {
    // Try HTTPS first, then HTTP, to match the C client's curl behaviour.
    for scheme in &["https", "http"] {
        let url = format!("{scheme}://{ip}/axis-cgi/systemready.cgi?action=1");
        match http
            .get(&url)
            .timeout(Duration::from_secs(3))
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status();
                debug!("systemready {url} → {status}");
                // 200 OK or 401 Unauthorized both indicate an Axis camera
                return status.is_success() || status.as_u16() == 401;
            }
            Err(e) => {
                debug!("systemready {url} failed: {e}");
            }
        }
    }
    false
}

/// Retrieve the SD card status for a camera.
pub async fn get_sd_status(http: &Client, ip: &str) -> String {
    let url = format!("http://{ip}/axis-cgi/disks/list.cgi?diskid=SD_DISK");
    match http
        .get(&url)
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .and_then(|r| Ok(r))
    {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                "ok".to_string()
            } else {
                format!("http-{}", status.as_u16())
            }
        }
        Err(e) => {
            debug!("SD status check for {ip} failed: {e}");
            "unknown".to_string()
        }
    }
}

/// Capture a JPEG snapshot from an Axis camera.
///
/// Returns the raw JPEG bytes, or `None` if the capture failed.
pub async fn capture_image(http: &Client, ip: &str) -> Option<Vec<u8>> {
    // Axis image CGI — CIF resolution is a reasonable default
    let url = format!("http://{ip}/axis-cgi/jpg/image.cgi?resolution=CIF");
    match http
        .get(&url)
        .timeout(Duration::from_secs(15))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => match resp.bytes().await {
            Ok(b) => {
                info!("captured {} bytes from {ip}", b.len());
                Some(b.to_vec())
            }
            Err(e) => {
                warn!("reading image from {ip}: {e}");
                None
            }
        },
        Ok(resp) => {
            warn!("image capture from {ip}: HTTP {}", resp.status());
            None
        }
        Err(e) => {
            warn!("image capture from {ip}: {e}");
            None
        }
    }
}

/// Build the permissive HTTP client used for all camera API calls.
///
/// Accepts any TLS certificate — matches the C client's behaviour of
/// `CURLOPT_SSL_VERIFYPEER = 0` / `CURLOPT_SSL_VERIFYHOST = 0`.
pub fn build_camera_http_client() -> Result<Client> {
    Client::builder()
        .danger_accept_invalid_certs(true)
        .danger_accept_invalid_hostnames(true)
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(AcError::Http)
}

/// Save a camera image to the local image directory.
///
/// `img_dir/<mac_no_colons>/<timestamp>.jpg`
pub async fn save_image_locally(
    cfg:    &ClientConfig,
    camera: &Camera,
    image:  &[u8],
) -> Option<PathBuf> {
    let dir = cfg.img_dir.join(util::mac_no_colons(&camera.mac));
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        warn!("cannot create image dir {}: {e}", dir.display());
        return None;
    }
    let ts = chrono::Local::now().format("%Y%m%d%H%M%S").to_string();
    let path = dir.join(format!("{ts}.jpg"));
    if let Err(e) = tokio::fs::write(&path, image).await {
        warn!("cannot write image {}: {e}", path.display());
        return None;
    }
    Some(path)
}
