//! Camera manager — orchestrates per-camera subsystems.
//!
//! The [`CameraManager`] loads configuration, spawns a task set per camera,
//! creates the shared event bus, MQTT bridge, and live stream server.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use log::{debug, error, info, warn};
use tokio::sync::{broadcast, watch, RwLock};
use tokio::task::JoinHandle;

use super::capture::CaptureSession;
use super::config::{self, CameraConfig, CameraGlobalConfig, RecordingMode};
use super::events::CameraEvent;
use super::live_stream::LiveStreamServer;
use super::motion::MotionDetector;
use super::mqtt_bridge::MqttBridge;
use super::onvif_discovery;
use super::recording::Recorder;
use super::storage::VaultUploader;

/// Runtime state for a single camera.
struct CameraSubsystem {
    config: CameraConfig,
    tasks: Vec<JoinHandle<()>>,
    #[allow(dead_code)]
    motion_rx: watch::Receiver<bool>,
}

/// Status snapshot for a single camera (exposed to USP / API).
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct CameraStatus {
    pub id: String,
    pub name: String,
    pub connected: bool,
    pub recording: bool,
    pub motion_active: bool,
}

/// Manages all camera subsystems.
pub struct CameraManager {
    global: CameraGlobalConfig,
    cameras: Arc<RwLock<HashMap<String, CameraSubsystem>>>,
    event_tx: broadcast::Sender<CameraEvent>,
    live_server: Option<Arc<LiveStreamServer>>,
}

impl CameraManager {
    pub fn new() -> Self {
        let global = config::load_global_config();
        let (event_tx, _) = broadcast::channel(256);

        let live_server = if global.live_stream_port > 0 {
            Some(Arc::new(LiveStreamServer::new(global.live_stream_port)))
        } else {
            None
        };

        Self {
            global,
            cameras: Arc::new(RwLock::new(HashMap::new())),
            event_tx,
            live_server,
        }
    }

    pub async fn start(&self) {
        // ── Startup banner with global config ─────────────────────────
        info!("╔══════════════════════════════════════════════════════╗");
        info!("║          OptimACS Camera Module Starting            ║");
        info!("╚══════════════════════════════════════════════════════╝");
        info!("Global config:");
        info!("  Recording dir:    {}", self.global.recording_dir);
        info!(
            "  Live stream:      {}",
            if self.global.live_stream_port > 0 {
                format!("port {}", self.global.live_stream_port)
            } else {
                "disabled".into()
            }
        );
        info!(
            "  MQTT:             {}",
            if self.global.mqtt_uri.is_empty() {
                "not configured".into()
            } else {
                format!("{} (prefix: {})", self.global.mqtt_uri, self.global.mqtt_topic_prefix)
            }
        );
        info!(
            "  ONVIF discovery:  {}",
            if self.global.discovery_enabled {
                format!("enabled (interval: {}s)", self.global.discovery_interval)
            } else {
                "disabled".into()
            }
        );
        info!(
            "  NVR server:       {}",
            if self.global.vault_uri.is_empty() {
                "not configured (recordings stay local)".into()
            } else {
                self.global.vault_uri.clone()
            }
        );

        let camera_configs = config::load_cameras();

        if camera_configs.is_empty() {
            warn!("No cameras configured in UCI — waiting for configuration");
            // Still start discovery if enabled so new cameras can be found
            if self.global.discovery_enabled {
                self.start_discovery_loop().await;
            }
            return;
        }

        info!("Found {} camera section(s) in UCI config", camera_configs.len());

        // ── ONVIF discovery scan at startup ───────────────────────────
        if self.global.discovery_enabled {
            self.run_startup_discovery(&camera_configs).await;
        }

        // ── Log per-camera config summary ─────────────────────────────
        let mut enabled_count = 0u32;
        let mut disabled_count = 0u32;
        for (id, cfg) in &camera_configs {
            if !cfg.enabled {
                disabled_count += 1;
                debug!("[{}] {} — DISABLED", id, cfg.name);
                continue;
            }
            enabled_count += 1;
            info!(
                "[{}] {} — rtsp={} mode={:?} onvif={} threshold={}",
                id,
                if cfg.name.is_empty() { "(unnamed)" } else { &cfg.name },
                if cfg.rtsp_url.is_empty() { "(none)" } else { &cfg.rtsp_url },
                cfg.recording_mode,
                if cfg.onvif_enabled { &cfg.onvif_xaddr } else { "off" },
                cfg.pixel_threshold,
            );
        }
        info!(
            "Camera summary: {} enabled, {} disabled, {} total",
            enabled_count, disabled_count, camera_configs.len()
        );

        // Start MQTT bridge if configured
        if !self.global.mqtt_uri.is_empty() {
            let bridge = MqttBridge::new(
                self.global.mqtt_topic_prefix.clone(),
                self.global.mqtt_uri.clone(),
                self.event_tx.subscribe(),
            );
            tokio::spawn(async move {
                bridge.run().await;
            });
            info!("MQTT bridge started → {}", self.global.mqtt_uri);
        }

        // Start live stream server if configured
        if let Some(ref server) = self.live_server {
            let srv = Arc::clone(server);
            tokio::spawn(async move {
                srv.run().await;
            });
        }

        // ── Start camera subsystems ───────────────────────────────────
        let mut cameras = self.cameras.write().await;
        let mut started = 0u32;
        let mut failed = 0u32;

        for (id, cam_cfg) in &camera_configs {
            if !cam_cfg.enabled {
                info!("[{}] Camera disabled, skipping", id);
                continue;
            }

            info!("[{}] Starting camera subsystem...", id);
            match self.spawn_camera(cam_cfg).await {
                Ok(subsystem) => {
                    info!(
                        "[{}] ✓ Camera started — {} tasks (capture{}{}{})",
                        id,
                        subsystem.tasks.len(),
                        if cam_cfg.recording_mode == RecordingMode::Motion {
                            " + motion"
                        } else {
                            ""
                        },
                        match cam_cfg.recording_mode {
                            RecordingMode::Motion => " + recorder",
                            RecordingMode::Continuous => " + recorder(continuous)",
                            RecordingMode::Disabled => "",
                        },
                        if !self.global.vault_uri.is_empty() {
                            " + uploader"
                        } else {
                            ""
                        },
                    );
                    started += 1;
                    cameras.insert(id.clone(), subsystem);
                }
                Err(e) => {
                    error!("[{}] ✗ Failed to start camera: {}", id, e);
                    failed += 1;
                }
            }
        }

        info!("═══════════════════════════════════════════════════════");
        info!(
            "Camera startup complete: {} running, {} failed, {} disabled",
            started, failed, disabled_count
        );
        info!("═══════════════════════════════════════════════════════");

        // ── Start periodic discovery ──────────────────────────────────
        if self.global.discovery_enabled {
            self.start_discovery_loop().await;
        }
    }

    /// Run ONVIF discovery at startup and cross-reference with configured cameras.
    async fn run_startup_discovery(&self, configured: &HashMap<String, CameraConfig>) {
        info!("Running ONVIF discovery scan at startup...");

        let devices = tokio::task::spawn_blocking(move || {
            onvif_discovery::discover(Duration::from_secs(5))
        })
        .await
        .unwrap_or_default();

        if devices.is_empty() {
            info!("ONVIF discovery: no devices found on the network");
            return;
        }

        info!("ONVIF discovery: found {} device(s) on the network", devices.len());

        // Build a set of configured ONVIF addresses and IPs for cross-reference
        let configured_xaddrs: Vec<&str> = configured
            .values()
            .filter(|c| c.onvif_enabled && !c.onvif_xaddr.is_empty())
            .map(|c| c.onvif_xaddr.as_str())
            .collect();

        let configured_ips: Vec<String> = configured
            .values()
            .filter_map(|c| {
                // Extract IP from RTSP URL
                url_ip(&c.rtsp_url)
            })
            .collect();

        let mut matched = 0u32;
        let mut new = 0u32;

        for dev in &devices {
            let xaddr_match = configured_xaddrs.iter().any(|x| *x == dev.xaddr);
            let ip_match = configured_ips.iter().any(|ip| *ip == dev.ip);

            if xaddr_match || ip_match {
                matched += 1;
                let cam_name = configured
                    .values()
                    .find(|c| {
                        (c.onvif_enabled && c.onvif_xaddr == dev.xaddr)
                            || url_ip(&c.rtsp_url).as_deref() == Some(&dev.ip)
                    })
                    .map(|c| c.name.as_str())
                    .unwrap_or("?");
                info!(
                    "  ✓ {} ({} {}) — matches configured camera \"{}\"",
                    dev.ip,
                    dev.manufacturer.as_deref().unwrap_or("unknown"),
                    dev.model.as_deref().unwrap_or(""),
                    cam_name,
                );
            } else {
                new += 1;
                warn!(
                    "  ✦ {} ({} {}) — NOT CONFIGURED (xaddr: {})",
                    dev.ip,
                    dev.manufacturer.as_deref().unwrap_or("unknown"),
                    dev.model.as_deref().unwrap_or(""),
                    dev.xaddr,
                );
            }
        }

        info!(
            "ONVIF summary: {} matched configured cameras, {} new/unconfigured",
            matched, new
        );
        if new > 0 {
            info!(
                "Tip: Use LuCI → Services → Cameras → Discovery to add unconfigured cameras"
            );
        }
    }

    /// Start the periodic ONVIF discovery loop as a background task.
    async fn start_discovery_loop(&self) {
        let interval = Duration::from_secs(self.global.discovery_interval);
        let cameras = Arc::clone(&self.cameras);
        info!(
            "Starting ONVIF discovery loop (interval: {}s)",
            self.global.discovery_interval
        );
        tokio::spawn(async move {
            discovery_loop_with_matching(interval, cameras).await;
        });
    }

    async fn spawn_camera(&self, cfg: &CameraConfig) -> anyhow::Result<CameraSubsystem> {
        let mut tasks = Vec::new();
        let id = cfg.id.clone();

        if cfg.rtsp_url.is_empty() {
            error!("[{id}] No RTSP URL configured — cannot start camera");
            anyhow::bail!("No RTSP URL configured for camera {id}");
        }

        // Build authenticated RTSP URLs
        let main_url = cfg.authenticated_rtsp_url(&cfg.rtsp_url);
        let has_creds = main_url != cfg.rtsp_url;

        debug!("[{id}] Spawning subsystems: rtsp={}, auth={}, sub_rtsp={}, recording={:?}, onvif={}",
            cfg.rtsp_url,
            if has_creds { "yes" } else { "no" },
            if cfg.sub_rtsp_url.is_empty() { "(using main)" } else { &cfg.sub_rtsp_url },
            cfg.recording_mode,
            cfg.onvif_enabled,
        );

        if !has_creds {
            warn!("[{id}] No RTSP credentials configured — authentication may fail");
        }

        let (motion_tx, motion_rx) = watch::channel(false);

        // ── Main stream capture ──────────────────────────────────────────
        let (main_capture, _main_rx) = CaptureSession::new(
            format!("{id}/main"),
            main_url,
        );
        let main_capture = main_capture.with_event_tx(self.event_tx.clone());
        let main_sender = main_capture.sender().clone();

        // Register with live stream server
        if let Some(ref server) = self.live_server {
            server
                .register_camera(id.clone(), main_sender.clone())
                .await;
        }

        let main_handle = tokio::spawn(async move {
            main_capture.run().await;
        });
        tasks.push(main_handle);

        // ── Sub stream capture (optional, for motion detection) ──────────
        let motion_frame_rx = if !cfg.sub_rtsp_url.is_empty() {
            let sub_url = cfg.authenticated_rtsp_url(&cfg.sub_rtsp_url);
            let (sub_capture, sub_rx) = CaptureSession::new(
                format!("{id}/sub"),
                sub_url,
            );
            let sub_handle = tokio::spawn(async move {
                sub_capture.run().await;
            });
            tasks.push(sub_handle);
            sub_rx
        } else {
            // Use main stream for motion detection
            main_sender.subscribe()
        };

        // ── Motion detector ──────────────────────────────────────────────
        if cfg.recording_mode == RecordingMode::Motion {
            let detector = MotionDetector::new(
                id.clone(),
                motion_frame_rx,
                cfg.pixel_threshold,
                self.event_tx.clone(),
                motion_tx,
            );
            let motion_handle = tokio::spawn(async move {
                detector.run().await;
            });
            tasks.push(motion_handle);
        }

        // ── Recorder ─────────────────────────────────────────────────────
        let rec_frame_rx = main_sender.subscribe();
        let recorder = Recorder::new(
            id.clone(),
            cfg.clone(),
            self.global.recording_dir.clone(),
            rec_frame_rx,
            motion_rx.clone(),
            self.event_tx.clone(),
        );
        let rec_handle = tokio::spawn(async move {
            recorder.run().await;
        });
        tasks.push(rec_handle);

        // ── Vault uploader ───────────────────────────────────────────────
        if !self.global.vault_uri.is_empty() {
            let uploader = VaultUploader::new(
                id.clone(),
                self.global.vault_uri.clone(),
                self.global.vault_access_key.clone(),
                self.global.vault_secret_key.clone(),
                self.global.recording_dir.clone(),
            );
            let upload_handle = tokio::spawn(async move {
                uploader.run().await;
            });
            tasks.push(upload_handle);
        } else {
            warn!("[{id}] No Vault URI configured — recordings will stay local");
        }

        Ok(CameraSubsystem {
            config: cfg.clone(),
            tasks,
            motion_rx,
        })
    }

    #[allow(dead_code)]
    pub async fn status(&self) -> Vec<CameraStatus> {
        let cameras = self.cameras.read().await;
        cameras
            .iter()
            .map(|(id, sub)| CameraStatus {
                id: id.clone(),
                name: sub.config.name.clone(),
                connected: sub.tasks.iter().all(|t| !t.is_finished()),
                recording: sub.config.recording_mode != RecordingMode::Disabled,
                motion_active: *sub.motion_rx.borrow(),
            })
            .collect()
    }

    #[allow(dead_code)]
    pub async fn stop(&self) {
        info!("Stopping all camera subsystems...");
        let mut cameras = self.cameras.write().await;
        let count = cameras.len();
        for (id, sub) in cameras.drain() {
            info!("[{id}] Stopping camera subsystem ({} tasks)", sub.tasks.len());
            for task in sub.tasks {
                task.abort();
            }
        }
        info!("All {} camera subsystem(s) stopped", count);
    }
}

/// Extract IP address from a URL string (e.g., "rtsp://192.168.1.100:554/stream" → "192.168.1.100").
fn url_ip(url: &str) -> Option<String> {
    // Simple extraction: find :// then take until : or /
    let after_scheme = url.split("://").nth(1)?;
    let host = after_scheme.split('/').next()?;
    let ip = host.split(':').next()?;
    if ip.is_empty() {
        None
    } else {
        Some(ip.to_string())
    }
}

/// Periodic discovery loop that cross-references found devices with running cameras.
async fn discovery_loop_with_matching(
    interval: Duration,
    cameras: Arc<RwLock<HashMap<String, CameraSubsystem>>>,
) {
    let probe_timeout = Duration::from_secs(5);

    loop {
        tokio::time::sleep(interval).await;

        info!("Running periodic ONVIF discovery scan...");

        let devices = tokio::task::spawn_blocking(move || {
            onvif_discovery::discover(probe_timeout)
        })
        .await
        .unwrap_or_default();

        if devices.is_empty() {
            debug!("Periodic discovery: no devices found");
            continue;
        }

        info!("Periodic discovery: found {} device(s)", devices.len());

        // Cross-reference with running cameras
        let running = cameras.read().await;
        let running_ips: Vec<String> = running
            .values()
            .filter_map(|sub| url_ip(&sub.config.rtsp_url))
            .collect();
        let running_xaddrs: Vec<&str> = running
            .values()
            .filter(|sub| sub.config.onvif_enabled)
            .map(|sub| sub.config.onvif_xaddr.as_str())
            .collect();

        for dev in &devices {
            let matched = running_xaddrs.iter().any(|x| *x == dev.xaddr)
                || running_ips.iter().any(|ip| *ip == dev.ip);

            if matched {
                debug!(
                    "  Discovery: {} — already configured and running",
                    dev.ip
                );
            } else {
                warn!(
                    "  Discovery: {} ({} {}) — found on network but NOT CONFIGURED",
                    dev.ip,
                    dev.manufacturer.as_deref().unwrap_or("unknown"),
                    dev.model.as_deref().unwrap_or(""),
                );
            }
        }
        drop(running);
    }
}
