//! MP4 recording with pre/post motion buffers.
//!
//! The recorder maintains a circular buffer of recent keyframes (the
//! "pre-recording" window). When motion triggers recording, it flushes
//! the buffer and continues writing until the post-recording timeout
//! expires.
//!
//! Recordings are written as raw H.264 Annex B byte streams (.h264 files)
//! which can be remuxed to MP4 by the uploader or played directly by
//! ffplay/VLC. This avoids the complexity of MP4 muxing on constrained
//! devices.

use std::collections::VecDeque;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::Utc;
use log::{debug, error, info, warn};
use tokio::sync::{broadcast, watch};

use super::capture::VideoFrame;
use super::config::{CameraConfig, RecordingMode};
use super::events::{CameraEvent, CameraEventKind};

/// Manages recording for a single camera.
pub struct Recorder {
    camera_id: String,
    config: CameraConfig,
    output_dir: PathBuf,
    frame_rx: broadcast::Receiver<VideoFrame>,
    motion_rx: watch::Receiver<bool>,
    event_tx: broadcast::Sender<CameraEvent>,
}

impl Recorder {
    pub fn new(
        camera_id: String,
        config: CameraConfig,
        output_dir: String,
        frame_rx: broadcast::Receiver<VideoFrame>,
        motion_rx: watch::Receiver<bool>,
        event_tx: broadcast::Sender<CameraEvent>,
    ) -> Self {
        let output_dir = PathBuf::from(output_dir).join(&camera_id);
        Self {
            camera_id,
            config,
            output_dir,
            frame_rx,
            motion_rx,
            event_tx,
        }
    }

    /// Run the recording loop.
    pub async fn run(self) {
        if self.config.recording_mode == RecordingMode::Disabled {
            info!("[{}] Recording disabled", self.camera_id);
            return;
        }

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
    async fn run_continuous(mut self) {
        let segment_duration = Duration::from_secs(self.config.max_recording_secs as u64);

        loop {
            let started_at = Utc::now();
            let filename = format!(
                "{}_{}.mp4",
                self.camera_id,
                started_at.format("%Y%m%d_%H%M%S")
            );
            let path = self.output_dir.join(&filename);

            // Emit recording started event
            let _ = self.event_tx.send(CameraEvent {
                camera_id: self.camera_id.clone(),
                kind: CameraEventKind::RecordingStarted {
                    filename: filename.clone(),
                },
            });

            info!("[{}] Recording segment: {}", self.camera_id, filename);

            let segment_start = Instant::now();
            let mut file = match fs::File::create(&path) {
                Ok(f) => f,
                Err(e) => {
                    error!("[{}] Cannot create {}: {}", self.camera_id, path.display(), e);
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };

            // Write frames for segment_duration
            while segment_start.elapsed() < segment_duration {
                match self.frame_rx.recv().await {
                    Ok(frame) => {
                        // Write Annex B start code + NAL data
                        let _ = file.write_all(&[0, 0, 0, 1]);
                        let _ = file.write_all(&frame.data);
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => {
                        info!("[{}] Capture ended, recorder exiting", self.camera_id);
                        return;
                    }
                }
            }

            drop(file);
            let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

            // Emit recording completed event
            let _ = self.event_tx.send(CameraEvent {
                camera_id: self.camera_id.clone(),
                kind: CameraEventKind::RecordingCompleted {
                    filename: filename.clone(),
                    duration_secs: segment_start.elapsed().as_secs_f64(),
                    size_bytes: size,
                },
            });

            info!(
                "[{}] Segment complete: {} ({:.1} KB)",
                self.camera_id,
                filename,
                size as f64 / 1024.0
            );

            if self.config.auto_clean {
                clean_old_recordings(&self.camera_id, &self.output_dir, self.config.max_storage_mb);
            }
        }
    }

    /// Motion-triggered recording with pre/post buffers.
    async fn run_motion_triggered(mut self) {
        let pre_secs = self.config.prerecording_secs as usize;
        let post_secs = Duration::from_secs(self.config.postrecording_secs as u64);
        let max_duration = Duration::from_secs(self.config.max_recording_secs as u64);

        info!(
            "[{}] Motion recording: pre={}s post={}s max={}s",
            self.camera_id,
            pre_secs,
            post_secs.as_secs(),
            max_duration.as_secs()
        );

        // Circular buffer for pre-recording (stores keyframes with their data)
        // Estimate ~1 keyframe per second, keep pre_secs worth
        let pre_buffer_cap = pre_secs.max(1) * 2;
        let mut pre_buffer: VecDeque<Vec<u8>> = VecDeque::with_capacity(pre_buffer_cap);

        loop {
            // Phase 1: Wait for motion, buffering keyframes
            loop {
                tokio::select! {
                    result = self.frame_rx.recv() => {
                        match result {
                            Ok(frame) => {
                                if frame.is_keyframe {
                                    if pre_buffer.len() >= pre_buffer_cap {
                                        pre_buffer.pop_front();
                                    }
                                    pre_buffer.push_back(frame.data);
                                }
                            }
                            Err(broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(broadcast::error::RecvError::Closed) => return,
                        }
                    }
                    _ = self.motion_rx.changed() => {
                        if *self.motion_rx.borrow() {
                            break; // Motion started!
                        }
                    }
                }
            }

            // Phase 2: Record — flush pre-buffer + continue until motion stops + post_secs
            let started_at = Utc::now();
            let filename = format!(
                "{}_{}.mp4",
                self.camera_id,
                started_at.format("%Y%m%d_%H%M%S")
            );
            let path = self.output_dir.join(&filename);

            let mut file = match fs::File::create(&path) {
                Ok(f) => f,
                Err(e) => {
                    error!("[{}] Cannot create {}: {}", self.camera_id, path.display(), e);
                    continue;
                }
            };

            let _ = self.event_tx.send(CameraEvent {
                camera_id: self.camera_id.clone(),
                kind: CameraEventKind::RecordingStarted {
                    filename: filename.clone(),
                },
            });

            info!("[{}] Motion recording started: {}", self.camera_id, filename);

            // Flush pre-buffer
            for buf in pre_buffer.drain(..) {
                let _ = file.write_all(&[0, 0, 0, 1]);
                let _ = file.write_all(&buf);
            }

            let record_start = Instant::now();
            let mut motion_stopped_at: Option<Instant> = None;

            // Phase 3: Write frames until post-recording timeout or max duration
            loop {
                if record_start.elapsed() >= max_duration {
                    info!("[{}] Max recording duration reached", self.camera_id);
                    break;
                }

                if let Some(stopped) = motion_stopped_at {
                    if stopped.elapsed() >= post_secs {
                        debug!("[{}] Post-recording timeout reached", self.camera_id);
                        break;
                    }
                }

                tokio::select! {
                    result = self.frame_rx.recv() => {
                        match result {
                            Ok(frame) => {
                                let _ = file.write_all(&[0, 0, 0, 1]);
                                let _ = file.write_all(&frame.data);
                            }
                            Err(broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(broadcast::error::RecvError::Closed) => break,
                        }
                    }
                    _ = self.motion_rx.changed() => {
                        if *self.motion_rx.borrow() {
                            // Motion resumed
                            motion_stopped_at = None;
                        } else {
                            // Motion stopped, start post-recording countdown
                            motion_stopped_at = Some(Instant::now());
                        }
                    }
                }
            }

            drop(file);
            let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            let duration = record_start.elapsed();

            let _ = self.event_tx.send(CameraEvent {
                camera_id: self.camera_id.clone(),
                kind: CameraEventKind::RecordingCompleted {
                    filename: filename.clone(),
                    duration_secs: duration.as_secs_f64(),
                    size_bytes: size,
                },
            });

            info!(
                "[{}] Motion recording complete: {} ({:.1}s, {:.1} KB)",
                self.camera_id,
                filename,
                duration.as_secs_f64(),
                size as f64 / 1024.0
            );

            if self.config.auto_clean {
                clean_old_recordings(&self.camera_id, &self.output_dir, self.config.max_storage_mb);
            }
        }
    }
}

/// Remove oldest recordings when total size exceeds max_storage_mb.
fn clean_old_recordings(camera_id: &str, dir: &Path, max_storage_mb: u64) {
    let max_bytes = max_storage_mb * 1024 * 1024;

    let mut entries: Vec<(PathBuf, u64, std::time::SystemTime)> = Vec::new();
    let read_dir = match fs::read_dir(dir) {
        Ok(d) => d,
        Err(e) => {
            warn!("[{camera_id}] Cannot read recording dir: {e}");
            return;
        }
    };

    for entry in read_dir.flatten() {
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

    entries.sort_by_key(|(_, _, t)| *t);

    let mut freed = 0u64;
    let need_to_free = total - max_bytes;

    for (path, size, _) in &entries {
        if freed >= need_to_free {
            break;
        }
        if let Err(e) = fs::remove_file(path) {
            warn!("[{camera_id}] Cannot remove {}: {e}", path.display());
        } else {
            debug!("[{camera_id}] Cleaned old recording: {}", path.display());
            freed += size;
        }
    }

    info!(
        "[{camera_id}] Cleaned {:.1} MB of old recordings",
        freed as f64 / 1024.0 / 1024.0
    );
}
