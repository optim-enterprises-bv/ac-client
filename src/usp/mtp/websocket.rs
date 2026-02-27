//! USP WebSocket MTP — agent side (WSS client connecting to controller).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use log::{debug, error, info, warn};
use tokio_tungstenite::{
    connect_async_tls_with_config,
    tungstenite::{Message, handshake::client::Request},
    Connector,
};

use crate::config::ClientConfig;
use super::super::{
    endpoint::EndpointId,
    message::{build_get_supported_proto, decode_msg, encode_msg},
    record::{decode_record, encode_record, extract_msg_payload, no_session_record, websocket_connect_record},
    usp_record::record::RecordType,
    RecordType as RT,
};

const RECONNECT_DELAY: Duration = Duration::from_secs(10);

/// Run the WebSocket MTP agent loop.  Reconnects automatically.
pub async fn run(cfg: Arc<ClientConfig>, agent_id: EndpointId) {
    let negotiated_ver: Arc<Mutex<String>> = Arc::new(Mutex::new("1.3".into()));
    loop {
        let ws_url = match &cfg.ws_url {
            Some(u) => u.clone(),
            None => { info!("WebSocket MTP disabled"); return; }
        };
        info!("USP WS: connecting to {ws_url}");
        match connect_and_serve(cfg.clone(), agent_id.clone(), &ws_url, Arc::clone(&negotiated_ver)).await {
            Ok(()) => { info!("USP WS: disconnected gracefully"); }
            Err(e) => { error!("USP WS error: {e}"); }
        }
        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

async fn connect_and_serve(
    cfg:            Arc<ClientConfig>,
    agent_id:       EndpointId,
    ws_url:         &str,
    negotiated_ver: Arc<Mutex<String>>,
) -> anyhow::Result<()> {
    use tokio_rustls::rustls::ClientConfig as RustlsClientConfig;

    // Build mTLS config using the agent's cert
    let tls_cfg = crate::tls::build_tls_config(&cfg)?;
    let connector = Connector::Rustls(tls_cfg);

    let url = url::Url::parse(ws_url)?;
    let req = Request::builder()
        .uri(ws_url)
        .header("Sec-WebSocket-Protocol", "v1.usp")
        .body(())?;

    let (mut ws, response) = connect_async_tls_with_config(req, None, false, Some(connector)).await?;

    // W3 / TR-369 §10.2.1: verify server echoed Sec-WebSocket-Protocol: v1.usp
    let echoed = response
        .headers()
        .get("Sec-WebSocket-Protocol")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(',').any(|p| p.trim() == "v1.usp"))
        .unwrap_or(false);
    if !echoed {
        warn!("USP WS: server did not echo Sec-WebSocket-Protocol: v1.usp");
    }

    info!("USP WS: connected to {ws_url}");

    let controller_id = cfg.controller_id.clone();

    // Send WebSocketConnectRecord to identify ourselves
    let connect_rec = websocket_connect_record(agent_id.as_str(), &controller_id);
    let connect_bytes = encode_record(&connect_rec)?;
    ws.send(Message::Binary(connect_bytes)).await?;

    // Send GetSupportedProto to negotiate version
    let gsp_msg = build_get_supported_proto();
    let gsp_bytes = encode_msg(&gsp_msg)?;
    let gsp_rec = no_session_record(agent_id.as_str(), &controller_id, gsp_bytes, "1.3");
    ws.send(Message::Binary(encode_record(&gsp_rec)?)).await?;

    while let Some(frame) = ws.next().await {
        let frame = frame?;
        let data = match frame {
            Message::Binary(b) => b,
            Message::Close(_)  => break,
            Message::Ping(p)   => { ws.send(Message::Pong(p)).await?; continue; }
            _                  => continue,
        };
        let record = match decode_record(&data) {
            Ok(r)  => r,
            Err(e) => { warn!("USP WS: bad record: {e}"); continue; }
        };
        // TR-369 §5.1: discard records not addressed to this endpoint
        if !record.to_id.is_empty() && record.to_id != agent_id.as_str() {
            warn!("USP WS: to_id={} mismatch (expected {}), discarding",
                  record.to_id, agent_id.as_str());
            continue;
        }
        let msg_bytes = match extract_msg_payload(&record) {
            Some(b) => b.to_vec(),
            None    => continue,
        };
        if let Some(resp) = super::super::agent::handle_incoming(
            cfg.clone(), agent_id.clone(), &msg_bytes, Arc::clone(&negotiated_ver)
        ).await {
            let ver = negotiated_ver.lock().unwrap().clone();
            let resp_rec = no_session_record(agent_id.as_str(), &record.from_id, resp, &ver);
            ws.send(Message::Binary(encode_record(&resp_rec)?)).await?;
        }
    }
    Ok(())
}
