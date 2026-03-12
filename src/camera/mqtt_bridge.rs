//! MQTT event bridge — publishes camera events to EMQX.
//!
//! Subscribes to the camera event bus and publishes JSON messages for:
//! - Motion start/stop
//! - Recording start/complete
//! - Camera connect/disconnect
//!
//! Topic format: `{prefix}/{camera_id}/{event_type}`
//! e.g. `kerberos/cam0/motion`, `kerberos/cam0/recording`

use std::sync::Arc;
use std::time::Duration;

use log::{debug, error, info, warn};
use rumqttc::{AsyncClient, MqttOptions, QoS};
use serde::Serialize;
use tokio::sync::broadcast;

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

/// MQTT bridge that forwards camera events to the broker.
pub struct MqttBridge {
    topic_prefix: String,
    mqtt_uri: String,
    event_rx: broadcast::Receiver<CameraEvent>,
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
        }
    }

    pub async fn run(mut self) {
        info!(
            "Camera MQTT bridge starting (prefix={}, broker={})",
            self.topic_prefix, self.mqtt_uri
        );

        let client_id = format!("ac-camera-{}", uuid::Uuid::new_v4().as_simple());
        let (host, port) = parse_mqtt_uri(&self.mqtt_uri);

        let mut opts = MqttOptions::new(&client_id, &host, port);
        opts.set_keep_alive(Duration::from_secs(30));
        opts.set_max_packet_size(64 * 1024, 64 * 1024);

        let (client, mut eventloop) = AsyncClient::new(opts, 64);
        let client = Arc::new(client);

        // Spawn MQTT event loop
        let mqtt_client = Arc::clone(&client);
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
                duration_secs,
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
            CameraEventKind::Disconnected { error } => {
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

/// Parse mqtt://host:port or tcp://host:port into (host, port).
fn parse_mqtt_uri(uri: &str) -> (String, u16) {
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
