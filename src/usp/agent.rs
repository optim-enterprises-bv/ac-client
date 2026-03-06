//! USP Agent — main connection and message handling loop.
//!
//! Flow:
//!   1. Connect via WebSocket or MQTT MTP
//!   2. Send Boot! Notify with Device.DeviceInfo.* parameters
//!   3. Loop: handle incoming Controller messages (GET, SET, OPERATE, NOTIFY_RESP)
//!   4. Periodic ValueChange Notify for status (UpTime, LoadAvg, etc.)

#![allow(clippy::all)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use log::{debug, error, info, trace, warn};

use crate::config::{ClientConfig, MtpType};
use crate::gnss::GnssPosition;
use crate::util;

use super::{
    dm,
    tp469,
    endpoint::EndpointId,
    message::{
        build_boot_notify, build_error, build_operate_resp,
        build_set_resp, decode_msg, encode_msg,
    },
    mtp,
    usp_msg::{body::MsgBody, header::MessageType},
};

const _STATUS_SUBSCRIPTION_ID: &str = "status";

/// Run the USP agent.  Called from main after config is loaded.
pub async fn run(
    cfg:  Arc<ClientConfig>,
    gnss: Arc<std::sync::Mutex<Option<GnssPosition>>>,
) {
    debug!("Initializing USP Agent...");
    
    let agent_id = if cfg.usp_endpoint_id.is_empty() {
        // Build from vendor OUI (00005A = placeholder) + MAC
        debug!("Building endpoint ID from MAC: {}", cfg.mac_addr);
        EndpointId::from_mac("00005A", &cfg.mac_addr)
    } else {
        debug!("Using configured endpoint ID: {}", cfg.usp_endpoint_id);
        EndpointId::new(cfg.usp_endpoint_id.clone())
    };

    info!("USP Agent endpoint ID: {agent_id}");
    debug!("MTP type: {:?}", cfg.mtp);

    // Spawn status heartbeat task
    {
        debug!("Spawning status heartbeat task");
        let cfg2 = Arc::clone(&cfg);
        let agent2 = agent_id.clone();
        let gnss2  = Arc::clone(&gnss);
        tokio::spawn(async move {
            debug!("Status heartbeat task started");
            status_loop(cfg2, agent2, gnss2).await;
        });
    }

    // Connect MTP
    info!("Starting MTP connection...");
    match cfg.mtp {
        MtpType::WebSocket => {
            debug!("Starting WebSocket MTP");
            mtp::websocket::run(cfg, agent_id).await
        }
        MtpType::Mqtt      => {
            debug!("Starting MQTT MTP");
            mtp::mqtt::run(cfg, agent_id).await
        }
        MtpType::Both      => {
            debug!("Starting both WebSocket and MQTT MTP");
            let cfg2     = Arc::clone(&cfg);
            let agent2   = agent_id.clone();
            tokio::spawn(async move { 
                debug!("Starting MQTT MTP in background task");
                mtp::mqtt::run(cfg2, agent2).await; 
            });
            mtp::websocket::run(cfg, agent_id).await;
        }
    }
}

/// Handle an incoming encoded USP Msg bytes.
/// Returns encoded response bytes if a response is required.
///
/// `negotiated_ver` is updated when a `GetSupportedProtoResp` is received
/// (TR-369 §6.2.1 version negotiation).
pub async fn handle_incoming(
    cfg:            Arc<ClientConfig>,
    _agent_id:       EndpointId,
    msg_bytes:      &[u8],
    negotiated_ver: Arc<Mutex<String>>,
) -> Option<Vec<u8>> {
    trace!("handle_incoming called with {} bytes", msg_bytes.len());
    trace!("Raw message bytes (first 64): {:?}", &msg_bytes[..msg_bytes.len().min(64)]);
    
    let msg = decode_msg(msg_bytes)
        .map_err(|e| {
            error!("Failed to decode USP message: {}", e);
            trace!("Failed message bytes (first 128): {:?}", &msg_bytes[..msg_bytes.len().min(128)]);
            e
        })
        .ok()?;
    debug!("Successfully decoded USP message");

    let header = msg.header.as_ref()?;
    let msg_id   = header.msg_id.clone();
    let msg_type = MessageType::try_from(header.msg_type).ok()?;
    
    info!("Received {} message (msg_id={})", msg_type.as_str_name(), msg_id);
    trace!("Message type: {:?}, ID: {}", msg_type, msg_id);

    let body = msg.body.as_ref()?;

    let response = match msg_type {
        MessageType::Get => {
            debug!("Handling GET request (msg_id={})", msg_id);
            let (paths, max_depth) = match &body.msg_body {
                Some(MsgBody::Request(req)) => {
                    if let Some(super::usp_msg::request::ReqType::Get(g)) = &req.req_type {
                        debug!("GET paths: {:?}, max_depth={}", g.param_paths, g.max_depth);
                        (g.param_paths.clone(), g.max_depth)
                    } else { (vec![], 0) }
                }
                _ => (vec![], 0),
            };
            let params = dm::get_params(&cfg, &paths, max_depth).await;
            debug!("GET completed: {} parameter sets retrieved", params.len());
            build_get_resp(&msg_id, params)
        }

        MessageType::Set => {
            debug!("Handling SET request (msg_id={})", msg_id);
            let updates   = extract_set_updates(&body);
            let obj_paths = extract_set_obj_paths(&body);
            debug!("SET: {} parameter(s) to update in {} object path(s)", updates.len(), obj_paths.len());
            trace!("SET updates: {:?}", updates);
            match dm::set_params(&cfg, &updates).await {
                Ok(())  => {
                    debug!("SET completed successfully (msg_id={})", msg_id);
                    Some(build_set_resp(&msg_id, &obj_paths))
                }
                Err(e)  => {
                    error!("SET failed (msg_id={}): {}", msg_id, e);
                    Some(build_error(&msg_id, 7200, &e))
                }
            }
        }

        MessageType::Operate => {
            debug!("Handling OPERATE request (msg_id={})", msg_id);
            let (command, command_key, input_args) = extract_operate(&body);
            info!("OPERATE: command='{}', key='{}'", command, command_key);
            trace!("OPERATE input args: {:?}", input_args);
            match dm::operate(&cfg, &command, &input_args).await {
                Ok(output) => {
                    debug!("OPERATE completed successfully (msg_id={})", msg_id);
                    trace!("OPERATE output: {:?}", output);
                    Some(build_operate_resp(&msg_id, &command, &command_key, output))
                }
                Err(e)     => {
                    error!("OPERATE failed (msg_id={}): {}", msg_id, e);
                    Some(build_error(&msg_id, 7800, &e))
                }
            }
        }

        MessageType::NotifyResp => {
            debug!("Received NotifyResp (msg_id={}) - controller acknowledged notify", msg_id);
            None
        }

        MessageType::GetSupportedProtoResp => {
            debug!("Handling GetSupportedProtoResp (msg_id={})", msg_id);
            let versions = extract_supported_versions(&body);
            info!("Controller supports USP versions: {:?}", versions);
            // Store the first agreed version (W2: TR-369 §6.2.1)
            if let Some(ver) = versions.first() {
                *negotiated_ver.lock().unwrap() = ver.clone();
                info!("USP version negotiated: {ver}");
            }
            // Send Boot! Notify now that version is negotiated
            debug!("Building Boot! Notify after version negotiation");
            let boot_params = collect_boot_params(&cfg);
            let boot_msg = build_boot_notify("", false, boot_params);
            Some(boot_msg)
        }

        // TR-369 §6.1.5: GetSupportedDM - return supported data model
        MessageType::GetSupportedDm => {
            let (obj_paths, first_level_only, include_commands, include_events) = 
                extract_get_supported_dm_args(&body);
            
            tp469::handle_get_supported_dm(
                &msg_id,
                &obj_paths,
                first_level_only,
                include_commands,
                include_events,
            )
        }
        
        // TR-369 §6.1.6: GetInstances - enumerate object instances
        MessageType::GetInstances => {
            let (obj_paths, first_level_only) = extract_get_instances_args(&body);
            
            tp469::handle_get_instances(
                &cfg,
                &msg_id,
                &obj_paths,
                first_level_only,
            ).await
        }
        
        // TR-369 §6.1.3: Add - create new object instances
        MessageType::Add => {
            let (create_objs, allow_partial) = extract_add_args(&body);
            let results = tp469::handle_add(&cfg, &create_objs, allow_partial).await;
            
            // Build ADD_RESP
            Some(build_add_resp(&msg_id, results))
        }
        
        // TR-369 §6.1.4: Delete - remove object instances
        MessageType::Delete => {
            let (obj_paths, allow_partial) = extract_delete_args(&body);
            let results = tp469::handle_delete(&cfg, &obj_paths, allow_partial).await;
            
            // Build DELETE_RESP
            Some(build_delete_resp(&msg_id, results))
        }

        _ => {
            warn!("USP Agent: unknown message type {:?}", msg_type);
            Some(build_error(&msg_id, 7000, "MESSAGE_NOT_UNDERSTOOD"))
        }
    };

    response.and_then(|msg| encode_msg(&msg).ok())
}

// ── Boot params ───────────────────────────────────────────────────────────────

fn collect_boot_params(cfg: &ClientConfig) -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("Device.DeviceInfo.HostName".into(),         cfg.sys_model.clone());
    m.insert("Device.DeviceInfo.SoftwareVersion".into(),  util::read_fw_version());
    m.insert("Device.DeviceInfo.HardwareVersion".into(),  cfg.sys_model.clone());
    m.insert("Device.DeviceInfo.SerialNumber".into(),     cfg.mac_addr.clone());
    m.insert("Device.DeviceInfo.UpTime".into(),           util::read_uptime());
    m.insert("Device.DeviceInfo.X_OptimACS_LoadAvg".into(), util::read_load_avg());
    m.insert("Device.DeviceInfo.X_OptimACS_FreeMem".into(),  util::read_free_mem());
    // TR-181 §9.3.6 required Boot! event parameters
    m.insert("Cause".into(),           "LocalReboot".into());
    m.insert("FirmwareUpdated".into(), "false".into());
    m
}

// ── Status heartbeat ─────────────────────────────────────────────────────────

async fn status_loop(
    cfg:      Arc<ClientConfig>,
    _agent_id: EndpointId,
    _gnss:     Arc<std::sync::Mutex<Option<GnssPosition>>>,
) {
    let interval = Duration::from_secs(cfg.status_interval);
    loop {
        tokio::time::sleep(interval).await;
        let params: Vec<(&str, String)> = vec![
            ("Device.DeviceInfo.UpTime",             util::read_uptime()),
            ("Device.DeviceInfo.X_OptimACS_LoadAvg", util::read_load_avg()),
            ("Device.DeviceInfo.X_OptimACS_FreeMem",  util::read_free_mem()),
        ];
        // We don't have direct access to the MTP sender here.
        // Log the values; the MTP loop will pick up periodically.
        for (path, val) in &params {
            info!("USP status: {path} = {val}");
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn build_get_resp(msg_id: &str, params: HashMap<String, String>) -> Option<super::usp_msg::Msg> {
    use super::usp_msg::{get_resp::*, *};
    Some(super::usp_msg::Msg {
        header: Some(Header {
            msg_id:   msg_id.into(),
            msg_type: MessageType::GetResp as i32,
        }),
        body: Some(Body {
            msg_body: Some(MsgBody::Response(Response {
                resp_type: Some(response::RespType::GetResp(GetResp {
                    req_path_results: params
                        .into_iter()
                        .map(|(k, v)| {
                            let mut result_params = std::collections::HashMap::new();
                            result_params.insert(String::new(), v);
                            RequestedPathResult {
                                requested_path: k.clone(),
                                err_code: 0,
                                err_msg: String::new(),
                                resolved_path_results: vec![ResolvedPathResult {
                                    resolved_path: k,
                                    result_params,
                                }],
                            }
                        })
                        .collect(),
                })),
            })),
        }),
    })
}

fn extract_set_updates(body: &super::usp_msg::Body) -> Vec<(String, String)> {
    use super::usp_msg::body::MsgBody;
    let mut updates = vec![];
    if let Some(MsgBody::Request(req)) = &body.msg_body {
        if let Some(super::usp_msg::request::ReqType::Set(s)) = &req.req_type {
            for obj in &s.update_objs {
                for param in &obj.param_settings {
                    updates.push((
                        format!("{}{}", obj.obj_path, param.param),
                        param.value.clone(),
                    ));
                }
            }
        }
    }
    updates
}

fn extract_set_obj_paths(body: &super::usp_msg::Body) -> Vec<String> {
    use super::usp_msg::body::MsgBody;
    let mut paths = vec![];
    if let Some(MsgBody::Request(req)) = &body.msg_body {
        if let Some(super::usp_msg::request::ReqType::Set(s)) = &req.req_type {
            for obj in &s.update_objs {
                paths.push(obj.obj_path.clone());
            }
        }
    }
    paths
}

fn extract_operate(body: &super::usp_msg::Body) -> (String, String, HashMap<String, String>) {
    if let Some(MsgBody::Request(req)) = &body.msg_body {
        if let Some(super::usp_msg::request::ReqType::Operate(op)) = &req.req_type {
            return (op.command.clone(), op.command_key.clone(), op.input_args.clone());
        }
    }
    (String::new(), String::new(), HashMap::new())
}

fn extract_supported_versions(body: &super::usp_msg::Body) -> Vec<String> {
    if let Some(MsgBody::Response(resp)) = &body.msg_body {
        if let Some(super::usp_msg::response::RespType::GetSupportedProtoResp(r)) = &resp.resp_type {
            return r.agent_supported_versions.split(',').map(str::trim).map(String::from).collect();
        }
    }
    vec![]
}

// ── TP-469 Helper Functions ───────────────────────────────────────────────────

fn extract_get_supported_dm_args(body: &super::usp_msg::Body) -> (Vec<String>, bool, bool, bool) {
    use super::usp_msg::body::MsgBody;
    if let Some(MsgBody::Request(req)) = &body.msg_body {
        if let Some(super::usp_msg::request::ReqType::GetSupportedDm(r)) = &req.req_type {
            return (
                r.obj_paths.clone(),
                r.first_level_only,
                r.return_commands,
                r.return_events,
            );
        }
    }
    (vec![], true, false, false)
}

fn extract_get_instances_args(body: &super::usp_msg::Body) -> (Vec<String>, bool) {
    use super::usp_msg::body::MsgBody;
    if let Some(MsgBody::Request(req)) = &body.msg_body {
        if let Some(super::usp_msg::request::ReqType::GetInstances(r)) = &req.req_type {
            return (r.obj_paths.clone(), r.first_level_only);
        }
    }
    (vec![], true)
}

fn extract_add_args(body: &super::usp_msg::Body) -> (Vec<super::usp_msg::add::CreateObject>, bool) {
    use super::usp_msg::body::MsgBody;
    if let Some(MsgBody::Request(req)) = &body.msg_body {
        if let Some(super::usp_msg::request::ReqType::Add(r)) = &req.req_type {
            return (r.create_objs.clone(), r.allow_partial);
        }
    }
    (vec![], false)
}

fn extract_delete_args(body: &super::usp_msg::Body) -> (Vec<String>, bool) {
    use super::usp_msg::body::MsgBody;
    if let Some(MsgBody::Request(req)) = &body.msg_body {
        if let Some(super::usp_msg::request::ReqType::Delete(r)) = &req.req_type {
            return (r.obj_paths.clone(), r.allow_partial);
        }
    }
    (vec![], false)
}

fn build_add_resp(msg_id: &str, results: Vec<tp469::AddResult>) -> super::usp_msg::Msg {
    use super::usp_msg::{add_resp::*, *};
    
    let created_obj_results = results.into_iter().map(|r| {
        let oper_status = if r.success {
            Some(created_object_result::OperStatus::OperSuccess(created_object_result::OperSuccess {
                instantiated_path: format!("{}.{}", r.obj_path.trim_end_matches('.'), r.instance),
                param_errs: vec![],
                unique_keys: std::collections::HashMap::new(),
            }))
        } else {
            Some(created_object_result::OperStatus::OperFailure(created_object_result::OperFailure {
                err_code: r.err_code.map(|e| e.as_u32()).unwrap_or(7200),
                err_msg: r.err_msg.unwrap_or_default(),
            }))
        };
        
        CreatedObjectResult {
            requested_path: r.obj_path,
            oper_status,
        }
    }).collect();
    
    super::usp_msg::Msg {
        header: Some(Header {
            msg_id: msg_id.into(),
            msg_type: MessageType::AddResp as i32,
        }),
        body: Some(Body {
            msg_body: Some(MsgBody::Response(Response {
                resp_type: Some(response::RespType::AddResp(AddResp {
                    created_obj_results,
                })),
            })),
        }),
    }
}

fn build_delete_resp(msg_id: &str, results: Vec<tp469::DeleteResult>) -> super::usp_msg::Msg {
    use super::usp_msg::{delete_resp::*, *};
    
    let deleted_obj_results = results.into_iter().map(|r| {
        let oper_status = if r.success {
            Some(deleted_object_result::OperStatus::OperSuccess(deleted_object_result::OperSuccess {
                affected_paths: vec![],
            }))
        } else {
            Some(deleted_object_result::OperStatus::OperFailure(deleted_object_result::OperFailure {
                err_code: r.err_code.map(|e| e.as_u32()).unwrap_or(7200),
                err_msg: r.err_msg.unwrap_or_default(),
                unaffected_path_errs: vec![],
            }))
        };
        
        DeletedObjectResult {
            requested_path: r.obj_path,
            oper_status,
        }
    }).collect();
    
    super::usp_msg::Msg {
        header: Some(Header {
            msg_id: msg_id.into(),
            msg_type: MessageType::DeleteResp as i32,
        }),
        body: Some(Body {
            msg_body: Some(MsgBody::Response(Response {
                resp_type: Some(response::RespType::DeleteResp(DeleteResp {
                    deleted_obj_results,
                })),
            })),
        }),
    }
}
