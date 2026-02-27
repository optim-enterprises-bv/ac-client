//! USP Message encode / decode and builder helpers for the Agent side.

use prost::Message;
use uuid::Uuid;

use super::usp_msg::{
    body::MsgBody,
    header::MessageType,
    notify,
    Body, Error, Header, Msg, NotifyResp, OperateResp,
};
use super::{Result, UspError};

// ── Decode / encode ──────────────────────────────────────────────────────────

pub fn decode_msg(data: &[u8]) -> Result<Msg> {
    Msg::decode(data).map_err(UspError::Decode)
}

pub fn encode_msg(msg: &Msg) -> Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(msg.encoded_len());
    msg.encode(&mut buf)?;
    Ok(buf)
}

// ── Helpers ──────────────────────────────────────────────────────────────────

pub fn new_msg_id() -> String { Uuid::new_v4().to_string() }

fn make_header(msg_type: MessageType) -> Header {
    Header { msg_id: new_msg_id(), msg_type: msg_type as i32 }
}

// ── Builder: NOTIFY Boot! ────────────────────────────────────────────────────

/// Build a Boot! Notify message sent when the agent connects.
/// `parameter_map` contains Device.DeviceInfo.* key/value pairs.
pub fn build_boot_notify(
    subscription_id: &str,
    send_resp:       bool,
    parameter_map:   std::collections::HashMap<String, String>,
) -> Msg {
    Msg {
        header: Some(make_header(MessageType::Notify)),
        body: Some(Body {
            msg_body: Some(MsgBody::Request(super::usp_msg::Request {
                req_type: Some(super::usp_msg::request::ReqType::Notify(
                    super::usp_msg::Notify {
                        subscription_id: subscription_id.into(),
                        send_resp,
                        notification: Some(notify::Notification::Event(notify::Event {
                            obj_path:    "Device.".into(),
                            event_name:  "Boot!".into(),
                            command_key: String::new(),
                            params:      parameter_map,
                        })),
                    },
                )),
            })),
        }),
    }
}

// ── Builder: NOTIFY ValueChange ──────────────────────────────────────────────

/// Build a ValueChange Notify message for periodic status heartbeats.
pub fn build_value_change_notify(
    subscription_id: &str,
    param_path:      &str,
    param_value:     &str,
) -> Msg {
    Msg {
        header: Some(make_header(MessageType::Notify)),
        body: Some(Body {
            msg_body: Some(MsgBody::Request(super::usp_msg::Request {
                req_type: Some(super::usp_msg::request::ReqType::Notify(
                    super::usp_msg::Notify {
                        subscription_id: subscription_id.into(),
                        send_resp: false,
                        notification: Some(notify::Notification::ValueChange(notify::ValueChange {
                            param_path:  param_path.into(),
                            param_value: param_value.into(),
                        })),
                    },
                )),
            })),
        }),
    }
}

// ── Builder: GET_SUPPORTED_PROTO ─────────────────────────────────────────────

pub fn build_get_supported_proto() -> Msg {
    Msg {
        header: Some(make_header(MessageType::GetSupportedProto)),
        body: Some(Body {
            msg_body: Some(MsgBody::Request(super::usp_msg::Request {
                req_type: Some(super::usp_msg::request::ReqType::GetSupportedProto(
                    super::usp_msg::GetSupportedProto {
                        controller_supported_versions: "1.3".into(),
                    },
                )),
            })),
        }),
    }
}

// ── Builder: NOTIFY_RESP ─────────────────────────────────────────────────────

pub fn build_notify_resp(msg_id: &str, subscription_id: &str) -> Msg {
    Msg {
        header: Some(Header {
            msg_id:   msg_id.into(),
            msg_type: MessageType::NotifyResp as i32,
        }),
        body: Some(Body {
            msg_body: Some(MsgBody::Response(super::usp_msg::Response {
                resp_type: Some(super::usp_msg::response::RespType::NotifyResp(NotifyResp {
                    subscription_id: subscription_id.into(),
                })),
            })),
        }),
    }
}

// ── Builder: OPERATE_RESP ────────────────────────────────────────────────────

/// Build an OPERATE_RESP with output arguments.
pub fn build_operate_resp(
    msg_id:      &str,
    command:     &str,
    command_key: &str,
    output_args: std::collections::HashMap<String, String>,
) -> Msg {
    Msg {
        header: Some(Header {
            msg_id:   msg_id.into(),
            msg_type: MessageType::OperateResp as i32,
        }),
        body: Some(Body {
            msg_body: Some(MsgBody::Response(super::usp_msg::Response {
                resp_type: Some(super::usp_msg::response::RespType::OperateResp(
                    OperateResp {
                        command_key: command_key.into(),
                        operation_results: vec![super::usp_msg::operate_resp::OperationResult {
                            executed_command: command.into(),
                            operate_resp_type: Some(
                                super::usp_msg::operate_resp::operation_result::OperateRespType::ReqOutputArgs(
                                    super::usp_msg::operate_resp::OutputArgs {
                                        output_args,
                                    },
                                ),
                            ),
                        }],
                    },
                )),
            })),
        }),
    }
}

// ── Builder: SET_RESP ────────────────────────────────────────────────────────

/// Build a SET_RESP acknowledging a successful SET.
///
/// `updated_obj_paths` should contain the `obj_path` values from each
/// `UpdateObj` in the SET request (TR-369 §6.2.4).
pub fn build_set_resp(msg_id: &str, updated_obj_paths: &[String]) -> Msg {
    use super::usp_msg::set_resp::{updated_object_result, UpdatedObjectResult};
    let updated_obj_results = updated_obj_paths
        .iter()
        .map(|path| UpdatedObjectResult {
            requested_path: path.clone(),
            oper_status: Some(updated_object_result::OperStatus::OperSuccess(
                updated_object_result::OperSuccess { updated_inst_results: vec![] },
            )),
        })
        .collect();
    Msg {
        header: Some(Header {
            msg_id:   msg_id.into(),
            msg_type: MessageType::SetResp as i32,
        }),
        body: Some(Body {
            msg_body: Some(MsgBody::Response(super::usp_msg::Response {
                resp_type: Some(super::usp_msg::response::RespType::SetResp(
                    super::usp_msg::SetResp { updated_obj_results },
                )),
            })),
        }),
    }
}

// ── Builder: ERROR ───────────────────────────────────────────────────────────

pub fn build_error(msg_id: &str, err_code: u32, err_msg: &str) -> Msg {
    Msg {
        header: Some(Header {
            msg_id:   msg_id.into(),
            msg_type: MessageType::Error as i32,
        }),
        body: Some(Body {
            msg_body: Some(MsgBody::Error(Error {
                err_code,
                err_msg:    err_msg.into(),
                param_errs: vec![],
            })),
        }),
    }
}
