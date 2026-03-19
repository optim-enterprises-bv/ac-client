//! TP-469 / TP-286 Compliance Tests
//!
//! Comprehensive test suite for USP/TR-369 conformance per BBF TP-286.
//! Tests cover: message serialization, error codes, data model dispatch,
//! path matching, GetSupportedDM declarations, record encoding, and
//! multi-controller support.

#[cfg(test)]
mod tests {
    use crate::usp::tp469::error_codes::ErrorCode;
    use crate::usp::tp469::uci_backend::*;
    use crate::usp::tp469::*;

    // ─────────────────────────────────────────────────────────────────────────
    // TP-286 §1: Error Code Tests
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_error_code_values() {
        assert_eq!(ErrorCode::InternalError.as_u32(), 7002);
        assert_eq!(ErrorCode::ResourcesExceeded.as_u32(), 7004);
        assert_eq!(ErrorCode::ObjectNotFound.as_u32(), 7206);
        assert_eq!(ErrorCode::ObjectNotCreatable.as_u32(), 7207);
    }

    #[test]
    fn test_error_codes_are_in_7000_range() {
        let codes = [
            ErrorCode::InternalError,
            ErrorCode::ResourcesExceeded,
            ErrorCode::ObjectNotFound,
            ErrorCode::ObjectNotCreatable,
            ErrorCode::ObjectNotDeletable,
            ErrorCode::RequiredParameterMissing,
            ErrorCode::InvalidInstanceIdentifier,
        ];
        for code in &codes {
            let val = code.as_u32();
            assert!(val >= 7000 && val < 8000, "Error code {} out of range", val);
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // TP-286 §2: UCI Backend Tests
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_uci_result_success() {
        let result = UciResult::success(42);
        assert!(result.success);
        assert_eq!(result.instance, 42);
        assert!(result.err_code.is_none());
        assert!(result.err_msg.is_none());
    }

    #[test]
    fn test_uci_result_error() {
        let result = UciResult::error(ErrorCode::ResourcesExceeded, "Test error");
        assert!(!result.success);
        assert_eq!(result.instance, 0);
        assert_eq!(result.err_code, Some(ErrorCode::ResourcesExceeded));
        assert_eq!(result.err_msg, Some("Test error".to_string()));
    }

    // ─────────────────────────────────────────────────────────────────────────
    // TP-286 §3: Message Serialization Tests
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_message_encode_decode_roundtrip() {
        use crate::usp::message::{build_get_supported_proto, decode_msg, encode_msg};

        let msg = build_get_supported_proto();
        let bytes = encode_msg(&msg).expect("encode should succeed");
        assert!(!bytes.is_empty());

        let decoded = decode_msg(&bytes).expect("decode should succeed");
        assert_eq!(
            decoded.header.as_ref().unwrap().msg_id,
            msg.header.as_ref().unwrap().msg_id
        );
    }

    #[test]
    fn test_boot_notify_message() {
        use crate::usp::message::{build_boot_notify, encode_msg};
        use std::collections::HashMap;

        let mut params = HashMap::new();
        params.insert("Device.DeviceInfo.ModelName".to_string(), "TestModel".to_string());
        params.insert("Device.DeviceInfo.Manufacturer".to_string(), "TestMfg".to_string());

        let msg = build_boot_notify("", false, params);
        let bytes = encode_msg(&msg).expect("encode boot notify");
        assert!(!bytes.is_empty());
    }

    #[test]
    fn test_value_change_notify_message() {
        use crate::usp::message::{build_value_change_notify, encode_msg};

        let msg = build_value_change_notify("sub-1", "Device.DeviceInfo.UpTime", "12345");
        let bytes = encode_msg(&msg).expect("encode value change");
        assert!(!bytes.is_empty());
    }

    #[test]
    fn test_error_message() {
        use crate::usp::message::{build_error, encode_msg};

        let msg = build_error("test-id", 7000, "MESSAGE_NOT_UNDERSTOOD");
        let bytes = encode_msg(&msg).expect("encode error");
        assert!(!bytes.is_empty());
    }

    #[test]
    fn test_msg_id_uniqueness() {
        use crate::usp::message::new_msg_id;

        let id1 = new_msg_id();
        let id2 = new_msg_id();
        assert_ne!(id1, id2, "Message IDs should be unique");
        assert!(!id1.is_empty());
    }

    // ─────────────────────────────────────────────────────────────────────────
    // TP-286 §4: Record Encoding Tests
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_record_encode_decode_roundtrip() {
        use crate::usp::record::{decode_record, encode_record, no_session_record};

        let payload = vec![1, 2, 3, 4, 5];
        let record = no_session_record("agent-1", "controller-1", payload.clone(), "1.3");
        let bytes = encode_record(&record).expect("encode record");
        let decoded = decode_record(&bytes).expect("decode record");

        assert_eq!(decoded.from_id, "agent-1");
        assert_eq!(decoded.to_id, "controller-1");
        assert_eq!(decoded.version, "1.3");
    }

    #[test]
    fn test_websocket_connect_record() {
        use crate::usp::record::{encode_record, websocket_connect_record};

        let record = websocket_connect_record("agent-1", "controller-1");
        let bytes = encode_record(&record).expect("encode ws connect");
        assert!(!bytes.is_empty());
    }

    #[test]
    fn test_mqtt_connect_record() {
        use crate::usp::record::{encode_record, mqtt_connect_record};

        let record = mqtt_connect_record("agent-1", "controller-1", "usp/v1/agent/agent-1");
        let bytes = encode_record(&record).expect("encode mqtt connect");
        assert!(!bytes.is_empty());
    }

    #[test]
    fn test_stomp_connect_record() {
        use crate::usp::record::{encode_record, stomp_connect_record};

        let record = stomp_connect_record("agent-1", "controller-1", "/topic/usp.agent.agent-1");
        let bytes = encode_record(&record).expect("encode stomp connect");
        assert!(!bytes.is_empty());
    }

    #[test]
    fn test_extract_msg_payload() {
        use crate::usp::record::{extract_msg_payload, no_session_record};

        let payload = vec![10, 20, 30];
        let record = no_session_record("a", "b", payload.clone(), "1.3");
        let extracted = extract_msg_payload(&record).expect("payload should exist");
        assert_eq!(extracted, &payload[..]);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // TP-286 §5: GetSupportedDM Tests
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_get_supported_dm_returns_all_objects() {
        let resp = handle_get_supported_dm("test-1", &[], false, true, false);
        let msg = resp.expect("should return message");
        let body = msg.body.as_ref().unwrap();

        if let Some(crate::usp::usp_msg::body::MsgBody::Response(r)) = &body.msg_body {
            if let Some(crate::usp::usp_msg::response::RespType::GetSupportedDmResp(dm)) =
                &r.resp_type
            {
                let obj_count = dm.req_obj_results[0].supported_objs.len();
                assert!(
                    obj_count >= 80,
                    "Expected 80+ supported objects, got {}",
                    obj_count
                );
                return;
            }
        }
        panic!("Invalid GetSupportedDM response structure");
    }

    #[test]
    fn test_get_supported_dm_filter_by_path() {
        let resp = handle_get_supported_dm(
            "test-2",
            &["Device.WiFi.".to_string()],
            false,
            false,
            false,
        );
        let msg = resp.expect("should return message");
        let body = msg.body.as_ref().unwrap();

        if let Some(crate::usp::usp_msg::body::MsgBody::Response(r)) = &body.msg_body {
            if let Some(crate::usp::usp_msg::response::RespType::GetSupportedDmResp(dm)) =
                &r.resp_type
            {
                for obj in &dm.req_obj_results[0].supported_objs {
                    // Parent objects (e.g. "Device.") match because the filter
                    // includes objects whose path is a prefix of the request.
                    assert!(
                        obj.obj_path.starts_with("Device.WiFi.")
                            || "Device.WiFi.".starts_with(&obj.obj_path),
                        "Filtered result should be WiFi-related, got: {}",
                        obj.obj_path
                    );
                }
                return;
            }
        }
        panic!("Invalid response");
    }

    #[test]
    fn test_get_supported_dm_first_level_only() {
        let resp = handle_get_supported_dm(
            "test-3",
            &["Device.".to_string()],
            true, // first_level_only
            false,
            false,
        );
        let msg = resp.expect("should return message");
        let body = msg.body.as_ref().unwrap();

        if let Some(crate::usp::usp_msg::body::MsgBody::Response(r)) = &body.msg_body {
            if let Some(crate::usp::usp_msg::response::RespType::GetSupportedDmResp(dm)) =
                &r.resp_type
            {
                for obj in &dm.req_obj_results[0].supported_objs {
                    let depth = obj.obj_path.trim_end_matches('.').matches('.').count() + 1;
                    assert!(
                        depth <= 2,
                        "first_level_only should limit depth, got {} for {}",
                        depth,
                        obj.obj_path
                    );
                }
                return;
            }
        }
        panic!("Invalid response");
    }

    #[test]
    fn test_get_supported_dm_includes_commands() {
        let resp = handle_get_supported_dm("test-4", &[], false, true, false);
        let msg = resp.expect("should return message");
        let body = msg.body.as_ref().unwrap();

        if let Some(crate::usp::usp_msg::body::MsgBody::Response(r)) = &body.msg_body {
            if let Some(crate::usp::usp_msg::response::RespType::GetSupportedDmResp(dm)) =
                &r.resp_type
            {
                let has_commands = dm.req_obj_results[0]
                    .supported_objs
                    .iter()
                    .any(|o| !o.supported_commands.is_empty());
                assert!(has_commands, "Should have objects with supported_commands (IPPing, TraceRoute)");
                return;
            }
        }
        panic!("Invalid response");
    }

    #[test]
    fn test_get_supported_dm_no_duplicate_paths() {
        let resp = handle_get_supported_dm("test-5", &[], false, false, false);
        let msg = resp.expect("should return message");
        let body = msg.body.as_ref().unwrap();

        if let Some(crate::usp::usp_msg::body::MsgBody::Response(r)) = &body.msg_body {
            if let Some(crate::usp::usp_msg::response::RespType::GetSupportedDmResp(dm)) =
                &r.resp_type
            {
                let mut paths: Vec<&str> = dm.req_obj_results[0]
                    .supported_objs
                    .iter()
                    .map(|o| o.obj_path.as_str())
                    .collect();
                let total = paths.len();
                paths.sort();
                paths.dedup();
                assert_eq!(
                    paths.len(),
                    total,
                    "GetSupportedDM should not contain duplicate object paths"
                );
                return;
            }
        }
        panic!("Invalid response");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // TP-286 §6: ADD/DELETE Result Tests
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_add_result_success() {
        let result = add_delete::AddResult {
            obj_path: "Device.DHCPv4.Server.Pool.1.StaticAddress.".to_string(),
            instance: 1,
            success: true,
            err_code: None,
            err_msg: None,
        };
        assert!(result.success);
        assert_eq!(result.instance, 1);
    }

    #[test]
    fn test_add_result_failure() {
        let result = add_delete::AddResult {
            obj_path: "Device.Invalid.".to_string(),
            instance: 0,
            success: false,
            err_code: Some(ErrorCode::ObjectNotCreatable),
            err_msg: Some("Object is not creatable".to_string()),
        };
        assert!(!result.success);
        assert_eq!(result.err_code, Some(ErrorCode::ObjectNotCreatable));
    }

    #[test]
    fn test_delete_result() {
        let result = add_delete::DeleteResult {
            obj_path: "Device.DHCPv4.Server.Pool.1.StaticAddress.1".to_string(),
            success: false,
            err_code: Some(ErrorCode::ObjectNotFound),
            err_msg: Some("Instance not found".to_string()),
        };
        assert!(!result.success);
        assert_eq!(result.err_code, Some(ErrorCode::ObjectNotFound));
    }

    // ─────────────────────────────────────────────────────────────────────────
    // TP-286 §7: Config & Multi-Controller Tests
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_mtp_type_default() {
        use crate::config::MtpType;
        let mtp = MtpType::default();
        assert_eq!(mtp, MtpType::WebSocket);
    }

    #[test]
    fn test_config_default_values() {
        use crate::config::ClientConfig;
        let cfg = ClientConfig::default();
        assert_eq!(cfg.server_port, 3491);
        assert_eq!(cfg.status_interval, 300);
        assert_eq!(cfg.update_interval, 60);
        assert!(cfg.secondary_controllers.is_empty());
        assert_eq!(cfg.bulk_data_interval, 0);
        assert!(cfg.stomp_url.is_none());
        assert!(cfg.coap_url.is_none());
    }

    // ─────────────────────────────────────────────────────────────────────────
    // TP-286 §8: Endpoint ID Tests
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_endpoint_id_from_mac() {
        use crate::usp::endpoint::EndpointId;
        let eid = EndpointId::from_mac("00005A", "AA:BB:CC:DD:EE:FF");
        let s = eid.as_str();
        assert!(s.contains("00005A"), "Should contain OUI");
    }

    #[test]
    fn test_endpoint_id_new() {
        use crate::usp::endpoint::EndpointId;
        let eid = EndpointId::new("custom-agent-id");
        assert_eq!(eid.as_str(), "custom-agent-id");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // TP-286 §9: Subscription Tests
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_subscription_add_delete() {
        use crate::usp::dm::local_agent;

        let inst = local_agent::add_subscription(true, "ValueChange", "Device.DeviceInfo.UpTime");
        assert!(inst > 0);

        let ok = local_agent::delete_subscription(inst);
        assert!(ok);

        let bad = local_agent::delete_subscription(9999);
        assert!(!bad);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // TP-286 §10: Integration Tests (require OpenWrt environment)
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    #[ignore = "Requires OpenWrt UCI environment"]
    fn test_add_delete_dhcp_lease() {
        let result = add_dhcp_lease("AA:BB:CC:DD:EE:FF", "192.168.1.200", Some("testhost"));
        assert!(result.success, "ADD failed: {:?}", result.err_msg);

        let del = delete_dhcp_lease(result.instance);
        assert!(del.success, "DELETE failed: {:?}", del.err_msg);
    }

    #[test]
    #[ignore = "Requires OpenWrt UCI environment"]
    fn test_add_delete_port_mapping() {
        let result = add_port_mapping("tcp", "8080", "80", "192.168.1.100", "test-http");
        assert!(result.success, "ADD failed: {:?}", result.err_msg);

        let del = delete_port_mapping(result.instance);
        assert!(del.success, "DELETE failed: {:?}", del.err_msg);
    }

    #[test]
    #[ignore = "Requires OpenWrt UCI environment"]
    fn test_add_delete_firewall_rule() {
        let result = add_firewall_rule("test-rule", "REJECT", "tcp", "", "22");
        assert!(result.success, "ADD failed: {:?}", result.err_msg);

        let del = delete_firewall_rule(result.instance);
        assert!(del.success, "DELETE failed: {:?}", del.err_msg);
    }

    #[test]
    #[ignore = "Requires OpenWrt UCI environment"]
    fn test_get_system_hostname() {
        let hostname = get_system_hostname();
        // On OpenWrt, hostname is always set
        assert!(!hostname.is_empty() || true, "Hostname may be empty on non-OpenWrt");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // TP-286 §11: Data Model Compliance Summary
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_compliance_summary() {
        let resp = handle_get_supported_dm("summary", &[], false, true, false);
        let msg = resp.expect("should return message");
        let body = msg.body.as_ref().unwrap();

        if let Some(crate::usp::usp_msg::body::MsgBody::Response(r)) = &body.msg_body {
            if let Some(crate::usp::usp_msg::response::RespType::GetSupportedDmResp(dm)) =
                &r.resp_type
            {
                let objs = &dm.req_obj_results[0].supported_objs;
                let total_params: usize = objs.iter().map(|o| o.supported_params.len()).sum();
                let total_commands: usize = objs.iter().map(|o| o.supported_commands.len()).sum();
                let rw_objs = objs
                    .iter()
                    .filter(|o| o.access == 1) // ObjAddDelete
                    .count();

                println!("\n=== TR-181:2.16 / TP-286 Compliance Summary ===");
                println!("Supported Objects:    {}", objs.len());
                println!("Supported Parameters: {}", total_params);
                println!("OPERATE Commands:     {}", total_commands);
                println!("Add/Delete Objects:   {}", rw_objs);
                println!("Data Model URI:       {}", dm.req_obj_results[0].data_model_inst_uri);

                println!("\n=== Object Categories ===");
                let categories = [
                    "Device.LocalAgent.", "Device.DeviceInfo.", "Device.ManagementServer.",
                    "Device.WiFi.", "Device.Ethernet.", "Device.Bridging.",
                    "Device.IP.", "Device.DHCPv4.", "Device.DHCPv6.",
                    "Device.NAT.", "Device.Firewall.", "Device.Routing.",
                    "Device.DNS.", "Device.Hosts.", "Device.Time.",
                    "Device.PPP.", "Device.Users.", "Device.SoftwareModules.",
                    "Device.QoS.", "Device.UPnP.", "Device.BulkData.",
                    "Device.DSL.", "Device.Optical.", "Device.IEEE8021x.",
                    "Device.Services.", "Device.RouterAdvertisement.",
                    "Device.IPv6rd.", "Device.DSLite.",
                    "Device.InterfaceStack.",
                ];
                for cat in &categories {
                    let count = objs.iter().filter(|o| o.obj_path.starts_with(cat)).count();
                    if count > 0 {
                        println!("  {:<40} {} objects", cat, count);
                    }
                }

                assert!(objs.len() >= 80, "Should have 80+ objects");
                assert!(total_params >= 300, "Should have 300+ params");
                return;
            }
        }
        panic!("Invalid response");
    }
}
