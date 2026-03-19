//! USP CoAP MTP — agent side.
//!
//! Implements CoAP (Constrained Application Protocol) for USP message transport.
//! CoAP is a lightweight UDP-based protocol suitable for constrained devices.
//! Uses simple UDP request/response pattern with confirmable messages.

use std::net::UdpSocket;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use log::{debug, error, info, warn};

use super::super::{
    endpoint::EndpointId,
    record::{decode_record, encode_record, extract_msg_payload, no_session_record},
};
use crate::config::ClientConfig;
use tokio::sync::mpsc::Receiver;

const RECONNECT_DELAY: Duration = Duration::from_secs(10);
const COAP_PORT: u16 = 5683;

const COAP_TYPE_CON: u8 = 0;
const COAP_CODE_POST: u8 = 0x02;

pub async fn run(
    cfg: Arc<ClientConfig>,
    agent_id: EndpointId,
    status_rx: Arc<Mutex<Receiver<Vec<u8>>>>,
) {
    let negotiated_ver: Arc<Mutex<String>> = Arc::new(Mutex::new("1.3".into()));

    loop {
        let coap_url = match &cfg.coap_url {
            Some(u) => u.clone(),
            None => {
                warn!("CoAP MTP disabled (no coap_url configured)");
                return;
            }
        };

        info!("USP CoAP: connecting to {coap_url}");

        match coap_loop(
            cfg.clone(),
            agent_id.clone(),
            &coap_url,
            Arc::clone(&negotiated_ver),
            Arc::clone(&status_rx),
        )
        .await
        {
            Ok(()) => debug!("CoAP loop ended normally"),
            Err(e) => error!("CoAP MTP error: {e}"),
        }

        warn!("CoAP: reconnecting in {}s...", RECONNECT_DELAY.as_secs());
        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

async fn coap_loop(
    cfg: Arc<ClientConfig>,
    agent_id: EndpointId,
    coap_url: &str,
    negotiated_ver: Arc<Mutex<String>>,
    status_rx: Arc<Mutex<Receiver<Vec<u8>>>>,
) -> anyhow::Result<()> {
    // Parse URL: coap://host:port/path or coaps://host:port/path
    let url = coap_url
        .trim_start_matches("coap://")
        .trim_start_matches("coaps://");
    let (host_port, path) = if let Some((hp, p)) = url.split_once('/') {
        (hp.to_string(), format!("/{p}"))
    } else {
        (url.to_string(), "/usp".to_string())
    };
    let (host, port) = if let Some((h, p)) = host_port.split_once(':') {
        (h.to_string(), p.parse::<u16>().unwrap_or(COAP_PORT))
    } else {
        (host_port.clone(), COAP_PORT)
    };

    // Bind UDP socket
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.connect(format!("{host}:{port}"))?;
    socket.set_read_timeout(Some(Duration::from_millis(500)))?;
    socket.set_nonblocking(false)?;

    info!("USP CoAP: bound to {host}:{port}, path={path}");

    // Send GetSupportedProto
    let proto_msg = super::super::message::build_get_supported_proto();
    let proto_bytes = super::super::message::encode_msg(&proto_msg)?;
    let proto_rec = no_session_record(
        agent_id.as_str(),
        &cfg.controller_id,
        proto_bytes,
        "1.3",
    );
    let proto_enc = encode_record(&proto_rec)?;

    let mut msg_id: u16 = 1;
    send_coap_post(&socket, msg_id, &path, &proto_enc)?;
    msg_id = msg_id.wrapping_add(1);

    // Main loop: listen for incoming CoAP messages, send status updates
    let mut recv_buf = vec![0u8; 65536];

    loop {
        // Check for status messages to send
        {
            if let Ok(mut rx) = status_rx.try_lock() {
                if let Ok(record_bytes) = rx.try_recv() {
                    send_coap_post(&socket, msg_id, &path, &record_bytes)?;
                    msg_id = msg_id.wrapping_add(1);
                }
            }
        }

        // Try to receive
        match socket.recv(&mut recv_buf) {
            Ok(n) if n >= 4 => {
                let data = &recv_buf[..n];
                // Parse minimal CoAP header
                let token_len = (data[0] & 0x0F) as usize;

                // Skip header + token to find payload marker (0xFF)
                let header_len = 4 + token_len;
                if let Some(payload_start) = data[header_len..]
                    .iter()
                    .position(|&b| b == 0xFF)
                    .map(|p| header_len + p + 1)
                {
                    let payload = &data[payload_start..];

                    // Decode USP record
                    if let Ok(record) = decode_record(payload) {
                        if !record.to_id.is_empty() && record.to_id != agent_id.as_str() {
                            continue;
                        }
                        if let Some(msg_bytes) = extract_msg_payload(&record) {
                            if let Some(resp) = super::super::agent::handle_incoming(
                                cfg.clone(),
                                agent_id.clone(),
                                msg_bytes,
                                Arc::clone(&negotiated_ver),
                            )
                            .await
                            {
                                let ver = negotiated_ver.lock().unwrap().clone();
                                let resp_rec = no_session_record(
                                    agent_id.as_str(),
                                    &record.from_id,
                                    resp,
                                    &ver,
                                );
                                if let Ok(encoded) = encode_record(&resp_rec) {
                                    send_coap_post(&socket, msg_id, &path, &encoded)?;
                                    msg_id = msg_id.wrapping_add(1);
                                }
                            }
                        }
                    }
                }
            }
            Ok(_) => {} // Too short
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // Timeout, loop back to check status_rx
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// Send a CoAP POST (Confirmable) with the given payload.
fn send_coap_post(
    socket: &UdpSocket,
    msg_id: u16,
    path: &str,
    payload: &[u8],
) -> anyhow::Result<()> {
    let mut packet = Vec::with_capacity(4 + path.len() + 1 + payload.len());

    // CoAP header: Version 1, Type CON, Token Length 0
    packet.push(0x40 | COAP_TYPE_CON); // Ver=1, T=CON, TKL=0
    packet.push(COAP_CODE_POST); // Code: 0.02 POST
    packet.push((msg_id >> 8) as u8);
    packet.push(msg_id as u8);

    // Uri-Path option (option number 11)
    // For simple single-segment paths like "/usp"
    let path_segment = path.trim_start_matches('/');
    if !path_segment.is_empty() {
        let opt_delta = 11u8; // Uri-Path option number
        let opt_len = path_segment.len() as u8;
        if opt_len < 13 {
            packet.push((opt_delta << 4) | opt_len);
        } else {
            packet.push((opt_delta << 4) | 13);
            packet.push(opt_len - 13);
        }
        packet.extend_from_slice(path_segment.as_bytes());
    }

    // Content-Format option (12) = application/vnd.bbf.usp.msg (65200)
    let content_format: u16 = 65200;
    packet.push(0x12); // Delta=1 (12-11), Length=2
    packet.push((content_format >> 8) as u8);
    packet.push(content_format as u8);

    // Payload marker
    packet.push(0xFF);
    packet.extend_from_slice(payload);

    socket.send(&packet)?;
    debug!("CoAP: sent POST ({} bytes payload)", payload.len());
    Ok(())
}
