//! Camera manager — orchestrates per-camera subsystems.
//!
//! The [`CameraManager`] loads configuration, spawns a task set per camera,
//! and exposes a handle for the USP agent to query status and trigger actions.

use std::collections::HashMap;
use std::sync::Arc;

use log::{error, info, warn};
use tokio::sync::RwLock;
use tokio::task::JoinHandle;

use super::capture::CaptureSession;
use super::config::{self, CameraConfig, CameraGlobalConfig, RecordingMode};
use super::motion::MotionDetector;
use super::recording::Recorder;
use super::storage::VaultUploader;

/// Runtime state for a single camera.
struct CameraSubsystem {
    config: CameraConfig,
    /// Task handles for graceful shutdown.
    tasks: Vec<JoinHandle<()>>,
}

/// Status snapshot for a single camera (exposed to USP / API).
#[derive(Debug, Clone)]
pub struct CameraStatus {
    pub id: String,
    pub name: String,
    pub connected: bool,
    pub recording: bool,
    pub motion_active: bool,
    pub recordings_count: u64,
}

/// Manages all camera subsystems.
pub struct CameraManager {
    global: CameraGlobalConfig,
    cameras: Arc<RwLock<HashMap<String, CameraSubsystem>>>,
}

impl CameraManager {
    /// Load config from UCI and create the manager (does not start cameras yet).
    pub fn new() -> Self {
        let global = config::load_global_config();
        Self {
            global,
            cameras: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Start all enabled cameras. Call this once from main.
    pub async fn start(&self) {
        let camera_configs = config::load_cameras();

        if camera_configs.is_empty() {
            info!("No cameras configured in UCI");
            return;
        }

        info!("Starting {} camera(s)", camera_configs.len());

        let mut cameras = self.cameras.write().await;

        for (id, cam_cfg) in camera_configs {
            if !cam_cfg.enabled {
                info!("[{}] Camera disabled, skipping", id);
                continue;
            }

            match self.spawn_camera(&cam_cfg).await {
                Ok(subsystem) => {
                    info!("[{}] Camera subsystem started ({} tasks)", id, subsystem.tasks.len());
                    cameras.insert(id, subsystem);
                }
                Err(e) => {
                    error!("[{}] Failed to start camera: {}", id, e);
                }
            }
        }
    }

    /// Spawn the task set for a single camera.
    async fn spawn_camera(&self, cfg: &CameraConfig) -> anyhow::Result<CameraSubsystem> {
        let mut tasks = Vec::new();
        let id = cfg.id.clone();

        // Choose the stream URL — use sub stream for motion detection if available,
        // main stream for recording.
        let capture_url = if cfg.rtsp_url.is_empty() {
            anyhow::bail!("No RTSP URL configured for camera {id}");
        } else {
            cfg.rtsp_url.clone()
        };

        // Main stream capture (high-res, used for recording)
        let (main_capture, main_rx) = CaptureSession::new(
            format!("{id}/main"),
            capture_url,
        );

        let main_handle = tokio::spawn(async move {
            main_capture.run().await;
        });
        tasks.push(main_handle);

        // Sub stream capture (low-res, used for motion detection) — optional
        let motion_rx = if !cfg.sub_rtsp_url.is_empty() {
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
            // Fall back to main stream for motion detection
            main_rx
        };

        // Motion detector (reads low-res frames, emits motion events)
        let motion_enabled = cfg.recording_mode == RecordingMode::Motion;
        if motion_enabled {
            let detector = MotionDetector::new(
                id.clone(),
                motion_rx,
                cfg.pixel_threshold,
            );
            let motion_handle = tokio::spawn(async move {
                detector.run().await;
            });
            tasks.push(motion_handle);
        }

        // Recorder — subscribes to main stream, triggered by motion or continuous
        let recording_dir = self.global.recording_dir.clone();
        let rec_cfg = cfg.clone();
        let recorder = Recorder::new(
            id.clone(),
            rec_cfg,
            recording_dir.clone(),
        );
        let rec_handle = tokio::spawn(async move {
            recorder.run().await;
        });
        tasks.push(rec_handle);

        // Vault uploader — watches recording_dir for completed files
        if !self.global.vault_uri.is_empty() {
            let uploader = VaultUploader::new(
                id.clone(),
                self.global.vault_uri.clone(),
                self.global.vault_access_key.clone(),
                self.global.vault_secret_key.clone(),
                recording_dir,
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
        })
    }

    /// Get a status snapshot for all cameras.
    pub async fn status(&self) -> Vec<CameraStatus> {
        let cameras = self.cameras.read().await;
        cameras
            .iter()
            .map(|(id, sub)| CameraStatus {
                id: id.clone(),
                name: sub.config.name.clone(),
                connected: sub.tasks.iter().all(|t| !t.is_finished()),
                recording: sub.config.recording_mode != RecordingMode::Disabled,
                motion_active: false, // TODO: wire up motion state
                recordings_count: 0,  // TODO: count files
            })
            .collect()
    }

    /// Gracefully stop all cameras.
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
