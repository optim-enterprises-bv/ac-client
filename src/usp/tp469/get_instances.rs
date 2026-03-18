//! TP-469 GetInstances Message Handler
//!
//! Implements GetInstances request/response per TR-369 §6.1.6

use super::search::extract_instance_number;
use crate::config::ClientConfig;
use crate::usp::dm;
use crate::usp::usp_msg;

/// Handle GetInstances request and return response message
pub async fn handle_get_instances(
    cfg: &ClientConfig,
    msg_id: &str,
    obj_paths: &[String],
    first_level_only: bool,
) -> Option<usp_msg::Msg> {
    let mut path_results = Vec::new();

    for path in obj_paths {
        let instances = get_instances_for_path(cfg, path, first_level_only).await;

        // Convert to CurrInstance type
        let curr_insts: Vec<usp_msg::get_instances_resp::CurrInstance> = instances
            .into_iter()
            .map(|(path, _instance)| {
                usp_msg::get_instances_resp::CurrInstance {
                    obj_path: path,
                    unique_keys: std::collections::HashMap::new(), // Would populate from schema
                }
            })
            .collect();

        path_results.push(usp_msg::get_instances_resp::RequestedPathResult {
            requested_path: path.clone(),
            err_code: 0,
            err_msg: String::new(),
            curr_insts,
        });
    }

    Some(usp_msg::Msg {
        header: Some(usp_msg::Header {
            msg_id: msg_id.into(),
            msg_type: usp_msg::header::MessageType::GetInstancesResp as i32,
        }),
        body: Some(usp_msg::Body {
            msg_body: Some(usp_msg::body::MsgBody::Response(usp_msg::Response {
                resp_type: Some(usp_msg::response::RespType::GetInstancesResp(
                    usp_msg::GetInstancesResp {
                        req_path_results: path_results,
                    },
                )),
            })),
        }),
    })
}

async fn get_instances_for_path(
    cfg: &ClientConfig,
    path: &str,
    first_level_only: bool,
) -> Vec<(String, u32)> {
    let mut instances = Vec::new();

    // Get all parameters under this path
    let max_depth = if first_level_only { 1 } else { 0 };
    let params = dm::get_params(cfg, &[path.into()], max_depth).await;

    // Extract unique instance numbers from parameter paths
    let mut seen_instances = std::collections::HashSet::new();

    for (param_path, _) in params {
        if let Some(instance) = extract_instance_number(&param_path) {
            if seen_instances.insert(instance) {
                // Get the object path (without parameter)
                if let Some(last_dot) = param_path.rfind('.') {
                    let obj_path = &param_path[..last_dot];
                    instances.push((obj_path.to_string(), instance));
                }
            }
        }
    }

    instances
}
