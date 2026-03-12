//! MQTT control channel — receives PTZ and other commands from the server.
//!
//! Subscribes to `{prefix}/{camera_id}/ptz` topics and executes ONVIF PTZ
//! commands on the local camera. This allows the server (which cannot reach
//! cameras behind NAT) to relay user PTZ requests.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use log::{debug, info, warn};
use rumqttc::{AsyncClient, MqttOptions, QoS, Event, Packet};
use serde::Deserialize;
use tokio::sync::RwLock;

/// PTZ command received from the server.
#[derive(Debug, Deserialize)]
pub struct PtzCommand {
    pub action: String,       // "move", "stop", "preset"
    #[serde(default)]
    pub direction: String,    // "up", "down", "left", "right", "zoom_in", "zoom_out"
    #[serde(default)]
    pub speed: f32,           // 0.0-1.0
    #[serde(default)]
    pub preset: u32,          // preset number for "preset" action
}

/// Camera ONVIF info needed for PTZ control.
pub struct CameraOnvifInfo {
    pub onvif_xaddr: String,
    pub username: String,
    pub password: String,
}

/// MQTT control channel that listens for PTZ commands.
pub struct MqttControl {
    topic_prefix: String,
    mqtt_uri: String,
    cameras: Arc<RwLock<HashMap<String, CameraOnvifInfo>>>,
}

impl MqttControl {
    pub fn new(
        topic_prefix: String,
        mqtt_uri: String,
    ) -> Self {
        Self {
            topic_prefix,
            mqtt_uri,
            cameras: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register a camera for PTZ control.
    pub async fn add_camera(&self, camera_id: String, info: CameraOnvifInfo) {
        self.cameras.write().await.insert(camera_id, info);
    }

    pub async fn run(self) {
        let client_id = format!("ac-control-{}", uuid::Uuid::new_v4().as_simple());
        let (host, port) = super::mqtt_bridge::parse_mqtt_uri(&self.mqtt_uri);

        let mut opts = MqttOptions::new(&client_id, &host, port);
        opts.set_keep_alive(Duration::from_secs(30));

        let (client, mut eventloop) = AsyncClient::new(opts, 64);

        // Subscribe to PTZ topics for all cameras
        let topic = format!("{}/+/ptz", self.topic_prefix);
        if let Err(e) = client.subscribe(&topic, QoS::AtLeastOnce).await {
            warn!("Failed to subscribe to PTZ topic: {e}");
            return;
        }
        info!("MQTT control channel subscribed to {topic}");

        loop {
            match eventloop.poll().await {
                Ok(Event::Incoming(Packet::Publish(msg))) => {
                    let parts: Vec<&str> = msg.topic.split('/').collect();
                    if parts.len() < 3 {
                        continue;
                    }
                    let camera_id = parts[parts.len() - 2];

                    match serde_json::from_slice::<PtzCommand>(&msg.payload) {
                        Ok(cmd) => {
                            info!("[{camera_id}] PTZ command: {:?}", cmd);
                            let cameras = self.cameras.read().await;
                            if let Some(info) = cameras.get(camera_id) {
                                execute_ptz(camera_id, info, &cmd).await;
                            } else {
                                warn!("[{camera_id}] PTZ command for unknown camera");
                            }
                        }
                        Err(e) => {
                            warn!("[{camera_id}] Invalid PTZ command: {e}");
                        }
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    warn!("MQTT control connection error: {e}");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    }
}

/// Execute a PTZ command on a camera via ONVIF.
async fn execute_ptz(camera_id: &str, info: &CameraOnvifInfo, cmd: &PtzCommand) {
    if info.onvif_xaddr.is_empty() {
        warn!("[{camera_id}] No ONVIF xaddr configured — cannot execute PTZ");
        return;
    }

    // Build ONVIF PTZ SOAP request
    let (pan, tilt, zoom) = match cmd.direction.as_str() {
        "up"       => (0.0, cmd.speed.max(0.3), 0.0),
        "down"     => (0.0, -cmd.speed.max(0.3), 0.0),
        "left"     => (-cmd.speed.max(0.3), 0.0, 0.0),
        "right"    => (cmd.speed.max(0.3), 0.0, 0.0),
        "zoom_in"  => (0.0, 0.0, cmd.speed.max(0.3)),
        "zoom_out" => (0.0, 0.0, -cmd.speed.max(0.3)),
        _ => {
            warn!("[{camera_id}] Unknown PTZ direction: {}", cmd.direction);
            return;
        }
    };

    match cmd.action.as_str() {
        "move" => {
            let soap = build_continuous_move_soap(&info.onvif_xaddr, pan, tilt, zoom);
            send_onvif_request(camera_id, info, &soap).await;
        }
        "stop" => {
            let soap = build_stop_soap(&info.onvif_xaddr);
            send_onvif_request(camera_id, info, &soap).await;
        }
        _ => {
            warn!("[{camera_id}] Unknown PTZ action: {}", cmd.action);
        }
    }
}

async fn send_onvif_request(camera_id: &str, info: &CameraOnvifInfo, soap_body: &str) {
    // ONVIF PTZ service is typically at the same host as the device service
    // but on the /onvif/ptz_service path
    let ptz_url = if info.onvif_xaddr.contains("/onvif/device_service") {
        info.onvif_xaddr.replace("/onvif/device_service", "/onvif/ptz_service")
    } else {
        format!("{}/onvif/ptz_service", info.onvif_xaddr.trim_end_matches('/'))
    };

    let client = reqwest::Client::new();
    let mut builder = client
        .post(&ptz_url)
        .header("Content-Type", "application/soap+xml; charset=utf-8")
        .timeout(Duration::from_secs(5));

    if !info.username.is_empty() {
        builder = builder.basic_auth(&info.username, Some(&info.password));
    }

    match builder
        .body(soap_body.to_string())
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            debug!("[{camera_id}] PTZ command executed successfully");
        }
        Ok(resp) => {
            warn!("[{camera_id}] PTZ ONVIF error: HTTP {}", resp.status());
        }
        Err(e) => {
            warn!("[{camera_id}] PTZ request failed: {e}");
        }
    }
}

fn build_continuous_move_soap(_xaddr: &str, pan: f32, tilt: f32, zoom: f32) -> String {
    format!(r#"<?xml version="1.0" encoding="UTF-8"?>
<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope"
            xmlns:tptz="http://www.onvif.org/ver20/ptz/wsdl"
            xmlns:tt="http://www.onvif.org/ver10/schema">
  <s:Body>
    <tptz:ContinuousMove>
      <tptz:ProfileToken>Profile_1</tptz:ProfileToken>
      <tptz:Velocity>
        <tt:PanTilt x="{pan}" y="{tilt}"/>
        <tt:Zoom x="{zoom}"/>
      </tptz:Velocity>
    </tptz:ContinuousMove>
  </s:Body>
</s:Envelope>"#)
}

fn build_stop_soap(_xaddr: &str) -> String {
    r#"<?xml version="1.0" encoding="UTF-8"?>
<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope"
            xmlns:tptz="http://www.onvif.org/ver20/ptz/wsdl">
  <s:Body>
    <tptz:Stop>
      <tptz:ProfileToken>Profile_1</tptz:ProfileToken>
      <tptz:PanTilt>true</tptz:PanTilt>
      <tptz:Zoom>true</tptz:Zoom>
    </tptz:Stop>
  </s:Body>
</s:Envelope>"#.to_string()
}
