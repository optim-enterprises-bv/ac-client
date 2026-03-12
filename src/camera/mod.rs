//! Multi-camera surveillance module for ac-client.
//!
//! Provides RTSP/ONVIF camera capture, motion detection, and live H.264
//! streaming via MQTT to the server — all integrated into the USP agent.
//!
//! # Architecture
//!
//! ```text
//! CameraManager
//!   ├── CameraSubsystem("cam-1")
//!   │     ├── RtspCapture → PacketQueue
//!   │     ├── MotionDetector (reads from queue)
//!   │     └── MqttBridge (streams ALL H.264 frames to server)
//!   ├── CameraSubsystem("cam-2")
//!   │     └── ...
//!   ├── OnvifDiscovery (periodic network scan)
//!   └── MqttBridge (publishes events to EMQX)
//! ```
//!
//! Each camera runs as an independent async task set, isolated from others.
//! A camera failure does not affect other cameras or the USP agent.

pub mod capture;
pub mod config;
pub mod events;
pub mod live_stream;
pub mod manager;
pub mod motion;
pub mod mqtt_bridge;
pub mod onvif_discovery;

pub use manager::CameraManager;
