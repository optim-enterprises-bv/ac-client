//! Protobuf message types for the USP/TR-369 protocol (client side).
//!
//! Types are generated from `proto/acp.proto` by `prost-build` at compile time.
//! The proto file defines USP message payloads for device communication.

use prost::Message;

// Include the prost-generated code.
include!(concat!(env!("OUT_DIR"), "/acp.rs"));

/// Encode any prost [`Message`] to a `Vec<u8>`.
pub fn encode<M: Message>(msg: &M) -> Vec<u8> {
    msg.encode_to_vec()
}
