//! Frame-differential motion detection.
//!
//! Compares consecutive frames by computing a simple pixel-difference metric
//! on decoded grayscale thumbnails. When the metric exceeds the configured
//! threshold, a motion event is emitted.
//!
//! This is intentionally simple — matching the Kerberos Agent's approach of
//! pixel-level delta with a configurable threshold. More sophisticated
//! algorithms (background subtraction, optical flow) can be added later.

use std::time::{Duration, Instant};

use log::{debug, info, warn};
use tokio::sync::broadcast;

use super::capture::VideoFrame;

/// Motion event emitted when motion is detected or stops.
#[derive(Debug, Clone)]
pub struct MotionEvent {
    pub camera_id: String,
    pub started_at: Instant,
    pub ended_at: Option<Instant>,
    /// Peak change percentage during this event.
    pub peak_change: f32,
}

/// Lightweight motion detector operating on raw frame data.
///
/// Since we receive raw NAL units (not decoded pixels), this detector uses
/// a simple heuristic: compare the byte-level entropy/difference between
/// consecutive frames. For proper pixel-level detection, frames would need
/// to be decoded first — that's a future enhancement.
pub struct MotionDetector {
    camera_id: String,
    rx: broadcast::Receiver<VideoFrame>,
    threshold: u32,
}

impl MotionDetector {
    pub fn new(
        camera_id: String,
        rx: broadcast::Receiver<VideoFrame>,
        threshold: u32,
    ) -> Self {
        Self {
            camera_id,
            rx,
            threshold,
        }
    }

    /// Run the motion detection loop.
    pub async fn run(mut self) {
        info!("[{}] Motion detector started (threshold={})", self.camera_id, self.threshold);

        let mut prev_frame: Option<Vec<u8>> = None;
        let mut in_motion = false;
        let mut motion_start = Instant::now();
        let mut last_motion = Instant::now();
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
                                info!(
                                    "[{}] Motion started (change={:.1}%)",
                                    self.camera_id, change
                                );
                            }
                            last_motion = Instant::now();
                        } else if in_motion && last_motion.elapsed() > cooldown {
                            in_motion = false;
                            let duration = motion_start.elapsed();
                            info!(
                                "[{}] Motion ended (duration={:.1}s)",
                                self.camera_id,
                                duration.as_secs_f64()
                            );
                        }
                    }

                    // Only keep keyframes as reference to reduce CPU usage
                    if frame.is_keyframe {
                        prev_frame = Some(frame.data);
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!("[{}] Motion detector lagged, skipped {n} frames", self.camera_id);
                }
                Err(broadcast::error::RecvError::Closed) => {
                    debug!("[{}] Capture channel closed, motion detector exiting", self.camera_id);
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
