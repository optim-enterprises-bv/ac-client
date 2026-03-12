//! Camera manager — orchestrates per-camera subsystems.
//!
//! The [`CameraManager`] loads configuration, spawns a task set per camera,
//! creates the shared event bus, MQTT bridge, and live stream server.

use std::collections::HashMap;
use std::sync::Arc;

use log::{error, info, warn};
use tokio::sync::{broadcast, watch, RwLock};
use tokio::task::JoinHandle;

use super::capture::CaptureSession;
use super::config::{self, CameraConfig, CameraGlobalConfig, RecordingMode};
use super::events::CameraEvent;
use super::live_stream::LiveStreamServer;
use super::motion::MotionDetector;
use super::mqtt_bridge::MqttBridge;
use super::recording::Recorder;
use super::storage::VaultUploader;

/// Runtime state for a single camera.
struct CameraSubsystem {
    config: CameraConfig,
    tasks: Vec<JoinHandle<()>>,
    motion_rx: watch::Receiver<bool>,
}

/// Status snapshot for a single camera (exposed to USP / API).
#[derive(Debug, Clone)]
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
        let camera_configs = config::load_cameras();

        if camera_configs.is_empty() {
            info!("No cameras configured in UCI");
            return;
        }

        info!("Starting {} camera(s)", camera_configs.len());

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
            info!("Camera MQTT bridge started");
        }

        // Start live stream server if configured
        if let Some(ref server) = self.live_server {
            let srv = Arc::clone(server);
            tokio::spawn(async move {
                srv.run().await;
            });
        }

        let mut cameras = self.cameras.write().await;

        for (id, cam_cfg) in camera_configs {
            if !cam_cfg.enabled {
                info!("[{}] Camera disabled, skipping", id);
                continue;
            }

            match self.spawn_camera(&cam_cfg).await {
                Ok(subsystem) => {
                    info!(
                        "[{}] Camera subsystem started ({} tasks)",
                        id,
                        subsystem.tasks.len()
                    );
                    cameras.insert(id, subsystem);
                }
                Err(e) => {
                    error!("[{}] Failed to start camera: {}", id, e);
                }
            }
        }
    }

    async fn spawn_camera(&self, cfg: &CameraConfig) -> anyhow::Result<CameraSubsystem> {
        let mut tasks = Vec::new();
        let id = cfg.id.clone();

        if cfg.rtsp_url.is_empty() {
            anyhow::bail!("No RTSP URL configured for camera {id}");
        }

        let (motion_tx, motion_rx) = watch::channel(false);

        // ── Main stream capture ──────────────────────────────────────────
        let (main_capture, _main_rx) = CaptureSession::new(
            format!("{id}/main"),
            cfg.rtsp_url.clone(),
        );
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
            let (sub_capture, sub_rx) = CaptureSession::new(
                format!("{id}/sub"),
                cfg.sub_rtsp_url.clone(),
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

    pub async fn stop(&self) {
        let mut cameras = self.cameras.write().await;
        for (id, sub) in cameras.drain() {
            info!("[{id}] Stopping camera subsystem");
            for task in sub.tasks {
                task.abort();
            }
        }
    }
}
