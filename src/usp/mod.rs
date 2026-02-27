//! TR-369 / USP (User Services Platform) support for ac-client.
//! ac-client acts as a USP Agent.

pub mod agent;
pub mod dm;
pub mod endpoint;
pub mod message;
pub mod mtp;
pub mod record;
pub mod session;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum UspError {
    #[error("protobuf encode: {0}")]
    Encode(#[from] prost::EncodeError),
    #[error("protobuf decode: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error("WebSocket: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("MQTT: {0}")]
    Mqtt(String),
    #[error("data model: {0}")]
    DataModel(String),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("IO: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, UspError>;

pub mod usp_record {
    #![allow(dead_code, clippy::all)]
    include!(concat!(env!("OUT_DIR"), "/usp_record.rs"));
}

pub mod usp_msg {
    #![allow(dead_code, clippy::all)]
    include!(concat!(env!("OUT_DIR"), "/usp_msg.rs"));
}

pub use usp_msg::header::MessageType;
pub use usp_record::record::RecordType;
