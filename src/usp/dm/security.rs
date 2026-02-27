//! TR-181 Device.X_OptimACS_Security.IssueCert() — certificate issuance flow.

use std::collections::HashMap;
use crate::config::ClientConfig;

pub async fn set(_cfg: &ClientConfig, _path: &str, _value: &str) -> Result<(), String> {
    // cert SET is handled via apply::save_certs called from the agent
    Ok(())
}

pub async fn operate_issue_cert(
    cfg:        &ClientConfig,
    _command:    &str,
    _input_args: &HashMap<String, String>,
) -> Result<HashMap<String, String>, String> {
    // Generate a CSR and return it to the controller
    // For now, read the existing init cert and return its CN as proof
    let cert_pem = tokio::fs::read_to_string(&cfg.init_cert)
        .await
        .map_err(|e| e.to_string())?;
    let mut out = HashMap::new();
    out.insert("csr".into(), cert_pem);
    Ok(out)
}
