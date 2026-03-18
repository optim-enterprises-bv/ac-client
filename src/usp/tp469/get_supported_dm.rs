//! TP-469 GetSupportedDM Message Handler
//!
//! Implements GetSupportedDM request/response per TR-369 §6.1.5
//! NOTE: Simplified version - full implementation requires matching exact protobuf schema

use crate::usp::usp_msg;

/// Handle GetSupportedDM request and return response message
/// NOTE: This is a simplified stub that returns basic info
pub fn handle_get_supported_dm(
    msg_id: &str,
    _obj_paths: &[String],
    _first_level_only: bool,
    _include_commands: bool,
    _include_events: bool,
) -> Option<usp_msg::Msg> {
    // Simplified implementation - return empty supported objects
    let path_results: Vec<usp_msg::get_supported_dm_resp::RequestedObjectResult> = vec![];

    Some(usp_msg::Msg {
        header: Some(usp_msg::Header {
            msg_id: msg_id.into(),
            msg_type: usp_msg::header::MessageType::GetSupportedDmResp as i32,
        }),
        body: Some(usp_msg::Body {
            msg_body: Some(usp_msg::body::MsgBody::Response(usp_msg::Response {
                resp_type: Some(usp_msg::response::RespType::GetSupportedDmResp(
                    usp_msg::GetSupportedDmResp {
                        req_obj_results: path_results,
                    },
                )),
            })),
        }),
    })
}
