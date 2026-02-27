//! USP Record encode / decode helpers.
//!
//! USP Records are the binary envelope framing USP Messages over any MTP.
//! They are serialised with prost (protobuf 3).

use prost::Message;

use super::usp_record::{
    record::RecordType, NoSessionContextRecord, Record, WebSocketConnectRecord,
    MqttConnectRecord, DisconnectRecord,
};
use super::{Result, UspError};

// ── Decode ────────────────────────────────────────────────────────────────────

/// Decode a [`Record`] from raw bytes (as received from a WebSocket frame or
/// MQTT message payload).
pub fn decode_record(data: &[u8]) -> Result<Record> {
    Record::decode(data).map_err(UspError::Decode)
}

// ── Encode ────────────────────────────────────────────────────────────────────

/// Encode a [`Record`] to bytes ready to send over the MTP.
pub fn encode_record(record: &Record) -> Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(record.encoded_len());
    record.encode(&mut buf)?;
    Ok(buf)
}

// ── Constructors ──────────────────────────────────────────────────────────────

/// Build a `NoSessionContextRecord` carrying an encoded USP Msg payload.
/// Used for WebSocket MTP where session ordering is provided by the transport.
///
/// `usp_version` should be the agreed version from `GetSupportedProto`
/// negotiation (TR-369 §6.2.1); use `"1.3"` before negotiation completes.
pub fn no_session_record(
    from_id: &str,
    to_id: &str,
    msg_bytes: Vec<u8>,
    usp_version: &str,
) -> Record {
    Record {
        version: usp_version.into(),
        to_id: to_id.into(),
        from_id: from_id.into(),
        payload_security: 0, // PLAINTEXT
        mac_signature: vec![],
        sender_cert: vec![],
        record_type: Some(RecordType::NoSessionContext(
            NoSessionContextRecord { payload: msg_bytes },
        )),
    }
}

/// Build a `WebSocketConnectRecord` — sent once when a controller first
/// accepts a WebSocket connection from an agent.
pub fn websocket_connect_record(from_id: &str, to_id: &str) -> Record {
    Record {
        version: "1.3".into(),
        to_id: to_id.into(),
        from_id: from_id.into(),
        payload_security: 0,
        mac_signature: vec![],
        sender_cert: vec![],
        record_type: Some(RecordType::WebsocketConnect(WebSocketConnectRecord {})),
    }
}

/// Build an `MqttConnectRecord`.
pub fn mqtt_connect_record(
    from_id: &str,
    to_id: &str,
    subscribed_topic: &str,
) -> Record {
    Record {
        version: "1.3".into(),
        to_id: to_id.into(),
        from_id: from_id.into(),
        payload_security: 0,
        mac_signature: vec![],
        sender_cert: vec![],
        record_type: Some(RecordType::MqttConnect(MqttConnectRecord {
            version: 0, // V3_1_1
            subscribed_topic: subscribed_topic.into(),
        })),
    }
}

/// Build a `DisconnectRecord`.
pub fn disconnect_record(from_id: &str, to_id: &str, reason: &str) -> Record {
    Record {
        version: "1.3".into(),
        to_id: to_id.into(),
        from_id: from_id.into(),
        payload_security: 0,
        mac_signature: vec![],
        sender_cert: vec![],
        record_type: Some(RecordType::Disconnect(DisconnectRecord {
            reason: reason.into(),
            reason_code: 0,
        })),
    }
}

/// Extract the serialised `Msg` payload bytes from a Record, regardless of
/// whether it uses NoSessionContext or SessionContext framing.
pub fn extract_msg_payload(record: &Record) -> Option<&[u8]> {
    match record.record_type.as_ref()? {
        RecordType::NoSessionContext(r) => Some(&r.payload),
        RecordType::SessionContext(r) => r.payload.first().map(|b| b.as_slice()),
        _ => None,
    }
}
