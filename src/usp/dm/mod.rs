//! TR-181 data model — Agent side.
//!
//! The agent handles incoming GET and SET requests from the Controller
//! by dispatching to the appropriate sub-module based on the TR-181 path prefix.

pub mod cameras;
pub mod device_info;
pub mod dhcp;
pub mod firmware;
pub mod hosts;
pub mod ip;
pub mod security;
pub mod wifi;

use std::collections::HashMap;
use log::warn;
use crate::config::ClientConfig;

pub type Params = HashMap<String, String>;

/// Handle a GET request for the given paths.
///
/// `max_depth` limits how many levels below the requested path are returned.
/// 0 means unlimited (TR-369 §6.1.2).
pub async fn get_params(cfg: &ClientConfig, paths: &[String], max_depth: u32) -> Params {
    let mut result = Params::new();
    for path in paths {
        let partial = dispatch_get(cfg, path).await;
        if max_depth == 0 {
            result.extend(partial);
        } else {
            let base_depth = path.chars().filter(|&c| c == '.').count();
            result.extend(partial.into_iter().filter(|(k, _)| {
                k.chars().filter(|&c| c == '.').count() <= base_depth + max_depth as usize
            }));
        }
    }
    result
}

/// Handle a SET request for the given (path, value) pairs.
pub async fn set_params(cfg: &ClientConfig, updates: &[(String, String)]) -> Result<(), String> {
    for (path, value) in updates {
        dispatch_set(cfg, path, value).await?;
    }
    Ok(())
}

/// Handle an OPERATE command; returns output_args on success.
pub async fn operate(
    cfg:         &ClientConfig,
    command:     &str,
    input_args:  &HashMap<String, String>,
) -> Result<HashMap<String, String>, String> {
    if command.starts_with("Device.X_OptimACS_Camera.") && command.ends_with(".Capture()") {
        cameras::operate_capture(cfg, command, input_args).await
    } else if command.starts_with("Device.X_OptimACS_Firmware.") && command.ends_with(".Download()") {
        firmware::operate_download(cfg, command, input_args).await
    } else if command.starts_with("Device.X_OptimACS_Security.") && command.ends_with(".IssueCert()") {
        security::operate_issue_cert(cfg, command, input_args).await
    } else {
        Err(format!("unknown command: {command}"))
    }
}

async fn dispatch_get(cfg: &ClientConfig, path: &str) -> Params {
    if path.starts_with("Device.DeviceInfo.") {
        device_info::get(cfg, path)
    } else if path.starts_with("Device.WiFi.") {
        wifi::get(cfg, path).await
    } else if path.starts_with("Device.IP.Interface.") {
        ip::get(cfg, path).await
    } else if path.starts_with("Device.DHCPv4.") {
        dhcp::get(cfg, path).await
    } else if path.starts_with("Device.Hosts.") {
        hosts::get(cfg, path).await
    } else if path.starts_with("Device.X_OptimACS_Camera.") {
        cameras::get(cfg, path).await
    } else if path.starts_with("Device.X_OptimACS_Firmware.") {
        firmware::get(cfg, path)
    } else {
        warn!("DM GET: unknown path prefix: {path}");
        Params::new()
    }
}

async fn dispatch_set(cfg: &ClientConfig, path: &str, value: &str) -> Result<(), String> {
    if path.starts_with("Device.DeviceInfo.") {
        device_info::set(cfg, path, value)
    } else if path.starts_with("Device.WiFi.") {
        wifi::set(cfg, path, value).await
    } else if path.starts_with("Device.IP.Interface.") {
        ip::set(cfg, path, value).await
    } else if path.starts_with("Device.DHCPv4.") {
        dhcp::set(cfg, path, value).await
    } else if path.starts_with("Device.Hosts.") {
        hosts::set(cfg, path, value).await
    } else if path.starts_with("Device.X_OptimACS_Security.") {
        security::set(cfg, path, value).await
    } else {
        Err(format!("read-only or unknown path: {path}"))
    }
}
