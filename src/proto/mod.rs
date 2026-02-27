//! Protobuf message types for the ACP/1.0 protocol (client side).
//!
//! Types are generated from `proto/acp.proto` by `prost-build` at compile time.
//!
//! The client encodes outgoing request messages and decodes incoming response
//! messages:
//!   Outgoing: `InitRequest`, `StatusRequest`, `CamInfoRequest`, `CamImgRequest`
//!   Incoming: `CertsResponse`, `SystemConfig`

// The generated schema includes MeshConnectConfig and other types the client
// doesn't use directly — suppress the dead_code lints for the whole module.
#![allow(dead_code)]

use prost::Message;

use crate::error::{AcError, Result};

// Include the prost-generated code (generated from ../ac-server/proto/acp.proto).
include!(concat!(env!("OUT_DIR"), "/acp.rs"));

// ── Encode helpers (outgoing requests) ───────────────────────────────────────

/// Encode any prost [`Message`] to a `Vec<u8>`.
pub fn encode<M: Message>(msg: &M) -> Vec<u8> {
    msg.encode_to_vec()
}

// ── Decode helpers (incoming responses) ──────────────────────────────────────

/// Decode a [`CertsResponse`] from raw bytes (body of a CERT reply).
pub fn decode_certs(data: &[u8]) -> Result<CertsResponse> {
    CertsResponse::decode(data).map_err(AcError::Proto)
}

/// Decode a [`SystemConfig`] from raw bytes (body of an ACK reply to GET_CONFIG).
pub fn decode_config(data: &[u8]) -> Result<SystemConfig> {
    SystemConfig::decode(data).map_err(AcError::Proto)
}
