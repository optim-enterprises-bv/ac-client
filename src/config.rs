//! USP Agent configuration file parser.
//!
//! Parses the same key = value format used by `ac_server.conf`.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{AcError, Result};

// Default interval constants (seconds)
const PORT:            u16 = 3490;
const STATUS_INTERVAL: u64 = 300;
const CAM_INTERVAL:    u64 = 360;
const UPDATE_INTERVAL: u64 = 60;

/// MTP selection for the USP Agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MtpType {
    WebSocket,
    Mqtt,
    Both,
}

impl Default for MtpType {
    fn default() -> Self { MtpType::WebSocket }
}

/// Full client configuration.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    // ── Legacy ACP / TLS fields (kept for TLS cert paths) ─────────────────────
    /// ACP server hostname or IP address (kept for SNI / backward compat).
    pub server_host: String,
    /// ACP server port (default 3490).
    pub server_port: u16,
    /// Expected TLS CN of the server cert (used for SNI).
    pub server_cn: String,
    /// Path to the CA certificate.
    pub ca_file: PathBuf,
    /// Path to the device's provisioned client certificate.
    pub cert_file: PathBuf,
    /// Path to the device's provisioned client private key.
    pub key_file: PathBuf,
    /// Path to the initial (unprovisioned) client certificate.
    pub init_cert: PathBuf,
    /// Path to the initial client private key.
    pub init_key: PathBuf,
    /// Directory where provisioned certs are saved.
    pub cert_dir: PathBuf,
    // ── Device identity ───────────────────────────────────────────────────────
    /// Device MAC address (used as identity).
    pub mac_addr: String,
    /// CPU architecture string (e.g. "mipsel_24kc").
    pub arch: String,
    /// System model string (e.g. "dir300").
    pub sys_model: String,
    // ── GNSS ──────────────────────────────────────────────────────────────────
    pub gnss_dev:  String,
    pub gnss_baud: u32,
    // ── Intervals ─────────────────────────────────────────────────────────────
    pub update_interval: u64,
    pub status_interval: u64,
    pub cam_interval:    u64,
    // ── Directories ───────────────────────────────────────────────────────────
    pub fw_dir:  PathBuf,
    pub img_dir: PathBuf,
    // ── Process ───────────────────────────────────────────────────────────────
    pub pid_file:   PathBuf,
    pub daemonize:  bool,
    pub log_syslog: bool,
    // ── USP / TR-369 ──────────────────────────────────────────────────────────
    /// Agent endpoint ID (auto-built from MAC if empty).
    pub usp_endpoint_id: String,
    /// Controller endpoint ID.
    pub controller_id: String,
    /// WebSocket MTP URL (e.g. `wss://ac-server:3491/usp`).
    pub ws_url: Option<String>,
    /// MQTT broker URL (e.g. `mqtt://emqx:1883`).
    pub mqtt_url: Option<String>,
    /// Which MTP(s) to use.
    pub mtp: MtpType,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            server_host:     String::new(),
            server_port:     PORT,
            server_cn:       "ac-server".to_string(),
            ca_file:         PathBuf::from("/etc/apclient/ca.crt"),
            cert_file:       PathBuf::from("/etc/apclient/client.crt"),
            key_file:        PathBuf::from("/etc/apclient/client.key"),
            init_cert:       PathBuf::from("/etc/apclient/init/client.crt"),
            init_key:        PathBuf::from("/etc/apclient/init/client.key"),
            cert_dir:        PathBuf::from("/etc/apclient"),
            mac_addr:        String::new(),
            arch:            String::new(),
            sys_model:       String::new(),
            gnss_dev:        String::new(),
            gnss_baud:       9600,
            update_interval: UPDATE_INTERVAL,
            status_interval: STATUS_INTERVAL,
            cam_interval:    CAM_INTERVAL,
            fw_dir:          PathBuf::from("/tmp/firmware"),
            img_dir:         PathBuf::from("/tmp/cam"),
            pid_file:        PathBuf::from("/var/run/apclient.pid"),
            daemonize:       false,
            log_syslog:      true,
            usp_endpoint_id: String::new(),
            controller_id:   "oui:00005A:OptimACS-Controller-1".to_string(),
            ws_url:          None,
            mqtt_url:        None,
            mtp:             MtpType::WebSocket,
        }
    }
}

/// Parse `path` as an `ac_client.conf` key=value configuration file.
pub fn load_config(path: &Path) -> Result<ClientConfig> {
    let content = fs::read_to_string(path)
        .map_err(|e| AcError::Config(format!("cannot read {}: {e}", path.display())))?;
    let mut cfg = ClientConfig::default();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.splitn(2, '=');
        let key = match parts.next() {
            Some(k) => k.trim().to_ascii_lowercase(),
            None => continue,
        };
        let val = match parts.next() {
            Some(v) => v.trim().to_string(),
            None => continue,
        };
        if val.is_empty() {
            continue;
        }

        match key.as_str() {
            "server_host"      => cfg.server_host     = val,
            "server_port"      => cfg.server_port     = val.parse().unwrap_or(PORT),
            "server_cn"        => cfg.server_cn       = val,
            "ca_file"          => cfg.ca_file         = PathBuf::from(&val),
            "cert_file"        => cfg.cert_file       = PathBuf::from(&val),
            "key_file"         => cfg.key_file        = PathBuf::from(&val),
            "init_cert"        => cfg.init_cert       = PathBuf::from(&val),
            "init_key"         => cfg.init_key        = PathBuf::from(&val),
            "cert_dir"         => cfg.cert_dir        = PathBuf::from(&val),
            "mac_addr"         => cfg.mac_addr        = val,
            "arch"             => cfg.arch            = val,
            "sys_model"        => cfg.sys_model       = val,
            "gnss_dev"         => cfg.gnss_dev        = val,
            "gnss_baud"        => cfg.gnss_baud       = val.parse().unwrap_or(9600),
            "update_interval"  => cfg.update_interval = val.parse().unwrap_or(UPDATE_INTERVAL),
            "status_interval"  => cfg.status_interval = val.parse().unwrap_or(STATUS_INTERVAL),
            "cam_interval"     => cfg.cam_interval    = val.parse().unwrap_or(CAM_INTERVAL),
            "fw_dir"           => cfg.fw_dir          = PathBuf::from(&val),
            "img_dir"          => cfg.img_dir         = PathBuf::from(&val),
            "pid_file"         => cfg.pid_file        = PathBuf::from(&val),
            "daemonize"        => cfg.daemonize       = val == "true" || val == "1" || val == "yes",
            "log_syslog"       => cfg.log_syslog      = val == "true" || val == "1" || val == "yes",
            // USP / TR-369
            "usp_endpoint_id"  => cfg.usp_endpoint_id = val,
            "controller_id"    => cfg.controller_id   = val,
            "ws_url"           => cfg.ws_url          = Some(val),
            "mqtt_url"         => cfg.mqtt_url        = Some(val),
            "mtp" => {
                cfg.mtp = match val.to_ascii_lowercase().as_str() {
                    "mqtt"       => MtpType::Mqtt,
                    "both"       => MtpType::Both,
                    _            => MtpType::WebSocket,
                };
            }
            _ => {} // ignore unknown keys
        }
    }

    Ok(cfg)
}

/// Validate that required fields are populated.
pub fn validate_config(cfg: &ClientConfig) -> Result<()> {
    if cfg.mac_addr.is_empty() && cfg.usp_endpoint_id.is_empty() {
        // mac_addr can be auto-detected, so only fail if both are missing
    }
    if cfg.ca_file.as_os_str().is_empty() {
        return Err(AcError::Config("ca_file is required".into()));
    }
    // At least one MTP must be configured
    match cfg.mtp {
        MtpType::WebSocket | MtpType::Both => {
            if cfg.ws_url.is_none() && cfg.server_host.is_empty() {
                return Err(AcError::Config(
                    "ws_url (or server_host) is required for WebSocket MTP".into()
                ));
            }
        }
        MtpType::Mqtt => {
            if cfg.mqtt_url.is_none() {
                return Err(AcError::Config("mqtt_url is required for MQTT MTP".into()));
            }
        }
    }
    Ok(())
}
