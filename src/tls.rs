//! TLS client connector for the ACP/1.0 protocol.
//!
//! Builds a `TlsConnector` configured for:
//!   - TLS 1.3 only (matches the server requirement for post-quantum KEM)
//!   - Mutual TLS: client presents its certificate
//!   - Server certificate validated against the configured CA
//!   - Hostname verification disabled — the C client behaviour (OpenSSL
//!     `SSL_VERIFY_PEER` without `SSL_set1_host`).  The server cert CN
//!     ("ac-server") is sent as the SNI hint.
//!
//! The `rustls-post-quantum` provider must be installed as the global default
//! before calling any function in this module.

use std::fs;
use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, Error as TlsError, RootCertStore, SignatureScheme};
use rustls_pemfile::{certs, private_key};
use tokio::net::TcpStream;
use tokio_rustls::{client::TlsStream, TlsConnector};

use crate::error::{AcError, Result};

// ── ACP server certificate verifier ──────────────────────────────────────────

/// Verifies the server certificate chain against our CA trust roots, but does
/// NOT check hostname / SAN matching.
///
/// This exactly matches the C client's OpenSSL behaviour:
/// `SSL_CTX_set_verify(ctx, SSL_VERIFY_PEER, NULL)` verifies the chain but
/// OpenSSL does not perform hostname matching unless `SSL_set1_host()` is also
/// called.
#[derive(Debug)]
struct AcpServerVerifier {
    /// Delegates all chain + revocation verification to the standard WebPki verifier.
    inner:    Arc<dyn ServerCertVerifier>,
    provider: Arc<CryptoProvider>,
}

impl AcpServerVerifier {
    fn new(root_store: RootCertStore, provider: Arc<CryptoProvider>) -> Result<Arc<Self>> {
        let inner = rustls::client::WebPkiServerVerifier::builder_with_provider(
            Arc::new(root_store),
            Arc::clone(&provider),
        )
        .build()
        .map_err(|e| AcError::Verifier(e.to_string()))?;

        Ok(Arc::new(Self { inner, provider }))
    }
}

impl ServerCertVerifier for AcpServerVerifier {
    fn verify_server_cert(
        &self,
        end_entity:    &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name:   &ServerName<'_>,
        ocsp_response: &[u8],
        now:           UnixTime,
    ) -> std::result::Result<ServerCertVerified, TlsError> {
        match self.inner.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        ) {
            Ok(v) => Ok(v),
            // Suppress hostname mismatch — same as C client.
            // Chain validity, expiry, and EKU are still enforced by `inner`.
            Err(TlsError::InvalidCertificate(rustls::CertificateError::NotValidForName)) => {
                Ok(ServerCertVerified::assertion())
            }
            Err(e) => Err(e),
        }
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert:    &CertificateDer<'_>,
        _dss:     &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, TlsError> {
        // TLS 1.2 is disabled — this should never be called.
        Err(TlsError::General("TLS 1.2 not supported".into()))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert:    &CertificateDer<'_>,
        dss:     &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, TlsError> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

// ── Connector factory ─────────────────────────────────────────────────────────

/// A pre-built TLS connector ready to open ACP connections.
pub struct AcpConnector {
    connector:   TlsConnector,
    server_name: ServerName<'static>,
    server_host: String,
    server_port: u16,
}

impl AcpConnector {
    /// Establish a new TCP+TLS connection to the ACP server.
    pub async fn connect(&self) -> Result<TlsStream<TcpStream>> {
        let addr = format!("{}:{}", self.server_host, self.server_port);
        let stream = TcpStream::connect(&addr).await?;
        let tls = self.connector
            .connect(self.server_name.clone(), stream)
            .await?;
        Ok(tls)
    }
}

/// Build a connector using a specific client certificate and key.
///
/// `ca_file`   — PEM file containing the CA cert that signed the server cert.
/// `cert_file` — PEM file containing the client cert chain.
/// `key_file`  — PEM file containing the client private key.
/// `server_cn` — Server name sent as SNI (e.g. "ac-server").
pub fn build_connector(
    ca_file:     &Path,
    cert_file:   &Path,
    key_file:    &Path,
    server_cn:   &str,
    server_host: &str,
    server_port: u16,
) -> Result<AcpConnector> {
    let provider = CryptoProvider::get_default()
        .expect("call rustls_post_quantum::provider().install_default() before build_connector")
        .clone();

    // ── CA trust store ────────────────────────────────────────────────────────
    let mut root_store = RootCertStore::empty();
    let ca_pem = fs::read(ca_file)?;
    let mut cursor = Cursor::new(ca_pem);
    for cert in certs(&mut cursor) {
        root_store.add(cert?)?;
    }

    // ── Client certificate chain ──────────────────────────────────────────────
    let cert_pem = fs::read(cert_file)?;
    let mut cursor = Cursor::new(cert_pem);
    let cert_chain: Vec<CertificateDer<'static>> = certs(&mut cursor)
        .collect::<std::io::Result<Vec<_>>>()?;

    // ── Client private key ────────────────────────────────────────────────────
    let key_pem = fs::read(key_file)?;
    let mut cursor = Cursor::new(key_pem);
    let private_key = private_key(&mut cursor)?
        .ok_or_else(|| AcError::Config(format!(
            "no private key found in {}",
            key_file.display()
        )))?;

    // ── TLS 1.3-only client config with custom chain verifier ─────────────────
    let verifier = AcpServerVerifier::new(root_store, Arc::clone(&provider))?;

    let tls_config = ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(AcError::Tls)?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(cert_chain, private_key)
        .map_err(AcError::Tls)?;

    let server_name = ServerName::try_from(server_cn.to_string())?;

    Ok(AcpConnector {
        connector:   TlsConnector::from(Arc::new(tls_config)),
        server_name,
        server_host: server_host.to_string(),
        server_port,
    })
}

/// Build and return a `rustls::ClientConfig` suitable for use with
/// tokio-tungstenite's `Connector::Rustls` (USP WebSocket MTP).
pub fn build_tls_config(cfg: &crate::config::ClientConfig) -> Result<Arc<ClientConfig>> {
    let provider = CryptoProvider::get_default()
        .expect("call rustls_post_quantum::provider().install_default() first")
        .clone();

    let mut root_store = RootCertStore::empty();
    let ca_pem = fs::read(&cfg.ca_file)?;
    for cert in certs(&mut Cursor::new(ca_pem)) {
        root_store.add(cert?)?;
    }

    let cert_pem = fs::read(&cfg.cert_file)?;
    let cert_chain: Vec<CertificateDer<'static>> = certs(&mut Cursor::new(cert_pem))
        .collect::<std::io::Result<Vec<_>>>()?;

    let key_pem = fs::read(&cfg.key_file)?;
    let private_key = private_key(&mut Cursor::new(key_pem))?
        .ok_or_else(|| AcError::Config("no private key found".into()))?;

    let verifier = AcpServerVerifier::new(root_store, Arc::clone(&provider))?;

    let tls_config = ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(AcError::Tls)?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(cert_chain, private_key)
        .map_err(AcError::Tls)?;

    Ok(Arc::new(tls_config))
}
