//! TP-469 ADD and DELETE Message Handlers
//!
//! Implements ADD (create objects) and DELETE (remove objects) per TR-369 §6.1.3-4
//! Now with full UCI backend integration

use super::error_codes::ErrorCode;
use super::uci_backend::{self, UciResult};
use crate::config::ClientConfig;
use crate::usp::usp_msg;
use log::info;

/// Result of an ADD operation
#[derive(Debug)]
pub struct AddResult {
    pub obj_path: String,
    pub instance: u32,
    pub success: bool,
    pub err_code: Option<ErrorCode>,
    pub err_msg: Option<String>,
}

/// Handle ADD request
pub async fn handle_add(
    _cfg: &ClientConfig,
    create_objs: &[usp_msg::add::CreateObject],
    allow_partial: bool,
) -> Vec<AddResult> {
    let mut results = Vec::new();
    let mut has_failure = false;

    for create_obj in create_objs {
        let result = create_object_instance(create_obj).await;

        if !result.success {
            has_failure = true;
        }

        results.push(result);

        // If allow_partial is false and we had a failure, stop processing
        if !allow_partial && has_failure {
            break;
        }
    }

    results
}

async fn create_object_instance(create_obj: &usp_msg::add::CreateObject) -> AddResult {
    let obj_path = &create_obj.obj_path;

    // Determine the object type and dispatch to appropriate handler
    if obj_path.contains("DHCP") || obj_path.contains("dhcp") {
        add_dhcp_static_lease(create_obj).await
    } else if obj_path.contains("Hosts") || obj_path.contains("hosts") {
        add_static_host(create_obj).await
    } else if obj_path.contains("WiFi") || obj_path.contains("wifi") {
        add_wifi_interface(create_obj).await
    } else {
        AddResult {
            obj_path: obj_path.clone(),
            instance: 0,
            success: false,
            err_code: Some(ErrorCode::ObjectNotCreatable),
            err_msg: Some(format!("Object {} is not creatable", obj_path)),
        }
    }
}

async fn add_dhcp_static_lease(create_obj: &usp_msg::add::CreateObject) -> AddResult {
    // Extract parameters from the create object request
    let mut mac = String::new();
    let mut ip = String::new();
    let mut hostname = None;

    for param in &create_obj.param_settings {
        match param.param.as_str() {
            "Chaddr" => mac = param.value.clone(),
            "Yiaddr" => ip = param.value.clone(),
            "X_OptimACS_Hostname" => hostname = Some(param.value.clone()),
            _ => {}
        }
    }

    if mac.is_empty() || ip.is_empty() {
        return AddResult {
            obj_path: create_obj.obj_path.clone(),
            instance: 0,
            success: false,
            err_code: Some(ErrorCode::RequiredParameterMissing),
            err_msg: Some("MAC address (Chaddr) and IP (Yiaddr) are required".into()),
        };
    }

    // Call UCI backend to add the lease
    let result = uci_backend::add_dhcp_lease(&mac, &ip, hostname.as_deref());

    convert_uci_result(&create_obj.obj_path, result)
}

async fn add_wifi_interface(create_obj: &usp_msg::add::CreateObject) -> AddResult {
    // Extract parameters
    let mut ssid = String::new();
    let mut encryption = None;
    let mut key = None;
    let mut device = None;

    for param in &create_obj.param_settings {
        match param.param.as_str() {
            "SSID" => ssid = param.value.clone(),
            "Security.ModeEnabled" => encryption = Some(param.value.clone()),
            "Security.KeyPassphrase" => key = Some(param.value.clone()),
            "Device" => device = Some(param.value.clone()),
            _ => {}
        }
    }

    if ssid.is_empty() {
        return AddResult {
            obj_path: create_obj.obj_path.clone(),
            instance: 0,
            success: false,
            err_code: Some(ErrorCode::RequiredParameterMissing),
            err_msg: Some("SSID is required".into()),
        };
    }

    // Call UCI backend
    let result = uci_backend::add_wifi_interface(
        &ssid,
        encryption.as_deref(),
        key.as_deref(),
        device.as_deref(),
    );

    convert_uci_result(&create_obj.obj_path, result)
}

async fn add_static_host(create_obj: &usp_msg::add::CreateObject) -> AddResult {
    // Extract parameters
    let mut ip = String::new();
    let mut hostname = String::new();

    for param in &create_obj.param_settings {
        match param.param.as_str() {
            "IPAddress" => ip = param.value.clone(),
            "HostName" => hostname = param.value.clone(),
            _ => {}
        }
    }

    if ip.is_empty() || hostname.is_empty() {
        return AddResult {
            obj_path: create_obj.obj_path.clone(),
            instance: 0,
            success: false,
            err_code: Some(ErrorCode::RequiredParameterMissing),
            err_msg: Some("IP address and hostname are required".into()),
        };
    }

    // Call UCI backend
    let result = uci_backend::add_static_host(&ip, &hostname);

    convert_uci_result(&create_obj.obj_path, result)
}

/// Convert UciResult to AddResult
fn convert_uci_result(obj_path: &str, result: UciResult) -> AddResult {
    AddResult {
        obj_path: obj_path.into(),
        instance: result.instance,
        success: result.success,
        err_code: result.err_code,
        err_msg: result.err_msg,
    }
}

/// Result of a DELETE operation
#[derive(Debug)]
pub struct DeleteResult {
    pub obj_path: String,
    pub success: bool,
    pub err_code: Option<ErrorCode>,
    pub err_msg: Option<String>,
}

/// Handle DELETE request
pub async fn handle_delete(
    _cfg: &ClientConfig,
    obj_paths: &[String],
    allow_partial: bool,
) -> Vec<DeleteResult> {
    let mut results = Vec::new();
    let mut has_failure = false;

    for obj_path in obj_paths {
        let result = delete_object_instance(obj_path).await;

        if !result.success {
            has_failure = true;
        }

        results.push(result);

        // If allow_partial is false and we had a failure, stop processing
        if !allow_partial && has_failure {
            break;
        }
    }

    results
}

async fn delete_object_instance(obj_path: &str) -> DeleteResult {
    // Extract instance number from the path
    // Format: Device.DHCPv4.Server.Pool.1.StaticAddress.1
    // We need to extract the instance number (the last numeric segment)
    let instance = extract_instance_from_path(obj_path);

    if instance == 0 {
        return DeleteResult {
            obj_path: obj_path.to_string(),
            success: false,
            err_code: Some(ErrorCode::InvalidInstanceIdentifier),
            err_msg: Some(format!(
                "Could not extract instance number from {}",
                obj_path
            )),
        };
    }

    // Determine object type and dispatch
    if obj_path.contains("DHCP") || obj_path.contains("dhcp") {
        delete_dhcp_static_lease(obj_path, instance).await
    } else if obj_path.contains("Hosts") || obj_path.contains("hosts") {
        delete_static_host(obj_path, instance).await
    } else if obj_path.contains("WiFi") || obj_path.contains("wifi") {
        delete_wifi_interface(obj_path, instance).await
    } else {
        DeleteResult {
            obj_path: obj_path.to_string(),
            success: false,
            err_code: Some(ErrorCode::ObjectNotDeletable),
            err_msg: Some(format!("Object {} is not deletable", obj_path)),
        }
    }
}

async fn delete_dhcp_static_lease(obj_path: &str, instance: u32) -> DeleteResult {
    info!("Deleting DHCP static lease instance {}", instance);

    let result = uci_backend::delete_dhcp_lease(instance);

    DeleteResult {
        obj_path: obj_path.to_string(),
        success: result.success,
        err_code: result.err_code,
        err_msg: result.err_msg,
    }
}

async fn delete_wifi_interface(obj_path: &str, instance: u32) -> DeleteResult {
    info!("Deleting WiFi interface instance {}", instance);

    let result = uci_backend::delete_wifi_interface(instance);

    DeleteResult {
        obj_path: obj_path.to_string(),
        success: result.success,
        err_code: result.err_code,
        err_msg: result.err_msg,
    }
}

async fn delete_static_host(obj_path: &str, instance: u32) -> DeleteResult {
    info!("Deleting static host instance {}", instance);

    let result = uci_backend::delete_static_host(instance);

    DeleteResult {
        obj_path: obj_path.to_string(),
        success: result.success,
        err_code: result.err_code,
        err_msg: result.err_msg,
    }
}

/// Extract instance number from path
/// Device.DHCPv4.Server.Pool.1.StaticAddress.3 -> 3
/// Device.WiFi.SSID.1 -> 1
fn extract_instance_from_path(path: &str) -> u32 {
    // Find the last numeric segment
    path.split('.')
        .rev()
        .find_map(|s| s.parse::<u32>().ok())
        .unwrap_or(0)
}
