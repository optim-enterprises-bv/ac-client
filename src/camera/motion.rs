//! Frame-differential motion detection.
//!
//! Compares consecutive frames by computing a simple pixel-difference metric
//! on raw NAL data. When the metric exceeds the configured threshold, a motion
//! event is emitted on both the event bus (for MQTT) and the motion watch
//! channel (for the recorder).

use std::time::{Duration, Instant};

use log::{debug, info, warn};
use tokio::sync::{broadcast, watch};

use super::capture::VideoFrame;
use super::events::{CameraEvent, CameraEventKind};

/// Lightweight motion detector operating on raw frame data.
///
/// Emits events to:
/// - `event_tx` (broadcast) — for MQTT bridge, status tracking
/// - `motion_tx` (watch) — for recorder trigger (true = motion active)
pub struct MotionDetector {
    camera_id: String,
    rx: broadcast::Receiver<VideoFrame>,
    threshold: u32,
    event_tx: broadcast::Sender<CameraEvent>,
    motion_tx: watch::Sender<bool>,
}

impl MotionDetector {
    pub fn new(
        camera_id: String,
        rx: broadcast::Receiver<VideoFrame>,
        threshold: u32,
        event_tx: broadcast::Sender<CameraEvent>,
        motion_tx: watch::Sender<bool>,
    ) -> Self {
        Self {
            camera_id,
            rx,
            threshold,
            event_tx,
            motion_tx,
        }
    }

    /// Run the motion detection loop.
    pub async fn run(mut self) {
        info!(
            "[{}] Motion detector started (threshold={})",
            self.camera_id, self.threshold
        );

        let mut prev_frame: Option<Vec<u8>> = None;
        let mut in_motion = false;
        let mut motion_start = Instant::now();
        let mut last_motion = Instant::now();
        let mut peak_change: f32 = 0.0;
        let cooldown = Duration::from_secs(3);

        loop {
            match self.rx.recv().await {
                Ok(frame) => {
                    if let Some(ref prev) = prev_frame {
                        let change = frame_difference(prev, &frame.data);

                        if change > self.threshold as f32 {
                            if !in_motion {
                                in_motion = true;
                                motion_start = Instant::now();
                                peak_change = change;

                                // Notify recorder
                                let _ = self.motion_tx.send(true);

                                // Emit event
                                let _ = self.event_tx.send(CameraEvent {
                                    camera_id: self.camera_id.clone(),
                                    kind: CameraEventKind::MotionStarted {
                                        change_pct: change,
                                    },
                                });

                                info!(
                                    "[{}] Motion started (change={:.1}%)",
                                    self.camera_id, change
                                );
                            }
                            if change > peak_change {
                                peak_change = change;
                            }
                            last_motion = Instant::now();
                        } else if in_motion && last_motion.elapsed() > cooldown {
                            in_motion = false;
                            let duration = motion_start.elapsed();

                            // Notify recorder
                            let _ = self.motion_tx.send(false);

                            // Emit event
                            let _ = self.event_tx.send(CameraEvent {
                                camera_id: self.camera_id.clone(),
                                kind: CameraEventKind::MotionStopped {
                                    duration_secs: duration.as_secs_f64(),
                                    peak_change,
                                },
                            });

                            info!(
                                "[{}] Motion ended (duration={:.1}s, peak={:.1}%)",
                                self.camera_id,
                                duration.as_secs_f64(),
                                peak_change
                            );

                            peak_change = 0.0;
                        }
                    }

                    // Only keep keyframes as reference to reduce CPU usage
                    if frame.is_keyframe {
                        prev_frame = Some(frame.data);
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(
                        "[{}] Motion detector lagged, skipped {n} frames",
                        self.camera_id
                    );
                }
                Err(broadcast::error::RecvError::Closed) => {
                    debug!(
                        "[{}] Capture channel closed, motion detector exiting",
                        self.camera_id
                    );
                    return;
                }
            }
        }
    }
}

/// Compute a simple difference metric between two frame buffers.
///
/// Returns a percentage (0.0–100.0) representing how different the frames are.
/// This operates on raw NAL bytes — it's a rough heuristic, not true pixel
/// comparison. Keyframe-to-keyframe comparison is most meaningful.
fn frame_difference(a: &[u8], b: &[u8]) -> f32 {
    let len = a.len().min(b.len());
    if len == 0 {
        return 0.0;
    }

    // Sample every Nth byte to keep CPU usage low
    let step = (len / 4096).max(1);
    let mut diff_sum: u64 = 0;
    let mut samples: u64 = 0;

    let mut i = 0;
    while i < len {
        let d = (a[i] as i32 - b[i] as i32).unsigned_abs();
        diff_sum += d as u64;
        samples += 1;
        i += step;
    }

    if samples == 0 {
        return 0.0;
    }

    // Normalize to percentage (255 = max byte difference)
    (diff_sum as f64 / (samples as f64 * 255.0) * 100.0) as f32
}
