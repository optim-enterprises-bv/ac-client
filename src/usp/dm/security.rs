//! TR-181 Device.X_OptimACS_Security.IssueCert() — certificate issuance flow.

use crate::config::ClientConfig;
use std::collections::HashMap;

pub async fn set(_cfg: &ClientConfig, path: &str, value: &str) -> Result<(), String> {
    match path {
        "Device.X_OptimACS_Security.DevicePassword" => {
            // Set device root password via OpenWrt UCI/chpasswd
            apply_device_password(value).await
        }
        _ => {
            // cert SET is handled via apply::save_certs called from the agent
            Ok(())
        }
    }
}

/// Apply device root password using OpenWrt chpasswd
async fn apply_device_password(password: &str) -> Result<(), String> {
    use tokio::process::Command;

    // Hash the password using openssl (OpenWrt compatible)
    let output = Command::new("openssl")
        .args(["passwd", "-1", password])
        .output()
        .await
        .map_err(|e| format!("Failed to run openssl: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "openssl passwd failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let hash = String::from_utf8(output.stdout)
        .map_err(|e| format!("Invalid utf8 from openssl: {}", e))?
        .trim()
        .to_string();

    // Update /etc/shadow using chpasswd
    let output = Command::new("sh")
        .arg("-c")
        .arg(format!("echo 'root:{}' | chpasswd -e", hash))
        .output()
        .await
        .map_err(|e| format!("Failed to run chpasswd: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "chpasswd failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    log::info!("Device root password updated successfully");
    Ok(())
}

pub async fn operate_issue_cert(
    cfg: &ClientConfig,
    _command: &str,
    input_args: &HashMap<String, String>,
) -> Result<HashMap<String, String>, String> {
    // Check if controller sent certificates (ca_cert, cert, key)
    if let (Some(ca_cert), Some(cert), Some(key)) = (
        input_args.get("ca_cert"),
        input_args.get("cert"),
        input_args.get("key"),
    ) {
        // Save the provisioned certificates
        tokio::fs::write(&cfg.ca_file, ca_cert)
            .await
            .map_err(|e| format!("Failed to write CA cert: {}", e))?;
        tokio::fs::write(&cfg.cert_file, cert)
            .await
            .map_err(|e| format!("Failed to write client cert: {}", e))?;
        tokio::fs::write(&cfg.key_file, key)
            .await
            .map_err(|e| format!("Failed to write client key: {}", e))?;

        log::info!("Installed provisioned certificates from controller");
        log::info!("Restarting agent to use new certificates...");

        // Return success response before restarting
        let mut out = HashMap::new();
        out.insert("status".into(), "success".into());
        out.insert("message".into(), "Certificates installed".into());

        // Exit the process to trigger restart by init system
        // Give a moment for the response to be sent
        tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            std::process::exit(0);
        });

        return Ok(out);
    }

    // No certificates provided - return CSR request (legacy behavior)
    let cert_pem = tokio::fs::read_to_string(&cfg.init_cert)
        .await
        .map_err(|e| e.to_string())?;
    let mut out = HashMap::new();
    out.insert("csr".into(), cert_pem);
    Ok(out)
}
