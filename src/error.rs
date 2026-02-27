//! Error types for the ACP client.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AcError {
    #[error("IO: {0}")]
    Io(#[from] std::io::Error),

    #[error("TLS: {0}")]
    Tls(#[from] rustls::Error),

    #[error("TLS DNS name: {0}")]
    TlsDns(#[from] rustls::pki_types::InvalidDnsNameError),

    #[error("Protobuf decode: {0}")]
    Proto(#[from] prost::DecodeError),

    #[error("Task join: {0}")]
    Join(#[from] tokio::task::JoinError),

    #[error("HTTP: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Protocol: {0}")]
    Protocol(String),

    #[error("Config: {0}")]
    Config(String),

    #[error("TLS verifier: {0}")]
    Verifier(String),
}

impl From<rustls::client::VerifierBuilderError> for AcError {
    fn from(e: rustls::client::VerifierBuilderError) -> Self {
        Self::Verifier(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, AcError>;
