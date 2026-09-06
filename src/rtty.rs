//! Remote terminal (rtty) — isolated device-side WebSocket + PTY.
//!
//! # Isolation from USP/TR-369
//!
//! This runs on its **own** WebSocket connection to the server, completely
//! separate from the USP MTP channel. It never touches the USP data model, the
//! `operate()` dispatch, or the agent message loop. A hung terminal or a flood
//! of shell output can only stall this task — it cannot stall device reporting.
//!
//! # Flow
//!
//! ```text
//!   Browser ──WS──► /api/v1/devices/:serial/rtty ──► server bridge
//!                                                       │  (device-facing WS)
//!                                                       ▼
//!   ac-client rtty WS ◄── wss://gw/rtty/device/:serial ──┘
//!        │
//!        ├── spawn /bin/ash on a PTY (nix::pty::openpty)
//!        ├── WS keystrokes ──► PTY master stdin
//!        └── PTY master stdout ──► WS
//! ```

use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use log::{debug, info, warn};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::connect_async_tls_with_config;

use crate::config::ClientConfig;
use crate::tls::build_tls_config;

/// Derive the rtty device WebSocket URL from the USP ws_url.
///
/// `wss://gw.aether-io.com/usp` → `wss://gw.aether-io.com/rtty/device/<id>`.
///
/// The id must match the `:serial` the browser uses in
/// `/api/v1/devices/:serial/rtty`, which is the device's USP endpoint id
/// (e.g. `oui:00005A:...`), NOT the MAC. Using the MAC here means the bridge
/// registry (keyed by the browser's serial) never matches the device's entry.
fn rtty_url(cfg: &ClientConfig) -> Option<String> {
    let ws = cfg.ws_url.as_deref()?;
    let base = ws
        .trim_end_matches('/')
        .strip_suffix("/usp")
        .unwrap_or(ws.trim_end_matches('/'));
    let id = if !cfg.usp_endpoint_id.is_empty() {
        cfg.usp_endpoint_id.clone()
    } else {
        // Match the agent's own derivation (usp/agent.rs): oui:00005A:<mac>.
        crate::usp::endpoint::EndpointId::from_mac("00005A", &cfg.mac_addr).0
    };
    Some(format!("{base}/rtty/device/{id}"))
}

/// Spawn `/bin/ash` on a PTY and return the master fd.
///
/// Returns the master fd (owned by the caller) and the child pid. `forkpty`
/// sets up the slave as the child's controlling terminal.
fn spawn_shell_pty() -> std::io::Result<(std::os::fd::OwnedFd, i32)> {
    use nix::pty::{forkpty, ForkptyResult};

    // SAFETY: the child only calls async-signal-safe exec; the parent is
    // unrestricted. This is the documented contract for forkpty.
    let res = unsafe { forkpty(None, None) }?;

    match res {
        ForkptyResult::Parent { master, child } => Ok((master, child.as_raw())),
        ForkptyResult::Child => {
            // Child: exec ash. forkpty already made the slave our controlling
            // terminal. If exec fails, exit.
            let err = std::process::Command::new("/bin/ash").arg("-i").status();
            let _ = err;
            std::process::exit(1);
        }
    }
}

/// Run the rtty session: connect to the server, spawn a shell, bridge the two.
///
/// This is spawned as an independent task. It reconnects on failure so a
/// transient network blip doesn't permanently kill the terminal capability.
pub async fn run(cfg: Arc<ClientConfig>) {
    loop {
        match run_once(&cfg).await {
            Ok(()) => debug!("rtty: session ended cleanly"),
            Err(e) => warn!("rtty: session error: {e}"),
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

async fn run_once(cfg: &ClientConfig) -> anyhow::Result<()> {
    let Some(url) = rtty_url(cfg) else {
        // No ws_url configured — nothing to do.
        return Ok(());
    };

    // Build mTLS config (same cert the USP channel uses).
    let tls_cfg = build_tls_config(cfg)?;
    let connector = tokio_tungstenite::Connector::Rustls(tls_cfg);

    let req = tokio_tungstenite::tungstenite::handshake::client::Request::builder()
        .method("GET")
        .uri(&url)
        .header("Host", url::Url::parse(&url)?.host_str().unwrap_or("localhost"))
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", tokio_tungstenite::tungstenite::handshake::client::generate_key())
        .body(())?;

    let (ws, _resp) = connect_async_tls_with_config(req, None, false, Some(connector)).await?;
    info!("rtty: connected to {url}");

    // Spawn the shell on a PTY.
    let (master, _child) = spawn_shell_pty()?;
    let master = tokio::fs::File::from_std(std::fs::File::from(master));

    // Bridge: WS <-> PTY master.
    let (mut ws_sink, mut ws_stream) = ws.split();
    let mut pty_reader = master.try_clone().await?;
    let mut pty_writer = master;
    let mut buf = vec![0u8; 4096];

    loop {
        tokio::select! {
            // WS → PTY (keystrokes)
            msg = ws_stream.next() => {
                match msg {
                    Some(Ok(Message::Binary(b))) => {
                        pty_writer.write_all(&b).await?;
                        pty_writer.flush().await?;
                    }
                    Some(Ok(Message::Text(t))) => {
                        pty_writer.write_all(t.as_bytes()).await?;
                        pty_writer.flush().await?;
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
            // PTY → WS (shell output)
            n = pty_reader.read(&mut buf) => {
                let n = n?;
                if n == 0 { break; }
                ws_sink.send(Message::Binary(buf[..n].to_vec())).await?;
            }
        }
    }

    Ok(())
}
