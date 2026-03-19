//! TR-181 Device.IP.Diagnostics.* — IPPing and TraceRoute diagnostics.
//!
//! Supports OPERATE commands for running ping and traceroute, with results
//! stored in process-level state. GET returns the last diagnostic results.

use crate::config::ClientConfig;
use log::info;
use std::collections::HashMap;
use std::sync::Mutex;

pub type Params = HashMap<String, String>;

// ── Diagnostic state ─────────────────────────────────────────────────────────

#[derive(Clone, Default)]
struct PingResult {
    state: String, // "None", "Requested", "Complete", "Error_CannotResolveHostName", etc.
    host: String,
    repetitions: u32,
    timeout: u32,
    success_count: u32,
    failure_count: u32,
    average_response_time: u32, // ms
    minimum_response_time: u32,
    maximum_response_time: u32,
}

#[derive(Clone, Default)]
struct TraceRouteResult {
    state: String,
    host: String,
    max_hops: u32,
    timeout: u32,
    hops: Vec<TraceHop>,
}

#[derive(Clone, Default)]
struct TraceHop {
    host: String,
    ip: String,
    rtt: u32, // ms
}

static PING_STATE: Mutex<Option<PingResult>> = Mutex::new(None);
static TRACEROUTE_STATE: Mutex<Option<TraceRouteResult>> = Mutex::new(None);

// ── GET handler ──────────────────────────────────────────────────────────────

pub async fn get(_cfg: &ClientConfig, path: &str) -> Params {
    let mut m = Params::new();

    if path.starts_with("Device.IP.Diagnostics.IPPing.") || path == "Device.IP.Diagnostics." {
        let ping = PING_STATE.lock().unwrap().clone().unwrap_or_default();
        let base = "Device.IP.Diagnostics.IPPing.";
        m.insert(
            format!("{base}DiagnosticsState"),
            if ping.state.is_empty() {
                "None".to_string()
            } else {
                ping.state.clone()
            },
        );
        m.insert(format!("{base}Host"), ping.host.clone());
        m.insert(
            format!("{base}NumberOfRepetitions"),
            ping.repetitions.to_string(),
        );
        m.insert(format!("{base}Timeout"), ping.timeout.to_string());
        m.insert(
            format!("{base}SuccessCount"),
            ping.success_count.to_string(),
        );
        m.insert(
            format!("{base}FailureCount"),
            ping.failure_count.to_string(),
        );
        m.insert(
            format!("{base}AverageResponseTime"),
            ping.average_response_time.to_string(),
        );
        m.insert(
            format!("{base}MinimumResponseTime"),
            ping.minimum_response_time.to_string(),
        );
        m.insert(
            format!("{base}MaximumResponseTime"),
            ping.maximum_response_time.to_string(),
        );
    }

    if path.starts_with("Device.IP.Diagnostics.TraceRoute.")
        || path == "Device.IP.Diagnostics."
    {
        let tr = TRACEROUTE_STATE
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_default();
        let base = "Device.IP.Diagnostics.TraceRoute.";
        m.insert(
            format!("{base}DiagnosticsState"),
            if tr.state.is_empty() {
                "None".to_string()
            } else {
                tr.state.clone()
            },
        );
        m.insert(format!("{base}Host"), tr.host.clone());
        m.insert(format!("{base}MaxHopCount"), tr.max_hops.to_string());
        m.insert(format!("{base}Timeout"), tr.timeout.to_string());
        m.insert(
            format!("{base}RouteHopsNumberOfEntries"),
            tr.hops.len().to_string(),
        );

        for (i, hop) in tr.hops.iter().enumerate() {
            let idx = i + 1;
            let hb = format!("{base}RouteHops.{idx}.");
            m.insert(format!("{hb}Host"), hop.host.clone());
            m.insert(format!("{hb}HostAddress"), hop.ip.clone());
            m.insert(format!("{hb}RTTimes"), hop.rtt.to_string());
        }
    }

    m
}

// ── OPERATE handlers ─────────────────────────────────────────────────────────

pub async fn operate_ping(
    _cfg: &ClientConfig,
    input_args: &HashMap<String, String>,
) -> Result<HashMap<String, String>, String> {
    let host = input_args
        .get("Host")
        .cloned()
        .unwrap_or_default();
    if host.is_empty() {
        return Err("Host parameter is required".to_string());
    }

    // Validate host - only allow hostname/IP characters
    if !host
        .chars()
        .all(|c| c.is_alphanumeric() || c == '.' || c == '-' || c == ':')
    {
        return Err("Invalid host parameter".to_string());
    }

    let count: u32 = input_args
        .get("NumberOfRepetitions")
        .and_then(|s| s.parse().ok())
        .unwrap_or(4);
    let timeout: u32 = input_args
        .get("Timeout")
        .and_then(|s| s.parse().ok())
        .unwrap_or(5000);

    let count = count.min(100); // safety limit
    let timeout_secs = (timeout / 1000).max(1).min(30);

    info!("Running IPPing: host={host}, count={count}, timeout={timeout_secs}s");

    // Set state to Requested
    {
        let mut state = PING_STATE.lock().unwrap();
        *state = Some(PingResult {
            state: "Requested".to_string(),
            host: host.clone(),
            repetitions: count,
            timeout,
            ..Default::default()
        });
    }

    // Run ping
    let output = tokio::process::Command::new("ping")
        .args([
            "-c",
            &count.to_string(),
            "-W",
            &timeout_secs.to_string(),
            &host,
        ])
        .output()
        .await
        .map_err(|e| format!("Failed to run ping: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();

    // Parse ping output
    let mut result = PingResult {
        state: "Complete".to_string(),
        host: host.clone(),
        repetitions: count,
        timeout,
        ..Default::default()
    };

    // Parse "X packets transmitted, Y received" line
    for line in stdout.lines() {
        if line.contains("packets transmitted") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if let Some(pos) = parts.iter().position(|&p| p == "packets") {
                if pos > 0 {
                    result.success_count = parts
                        .get(pos - 1)
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                }
            }
            if let Some(pos) = parts.iter().position(|&p| p == "received," || p == "received") {
                if pos > 0 {
                    let received: u32 = parts
                        .get(pos - 1)
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                    result.success_count = received;
                    result.failure_count = count.saturating_sub(received);
                }
            }
        }
        // Parse "rtt min/avg/max/mdev = 1.234/5.678/9.012/0.345 ms"
        if line.contains("min/avg/max") {
            if let Some(eq_pos) = line.find('=') {
                let vals = line[eq_pos + 1..].trim();
                let parts: Vec<&str> = vals.split('/').collect();
                if parts.len() >= 3 {
                    result.minimum_response_time =
                        parts[0].trim().parse::<f64>().unwrap_or(0.0) as u32;
                    result.average_response_time =
                        parts[1].trim().parse::<f64>().unwrap_or(0.0) as u32;
                    // Remove " ms" from the max value
                    let max_str = parts[2].trim().split_whitespace().next().unwrap_or("0");
                    result.maximum_response_time =
                        max_str.parse::<f64>().unwrap_or(0.0) as u32;
                }
            }
        }
    }

    if !output.status.success() && result.success_count == 0 {
        result.state = "Error_CannotResolveHostName".to_string();
    }

    // Store results
    {
        let mut state = PING_STATE.lock().unwrap();
        *state = Some(result.clone());
    }

    let mut out = HashMap::new();
    out.insert("Status".to_string(), result.state);
    out.insert("SuccessCount".to_string(), result.success_count.to_string());
    out.insert("FailureCount".to_string(), result.failure_count.to_string());
    out.insert(
        "AverageResponseTime".to_string(),
        result.average_response_time.to_string(),
    );
    out.insert(
        "MinimumResponseTime".to_string(),
        result.minimum_response_time.to_string(),
    );
    out.insert(
        "MaximumResponseTime".to_string(),
        result.maximum_response_time.to_string(),
    );
    Ok(out)
}

pub async fn operate_traceroute(
    _cfg: &ClientConfig,
    input_args: &HashMap<String, String>,
) -> Result<HashMap<String, String>, String> {
    let host = input_args
        .get("Host")
        .cloned()
        .unwrap_or_default();
    if host.is_empty() {
        return Err("Host parameter is required".to_string());
    }

    if !host
        .chars()
        .all(|c| c.is_alphanumeric() || c == '.' || c == '-' || c == ':')
    {
        return Err("Invalid host parameter".to_string());
    }

    let max_hops: u32 = input_args
        .get("MaxHopCount")
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);
    let timeout: u32 = input_args
        .get("Timeout")
        .and_then(|s| s.parse().ok())
        .unwrap_or(5000);

    let max_hops = max_hops.min(64);
    let timeout_secs = (timeout / 1000).max(1).min(10);

    info!("Running TraceRoute: host={host}, max_hops={max_hops}, timeout={timeout_secs}s");

    {
        let mut state = TRACEROUTE_STATE.lock().unwrap();
        *state = Some(TraceRouteResult {
            state: "Requested".to_string(),
            host: host.clone(),
            max_hops,
            timeout,
            hops: vec![],
        });
    }

    let output = tokio::process::Command::new("traceroute")
        .args([
            "-m",
            &max_hops.to_string(),
            "-w",
            &timeout_secs.to_string(),
            &host,
        ])
        .output()
        .await
        .map_err(|e| format!("Failed to run traceroute: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();

    let mut hops = Vec::new();
    // Parse traceroute output: " 1  gateway (192.168.1.1)  1.234 ms  ..."
    for line in stdout.lines().skip(1) {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 3 {
            continue;
        }
        // Skip hop number (parts[0])
        let host_name = parts.get(1).unwrap_or(&"*").to_string();
        if host_name == "*" {
            hops.push(TraceHop {
                host: "*".to_string(),
                ip: String::new(),
                rtt: 0,
            });
            continue;
        }

        let ip = parts
            .get(2)
            .map(|s| s.trim_matches(|c| c == '(' || c == ')').to_string())
            .unwrap_or_default();

        let rtt = parts
            .iter()
            .find(|s| s.contains('.') && !s.contains('('))
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0) as u32;

        hops.push(TraceHop {
            host: host_name,
            ip,
            rtt,
        });
    }

    let result = TraceRouteResult {
        state: if output.status.success() {
            "Complete"
        } else {
            "Error_MaxHopCountExceeded"
        }
        .to_string(),
        host: host.clone(),
        max_hops,
        timeout,
        hops: hops.clone(),
    };

    {
        let mut state = TRACEROUTE_STATE.lock().unwrap();
        *state = Some(result);
    }

    let mut out = HashMap::new();
    out.insert(
        "Status".to_string(),
        if output.status.success() {
            "Complete"
        } else {
            "Error_MaxHopCountExceeded"
        }
        .to_string(),
    );
    out.insert("NumberOfHops".to_string(), hops.len().to_string());
    Ok(out)
}

pub async fn set(cfg: &ClientConfig, path: &str, value: &str) -> Result<(), String> {
    // Setting DiagnosticsState to "Requested" triggers the diagnostic
    // using params previously SET (Host, Timeout, etc.)
    if path.ends_with("DiagnosticsState") && value == "Requested" {
        if path.contains("IPPing") {
            let args = {
                let state = PING_STATE.lock().unwrap();
                let ping = state.as_ref().cloned().unwrap_or_default();
                let mut a = HashMap::new();
                a.insert("Host".to_string(), ping.host);
                a.insert(
                    "NumberOfRepetitions".to_string(),
                    ping.repetitions.to_string(),
                );
                a.insert("Timeout".to_string(), ping.timeout.to_string());
                a
            };
            operate_ping(cfg, &args).await?;
        } else if path.contains("TraceRoute") {
            let args = {
                let state = TRACEROUTE_STATE.lock().unwrap();
                let tr = state.as_ref().cloned().unwrap_or_default();
                let mut a = HashMap::new();
                a.insert("Host".to_string(), tr.host);
                a.insert("MaxHopCount".to_string(), tr.max_hops.to_string());
                a.insert("Timeout".to_string(), tr.timeout.to_string());
                a
            };
            operate_traceroute(cfg, &args).await?;
        }
        return Ok(());
    }
    // Allow setting Host, NumberOfRepetitions, Timeout, MaxHopCount
    if path.contains("IPPing.") {
        let mut state = PING_STATE.lock().unwrap();
        let ping = state.get_or_insert_with(PingResult::default);
        if path.ends_with("Host") {
            ping.host = value.to_string();
        } else if path.ends_with("NumberOfRepetitions") {
            ping.repetitions = value.parse().unwrap_or(4);
        } else if path.ends_with("Timeout") {
            ping.timeout = value.parse().unwrap_or(5000);
        } else {
            return Err(format!("Read-only diagnostic param: {path}"));
        }
        return Ok(());
    }
    if path.contains("TraceRoute.") {
        let mut state = TRACEROUTE_STATE.lock().unwrap();
        let tr = state.get_or_insert_with(TraceRouteResult::default);
        if path.ends_with("Host") {
            tr.host = value.to_string();
        } else if path.ends_with("MaxHopCount") {
            tr.max_hops = value.parse().unwrap_or(30);
        } else if path.ends_with("Timeout") {
            tr.timeout = value.parse().unwrap_or(5000);
        } else {
            return Err(format!("Read-only diagnostic param: {path}"));
        }
        return Ok(());
    }
    Err(format!("Unknown diagnostics path: {path}"))
}
