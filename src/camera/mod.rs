//! Multi-camera surveillance module for ac-client.
//!
//! Provides RTSP/ONVIF camera capture, motion detection, recording,
//! and upload to Kerberos Vault — all integrated into the USP agent.
//!
//! # Architecture
//!
//! ```text
//! CameraManager
//!   ├── CameraSubsystem("cam-1")
//!   │     ├── RtspCapture → PacketQueue
//!   │     ├── MotionDetector (reads from queue)
//!   │     ├── Recorder (writes MP4, triggered by motion or continuous)
//!   │     └── Uploader (sends recordings to Vault/S3)
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
pub mod recording;
pub mod storage;

pub use manager::CameraManager;
