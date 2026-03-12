//! RTSP stream capture using the `retina` crate (pure Rust).
//!
//! Each camera gets its own [`CaptureSession`] which connects to the RTSP URL,
//! demuxes H.264/H.265 video frames, and pushes them into a bounded async channel
//! for downstream consumers (motion detector, recorder).

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use log::{debug, error, info, warn};
use retina::client::{SessionGroup, SetupOptions};
use retina::codec::CodecItem;
use tokio::sync::broadcast;

use super::events::{CameraEvent, CameraEventKind};

/// A single video frame extracted from an RTSP stream.
#[derive(Debug, Clone)]
pub struct VideoFrame {
    /// Frame timestamp relative to stream start (clock ticks converted to seconds).
    #[allow(dead_code)]
    pub pts_secs: f64,
    /// Wall-clock time when this frame was received.
    #[allow(dead_code)]
    pub received_at: Instant,
    /// Raw NAL unit data (H.264 or H.265).
    pub data: Vec<u8>,
    /// Whether this is a keyframe (IDR).
    pub is_keyframe: bool,
}

/// RTSP capture session for a single camera stream.
pub struct CaptureSession {
    /// Camera identifier.
    camera_id: String,
    /// RTSP URL.
    url: String,
    /// Broadcast sender — multiple consumers (motion, recorder) subscribe.
    tx: broadcast::Sender<VideoFrame>,
    /// Event bus for connection status events.
    event_tx: Option<broadcast::Sender<CameraEvent>>,
}

impl CaptureSession {
    /// Create a new capture session.
    ///
    /// Returns the session and a broadcast receiver for subscribing to frames.
    pub fn new(camera_id: String, url: String) -> (Self, broadcast::Receiver<VideoFrame>) {
        // Buffer 120 frames (~4 seconds at 30fps) to absorb consumer backpressure.
        let (tx, rx) = broadcast::channel(120);
        (Self { camera_id, url, tx, event_tx: None }, rx)
    }

    /// Set the event bus for emitting connection status events.
    pub fn with_event_tx(mut self, event_tx: broadcast::Sender<CameraEvent>) -> Self {
        self.event_tx = Some(event_tx);
        self
    }

    fn emit_event(&self, kind: CameraEventKind) {
        if let Some(ref tx) = self.event_tx {
            let _ = tx.send(CameraEvent {
                camera_id: self.camera_id.clone(),
                kind,
            });
        }
    }

    /// Get a clone of the broadcast sender (for registering with live stream server).
    pub fn sender(&self) -> &broadcast::Sender<VideoFrame> {
        &self.tx
    }

    /// Run the RTSP capture loop. Reconnects on failure with backoff.
    /// This method runs indefinitely until the task is cancelled.
    pub async fn run(&self) {
        let mut backoff = Duration::from_secs(1);
        let max_backoff = Duration::from_secs(60);

        loop {
            info!("[{}] Connecting to RTSP: {}", self.camera_id, self.url);

            match self.capture_stream().await {
                Ok(()) => {
                    warn!("[{}] RTSP stream ended cleanly", self.camera_id);
                    self.emit_event(CameraEventKind::Disconnected { error: None });
                    backoff = Duration::from_secs(1);
                }
                Err(e) => {
                    error!("[{}] RTSP capture error: {}", self.camera_id, e);
                    self.emit_event(CameraEventKind::Disconnected {
                        error: Some(e.to_string()),
                    });
                }
            }

            info!(
                "[{}] Reconnecting in {}s...",
                self.camera_id,
                backoff.as_secs()
            );
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(max_backoff);
        }
    }

    /// Connect to RTSP and process frames until disconnection or error.
    async fn capture_stream(&self) -> anyhow::Result<()> {
        let parsed_url = url::Url::parse(&self.url)?;
        let session_group = Arc::new(SessionGroup::default());

        let mut session = retina::client::Session::describe(
            parsed_url,
            retina::client::SessionOptions::default()
                .session_group(session_group),
        )
        .await?;

        // Find the first video stream.
        let video_idx = session
            .streams()
            .iter()
            .position(|s| s.media() == "video")
            .ok_or_else(|| anyhow::anyhow!("No video stream found in RTSP session"))?;

        session
            .setup(video_idx, SetupOptions::default())
            .await?;

        let playing = session.play(retina::client::PlayOptions::default()).await?;
        let mut demuxed = playing.demuxed()?;

        info!(
            "[{}] RTSP connected, receiving frames",
            self.camera_id
        );
        self.emit_event(CameraEventKind::Connected);

        let start = Instant::now();

        while let Some(item) = demuxed.next().await {
            match item? {
                CodecItem::VideoFrame(f) => {
                    let is_keyframe = f.is_random_access_point();
                    let pts_secs = f.timestamp().elapsed_secs();
                    let frame = VideoFrame {
                        pts_secs,
                        received_at: Instant::now(),
                        data: f.into_data(),
                        is_keyframe,
                    };

                    // Broadcast to all subscribers; ignore send errors (no subscribers).
                    let _ = self.tx.send(frame);
                }
                CodecItem::AudioFrame(_) | _ => {
                    // Non-video frames are ignored.
                }
            }
        }

        debug!(
            "[{}] Stream ran for {:.1}s",
            self.camera_id,
            start.elapsed().as_secs_f64()
        );

        Ok(())
    }
}
