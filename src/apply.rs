//! Apply server-provided configuration and firmware to the device.
//!
//! Configuration is applied via OpenWrt's UCI batch command mechanism.
//! Firmware is applied via `/sbin/sysupgrade` (preserves config).

use std::fs;
use std::path::PathBuf;

use log::{info, warn};
use tokio::process::Command;

use crate::config::ClientConfig;
use crate::error::{AcError, Result};
use crate::proto::{
    CertsResponse, InterfaceConfig, SystemConfig, WirelessConfig,
};
use crate::util;

// ── Certificate persistence ───────────────────────────────────────────────────

/// Save the certificates received in a `CertsResponse` to `cert_dir`.
///
/// Writes three files:
///   - `client.crt` — device certificate
///   - `client.key` — device private key
///   - `ca.crt`     — CA certificate (for server verification)
pub async fn save_certs(cfg: &ClientConfig, certs: &CertsResponse) -> Result<()> {
    tokio::fs::create_dir_all(&cfg.cert_dir).await?;

    let cert_path = cfg.cert_dir.join("client.crt");
    let key_path  = cfg.cert_dir.join("client.key");
    let ca_path   = cfg.cert_dir.join("ca.crt");

    tokio::fs::write(&cert_path, certs.cert.as_bytes()).await?;
    tokio::fs::write(&key_path,  certs.key.as_bytes()).await?;
    tokio::fs::write(&ca_path,   certs.ca_cert.as_bytes()).await?;

    info!(
        "certificates saved: cert={} key={} ca={}",
        cert_path.display(),
        key_path.display(),
        ca_path.display()
    );
    Ok(())
}

/// Returns `true` if provisioned client cert and key both exist on disk.
pub fn device_certs_exist(cfg: &ClientConfig) -> bool {
    cfg.cert_file.exists() && cfg.key_file.exists()
}

// ── System configuration (UCI) ────────────────────────────────────────────────

/// Apply a `SystemConfig` to the device using UCI batch commands.
///
/// Writes the command file to `/tmp/apclient_uci_batch` then runs
/// `uci batch` followed by `reload_config`.
pub async fn apply_config(cfg: &ClientConfig, sc: &SystemConfig) -> Result<()> {
    info!("applying system configuration");

    let batch = build_uci_batch(cfg, sc);
    let batch_path = PathBuf::from("/tmp/apclient_uci_batch");

    tokio::fs::write(&batch_path, batch.as_bytes()).await?;

    // Apply with uci batch
    let status = Command::new("uci")
        .arg("batch")
        .stdin(std::process::Stdio::from(
            fs::File::open(&batch_path)?,
        ))
        .status()
        .await?;

    if !status.success() {
        warn!("uci batch returned non-zero status");
    }

    // Commit and reload
    let _ = Command::new("uci").arg("commit").status().await;
    let _ = Command::new("reload_config").status().await;

    // Apply password if provided
    if !sc.password.is_empty() {
        apply_password(&sc.password).await;
    }

    info!("system configuration applied");
    Ok(())
}

/// Build a UCI batch command string from a `SystemConfig`.
fn build_uci_batch(_cfg: &ClientConfig, sc: &SystemConfig) -> String {
    let mut cmds = Vec::<String>::new();

    // ── Hostname ──────────────────────────────────────────────────────────────
    if !sc.hostname.is_empty() {
        cmds.push(format!("set system.@system[0].hostname='{}'", sc.hostname));
    }

    // ── Network interfaces ────────────────────────────────────────────────────
    for iface in &sc.interfaces {
        apply_interface_cmds(iface, &mut cmds);
    }

    // ── Hosts file entries ────────────────────────────────────────────────────
    if !sc.hosts.is_empty() {
        cmds.push("delete dhcp.@dnsmasq[0].address".into());
        for host in &sc.hosts {
            cmds.push(format!(
                "add_list dhcp.@dnsmasq[0].address='/{}/{}'",
                host.hostname, host.ip
            ));
        }
    }

    // ── Static DHCP leases ────────────────────────────────────────────────────
    for dhcp_host in &sc.dhcp_hosts {
        let id = util::mac_no_colons(&dhcp_host.mac);
        cmds.push(format!("set dhcp.host_{id}=host"));
        cmds.push(format!("set dhcp.host_{id}.mac='{}'", dhcp_host.mac));
        cmds.push(format!("set dhcp.host_{id}.ip='{}'", dhcp_host.ip));
    }

    cmds.join("\n")
}

fn apply_interface_cmds(iface: &InterfaceConfig, cmds: &mut Vec<String>) {
    let name = &iface.name;
    // Map interface name to UCI network name
    let uci_name = iface.network_name.as_deref().unwrap_or(name);

    cmds.push(format!("set network.{uci_name}=interface"));
    cmds.push(format!(
        "set network.{uci_name}.proto='{}'",
        iface.con_type.to_lowercase()
    ));

    if !iface.ip.is_empty() {
        cmds.push(format!("set network.{uci_name}.ipaddr='{}'", iface.ip));
    }
    if !iface.netmask.is_empty() {
        cmds.push(format!("set network.{uci_name}.netmask='{}'", iface.netmask));
    }
    if !iface.gateway.is_empty() {
        cmds.push(format!("set network.{uci_name}.gateway='{}'", iface.gateway));
    }
    if !iface.dns.is_empty() {
        cmds.push(format!("set network.{uci_name}.dns='{}'", iface.dns));
    }

    // Wireless sub-config
    if let Some(wireless) = &iface.wireless {
        apply_wireless_cmds(uci_name, wireless, cmds);
    }
}

fn apply_wireless_cmds(iface: &str, w: &WirelessConfig, cmds: &mut Vec<String>) {
    let dev = &w.dev_name;

    // UCI wireless device
    cmds.push(format!("set wireless.{dev}=wifi-device"));
    if !w.mode.is_empty() {
        cmds.push(format!("set wireless.{dev}.mode='{}'", w.mode));
    }

    // UCI wireless interface (iface is network name)
    let wif = format!("wif_{iface}");
    cmds.push(format!("set wireless.{wif}=wifi-iface"));
    cmds.push(format!("set wireless.{wif}.device='{dev}'"));
    cmds.push(format!("set wireless.{wif}.network='{iface}'"));
    if !w.essid.is_empty() {
        cmds.push(format!("set wireless.{wif}.ssid='{}'", w.essid));
    }
    if !w.enc_type.is_empty() {
        cmds.push(format!("set wireless.{wif}.encryption='{}'", w.enc_type));
    }
    if let Some(key) = &w.enc_key {
        if !key.is_empty() {
            cmds.push(format!("set wireless.{wif}.key='{key}'"));
        }
    }
}

/// Write a new root password hash to `/etc/shadow`.
async fn apply_password(password: &str) {
    // Use `passwd` utility if available, or write to shadow directly
    let status = Command::new("sh")
        .arg("-c")
        .arg(format!("echo 'root:{password}' | chpasswd -e"))
        .status()
        .await;
    match status {
        Ok(s) if s.success() => info!("root password updated"),
        Ok(s) => warn!("chpasswd returned {s}"),
        Err(e) => warn!("chpasswd failed: {e}"),
    }
}

// ── Firmware upgrade ──────────────────────────────────────────────────────────

/// Apply a firmware image stored at `fw_path` using `sysupgrade`.
///
/// This function does not return under normal circumstances — sysupgrade
/// reboots the device.  It only returns if sysupgrade fails.
pub async fn apply_firmware(fw_path: &PathBuf) -> Result<()> {
    info!("running sysupgrade on {}", fw_path.display());

    // -n: don't preserve config (server will re-provision), -q: quiet
    let status = Command::new("/sbin/sysupgrade")
        .args(["-q", fw_path.to_str().unwrap_or("")])
        .status()
        .await?;

    if !status.success() {
        return Err(AcError::Protocol(format!(
            "sysupgrade failed with status {status}"
        )));
    }
    Ok(())
}
