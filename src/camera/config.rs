//! Camera configuration — UCI-backed, multi-camera.
//!
//! UCI schema (/etc/config/optimacs):
//!
//! ```uci
//! config camera 'cam0'
//!     option enabled '1'
//!     option name 'Front Door'
//!     option rtsp_url 'rtsp://192.168.1.100:554/stream1'
//!     option sub_rtsp_url 'rtsp://192.168.1.100:554/stream2'
//!     option onvif_enabled '0'
//!     option onvif_xaddr 'http://192.168.1.100/onvif/device_service'
//!     option onvif_username 'admin'
//!     option onvif_password ''
//!     option recording_mode 'motion'  # motion | continuous | disabled
//!     option prerecording_secs '3'
//!     option postrecording_secs '10'
//!     option max_recording_secs '30'
//!     option pixel_threshold '150'
//!     option auto_clean '1'
//!     option max_storage_mb '500'
//!
//! config camera_global 'global'
//!     option vault_uri 'http://kerberos-vault.optimacs'
//!     option vault_access_key ''
//!     option vault_secret_key ''
//!     option mqtt_topic_prefix 'kerberos'
//!     option discovery_enabled '1'
//!     option discovery_interval '300'
//!     option recording_dir '/tmp/kerberos-agent/recordings'
//! ```

use std::collections::HashMap;
use std::process::Command;

use log::{debug, warn};

/// Global camera settings (shared across all cameras).
#[derive(Debug, Clone)]
pub struct CameraGlobalConfig {
    /// Kerberos Vault upload URI.
    pub vault_uri: String,
    /// Vault access key.
    pub vault_access_key: String,
    /// Vault secret key.
    pub vault_secret_key: String,
    /// MQTT topic prefix for camera events.
    pub mqtt_topic_prefix: String,
    /// Enable ONVIF network discovery.
    pub discovery_enabled: bool,
    /// Discovery scan interval (seconds).
    pub discovery_interval: u64,
    /// Local directory for recordings.
    pub recording_dir: String,
    /// MQTT broker URI for camera events (e.g. "mqtt://emqx.optimacs:1883").
    pub mqtt_uri: String,
    /// Port for the live stream HTTP server (0 = disabled).
    pub live_stream_port: u16,
}

impl Default for CameraGlobalConfig {
    fn default() -> Self {
        Self {
            vault_uri: String::new(),
            vault_access_key: String::new(),
            vault_secret_key: String::new(),
            mqtt_topic_prefix: "kerberos".into(),
            discovery_enabled: true,
            discovery_interval: 300,
            recording_dir: "/tmp/kerberos-agent/recordings".into(),
            mqtt_uri: String::new(),
            live_stream_port: 0,
        }
    }
}

/// Recording mode for a camera.
#[derive(Debug, Clone, PartialEq)]
pub enum RecordingMode {
    Motion,
    Continuous,
    Disabled,
}

impl From<&str> for RecordingMode {
    fn from(s: &str) -> Self {
        match s {
            "continuous" => Self::Continuous,
            "disabled" => Self::Disabled,
            _ => Self::Motion,
        }
    }
}

/// Per-camera configuration.
#[derive(Debug, Clone)]
pub struct CameraConfig {
    /// UCI section name (e.g., "cam0").
    pub id: String,
    /// Friendly name.
    pub name: String,
    /// Whether this camera is enabled.
    pub enabled: bool,
    /// Main RTSP stream URL (high resolution).
    pub rtsp_url: String,
    /// Sub RTSP stream URL (low resolution, used for motion detection).
    pub sub_rtsp_url: String,
    /// ONVIF enabled.
    pub onvif_enabled: bool,
    /// ONVIF device service address.
    pub onvif_xaddr: String,
    /// ONVIF username.
    pub onvif_username: String,
    /// ONVIF password.
    pub onvif_password: String,
    /// Recording mode.
    pub recording_mode: RecordingMode,
    /// Pre-recording buffer (seconds).
    pub prerecording_secs: u32,
    /// Post-recording duration after motion stops (seconds).
    pub postrecording_secs: u32,
    /// Maximum recording duration (seconds).
    pub max_recording_secs: u32,
    /// Pixel change threshold for motion detection.
    pub pixel_threshold: u32,
    /// Auto-clean old recordings when storage limit reached.
    pub auto_clean: bool,
    /// Maximum storage per camera (MB).
    pub max_storage_mb: u64,
}

impl Default for CameraConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            enabled: true,
            rtsp_url: String::new(),
            sub_rtsp_url: String::new(),
            onvif_enabled: false,
            onvif_xaddr: String::new(),
            onvif_username: String::new(),
            onvif_password: String::new(),
            recording_mode: RecordingMode::Motion,
            prerecording_secs: 3,
            postrecording_secs: 10,
            max_recording_secs: 30,
            pixel_threshold: 150,
            auto_clean: true,
            max_storage_mb: 500,
        }
    }
}

/// Read a single UCI value, returning empty string on failure.
fn uci_get(key: &str) -> String {
    Command::new("uci")
        .args(["get", key])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_default()
}

/// Parse a UCI boolean value ("1", "true", "yes" → true).
fn uci_bool(val: &str) -> bool {
    matches!(val.trim(), "1" | "true" | "yes" | "on")
}

/// Load the global camera config from UCI.
pub fn load_global_config() -> CameraGlobalConfig {
    let mut cfg = CameraGlobalConfig::default();

    let vault_uri = uci_get("optimacs.camera_global.vault_uri");
    if !vault_uri.is_empty() {
        cfg.vault_uri = vault_uri;
    }
    cfg.vault_access_key = uci_get("optimacs.camera_global.vault_access_key");
    cfg.vault_secret_key = uci_get("optimacs.camera_global.vault_secret_key");

    let prefix = uci_get("optimacs.camera_global.mqtt_topic_prefix");
    if !prefix.is_empty() {
        cfg.mqtt_topic_prefix = prefix;
    }

    let disc = uci_get("optimacs.camera_global.discovery_enabled");
    if !disc.is_empty() {
        cfg.discovery_enabled = uci_bool(&disc);
    }

    let interval = uci_get("optimacs.camera_global.discovery_interval");
    if let Ok(v) = interval.parse::<u64>() {
        cfg.discovery_interval = v;
    }

    let dir = uci_get("optimacs.camera_global.recording_dir");
    if !dir.is_empty() {
        cfg.recording_dir = dir;
    }

    let mqtt = uci_get("optimacs.camera_global.mqtt_uri");
    if !mqtt.is_empty() {
        cfg.mqtt_uri = mqtt;
    }

    if let Ok(port) = uci_get("optimacs.camera_global.live_stream_port").parse::<u16>() {
        cfg.live_stream_port = port;
    }

    cfg
}

/// Load all camera sections from UCI.
/// Returns a map of camera ID → CameraConfig.
pub fn load_cameras() -> HashMap<String, CameraConfig> {
    let mut cameras = HashMap::new();

    // List all camera sections: `uci show optimacs` and grep for camera entries
    let output = Command::new("uci")
        .args(["show", "optimacs"])
        .output()
        .ok();

    let output = match output {
        Some(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => {
            warn!("Failed to read UCI config for cameras");
            return cameras;
        }
    };

    // Find all camera section names (e.g., optimacs.cam0=camera)
    let mut section_ids = Vec::new();
    for line in output.lines() {
        if line.contains("=camera") {
            // optimacs.cam0=camera → cam0
            if let Some(section) = line.split('=').next() {
                if let Some(id) = section.strip_prefix("optimacs.") {
                    section_ids.push(id.to_string());
                }
            }
        }
    }

    for id in section_ids {
        let prefix = format!("optimacs.{id}");
        let mut cam = CameraConfig {
            id: id.clone(),
            ..Default::default()
        };

        let enabled = uci_get(&format!("{prefix}.enabled"));
        cam.enabled = enabled.is_empty() || uci_bool(&enabled);

        cam.name = uci_get(&format!("{prefix}.name"));
        cam.rtsp_url = uci_get(&format!("{prefix}.rtsp_url"));
        cam.sub_rtsp_url = uci_get(&format!("{prefix}.sub_rtsp_url"));

        let onvif = uci_get(&format!("{prefix}.onvif_enabled"));
        cam.onvif_enabled = uci_bool(&onvif);
        cam.onvif_xaddr = uci_get(&format!("{prefix}.onvif_xaddr"));
        cam.onvif_username = uci_get(&format!("{prefix}.onvif_username"));
        cam.onvif_password = uci_get(&format!("{prefix}.onvif_password"));

        let mode = uci_get(&format!("{prefix}.recording_mode"));
        if !mode.is_empty() {
            cam.recording_mode = RecordingMode::from(mode.as_str());
        }

        if let Ok(v) = uci_get(&format!("{prefix}.prerecording_secs")).parse() {
            cam.prerecording_secs = v;
        }
        if let Ok(v) = uci_get(&format!("{prefix}.postrecording_secs")).parse() {
            cam.postrecording_secs = v;
        }
        if let Ok(v) = uci_get(&format!("{prefix}.max_recording_secs")).parse() {
            cam.max_recording_secs = v;
        }
        if let Ok(v) = uci_get(&format!("{prefix}.pixel_threshold")).parse() {
            cam.pixel_threshold = v;
        }

        let clean = uci_get(&format!("{prefix}.auto_clean"));
        if !clean.is_empty() {
            cam.auto_clean = uci_bool(&clean);
        }
        if let Ok(v) = uci_get(&format!("{prefix}.max_storage_mb")).parse() {
            cam.max_storage_mb = v;
        }

        if cam.rtsp_url.is_empty() && !cam.onvif_enabled {
            debug!("camera {id}: no RTSP URL or ONVIF configured, skipping");
            continue;
        }

        debug!(
            "camera {id}: name={} rtsp={} mode={:?}",
            cam.name, cam.rtsp_url, cam.recording_mode
        );
        cameras.insert(id, cam);
    }

    cameras
}
