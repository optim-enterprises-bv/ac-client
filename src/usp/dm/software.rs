//! TR-181 Device.SoftwareModules.* — Software module management.
//!
//! Reports the Linux kernel as the primary execution environment.
//! Enumerates installed opkg packages as DeploymentUnits.
//! Supports Install(), Update(), Uninstall() OPERATE commands via opkg.

use crate::config::ClientConfig;
use crate::util;
use log::info;
use std::collections::HashMap;

pub type Params = HashMap<String, String>;

pub async fn get(_cfg: &ClientConfig, path: &str) -> Params {
    let mut m = Params::new();

    // ExecEnv
    if path == "Device.SoftwareModules."
        || path.contains("ExecEnvNumberOfEntries")
    {
        m.insert(
            "Device.SoftwareModules.ExecEnvNumberOfEntries".to_string(),
            "1".to_string(),
        );
    }

    if path == "Device.SoftwareModules."
        || path.starts_with("Device.SoftwareModules.ExecEnv.")
    {
        let base = "Device.SoftwareModules.ExecEnv.1.";
        m.insert(format!("{base}Enable"), "true".to_string());
        m.insert(format!("{base}Status"), "Up".to_string());
        m.insert(format!("{base}Name"), "Linux".to_string());
        m.insert(format!("{base}Type"), "Linux".to_string());
        m.insert(format!("{base}Vendor"), "kernel.org".to_string());
        m.insert(format!("{base}Version"), util::read_kernel_version());
        m.insert(format!("{base}AvailableMemory"), util::read_free_mem());
        m.insert(
            format!("{base}ProcessorArchitecture"),
            util::read_device_arch(),
        );
    }

    // DeploymentUnit (installed opkg packages)
    if path == "Device.SoftwareModules."
        || path.contains("DeploymentUnitNumberOfEntries")
        || path.starts_with("Device.SoftwareModules.DeploymentUnit.")
    {
        let packages = get_installed_packages();
        m.insert(
            "Device.SoftwareModules.DeploymentUnitNumberOfEntries".to_string(),
            packages.len().to_string(),
        );

        let specific_idx = extract_index(path, "DeploymentUnit.");
        for (i, pkg) in packages.iter().enumerate() {
            let idx = i + 1;
            if let Some(si) = specific_idx {
                if si != idx {
                    continue;
                }
            }
            let base = format!("Device.SoftwareModules.DeploymentUnit.{idx}.");
            m.insert(format!("{base}Name"), pkg.name.clone());
            m.insert(format!("{base}Version"), pkg.version.clone());
            m.insert(format!("{base}Status"), "Installed".to_string());
            m.insert(format!("{base}Resolved"), "true".to_string());
            m.insert(
                format!("{base}ExecutionEnvRef"),
                "Device.SoftwareModules.ExecEnv.1.".to_string(),
            );
        }
    }

    m
}

pub async fn set(_cfg: &ClientConfig, path: &str, _value: &str) -> Result<(), String> {
    Err(format!("Device.SoftwareModules path is read-only: {path}"))
}

/// OPERATE: Install a package via opkg
pub async fn operate_install(
    _cfg: &ClientConfig,
    input_args: &HashMap<String, String>,
) -> Result<HashMap<String, String>, String> {
    let url = input_args.get("URL").cloned().unwrap_or_default();
    if url.is_empty() {
        return Err("URL parameter is required for Install()".to_string());
    }

    info!("SoftwareModules: Installing package from {url}");

    // opkg update first
    let _ = std::process::Command::new("opkg")
        .arg("update")
        .output();

    let output = tokio::process::Command::new("opkg")
        .args(["install", &url])
        .output()
        .await
        .map_err(|e| format!("Failed to run opkg install: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    let mut result = HashMap::new();
    if output.status.success() {
        result.insert("Status".to_string(), "Installed".to_string());
        result.insert("Output".to_string(), stdout);
    } else {
        result.insert("Status".to_string(), "Failed".to_string());
        result.insert("Error".to_string(), stderr);
    }
    Ok(result)
}

/// OPERATE: Uninstall a package via opkg
pub async fn operate_uninstall(
    _cfg: &ClientConfig,
    input_args: &HashMap<String, String>,
) -> Result<HashMap<String, String>, String> {
    let name = input_args.get("Name").cloned().unwrap_or_default();
    if name.is_empty() {
        return Err("Name parameter is required for Uninstall()".to_string());
    }

    // Validate package name
    if !name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.') {
        return Err("Invalid package name".to_string());
    }

    info!("SoftwareModules: Uninstalling package {name}");

    let output = tokio::process::Command::new("opkg")
        .args(["remove", &name])
        .output()
        .await
        .map_err(|e| format!("Failed to run opkg remove: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    let mut result = HashMap::new();
    if output.status.success() {
        result.insert("Status".to_string(), "Uninstalled".to_string());
        result.insert("Output".to_string(), stdout);
    } else {
        result.insert("Status".to_string(), "Failed".to_string());
        result.insert("Error".to_string(), stderr);
    }
    Ok(result)
}

struct Package {
    name: String,
    version: String,
}

fn get_installed_packages() -> Vec<Package> {
    let output = std::process::Command::new("opkg")
        .args(["list-installed"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    output
        .lines()
        .filter_map(|line| {
            // Format: "package - version"
            let parts: Vec<&str> = line.splitn(3, " - ").collect();
            if parts.len() >= 2 {
                Some(Package {
                    name: parts[0].trim().to_string(),
                    version: parts[1].trim().to_string(),
                })
            } else {
                None
            }
        })
        .collect()
}

fn extract_index(path: &str, key: &str) -> Option<usize> {
    if let Some(pos) = path.find(key) {
        let rest = &path[pos + key.len()..];
        let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        num_str.parse().ok()
    } else {
        None
    }
}
