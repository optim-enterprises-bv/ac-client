//! EST enrolment — obtaining an operational certificate on first boot.
//!
//! # Why this exists
//!
//! `tls::build_tls_config` already prefers `cert_file`/`key_file` and falls back
//! to `init_cert`/`init_key`, so the agent was built expecting an operational
//! certificate to appear. Nothing ever produced one. On the test device
//! `/etc/apclient/certs/` was empty and every connection used the birth
//! certificate baked into the package — the same one on every install.
//!
//! The controller side has served EST at `/.well-known/est/simpleenroll` all
//! along (`crates/pki/src/est.rs`), and it issues certificates carrying the USP
//! Endpoint ID in a `subjectAltName` URI as TR-369 R-SEC.0a requires. Verified
//! by hand against the live endpoint:
//!
//! ```text
//! subject=CN=ea5ecacf3f18
//! issuer=O=Optim Aether Root CA, CN=Optim Aether Root CA Intermediate CA
//! X509v3 Subject Alternative Name:
//!     DNS:ea5ecacf3f18, URI:urn:bbf:usp:id:oui:00005A:ea:5e:ca:cf:3f:18
//! ```
//!
//! That certificate is what lets a controller check a USP Record's `from_id`
//! against the peer rather than trusting it as asserted. This module is the
//! agent half: generate the key, ask for the certificate, install it.
//!
//! # What it deliberately does not do
//!
//! It does not decide whether enrolment is *allowed*. The controller owns that
//! — the CSR's Common Name is checked against what the credential permits, and
//! a mismatch is refused there. Duplicating that judgement here would create a
//! second opinion about the same request, and the one that matters is the one
//! next to the CA.
//!
//! It also never overwrites an existing operational certificate. Re-enrolment
//! is `simplereenroll` and is a different exchange with different
//! authentication; silently replacing a working identity because a file looked
//! odd is how a fleet loses its certificates all at once.

use log::{debug, info, warn};

use crate::config::ClientConfig;

/// Common Name for the CSR: the MAC with separators stripped.
///
/// NOT the USP Endpoint ID. `valid_device_cn` on the controller accepts only
/// `[A-Za-z0-9_-]`, so `oui:00005A:aa:bb:...` is refused outright — the colons
/// are invalid. The controller derives the Endpoint ID URN from this bare form
/// and puts it in the SAN, which is where R-SEC.0a says it belongs.
fn csr_common_name(mac: &str) -> Option<String> {
    let hex: String = mac
        .chars()
        .filter(|c| !matches!(c, ':' | '-' | '.'))
        .collect();
    if hex.len() != 12 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(hex.to_ascii_lowercase())
}

/// Does an operational certificate already exist?
fn already_enrolled(cfg: &ClientConfig) -> bool {
    cfg.cert_file.exists() && cfg.key_file.exists()
}

/// Generate a keypair and a PKCS#10 CSR for `cn`.
///
/// Returns the DER CSR and the PEM private key. The key is generated HERE and
/// never leaves the device — the whole point of enrolling rather than shipping
/// a per-device secret in a package anyone can download.
fn build_csr(cn: &str) -> Result<(Vec<u8>, String), String> {
    use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair};

    let key_pair = KeyPair::generate().map_err(|e| format!("keypair generation failed: {e}"))?;
    let key_pem = key_pair.serialize_pem();

    let mut params =
        CertificateParams::new(vec![cn.to_owned()]).map_err(|e| format!("CSR params: {e}"))?;
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, cn);
    params.distinguished_name = dn;

    let csr = params
        .serialize_request(&key_pair)
        .map_err(|e| format!("CSR serialization failed: {e}"))?;

    Ok((csr.der().to_vec(), key_pem))
}

/// Write the certificate and key, key first and restrictively.
///
/// Order matters: `build_tls_config` treats "both files exist" as enrolled, so
/// writing the certificate first opens a window where a concurrent restart sees
/// a certificate with no key and fails to build any TLS config at all.
fn install(cfg: &ClientConfig, cert_pem: &str, key_pem: &str) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt;

    if let Some(dir) = cfg.cert_file.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }

    let mut k = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&cfg.key_file)
        .map_err(|e| format!("cannot write {}: {e}", cfg.key_file.display()))?;
    std::io::Write::write_all(&mut k, key_pem.as_bytes())
        .map_err(|e| format!("cannot write key: {e}"))?;
    drop(k);

    std::fs::write(&cfg.cert_file, cert_pem)
        .map_err(|e| format!("cannot write {}: {e}", cfg.cert_file.display()))?;
    Ok(())
}

/// Base URL for EST, derived from `ws_url` unless overridden.
///
/// `wss://gw.aether-io.com/usp` implies `https://est.aether-io.com`. Derived
/// rather than hardcoded so a private deployment does not have to patch the
/// binary, and overridable because the two need not share a hostname — on this
/// deployment they do not.
fn est_base(cfg: &ClientConfig) -> String {
    if let Ok(v) = std::env::var("EST_BASE_URL") {
        if !v.is_empty() {
            return v.trim_end_matches('/').to_owned();
        }
    }
    let host = cfg
        .ws_url
        .as_deref()
        .and_then(|u| u.split("://").nth(1))
        .and_then(|r| r.split('/').next())
        .unwrap_or("");
    match host.split_once('.') {
        Some((_, domain)) if !domain.is_empty() => format!("https://est.{domain}"),
        _ => "https://est.aether-io.com".to_owned(),
    }
}

/// Enrol if this device has no operational certificate yet.
///
/// Returns `true` when a certificate was installed, so the caller knows the TLS
/// config must be rebuilt before connecting.
pub async fn enrol_if_needed(cfg: &ClientConfig) -> bool {
    if already_enrolled(cfg) {
        debug!("est: operational certificate already present, not enrolling");
        return false;
    }

    let Some(cn) = csr_common_name(&cfg.mac_addr) else {
        // Loud, because everything downstream silently uses the shared birth
        // certificate instead and looks like it is working.
        warn!(
            "est: cannot derive a CSR Common Name from mac_addr {:?} -- \
             this device will keep using the shared bootstrap certificate and \
             will NOT satisfy TR-369 R-SEC.0a",
            cfg.mac_addr
        );
        return false;
    };

    let token = std::env::var("EST_ENROLMENT_TOKEN")
        .ok()
        .filter(|t| !t.is_empty())
        .or_else(|| Some(cfg.claim_token.clone()).filter(|t| !t.is_empty()));

    let (csr_der, key_pem) = match build_csr(&cn) {
        Ok(v) => v,
        Err(e) => {
            warn!("est: {e}");
            return false;
        }
    };

    let url = format!("{}/.well-known/est/simpleenroll", est_base(cfg));
    info!("est: enrolling {cn} at {url}");

    // RFC 7030 §4.2.1: base64-encoded DER PKCS#10, `application/pkcs10`.
    use base64::Engine;
    let body = base64::engine::general_purpose::STANDARD.encode(&csr_der);

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!("est: cannot build HTTP client: {e}");
            return false;
        }
    };

    let mut req = client
        .post(&url)
        .header("Content-Type", "application/pkcs10")
        .header("Content-Transfer-Encoding", "base64")
        .body(body);
    if let Some(t) = &token {
        req = req.header("Authorization", format!("Bearer {t}"));
    }

    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            warn!("est: enrolment request failed: {e}");
            return false;
        }
    };

    if !resp.status().is_success() {
        // The controller's refusals are specific -- no token, wrong serial,
        // expired -- and repeating them verbatim is more use than a code.
        let status = resp.status();
        let detail = resp.text().await.unwrap_or_default();
        warn!("est: enrolment refused ({status}): {}", detail.trim());
        return false;
    }

    let b64 = resp.text().await.unwrap_or_default();
    let der = match base64::engine::general_purpose::STANDARD.decode(b64.trim().as_bytes()) {
        Ok(d) => d,
        Err(e) => {
            warn!("est: response is not valid base64: {e}");
            return false;
        }
    };

    // RFC 7030 returns a degenerate PKCS#7 (certs-only). Extract the leaf.
    let cert_pem = match pkcs7_to_pem(&der) {
        Some(p) => p,
        None => {
            warn!("est: could not extract a certificate from the PKCS#7 response");
            return false;
        }
    };

    if let Err(e) = install(cfg, &cert_pem, &key_pem) {
        warn!("est: {e}");
        return false;
    }

    info!(
        "est: enrolled -- operational certificate installed at {}",
        cfg.cert_file.display()
    );
    true
}

/// Pull the certificates out of a degenerate PKCS#7 and re-emit them as PEM.
///
/// Deliberately a structural scan rather than a full ASN.1 parse: the payload
/// is a certs-only SignedData, so every `SEQUENCE` that parses as a certificate
/// IS one, and a full parser here would be a second, divergent opinion about a
/// format the CA already validated.
fn pkcs7_to_pem(der: &[u8]) -> Option<String> {
    use base64::Engine;

    let mut out = String::new();
    // X.509 certificates inside the blob start with SEQUENCE (0x30 0x82 len).
    let mut i = 0usize;
    while i + 4 < der.len() {
        if der[i] == 0x30 && der[i + 1] == 0x82 {
            let len = ((der[i + 2] as usize) << 8 | der[i + 3] as usize) + 4;
            if i + len <= der.len() {
                let candidate = &der[i..i + len];
                // A certificate's first inner element is also a SEQUENCE
                // (tbsCertificate); a bare SignedData wrapper is not.
                if candidate.len() > 8 && candidate[4] == 0x30 {
                    let b64 = base64::engine::general_purpose::STANDARD.encode(candidate);
                    out.push_str("-----BEGIN CERTIFICATE-----\n");
                    for chunk in b64.as_bytes().chunks(64) {
                        out.push_str(std::str::from_utf8(chunk).ok()?);
                        out.push('\n');
                    }
                    out.push_str("-----END CERTIFICATE-----\n");
                    i += len;
                    continue;
                }
            }
        }
        i += 1;
    }
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The CSR Common Name is the bare MAC, never the Endpoint ID.
    ///
    /// The controller's `valid_device_cn` accepts only `[A-Za-z0-9_-]`, so a
    /// colon-bearing endpoint ID is refused before it reaches the CA. Getting
    /// this wrong fails every enrolment with a message about the serial rather
    /// than about the format.
    #[test]
    fn the_common_name_is_the_bare_mac() {
        assert_eq!(
            csr_common_name("ea:5e:ca:cf:3f:18").as_deref(),
            Some("ea5ecacf3f18")
        );
        assert_eq!(
            csr_common_name("EA-5E-CA-CF-3F-18").as_deref(),
            Some("ea5ecacf3f18")
        );
        // Not a MAC: no Endpoint ID can be derived, so no CSR should be made.
        assert_eq!(csr_common_name("wlan-ap-1234"), None);
        assert_eq!(csr_common_name(""), None);
        assert_eq!(csr_common_name("ea:5e:ca:cf:3f"), None);
    }

    /// A CSR must actually carry the Common Name we asked for.
    #[test]
    fn the_csr_carries_the_common_name() {
        let (der, key) = build_csr("ea5ecacf3f18").expect("csr");
        assert!(!der.is_empty(), "CSR must not be empty");
        assert!(
            key.contains("PRIVATE KEY"),
            "a private key must be produced"
        );
        // The CN appears verbatim in the DER subject.
        assert!(
            der.windows(12).any(|w| w == b"ea5ecacf3f18"),
            "the CSR must carry the requested Common Name",
        );
    }

    /// An already-enrolled device must not re-enrol.
    ///
    /// Overwriting a working identity because a file looked odd is how a fleet
    /// loses its certificates at once. Re-enrolment is `simplereenroll`, a
    /// different exchange with different authentication.
    #[test]
    fn enrolment_is_skipped_when_a_certificate_exists() {
        let mut cfg = ClientConfig::default();
        let dir = std::env::temp_dir().join(format!("est-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        cfg.cert_file = dir.join("client.crt");
        cfg.key_file = dir.join("client.key");
        assert!(!already_enrolled(&cfg), "nothing written yet");
        std::fs::write(&cfg.cert_file, "x").unwrap();
        std::fs::write(&cfg.key_file, "y").unwrap();
        assert!(already_enrolled(&cfg), "both present means enrolled");
        // A certificate with no key is NOT enrolled -- that is the window the
        // write order in `install` exists to avoid.
        std::fs::remove_file(&cfg.key_file).unwrap();
        assert!(!already_enrolled(&cfg));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The EST host is derived from ws_url, not hardcoded.
    #[test]
    fn the_est_host_follows_the_controller_domain() {
        let mut cfg = ClientConfig::default();
        cfg.ws_url = Some("wss://gw.aether-io.com/usp".into());
        assert_eq!(est_base(&cfg), "https://est.aether-io.com");
        cfg.ws_url = Some("wss://usp.example.net/usp".into());
        assert_eq!(est_base(&cfg), "https://est.example.net");
    }
}

#[cfg(test)]
mod wiring_tests {
    /// Enrolment must be CALLED, not merely defined.
    ///
    /// An unwired module is this codebase's most persistent bug: complete code
    /// that nothing invokes. `Device.X_OptimACS.ClaimToken` was reported by
    /// every agent and read by nothing; seven controller tables were written
    /// and never queried. A certificate module that never runs would leave the
    /// whole fleet on the shared birth credential while looking finished.
    #[test]
    fn enrolment_is_actually_invoked_at_startup() {
        let main_rs = include_str!("main.rs");
        assert!(
            main_rs.contains("est::enrol_if_needed"),
            "est::enrol_if_needed is never called -- the agent would keep using \
             the shared bootstrap certificate and nothing would say so",
        );
    }

    /// It must run BEFORE the agent connects.
    ///
    /// Enrolling after `agent::run` would mean the first connection always used
    /// the birth certificate, and on a device that never restarts, every
    /// connection would.
    #[test]
    fn enrolment_runs_before_the_agent_connects() {
        let main_rs = include_str!("main.rs");
        let enrol = main_rs
            .find("est::enrol_if_needed")
            .expect("call must exist");
        let run = main_rs.find("usp::agent::run").expect("agent must run");
        assert!(
            enrol < run,
            "enrolment must precede the connection, or the first connection \
             always authenticates with the shared birth certificate",
        );
    }
}
