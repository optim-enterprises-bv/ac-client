//! Shared event types for inter-module communication.
//!
//! The camera event bus uses `tokio::sync::broadcast` for fan-out to multiple
//! consumers: MQTT bridge, recorder trigger, status tracker.

/// A camera event emitted by any subsystem.
#[derive(Debug, Clone)]
pub struct CameraEvent {
    pub camera_id: String,
    pub kind: CameraEventKind,
}

/// Kinds of camera events.
#[derive(Debug, Clone)]
pub enum CameraEventKind {
    /// Motion detected — change exceeds threshold.
    MotionStarted {
        change_pct: f32,
    },
    /// Motion ended — no change above threshold for cooldown period.
    MotionStopped {
        duration_secs: f64,
        peak_change: f32,
    },
    /// Recording segment started.
    RecordingStarted {
        filename: String,
    },
    /// Recording segment completed and written to disk.
    RecordingCompleted {
        filename: String,
        duration_secs: f64,
        size_bytes: u64,
    },
    /// RTSP stream connected.
    Connected,
    /// RTSP stream disconnected.
    Disconnected {
        error: Option<String>,
    },
}
