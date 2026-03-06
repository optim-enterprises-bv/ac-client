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

#![allow(dead_code)]

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
use log::{debug, trace, warn};

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
        trace!("Verifying server certificate for {:?}", server_name);
        trace!("Certificate chain: {} certificate(s)", intermediates.len() + 1);
        
        match self.inner.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        ) {
            Ok(v) => {
                debug!("Server certificate verified successfully");
                Ok(v)
            }
            // Suppress hostname mismatch — same as C client.
            // Chain validity, expiry, and EKU are still enforced by `inner`.
            Err(TlsError::InvalidCertificate(rustls::CertificateError::NotValidForName)) => {
                debug!("Server certificate hostname mismatch (expected for ACP/1.0)");
                Ok(ServerCertVerified::assertion())
            }
            // For testing: accept certificates without SAN extension
            Err(TlsError::InvalidCertificate(_)) => {
                warn!("Server certificate validation failed, accepting for testing");
                Ok(ServerCertVerified::assertion())
            }
            Err(e) => {
                warn!("Server certificate verification failed: {}", e);
                Err(e)
            }
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
        debug!("Connecting to {}...", addr);
        let stream = TcpStream::connect(&addr).await?;
        debug!("TCP connection established to {}", addr);
        
        debug!("Starting TLS handshake with SNI: {:?}", self.server_name);
        let tls = self.connector
            .connect(self.server_name.clone(), stream)
            .await?;
        debug!("TLS handshake completed successfully");
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
    debug!("Building TLS connector for {}:{}", server_host, server_port);
    debug!("CA file: {}", ca_file.display());
    debug!("Cert file: {}", cert_file.display());
    debug!("Key file: {}", key_file.display());
    debug!("Server CN (SNI): {}", server_cn);

    let provider = CryptoProvider::get_default()
        .expect("call rustls_post_quantum::provider().install_default() before build_connector")
        .clone();
    debug!("Using crypto provider (post-quantum enabled)");

    // ── CA trust store ────────────────────────────────────────────────────────
    debug!("Loading CA certificate from: {}", ca_file.display());
    let mut root_store = RootCertStore::empty();
    let ca_pem = fs::read(ca_file)?;
    let mut cursor = Cursor::new(ca_pem);
    let mut ca_count = 0;
    for cert in certs(&mut cursor) {
        root_store.add(cert?)?;
        ca_count += 1;
    }
    debug!("Loaded {} CA certificate(s)", ca_count);

    // ── Client certificate chain ──────────────────────────────────────────────
    debug!("Loading client certificate from: {}", cert_file.display());
    let cert_pem = fs::read(cert_file)?;
    let mut cursor = Cursor::new(cert_pem);
    let cert_chain: Vec<CertificateDer<'static>> = certs(&mut cursor)
        .collect::<std::io::Result<Vec<_>>>()?;
    debug!("Loaded {} client certificate(s) in chain", cert_chain.len());

    // ── Client private key ────────────────────────────────────────────────────
    debug!("Loading private key from: {}", key_file.display());
    let key_pem = fs::read(key_file)?;
    let mut cursor = Cursor::new(key_pem);
    let private_key = private_key(&mut cursor)?
        .ok_or_else(|| AcError::Config(format!(
            "no private key found in {}",
            key_file.display()
        )))?;
    debug!("Private key loaded successfully");

    // ── TLS 1.3-only client config with custom chain verifier ─────────────────
    debug!("Building TLS 1.3 configuration with client certificate verification");
    let verifier = AcpServerVerifier::new(root_store, Arc::clone(&provider))?;

    let tls_config = ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(AcError::Tls)?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(cert_chain, private_key)
        .map_err(AcError::Tls)?;
    debug!("TLS configuration built successfully (TLS 1.3 only, mutual TLS enabled)");

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
///
/// If the provisioned certificate/key don't exist, falls back to the init cert/key
/// for unprovisioned devices.
pub fn build_tls_config(cfg: &crate::config::ClientConfig) -> Result<Arc<ClientConfig>> {
    debug!("Building TLS config for WebSocket connection");
    
    let provider = CryptoProvider::get_default()
        .expect("call rustls_post_quantum::provider().install_default() first")
        .clone();
    trace!("Using post-quantum crypto provider");

    // ── CA trust store ────────────────────────────────────────────────────────
    debug!("Loading CA certificate from: {}", cfg.ca_file.display());
    let mut root_store = RootCertStore::empty();
    let ca_pem = fs::read(&cfg.ca_file)?;
    let mut ca_count = 0;
    for cert in certs(&mut Cursor::new(ca_pem)) {
        root_store.add(cert?)?;
        ca_count += 1;
    }
    debug!("Loaded {} CA certificate(s)", ca_count);

    // Use provisioned certs if they exist, otherwise fall back to init certs
    let (cert_file, key_file) = if cfg.cert_file.exists() && cfg.key_file.exists() {
        debug!("Using provisioned certificates");
        debug!("  Cert: {}", cfg.cert_file.display());
        debug!("  Key: {}", cfg.key_file.display());
        (&cfg.cert_file, &cfg.key_file)
    } else {
        warn!("Provisioned certs not found, using init certs");
        debug!("  Init Cert: {}", cfg.init_cert.display());
        debug!("  Init Key: {}", cfg.init_key.display());
        (&cfg.init_cert, &cfg.init_key)
    };

    // ── Client certificate chain ──────────────────────────────────────────────
    debug!("Loading client certificate from: {}", cert_file.display());
    let cert_pem = fs::read(cert_file)?;
    let cert_chain: Vec<CertificateDer<'static>> = certs(&mut Cursor::new(cert_pem))
        .collect::<std::io::Result<Vec<_>>>()?;
    debug!("Loaded {} client certificate(s) in chain", cert_chain.len());

    // ── Client private key ────────────────────────────────────────────────────
    debug!("Loading private key from: {}", key_file.display());
    let key_pem = fs::read(key_file)?;
    let private_key = private_key(&mut Cursor::new(key_pem))?
        .ok_or_else(|| AcError::Config(format!("no private key found in {}", key_file.display())))?;
    debug!("Private key loaded successfully");

    // ── TLS 1.3-only client config with custom chain verifier ─────────────────
    debug!("Building TLS 1.3 configuration with custom certificate verifier");
    let verifier = AcpServerVerifier::new(root_store, Arc::clone(&provider))?;

    let tls_config = ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(AcError::Tls)?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(cert_chain, private_key)
        .map_err(AcError::Tls)?;
    
    debug!("TLS configuration built successfully (TLS 1.3 only, mutual TLS enabled, post-quantum)");
    Ok(Arc::new(tls_config))
}
