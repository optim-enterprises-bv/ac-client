//! USP WebSocket MTP — agent side (WSS client connecting to controller).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use log::{debug, error, info, trace, warn};
use tokio_tungstenite::{
    connect_async_tls_with_config,
    tungstenite::{Message, handshake::client::Request},
    Connector,
};

use crate::config::ClientConfig;
use super::super::{
    endpoint::EndpointId,
    message::{build_get_supported_proto, encode_msg},
    record::{decode_record, encode_record, extract_msg_payload, no_session_record, websocket_connect_record},
};
use tokio::sync::mpsc::Receiver;

const RECONNECT_DELAY: Duration = Duration::from_secs(10);

/// Generate a Sec-WebSocket-Key header value (base64-encoded 16-byte nonce)
fn generate_websocket_key() -> String {
    use rand::Rng;
    let mut nonce = [0u8; 16];
    rand::thread_rng().fill(&mut nonce);
    base64::encode(nonce)
}

/// Run the WebSocket MTP agent loop.  Reconnects automatically.
pub async fn run(cfg: Arc<ClientConfig>, agent_id: EndpointId, status_rx: Arc<Mutex<Receiver<Vec<u8>>>>) {
    debug!("Starting WebSocket MTP run loop for agent: {}", agent_id.as_str());
    let negotiated_ver: Arc<Mutex<String>> = Arc::new(Mutex::new("1.3".into()));
    
    loop {
        let ws_url = match &cfg.ws_url {
            Some(u) => {
                debug!("WebSocket URL configured: {}", u);
                u.clone()
            }
            None => { 
                warn!("WebSocket MTP disabled (no ws_url configured)");
                return; 
            }
        };
        
        info!("USP WS: connecting to {ws_url}");
        debug!("Starting connect_and_serve with agent_id={}", agent_id.as_str());
        
        match connect_and_serve(cfg.clone(), agent_id.clone(), &ws_url, Arc::clone(&negotiated_ver), Arc::clone(&status_rx)).await {
            Ok(()) => { 
                info!("USP WS: disconnected gracefully");
                debug!("WebSocket connection closed normally, reconnecting...");
            }
            Err(e) => { 
                error!("USP WS error: {e}");
                debug!("WebSocket error details: {:?}", e);
            }
        }
        
        warn!("USP WS: reconnecting in {} seconds...", RECONNECT_DELAY.as_secs());
        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

async fn connect_and_serve(
    cfg:            Arc<ClientConfig>,
    agent_id:       EndpointId,
    ws_url:         &str,
    negotiated_ver: Arc<Mutex<String>>,
    status_rx:      Arc<Mutex<Receiver<Vec<u8>>>>,
) -> anyhow::Result<()> {
    debug!("Building TLS configuration for WebSocket connection");
    // Build mTLS config using the agent's cert
    let tls_cfg = crate::tls::build_tls_config(&cfg)?;
    let connector = Connector::Rustls(tls_cfg);
    debug!("TLS connector created with mTLS enabled");

    let parsed_url = url::Url::parse(ws_url)?;
    let host = parsed_url.host_str().unwrap_or("localhost");
    let port = parsed_url.port().unwrap_or(443);
    debug!("Parsed WebSocket URL - host: {}, port: {}", host, port);
    
    // Build WebSocket request with all required headers
    // When using Request::builder, we must add ALL WebSocket headers manually
    let ws_key = generate_websocket_key();
    debug!("Generated WebSocket key: {}", ws_key);
    
    let req = Request::builder()
        .method("GET")
        .uri(ws_url)
        .header("Host", format!("{}:{}", host, port))
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", &ws_key)
        .header("Sec-WebSocket-Protocol", "v1.usp")
        .body(())?;
    
    debug!("WebSocket handshake request built, initiating connection...");

    let (mut ws, response) = connect_async_tls_with_config(req, None, false, Some(connector)).await?;
    debug!("WebSocket connection established, TLS handshake completed");

    // W3 / TR-369 §10.2.1: verify server echoed Sec-WebSocket-Protocol: v1.usp
    let echoed = response
        .headers()
        .get("Sec-WebSocket-Protocol")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(',').any(|p| p.trim() == "v1.usp"))
        .unwrap_or(false);
    if !echoed {
        warn!("USP WS: server did not echo Sec-WebSocket-Protocol: v1.usp");
    } else {
        debug!("Server correctly echoed Sec-WebSocket-Protocol: v1.usp");
    }

    info!("USP WS: connected to {ws_url}");
    trace!("WebSocket connection response headers: {:?}", response.headers());

    let controller_id = cfg.controller_id.clone();
    debug!("Controller ID: {}", controller_id);

    // Send WebSocketConnectRecord to identify ourselves
    debug!("Sending WebSocketConnectRecord...");
    let connect_rec = websocket_connect_record(agent_id.as_str(), &controller_id);
    let connect_bytes = encode_record(&connect_rec)?;
    debug!("WebSocketConnectRecord encoded ({} bytes)", connect_bytes.len());
    ws.send(Message::Binary(connect_bytes)).await?;
    debug!("WebSocketConnectRecord sent successfully");

    // Send GetSupportedProto to negotiate version
    debug!("Sending GetSupportedProto request...");
    let gsp_msg = build_get_supported_proto();
    let gsp_bytes = encode_msg(&gsp_msg)?;
    let gsp_rec = no_session_record(agent_id.as_str(), &controller_id, gsp_bytes, "1.3");
    ws.send(Message::Binary(encode_record(&gsp_rec)?)).await?;
    info!("USP WS: version negotiation initiated (GetSupportedProto sent)");

    debug!("Entering message receive loop...");
    loop {
        tokio::select! {
            // Handle incoming WebSocket messages
            frame = ws.next() => {
                let frame = match frame {
                    Some(Ok(f)) => f,
                    Some(Err(e)) => {
                        error!("WebSocket error: {e}");
                        break;
                    }
                    None => {
                        debug!("WebSocket stream ended");
                        break;
                    }
                };
                
                trace!("Received WebSocket frame: {:?}", frame);
                
                let data = match frame {
                    Message::Binary(b) => {
                        debug!("Received binary frame ({} bytes)", b.len());
                        trace!("Binary data (first 64 bytes): {:?}", &b[..b.len().min(64)]);
                        b
                    }
                    Message::Close(reason)  => {
                        debug!("Received close frame: {:?}", reason);
                        break;
                    }
                    Message::Ping(p)   => { 
                        debug!("Received ping, sending pong");
                        ws.send(Message::Pong(p)).await?; 
                        continue; 
                    }
                    Message::Pong(_)   => {
                        trace!("Received pong");
                        continue;
                    }
                    Message::Text(t)   => {
                        warn!("Received unexpected text frame: {}", t);
                        continue;
                    }
                    _                  => {
                        trace!("Received other frame type, ignoring");
                        continue;
                    }
                };
                
                let record = match decode_record(&data) {
                    Ok(r)  => {
                        debug!("Successfully decoded USP record");
                        trace!("Record: from_id={}, to_id={}, version={}", r.from_id, r.to_id, r.version);
                        r
                    }
                    Err(e) => { 
                        error!("USP WS: failed to decode record: {e}");
                        trace!("Raw record data (first 128 bytes): {:?}", &data[..data.len().min(128)]);
                        continue; 
                    }
                };
                
                // TR-369 §5.1: discard records not addressed to this endpoint
                if !record.to_id.is_empty() && record.to_id != agent_id.as_str() {
                    warn!("USP WS: to_id={} mismatch (expected {}), discarding",
                          record.to_id, agent_id.as_str());
                    continue;
                }
                
                let msg_bytes = match extract_msg_payload(&record) {
                    Some(b) => {
                        debug!("Extracted {} bytes USP message payload", b.len());
                        b.to_vec()
                    }
                    None    => {
                        warn!("No USP message payload found in record");
                        continue;
                    }
                };
                
                debug!("Calling handle_incoming for message from {}", record.from_id);
                if let Some(resp) = super::super::agent::handle_incoming(
                    cfg.clone(), agent_id.clone(), &msg_bytes, Arc::clone(&negotiated_ver)
                ).await {
                    let ver = negotiated_ver.lock().unwrap().clone();
                    debug!("Sending response (version={})", ver);
                    let resp_rec = no_session_record(agent_id.as_str(), &record.from_id, resp, &ver);
                    let resp_bytes = encode_record(&resp_rec)?;
                    debug!("Response encoded ({} bytes), sending...", resp_bytes.len());
                    ws.send(Message::Binary(resp_bytes)).await?;
                    debug!("Response sent successfully");
                } else {
                    debug!("No response needed for this message");
                }
            }
            
            // Handle outgoing status messages from heartbeat loop
            status_msg = async {
                let mut rx = status_rx.lock().unwrap();
                rx.recv().await
            } => {
                if let Some(record_bytes) = status_msg {
                    info!("WebSocket: Sending status heartbeat ({} bytes)", record_bytes.len());
                    trace!("Status record bytes (first 64): {:?}", &record_bytes[..record_bytes.len().min(64)]);
                    match ws.send(Message::Binary(record_bytes)).await {
                        Ok(()) => info!("WebSocket: Status heartbeat sent successfully"),
                        Err(e) => {
                            warn!("WebSocket: Failed to send status heartbeat: {e}");
                            // Don't break here - let the connection error handling deal with it
                        }
                    }
                } else {
                    debug!("Status channel closed");
                }
            }
        }
    }
    
    info!("USP WS: message loop ended");
    Ok(())
}
