//! Site gateway — MQTT command channel, health reporter, and edge recording tracker.
//!
//! Turns ac-client into a remote site gateway that can be managed from the NVR
//! server over MQTT. This enables cameras behind firewalls/NAT to be fully
//! managed without direct network access from the server.
//!
//! # MQTT Topics
//!
//! ## Inbound (NVR → ac-client)
//! - `sites/{site_id}/cmd/discover`       — Trigger ONVIF/UPnP network scan
//! - `sites/{site_id}/cmd/snapshot`       — Capture snapshot from a camera
//! - `sites/{site_id}/cmd/stream_start`   — Start live streaming for a camera
//! - `sites/{site_id}/cmd/stream_stop`    — Stop live streaming for a camera
//! - `sites/{site_id}/cmd/reboot`         — Reboot a camera via ONVIF
//! - `sites/{site_id}/cmd/health_check`   — Request immediate health report
//! - `sites/{site_id}/cmd/config`         — Push config update for a camera
//! - `sites/{site_id}/cmd/firmware`       — Trigger firmware update on a camera
//!
//! ## Outbound (ac-client → NVR)
//! - `sites/{site_id}/resp/discover`      — Discovery results
//! - `sites/{site_id}/resp/health`        — Periodic health telemetry
//! - `sites/{site_id}/resp/status`        — Camera status updates
//! - `sites/{site_id}/resp/recordings`    — Edge recording inventory
//! - `sites/{site_id}/resp/snapshot`      — Snapshot capture result
//! - `sites/{site_id}/resp/ack`           — Command acknowledgment

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use log::{debug, error, info, warn};
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use super::config::{CameraConfig, RecordingMode};
use super::onvif_discovery;

// ── Types ──────────────────────────────────────────────────────────────

/// Inbound command payload from NVR server.
#[derive(Debug, Deserialize)]
struct RemoteCommand {
    #[serde(default)]
    camera_id: Option<String>,
    #[serde(default)]
    payload: serde_json::Value,
    #[serde(default)]
    sent_by: u32,
}

/// Health telemetry published periodically.
#[derive(Serialize)]
struct HealthReport {
    site_id: String,
    timestamp: String,
    cameras: Vec<CameraHealth>,
    edge_storage: StorageInfo,
    system: SystemInfo,
}

#[derive(Serialize)]
struct CameraHealth {
    camera_id: String,
    name: String,
    status: String, // "online", "offline", "error"
    recording_mode: String,
    onvif_enabled: bool,
}

#[derive(Serialize)]
struct StorageInfo {
    recording_dir: String,
    used_bytes: u64,
    total_files: u64,
}

#[derive(Serialize)]
struct SystemInfo {
    uptime_secs: u64,
    bandwidth_mbps: f64,
}

/// Edge recording file info for inventory reports.
#[derive(Serialize)]
struct EdgeRecordingInfo {
    camera_id: String,
    filename: String,
    size_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    has_motion: Option<bool>,
}

/// Discovery result sent back to NVR.
#[derive(Serialize)]
struct DiscoveryResult {
    ip: String,
    xaddr: String,
    manufacturer: Option<String>,
    model: Option<String>,
}

/// Command acknowledgment.
#[derive(Serialize)]
struct CommandAck {
    command_type: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<serde_json::Value>,
}

// ── Site Gateway ───────────────────────────────────────────────────────

/// The site gateway manages the MQTT command channel and health reporting.
pub struct SiteGateway {
    site_id: String,
    mqtt_uri: String,
    recording_dir: String,
    cameras: Arc<RwLock<HashMap<String, CameraConfig>>>,
    health_interval_secs: u64,
}

impl SiteGateway {
    pub fn new(
        site_id: String,
        mqtt_uri: String,
        recording_dir: String,
        health_interval_secs: u64,
    ) -> Self {
        Self {
            site_id,
            mqtt_uri,
            recording_dir,
            cameras: Arc::new(RwLock::new(HashMap::new())),
            health_interval_secs,
        }
    }

    /// Register cameras for health reporting and command dispatch.
    pub async fn set_cameras(&self, cameras: HashMap<String, CameraConfig>) {
        *self.cameras.write().await = cameras;
    }

    /// Run the site gateway (blocks forever).
    pub async fn run(self) {
        info!(
            "Site gateway starting (site_id={}, broker={}, health_interval={}s)",
            self.site_id, self.mqtt_uri, self.health_interval_secs
        );

        let client_id = format!("ac-site-{}-{}", self.site_id, uuid::Uuid::new_v4().as_simple());
        let (host, port) = super::mqtt_bridge::parse_mqtt_uri(&self.mqtt_uri);

        let mut opts = MqttOptions::new(&client_id, &host, port);
        opts.set_keep_alive(Duration::from_secs(30));
        opts.set_max_packet_size(300 * 1024, 300 * 1024);

        let (client, mut eventloop) = AsyncClient::new(opts, 128);
        let client = Arc::new(client);

        // Subscribe to command topics
        let cmd_topic = format!("sites/{}/cmd/#", self.site_id);
        if let Err(e) = client.subscribe(&cmd_topic, QoS::AtLeastOnce).await {
            error!("Failed to subscribe to site command topic {}: {}", cmd_topic, e);
            return;
        }
        info!("Site gateway subscribed to {}", cmd_topic);

        // Publish initial online status
        let status_topic = format!("sites/{}/resp/status", self.site_id);
        let online_msg = serde_json::json!({
            "site_id": self.site_id,
            "status": "online",
            "timestamp": chrono::Utc::now().to_rfc3339(),
        });
        let _ = client
            .publish(&status_topic, QoS::AtLeastOnce, true, online_msg.to_string().as_bytes())
            .await;

        // Spawn health reporter
        let health_client = Arc::clone(&client);
        let health_site_id = self.site_id.clone();
        let health_cameras = Arc::clone(&self.cameras);
        let health_recording_dir = self.recording_dir.clone();
        let health_interval = self.health_interval_secs;
        tokio::spawn(async move {
            health_reporter(
                health_client,
                health_site_id,
                health_cameras,
                health_recording_dir,
                health_interval,
            )
            .await;
        });

        // Spawn edge recording inventory reporter (every 5 minutes)
        let inv_client = Arc::clone(&client);
        let inv_site_id = self.site_id.clone();
        let inv_recording_dir = self.recording_dir.clone();
        tokio::spawn(async move {
            recording_inventory_reporter(inv_client, inv_site_id, inv_recording_dir).await;
        });

        // Main command dispatch loop
        let site_id = self.site_id.clone();
        let cameras = Arc::clone(&self.cameras);
        let recording_dir = self.recording_dir.clone();

        loop {
            match eventloop.poll().await {
                Ok(Event::Incoming(Packet::Publish(msg))) => {
                    // Extract command type from topic: sites/{site_id}/cmd/{command_type}
                    let parts: Vec<&str> = msg.topic.split('/').collect();
                    if parts.len() < 4 {
                        continue;
                    }
                    let command_type = parts[3];

                    debug!("Site gateway received command: {} (payload: {} bytes)",
                        command_type, msg.payload.len());

                    let cmd: RemoteCommand = serde_json::from_slice(&msg.payload)
                        .unwrap_or(RemoteCommand {
                            camera_id: None,
                            payload: serde_json::Value::Null,
                            sent_by: 0,
                        });

                    handle_command(
                        &client,
                        &site_id,
                        command_type,
                        &cmd,
                        &cameras,
                        &recording_dir,
                    )
                    .await;
                }
                Ok(_) => {}
                Err(e) => {
                    warn!("Site gateway MQTT connection error: {e}");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    }
}

// ── Command Handler ────────────────────────────────────────────────────

async fn handle_command(
    client: &AsyncClient,
    site_id: &str,
    command_type: &str,
    cmd: &RemoteCommand,
    cameras: &Arc<RwLock<HashMap<String, CameraConfig>>>,
    recording_dir: &str,
) {
    let ack_topic = format!("sites/{}/resp/ack", site_id);

    match command_type {
        "discover" => {
            info!("Site gateway: running ONVIF discovery (requested by user {})", cmd.sent_by);

            let devices = tokio::task::spawn_blocking(|| {
                onvif_discovery::discover(Duration::from_secs(5))
            })
            .await
            .unwrap_or_default();

            let results: Vec<DiscoveryResult> = devices
                .into_iter()
                .map(|d| DiscoveryResult {
                    ip: d.ip,
                    xaddr: d.xaddr,
                    manufacturer: d.manufacturer,
                    model: d.model,
                })
                .collect();

            let resp_topic = format!("sites/{}/resp/discover", site_id);
            let payload = serde_json::json!({
                "site_id": site_id,
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "devices": results,
            });
            let _ = client
                .publish(&resp_topic, QoS::AtLeastOnce, false, payload.to_string().as_bytes())
                .await;

            let ack = CommandAck {
                command_type: "discover".into(),
                status: "completed".into(),
                message: Some(format!("Found {} device(s)", results.len())),
                data: None,
            };
            let _ = client
                .publish(&ack_topic, QoS::AtLeastOnce, false, serde_json::to_string(&ack).unwrap_or_default().as_bytes())
                .await;
        }

        "health_check" => {
            info!("Site gateway: immediate health check requested");
            let report = build_health_report(site_id, cameras, recording_dir).await;
            let resp_topic = format!("sites/{}/resp/health", site_id);
            let _ = client
                .publish(&resp_topic, QoS::AtLeastOnce, false, serde_json::to_string(&report).unwrap_or_default().as_bytes())
                .await;

            send_ack(client, &ack_topic, "health_check", "completed", None).await;
        }

        "stream_start" | "stream_stop" => {
            // These commands are handled by forwarding to the camera subsystem.
            // For now, acknowledge receipt — the camera manager handles actual stream lifecycle.
            let cam_id = cmd.camera_id.as_deref().unwrap_or("unknown");
            info!("Site gateway: {} for camera {}", command_type, cam_id);
            send_ack(client, &ack_topic, command_type, "acknowledged", Some(format!("camera: {}", cam_id))).await;
        }

        "snapshot" => {
            let cam_id = cmd.camera_id.as_deref().unwrap_or("unknown");
            info!("Site gateway: snapshot request for camera {}", cam_id);
            // Acknowledge — actual snapshot capture would require integration with capture subsystem
            send_ack(client, &ack_topic, "snapshot", "acknowledged", Some(format!("camera: {}", cam_id))).await;
        }

        "reboot" => {
            let cam_id = cmd.camera_id.as_deref().unwrap_or("unknown");
            info!("Site gateway: reboot request for camera {}", cam_id);

            // Attempt ONVIF system reboot if camera has ONVIF enabled
            let cameras = cameras.read().await;
            if let Some(cfg) = cameras.get(cam_id) {
                if cfg.onvif_enabled && !cfg.onvif_xaddr.is_empty() {
                    let reboot_result = send_onvif_reboot(
                        cam_id,
                        &cfg.onvif_xaddr,
                        cfg.effective_rtsp_username(),
                        cfg.effective_rtsp_password(),
                    ).await;
                    let status = if reboot_result { "completed" } else { "failed" };
                    send_ack(client, &ack_topic, "reboot", status, Some(format!("camera: {}", cam_id))).await;
                } else {
                    send_ack(client, &ack_topic, "reboot", "failed", Some("ONVIF not enabled".into())).await;
                }
            } else {
                send_ack(client, &ack_topic, "reboot", "failed", Some(format!("camera {} not found", cam_id))).await;
            }
        }

        "config" => {
            let cam_id = cmd.camera_id.as_deref().unwrap_or("unknown");
            info!("Site gateway: config push for camera {} — {:?}", cam_id, cmd.payload);
            // Config updates would write to UCI and reload the camera subsystem
            send_ack(client, &ack_topic, "config", "acknowledged", Some(format!("camera: {}", cam_id))).await;
        }

        "firmware" => {
            let cam_id = cmd.camera_id.as_deref().unwrap_or("unknown");
            info!("Site gateway: firmware update for camera {}", cam_id);
            send_ack(client, &ack_topic, "firmware", "acknowledged", Some(format!("camera: {}", cam_id))).await;
        }

        _ => {
            warn!("Site gateway: unknown command type: {}", command_type);
            send_ack(client, &ack_topic, command_type, "failed", Some("unknown command".into())).await;
        }
    }
}

async fn send_ack(client: &AsyncClient, topic: &str, command_type: &str, status: &str, message: Option<String>) {
    let ack = CommandAck {
        command_type: command_type.into(),
        status: status.into(),
        message,
        data: None,
    };
    let _ = client
        .publish(topic, QoS::AtLeastOnce, false, serde_json::to_string(&ack).unwrap_or_default().as_bytes())
        .await;
}

// ── Health Reporter ────────────────────────────────────────────────────

async fn health_reporter(
    client: Arc<AsyncClient>,
    site_id: String,
    cameras: Arc<RwLock<HashMap<String, CameraConfig>>>,
    recording_dir: String,
    interval_secs: u64,
) {
    let topic = format!("sites/{}/resp/health", site_id);

    loop {
        tokio::time::sleep(Duration::from_secs(interval_secs)).await;

        let report = build_health_report(&site_id, &cameras, &recording_dir).await;

        match serde_json::to_string(&report) {
            Ok(payload) => {
                if let Err(e) = client.publish(&topic, QoS::AtLeastOnce, false, payload.as_bytes()).await {
                    warn!("Failed to publish health report: {e}");
                } else {
                    debug!("Published health report ({} cameras)", report.cameras.len());
                }
            }
            Err(e) => warn!("Failed to serialize health report: {e}"),
        }
    }
}

async fn build_health_report(
    site_id: &str,
    cameras: &Arc<RwLock<HashMap<String, CameraConfig>>>,
    recording_dir: &str,
) -> HealthReport {
    let cams = cameras.read().await;

    let camera_health: Vec<CameraHealth> = cams
        .iter()
        .map(|(id, cfg)| CameraHealth {
            camera_id: id.clone(),
            name: cfg.name.clone(),
            status: if cfg.enabled { "online" } else { "offline" }.into(),
            recording_mode: match cfg.recording_mode {
                RecordingMode::Motion => "motion",
                RecordingMode::Continuous => "continuous",
                RecordingMode::Disabled => "disabled",
            }
            .into(),
            onvif_enabled: cfg.onvif_enabled,
        })
        .collect();

    let storage = scan_edge_storage(recording_dir);

    // Simple uptime from /proc/uptime (Linux)
    let uptime_secs = std::fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok())
        .map(|f| f as u64)
        .unwrap_or(0);

    HealthReport {
        site_id: site_id.into(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        cameras: camera_health,
        edge_storage: storage,
        system: SystemInfo {
            uptime_secs,
            bandwidth_mbps: 0.0, // Measured externally
        },
    }
}

fn scan_edge_storage(recording_dir: &str) -> StorageInfo {
    let path = Path::new(recording_dir);
    let mut used_bytes = 0u64;
    let mut total_files = 0u64;

    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let entry_path = entry.path();
            if entry_path.is_dir() {
                // Per-camera subdirectory
                if let Ok(sub_entries) = std::fs::read_dir(&entry_path) {
                    for sub in sub_entries.flatten() {
                        if let Ok(meta) = sub.metadata() {
                            if meta.is_file() {
                                used_bytes += meta.len();
                                total_files += 1;
                            }
                        }
                    }
                }
            } else if let Ok(meta) = entry.metadata() {
                if meta.is_file() {
                    used_bytes += meta.len();
                    total_files += 1;
                }
            }
        }
    }

    StorageInfo {
        recording_dir: recording_dir.into(),
        used_bytes,
        total_files,
    }
}

// ── Edge Recording Inventory Reporter ──────────────────────────────────

async fn recording_inventory_reporter(
    client: Arc<AsyncClient>,
    site_id: String,
    recording_dir: String,
) {
    let topic = format!("sites/{}/resp/recordings", site_id);

    loop {
        tokio::time::sleep(Duration::from_secs(300)).await; // Every 5 minutes

        let recordings = scan_recordings(&recording_dir);

        let payload = serde_json::json!({
            "site_id": site_id,
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "recordings": recordings,
        });

        if let Err(e) = client
            .publish(&topic, QoS::AtLeastOnce, false, payload.to_string().as_bytes())
            .await
        {
            warn!("Failed to publish recording inventory: {e}");
        } else {
            debug!("Published recording inventory ({} files)", recordings.len());
        }
    }
}

fn scan_recordings(recording_dir: &str) -> Vec<EdgeRecordingInfo> {
    let mut recordings = Vec::new();
    let path = Path::new(recording_dir);

    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let entry_path = entry.path();
            if entry_path.is_dir() {
                // Per-camera subdirectory — dir name is camera_id
                let camera_id = entry
                    .file_name()
                    .to_string_lossy()
                    .to_string();

                if let Ok(sub_entries) = std::fs::read_dir(&entry_path) {
                    for sub in sub_entries.flatten() {
                        if let Ok(meta) = sub.metadata() {
                            if meta.is_file() {
                                recordings.push(EdgeRecordingInfo {
                                    camera_id: camera_id.clone(),
                                    filename: sub.file_name().to_string_lossy().to_string(),
                                    size_bytes: meta.len(),
                                    has_motion: None, // Could be inferred from filename
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    recordings
}

// ── ONVIF Reboot ───────────────────────────────────────────────────────

async fn send_onvif_reboot(camera_id: &str, xaddr: &str, username: &str, password: &str) -> bool {
    let reboot_url = if xaddr.contains("/onvif/device_service") {
        xaddr.to_string()
    } else {
        format!("{}/onvif/device_service", xaddr.trim_end_matches('/'))
    };

    let soap = r#"<?xml version="1.0" encoding="UTF-8"?>
<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope"
            xmlns:tds="http://www.onvif.org/ver10/device/wsdl">
  <s:Body>
    <tds:SystemReboot/>
  </s:Body>
</s:Envelope>"#;

    let client = reqwest::Client::new();
    let mut builder = client
        .post(&reboot_url)
        .header("Content-Type", "application/soap+xml; charset=utf-8")
        .timeout(Duration::from_secs(10));

    if !username.is_empty() {
        builder = builder.basic_auth(username, Some(password));
    }

    match builder.body(soap.to_string()).send().await {
        Ok(resp) if resp.status().is_success() => {
            info!("[{camera_id}] ONVIF reboot command sent successfully");
            true
        }
        Ok(resp) => {
            warn!("[{camera_id}] ONVIF reboot failed: HTTP {}", resp.status());
            false
        }
        Err(e) => {
            warn!("[{camera_id}] ONVIF reboot request failed: {e}");
            false
        }
    }
}
