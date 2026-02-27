//! USP Agent — main connection and message handling loop.
//!
//! Flow:
//!   1. Connect via WebSocket or MQTT MTP
//!   2. Send Boot! Notify with Device.DeviceInfo.* parameters
//!   3. Loop: handle incoming Controller messages (GET, SET, OPERATE, NOTIFY_RESP)
//!   4. Periodic ValueChange Notify for status (UpTime, LoadAvg, etc.)

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use log::{error, info, warn};

use crate::config::{ClientConfig, MtpType};
use crate::gnss::GnssPosition;
use crate::util;

use super::{
    dm,
    endpoint::EndpointId,
    message::{
        build_boot_notify, build_error, build_notify_resp, build_operate_resp,
        build_set_resp, build_value_change_notify, decode_msg, encode_msg,
    },
    mtp,
    usp_msg::{body::MsgBody, header::MessageType},
    Result,
};

const STATUS_SUBSCRIPTION_ID: &str = "status";

/// Run the USP agent.  Called from main after config is loaded.
pub async fn run(
    cfg:  Arc<ClientConfig>,
    gnss: Arc<std::sync::Mutex<Option<GnssPosition>>>,
) {
    let agent_id = if cfg.usp_endpoint_id.is_empty() {
        // Build from vendor OUI (00005A = placeholder) + MAC
        EndpointId::from_mac("00005A", &cfg.mac_addr)
    } else {
        EndpointId::new(cfg.usp_endpoint_id.clone())
    };

    info!("USP Agent endpoint ID: {agent_id}");

    // Spawn status heartbeat task
    {
        let cfg2 = Arc::clone(&cfg);
        let agent2 = agent_id.clone();
        let gnss2  = Arc::clone(&gnss);
        tokio::spawn(async move {
            status_loop(cfg2, agent2, gnss2).await;
        });
    }

    // Connect MTP
    match cfg.mtp {
        MtpType::WebSocket => mtp::websocket::run(cfg, agent_id).await,
        MtpType::Mqtt      => mtp::mqtt::run(cfg, agent_id).await,
        MtpType::Both      => {
            let cfg2     = Arc::clone(&cfg);
            let agent2   = agent_id.clone();
            tokio::spawn(async move { mtp::mqtt::run(cfg2, agent2).await; });
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
    agent_id:       EndpointId,
    msg_bytes:      &[u8],
    negotiated_ver: Arc<Mutex<String>>,
) -> Option<Vec<u8>> {
    let msg = decode_msg(msg_bytes).ok()?;

    let header = msg.header.as_ref()?;
    let msg_id   = header.msg_id.clone();
    let msg_type = MessageType::try_from(header.msg_type).ok()?;

    let body = msg.body.as_ref()?;

    let response = match msg_type {
        MessageType::Get => {
            let (paths, max_depth) = match &body.msg_body {
                Some(MsgBody::Request(req)) => {
                    if let Some(super::usp_msg::request::ReqType::Get(g)) = &req.req_type {
                        (g.param_paths.clone(), g.max_depth)
                    } else { (vec![], 0) }
                }
                _ => (vec![], 0),
            };
            let params = dm::get_params(&cfg, &paths, max_depth).await;
            build_get_resp(&msg_id, params)
        }

        MessageType::Set => {
            let updates   = extract_set_updates(&body);
            let obj_paths = extract_set_obj_paths(&body);
            match dm::set_params(&cfg, &updates).await {
                Ok(())  => Some(build_set_resp(&msg_id, &obj_paths)),
                Err(e)  => Some(build_error(&msg_id, 7200, &e)),
            }
        }

        MessageType::Operate => {
            let (command, command_key, input_args) = extract_operate(&body);
            match dm::operate(&cfg, &command, &input_args).await {
                Ok(output) => Some(build_operate_resp(&msg_id, &command, &command_key, output)),
                Err(e)     => Some(build_error(&msg_id, 7800, &e)),
            }
        }

        MessageType::NotifyResp => {
            // Controller acknowledged our notify — no response needed
            None
        }

        MessageType::GetSupportedProtoResp => {
            let versions = extract_supported_versions(&body);
            info!("Controller supports USP versions: {:?}", versions);
            // Store the first agreed version (W2: TR-369 §6.2.1)
            if let Some(ver) = versions.first() {
                *negotiated_ver.lock().unwrap() = ver.clone();
                info!("USP version negotiated: {ver}");
            }
            // Send Boot! Notify now that version is negotiated
            let boot_params = collect_boot_params(&cfg);
            let boot_msg = build_boot_notify("", false, boot_params);
            Some(boot_msg)
        }

        // TR-369 §6.4: known-but-unsupported message types must return Error 7004
        MessageType::GetSupportedDm
        | MessageType::GetInstances
        | MessageType::Add
        | MessageType::Delete => {
            warn!("USP Agent: unsupported message type {:?}", msg_type);
            Some(build_error(&msg_id, 7004, "NOT_SUPPORTED"))
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
    agent_id: EndpointId,
    gnss:     Arc<std::sync::Mutex<Option<GnssPosition>>>,
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
