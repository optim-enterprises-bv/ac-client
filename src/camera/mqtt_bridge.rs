//! MQTT event & live stream bridge — publishes camera events and H.264 frames to EMQX.
//!
//! Subscribes to the camera event bus and publishes JSON messages for:
//! - Motion start/stop
//! - Recording start/complete
//! - Camera connect/disconnect
//!
//! Also publishes ALL live H.264 frames (keyframes + P-frames) for each
//! camera so that the server can reassemble a full video stream.  Each
//! frame is prefixed with a 1-byte type header (0x01 = key, 0x00 = P).
//!
//! Topic format:
//!   `{prefix}/{camera_id}/{event_type}` — JSON events
//!   `{prefix}/{camera_id}/live`          — 1-byte header + raw H.264 frame bytes

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use log::{debug, info, warn};
use rumqttc::{AsyncClient, MqttOptions, QoS};
use serde::Serialize;
use tokio::sync::broadcast;

use super::capture::VideoFrame;
use super::events::{CameraEvent, CameraEventKind};

/// JSON payload for motion events.
#[derive(Serialize)]
struct MotionPayload {
    camera_id: String,
    event: String,
    change_pct: f32,
    timestamp: String,
}

/// JSON payload for recording events.
#[derive(Serialize)]
struct RecordingPayload {
    camera_id: String,
    event: String,
    filename: String,
    duration_secs: f64,
    size_bytes: u64,
    timestamp: String,
}

/// JSON payload for connection events.
#[derive(Serialize)]
struct ConnectionPayload {
    camera_id: String,
    event: String,
    timestamp: String,
}

/// A camera stream registered for live publishing.
pub struct LiveStream {
    pub camera_id: String,
    pub frame_rx: broadcast::Receiver<VideoFrame>,
}

/// MQTT bridge that forwards camera events and live frames to the broker.
pub struct MqttBridge {
    topic_prefix: String,
    mqtt_uri: String,
    event_rx: broadcast::Receiver<CameraEvent>,
    live_streams: Vec<LiveStream>,
}

impl MqttBridge {
    pub fn new(
        topic_prefix: String,
        mqtt_uri: String,
        event_rx: broadcast::Receiver<CameraEvent>,
    ) -> Self {
        Self {
            topic_prefix,
            mqtt_uri,
            event_rx,
            live_streams: Vec::new(),
        }
    }

    /// Register a camera's frame broadcast for live MQTT publishing.
    pub fn add_live_stream(&mut self, camera_id: String, frame_rx: broadcast::Receiver<VideoFrame>) {
        self.live_streams.push(LiveStream { camera_id, frame_rx });
    }

    pub async fn run(mut self) {
        info!(
            "Camera MQTT bridge starting (prefix={}, broker={}, live_streams={})",
            self.topic_prefix, self.mqtt_uri, self.live_streams.len()
        );

        let client_id = format!("ac-camera-{}", uuid::Uuid::new_v4().as_simple());
        let (host, port) = parse_mqtt_uri(&self.mqtt_uri);

        let mut opts = MqttOptions::new(&client_id, &host, port);
        opts.set_keep_alive(Duration::from_secs(30));
        // Allow up to 300KB packets for H.264 frames (250KB max frame + header + overhead)
        opts.set_max_packet_size(300 * 1024, 300 * 1024);

        let (client, mut eventloop) = AsyncClient::new(opts, 128);
        let client = Arc::new(client);

        // Spawn MQTT event loop
        tokio::spawn(async move {
            loop {
                match eventloop.poll().await {
                    Ok(_) => {}
                    Err(e) => {
                        warn!("Camera MQTT connection error: {e}");
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
            }
        });

        info!("Camera MQTT bridge connected");

        // Spawn live frame publishers for each camera
        let prefix = self.topic_prefix.clone();
        for stream in self.live_streams.drain(..) {
            let client = Arc::clone(&client);
            let topic = format!("{}/{}/live", prefix, stream.camera_id);
            tokio::spawn(publish_live_frames(client, topic, stream.camera_id, stream.frame_rx));
        }

        // Forward events to MQTT
        loop {
            match self.event_rx.recv().await {
                Ok(event) => {
                    if let Err(e) = self.publish_event(&client, &event).await {
                        warn!("Failed to publish camera event: {e}");
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!("Camera MQTT bridge lagged, skipped {n} events");
                }
                Err(broadcast::error::RecvError::Closed) => {
                    info!("Camera event bus closed, MQTT bridge exiting");
                    return;
                }
            }
        }
    }

    async fn publish_event(
        &self,
        client: &AsyncClient,
        event: &CameraEvent,
    ) -> anyhow::Result<()> {
        let now = chrono::Utc::now().to_rfc3339();

        let (subtopic, payload) = match &event.kind {
            CameraEventKind::MotionStarted { change_pct } => {
                let p = serde_json::to_string(&MotionPayload {
                    camera_id: event.camera_id.clone(),
                    event: "motion_started".into(),
                    change_pct: *change_pct,
                    timestamp: now,
                })?;
                ("motion", p)
            }
            CameraEventKind::MotionStopped {
                duration_secs: _,
                peak_change,
            } => {
                let p = serde_json::to_string(&MotionPayload {
                    camera_id: event.camera_id.clone(),
                    event: "motion_stopped".into(),
                    change_pct: *peak_change,
                    timestamp: now,
                })?;
                ("motion", p)
            }
            CameraEventKind::RecordingStarted { filename } => {
                let p = serde_json::to_string(&RecordingPayload {
                    camera_id: event.camera_id.clone(),
                    event: "recording_started".into(),
                    filename: filename.clone(),
                    duration_secs: 0.0,
                    size_bytes: 0,
                    timestamp: now,
                })?;
                ("recording", p)
            }
            CameraEventKind::RecordingCompleted {
                filename,
                duration_secs,
                size_bytes,
            } => {
                let p = serde_json::to_string(&RecordingPayload {
                    camera_id: event.camera_id.clone(),
                    event: "recording_completed".into(),
                    filename: filename.clone(),
                    duration_secs: *duration_secs,
                    size_bytes: *size_bytes,
                    timestamp: now,
                })?;
                ("recording", p)
            }
            CameraEventKind::Connected => {
                let p = serde_json::to_string(&ConnectionPayload {
                    camera_id: event.camera_id.clone(),
                    event: "connected".into(),
                    timestamp: now,
                })?;
                ("status", p)
            }
            CameraEventKind::Disconnected { error: _ } => {
                let p = serde_json::to_string(&ConnectionPayload {
                    camera_id: event.camera_id.clone(),
                    event: "disconnected".into(),
                    timestamp: now,
                })?;
                ("status", p)
            }
        };

        let topic = format!("{}/{}/{}", self.topic_prefix, event.camera_id, subtopic);
        debug!("Publishing camera event: {} → {}", topic, payload);

        client
            .publish(&topic, QoS::AtLeastOnce, false, payload.as_bytes())
            .await?;

        Ok(())
    }
}

/// Publish ALL live H.264 frames for a single camera.
///
/// Every frame (keyframes and P-frames) is published so the server can
/// reassemble a full H.264 stream.  A 1-byte header is prepended:
///   0x01 = keyframe (IDR), 0x00 = P-frame / non-key.
/// Frames larger than 250KB are skipped to avoid MQTT packet size issues.
async fn publish_live_frames(
    client: Arc<AsyncClient>,
    topic: String,
    camera_id: String,
    mut frame_rx: broadcast::Receiver<VideoFrame>,
) {
    info!("[{camera_id}] Live MQTT stream publisher started → {topic}");

    let max_frame_size = 250 * 1024; // 250KB max per MQTT message

    loop {
        match frame_rx.recv().await {
            Ok(frame) => {
                if frame.data.len() > max_frame_size {
                    debug!(
                        "[{camera_id}] Skipping oversized frame: {}KB",
                        frame.data.len() / 1024
                    );
                    continue;
                }

                // Prepend 1-byte header: 0x01 = keyframe, 0x00 = P-frame
                let header: u8 = if frame.is_keyframe { 0x01 } else { 0x00 };
                let mut payload = Vec::with_capacity(1 + frame.data.len());
                payload.push(header);
                payload.extend_from_slice(&frame.data);

                // QoS 0 — fire and forget, lowest latency
                if let Err(e) = client
                    .publish(&topic, QoS::AtMostOnce, false, payload)
                    .await
                {
                    debug!("[{camera_id}] Live frame publish failed: {e}");
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                debug!("[{camera_id}] Live stream lagged, skipped {n} frames");
            }
            Err(broadcast::error::RecvError::Closed) => {
                info!("[{camera_id}] Live stream closed");
                return;
            }
        }
    }
}

/// Parse mqtt://host:port or tcp://host:port into (host, port).
pub fn parse_mqtt_uri(uri: &str) -> (String, u16) {
    let stripped = uri
        .trim_start_matches("mqtt://")
        .trim_start_matches("mqtts://")
        .trim_start_matches("tcp://");

    if let Some((host, port_str)) = stripped.rsplit_once(':') {
        let port = port_str.parse().unwrap_or(1883);
        (host.to_string(), port)
    } else {
        (stripped.to_string(), 1883)
    }
}
