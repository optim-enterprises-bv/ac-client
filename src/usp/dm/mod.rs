//! TR-181 data model — Agent side.
//!
//! The agent handles incoming GET and SET requests from the Controller
//! by dispatching to the appropriate sub-module based on the TR-181 path prefix.

#![allow(dead_code)]

pub mod bridge;
pub mod bridging;
pub mod fingerprint;
pub mod bulk_data;
pub mod device_info;
pub mod dhcp;
pub mod dhcpv6;
pub mod diagnostics;
pub mod dsl;
pub mod dslite;
pub mod ethernet;
pub mod firewall_dm;
pub mod firmware;
pub mod hosts;
pub mod ieee8021x;
pub mod interface_stack;
pub mod ip;
pub mod ipv6rd;
pub mod local_agent;
pub mod management_server;
pub mod misc;
pub mod nat;
pub mod optical;
pub mod ppp;
pub mod qos;
pub mod router_adv;
pub mod security;
pub mod software;
pub mod upnp;
pub mod users;
pub mod voip;
pub mod wifi;

use crate::config::ClientConfig;
use log::{debug, info};
use std::collections::HashMap;
use std::sync::Mutex;

pub type Params = HashMap<String, String>;

/// Cache for tracking previous parameter values (delta tracking)
/// Key: parameter path, Value: previous value
static PARAM_CACHE: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

/// Initialize the parameter cache on first use
fn get_cache() -> Option<HashMap<String, String>> {
    PARAM_CACHE.lock().unwrap().clone()
}

fn update_cache(new_values: &HashMap<String, String>) {
    let mut cache = PARAM_CACHE.lock().unwrap();
    *cache = Some(new_values.clone());
}

/// Filter parameters to only return changed values (delta)
fn filter_delta(params: Params, force_full: bool) -> Params {
    if force_full {
        // On first call or explicit request, return all values
        update_cache(&params);
        return params;
    }

    let cache = match get_cache() {
        Some(c) => c,
        None => {
            // First call, cache and return all
            update_cache(&params);
            return params;
        }
    };

    // Only return changed values
    let mut delta = Params::new();
    let mut changed_count = 0;

    for (path, value) in &params {
        match cache.get(path) {
            Some(prev_value) if prev_value == value => {
                // Unchanged, skip
                continue;
            }
            _ => {
                // New or changed
                delta.insert(path.clone(), value.clone());
                changed_count += 1;
            }
        }
    }

    // Update cache with new values
    update_cache(&params);

    if changed_count > 0 {
        info!(
            "Delta update: {} of {} parameters changed",
            changed_count,
            params.len()
        );
    } else {
        debug!("No parameter changes detected");
    }

    delta
}

/// Counter for forcing periodic full updates (every N requests)
static POLL_COUNTER: Mutex<u32> = Mutex::new(0);
const FULL_UPDATE_INTERVAL: u32 = 10; // Force full update every 10 requests

/// Handle a GET request for the given paths.
///
/// `max_depth` limits how many levels below the requested path are returned.
/// 0 means unlimited (TR-369 §6.1.2).
///
/// Now implements delta tracking - only returns changed parameters
/// unless force_full is true or periodic full update interval reached.
pub async fn get_params(cfg: &ClientConfig, paths: &[String], max_depth: u32) -> Params {
    let mut result = Params::new();
    for path in paths {
        let partial = dispatch_get(cfg, path).await;
        if max_depth == 0 {
            result.extend(partial);
        } else {
            let base_depth = path.chars().filter(|&c| c == '.').count();
            result.extend(partial.into_iter().filter(|(k, _)| {
                k.chars().filter(|&c| c == '.').count() <= base_depth + max_depth as usize
            }));
        }
    }

    // Increment counter and check if we need a full update
    let counter = {
        let mut c = POLL_COUNTER.lock().unwrap();
        *c += 1;
        *c
    };

    let force_full = counter % FULL_UPDATE_INTERVAL == 1; // First call and every Nth call

    // Apply delta filtering
    filter_delta(result, force_full)
}

/// Handle a SET request for the given (path, value) pairs.
pub async fn set_params(cfg: &ClientConfig, updates: &[(String, String)]) -> Result<(), String> {
    for (path, value) in updates {
        dispatch_set(cfg, path, value).await?;
    }
    Ok(())
}

/// Handle an OPERATE command; returns output_args on success.
pub async fn operate(
    cfg: &ClientConfig,
    command: &str,
    input_args: &HashMap<String, String>,
) -> Result<HashMap<String, String>, String> {
    if command.starts_with("Device.X_OptimACS_Firmware.") && command.ends_with(".Download()") {
        firmware::operate_download(cfg, command, input_args).await
    } else if command.starts_with("Device.X_OptimACS_Security.")
        && command.ends_with(".IssueCert()")
    {
        security::operate_issue_cert(cfg, command, input_args).await
    } else if command.starts_with("Device.X_OptimACS_Network.Bridge.")
        && command.ends_with(".Restart()")
    {
        bridge::operate(cfg, command, input_args).await
    } else if command == "Device.IP.Diagnostics.IPPing()" {
        diagnostics::operate_ping(cfg, input_args).await
    } else if command == "Device.IP.Diagnostics.TraceRoute()" {
        diagnostics::operate_traceroute(cfg, input_args).await
    } else if command == "Device.SoftwareModules.InstallDU()" {
        software::operate_install(cfg, input_args).await
    } else if command == "Device.SoftwareModules.UninstallDU()" {
        software::operate_uninstall(cfg, input_args).await
    } else {
        Err(format!("unknown command: {command}"))
    }
}

async fn dispatch_get(cfg: &ClientConfig, path: &str) -> Params {
    if path.starts_with("Device.LocalAgent.") {
        local_agent::get(cfg, path)
    } else if path.starts_with("Device.ManagementServer.") {
        management_server::get(cfg, path)
    } else if path.starts_with("Device.DeviceInfo.") {
        device_info::get(cfg, path)
    } else if path.starts_with("Device.InterfaceStack") {
        interface_stack::get(cfg, path).await
    } else if path.starts_with("Device.WiFi.") {
        wifi::get(cfg, path).await
    } else if path.starts_with("Device.Ethernet.") {
        ethernet::get(cfg, path).await
    } else if path.starts_with("Device.Bridging.") {
        bridging::get(cfg, path).await
    } else if path.starts_with("Device.IP.Diagnostics.") {
        diagnostics::get(cfg, path).await
    } else if path.starts_with("Device.IP.Interface.") {
        ip::get(cfg, path).await
    } else if path.starts_with("Device.DHCPv4.") {
        dhcp::get(cfg, path).await
    } else if path.starts_with("Device.DHCPv6.") {
        dhcpv6::get(cfg, path).await
    } else if path.starts_with("Device.Hosts.") {
        hosts::get(cfg, path).await
    } else if path.starts_with("Device.NAT.") {
        nat::get(cfg, path).await
    } else if path.starts_with("Device.Firewall.") {
        firewall_dm::get(cfg, path).await
    } else if path.starts_with("Device.PPP.") {
        ppp::get(cfg, path).await
    } else if path.starts_with("Device.Users.") {
        users::get(cfg, path).await
    } else if path.starts_with("Device.SoftwareModules.") {
        software::get(cfg, path).await
    } else if path.starts_with("Device.RouterAdvertisement.") {
        router_adv::get(cfg, path).await
    } else if path.starts_with("Device.IPv6rd.") {
        ipv6rd::get(cfg, path).await
    } else if path.starts_with("Device.DSLite.") {
        dslite::get(cfg, path).await
    } else if path.starts_with("Device.QoS.") {
        qos::get(cfg, path).await
    } else if path.starts_with("Device.UPnP.") {
        upnp::get(cfg, path).await
    } else if path.starts_with("Device.BulkData.") {
        bulk_data::get(cfg, path).await
    } else if path.starts_with("Device.DSL.") {
        dsl::get(cfg, path).await
    } else if path.starts_with("Device.Optical.") {
        optical::get(cfg, path).await
    } else if path.starts_with("Device.IEEE8021x.") {
        ieee8021x::get(cfg, path).await
    } else if path.starts_with("Device.Services.") {
        voip::get(cfg, path).await
    } else if path.starts_with("Device.X_OptimACS_FP.") {
        fingerprint::get(cfg, path).await
    } else if path.starts_with("Device.X_OptimACS_Network.Bridge.")
        || path.starts_with("Device.X_OptimACS_Network.Bridge")
    {
        bridge::get(cfg, path).await
    } else if path.starts_with("Device.X_OptimACS_Firmware.") {
        firmware::get(cfg, path)
    } else if path.starts_with("Device.IP.")
        || path.starts_with("Device.DNS.")
        || path.starts_with("Device.Routing.")
        || path.starts_with("Device.WireGuard.")
        || path.starts_with("Device.X_TP_OpenVPN.")
        || path.starts_with("Device.Time.")
        || path.starts_with("Device.USB.")
        || path.starts_with("Device.Cellular.")
        || path.starts_with("Device.NeighborDiscovery.")
    {
        misc::get(cfg, path).await
    } else {
        debug!("DM GET: unimplemented path: {path}");
        Params::new()
    }
}

async fn dispatch_set(cfg: &ClientConfig, path: &str, value: &str) -> Result<(), String> {
    if path.starts_with("Device.LocalAgent.") {
        local_agent::set(cfg, path, value)
    } else if path.starts_with("Device.DeviceInfo.") {
        device_info::set(cfg, path, value)
    } else if path.starts_with("Device.WiFi.") {
        wifi::set(cfg, path, value).await
    } else if path.starts_with("Device.Ethernet.") {
        ethernet::set(cfg, path, value).await
    } else if path.starts_with("Device.Bridging.") {
        bridging::set(cfg, path, value).await
    } else if path.starts_with("Device.IP.Diagnostics.") {
        diagnostics::set(cfg, path, value).await
    } else if path.starts_with("Device.IP.Interface.") {
        ip::set(cfg, path, value).await
    } else if path.starts_with("Device.DHCPv4.") {
        dhcp::set(cfg, path, value).await
    } else if path.starts_with("Device.DHCPv6.") {
        dhcpv6::set(cfg, path, value).await
    } else if path.starts_with("Device.Hosts.") {
        hosts::set(cfg, path, value).await
    } else if path.starts_with("Device.NAT.") {
        nat::set(cfg, path, value).await
    } else if path.starts_with("Device.Firewall.") {
        firewall_dm::set(cfg, path, value).await
    } else if path.starts_with("Device.PPP.") {
        ppp::set(cfg, path, value).await
    } else if path.starts_with("Device.Users.") {
        users::set(cfg, path, value).await
    } else if path.starts_with("Device.SoftwareModules.") {
        software::set(cfg, path, value).await
    } else if path.starts_with("Device.X_OptimACS_Network.Bridge.")
        || path.starts_with("Device.X_OptimACS_Network.Bridge")
    {
        bridge::set(cfg, path, value).await
    } else if path.starts_with("Device.BulkData.") {
        bulk_data::set(cfg, path, value).await
    } else if path.starts_with("Device.X_OptimACS_Security.") {
        security::set(cfg, path, value).await
    } else if path.starts_with("Device.DNS.")
        || path.starts_with("Device.Routing.")
        || path.starts_with("Device.Time.")
    {
        misc::set(cfg, path, value).await
    } else {
        Err(format!("read-only or unknown path: {path}"))
    }
}
