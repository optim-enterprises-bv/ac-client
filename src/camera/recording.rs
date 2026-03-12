//! MP4 recording with pre/post motion buffers.
//!
//! The recorder maintains a circular buffer of recent keyframes (the
//! "pre-recording" window). When motion triggers recording, it flushes
//! the buffer and continues writing until the post-recording timeout
//! expires.
//!
//! Recordings are written as fragmented MP4 (fMP4) files using the `mp4`
//! crate, making them streamable and resilient to incomplete writes.

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::Utc;
use log::{debug, error, info, warn};

use super::config::{CameraConfig, RecordingMode};

/// A single recording segment on disk.
#[derive(Debug)]
pub struct Recording {
    pub camera_id: String,
    pub path: PathBuf,
    pub started_at: chrono::DateTime<Utc>,
    pub duration: Duration,
    pub size_bytes: u64,
}

/// Manages recording for a single camera.
pub struct Recorder {
    camera_id: String,
    config: CameraConfig,
    output_dir: PathBuf,
}

impl Recorder {
    pub fn new(camera_id: String, config: CameraConfig, output_dir: String) -> Self {
        let output_dir = PathBuf::from(output_dir).join(&camera_id);
        Self {
            camera_id,
            config,
            output_dir,
        }
    }

    /// Run the recording loop.
    ///
    /// In `Motion` mode: waits for motion events, writes pre+post buffer.
    /// In `Continuous` mode: always records, splitting into segments.
    /// In `Disabled` mode: returns immediately.
    pub async fn run(&self) {
        if self.config.recording_mode == RecordingMode::Disabled {
            info!("[{}] Recording disabled", self.camera_id);
            return;
        }

        // Ensure output directory exists
        if let Err(e) = fs::create_dir_all(&self.output_dir) {
            error!(
                "[{}] Cannot create recording dir {}: {}",
                self.camera_id,
                self.output_dir.display(),
                e
            );
            return;
        }

        info!(
            "[{}] Recorder started (mode={:?}, dir={})",
            self.camera_id, self.config.recording_mode, self.output_dir.display()
        );

        match self.config.recording_mode {
            RecordingMode::Continuous => self.run_continuous().await,
            RecordingMode::Motion => self.run_motion_triggered().await,
            RecordingMode::Disabled => unreachable!(),
        }
    }

    /// Continuous recording — split into segments of max_recording_secs.
    async fn run_continuous(&self) {
        let segment_duration = Duration::from_secs(self.config.max_recording_secs as u64);

        loop {
            let started_at = Utc::now();
            let filename = format!(
                "{}_{}.mp4",
                self.camera_id,
                started_at.format("%Y%m%d_%H%M%S")
            );
            let path = self.output_dir.join(&filename);

            info!("[{}] Recording segment: {}", self.camera_id, filename);

            // Record for segment_duration
            // TODO: wire up actual frame writing from CaptureSession broadcast
            tokio::time::sleep(segment_duration).await;

            let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            info!(
                "[{}] Segment complete: {} ({} bytes)",
                self.camera_id, filename, size
            );

            // Auto-clean if storage limit exceeded
            if self.config.auto_clean {
                self.clean_old_recordings().await;
            }
        }
    }

    /// Motion-triggered recording with pre/post buffers.
    async fn run_motion_triggered(&self) {
        let pre_secs = Duration::from_secs(self.config.prerecording_secs as u64);
        let post_secs = Duration::from_secs(self.config.postrecording_secs as u64);
        let max_duration = Duration::from_secs(self.config.max_recording_secs as u64);

        info!(
            "[{}] Motion recording: pre={}s post={}s max={}s",
            self.camera_id,
            pre_secs.as_secs(),
            post_secs.as_secs(),
            max_duration.as_secs()
        );

        // TODO: subscribe to motion events and CaptureSession frames
        // For now, this is a placeholder that waits for the motion/capture
        // integration to be wired up.
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    /// Remove oldest recordings when total size exceeds max_storage_mb.
    async fn clean_old_recordings(&self) {
        let max_bytes = self.config.max_storage_mb * 1024 * 1024;

        let mut entries: Vec<(PathBuf, u64, std::time::SystemTime)> = Vec::new();
        let dir = match fs::read_dir(&self.output_dir) {
            Ok(d) => d,
            Err(e) => {
                warn!("[{}] Cannot read recording dir: {}", self.camera_id, e);
                return;
            }
        };

        for entry in dir.flatten() {
            if let Ok(meta) = entry.metadata() {
                if meta.is_file() {
                    let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                    entries.push((entry.path(), meta.len(), modified));
                }
            }
        }

        let total: u64 = entries.iter().map(|(_, s, _)| s).sum();
        if total <= max_bytes {
            return;
        }

        // Sort oldest first
        entries.sort_by_key(|(_, _, t)| *t);

        let mut freed = 0u64;
        let need_to_free = total - max_bytes;

        for (path, size, _) in &entries {
            if freed >= need_to_free {
                break;
            }
            if let Err(e) = fs::remove_file(path) {
                warn!("[{}] Cannot remove {}: {}", self.camera_id, path.display(), e);
            } else {
                debug!("[{}] Cleaned old recording: {}", self.camera_id, path.display());
                freed += size;
            }
        }

        info!(
            "[{}] Cleaned {:.1} MB of old recordings",
            self.camera_id,
            freed as f64 / 1024.0 / 1024.0
        );
    }
}
