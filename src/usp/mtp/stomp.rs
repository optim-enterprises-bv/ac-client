//! USP STOMP MTP — agent side.
//!
//! Implements STOMP 1.2 over TCP/TLS for USP message transport.
//! STOMP is a simple text-based protocol widely used in enterprise deployments.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use log::{debug, error, info, trace, warn};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use super::super::{
    endpoint::EndpointId,
    record::{decode_record, encode_record, extract_msg_payload, no_session_record},
};
use crate::config::ClientConfig;
use crate::usp::record::stomp_connect_record;
use tokio::sync::mpsc::Receiver;

const RECONNECT_DELAY: Duration = Duration::from_secs(10);

pub async fn run(
    cfg: Arc<ClientConfig>,
    agent_id: EndpointId,
    status_rx: Arc<Mutex<Receiver<Vec<u8>>>>,
) {
    let negotiated_ver: Arc<Mutex<String>> = Arc::new(Mutex::new("1.3".into()));

    loop {
        let stomp_url = match &cfg.stomp_url {
            Some(u) => u.clone(),
            None => {
                warn!("STOMP MTP disabled (no stomp_url configured)");
                return;
            }
        };

        info!("USP STOMP: connecting to {stomp_url}");

        match stomp_loop(
            cfg.clone(),
            agent_id.clone(),
            &stomp_url,
            Arc::clone(&negotiated_ver),
            Arc::clone(&status_rx),
        )
        .await
        {
            Ok(()) => debug!("STOMP loop ended normally"),
            Err(e) => error!("STOMP MTP error: {e}"),
        }

        warn!("STOMP: reconnecting in {}s...", RECONNECT_DELAY.as_secs());
        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

async fn stomp_loop(
    cfg: Arc<ClientConfig>,
    agent_id: EndpointId,
    stomp_url: &str,
    negotiated_ver: Arc<Mutex<String>>,
    status_rx: Arc<Mutex<Receiver<Vec<u8>>>>,
) -> anyhow::Result<()> {
    // Parse URL: stomp://host:port or stomps://host:port
    let url = stomp_url
        .trim_start_matches("stomp://")
        .trim_start_matches("stomps://");
    let (host, port) = if let Some((h, p)) = url.split_once(':') {
        (h.to_string(), p.parse::<u16>().unwrap_or(61613))
    } else {
        (url.to_string(), 61613)
    };

    let stream = TcpStream::connect(format!("{host}:{port}")).await?;
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader);

    // STOMP CONNECT frame
    let connect_frame = format!(
        "CONNECT\naccept-version:1.2\nhost:{host}\nheart-beat:30000,30000\n\n\0"
    );
    writer.write_all(connect_frame.as_bytes()).await?;

    // Read CONNECTED response
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    if !line.starts_with("CONNECTED") {
        return Err(anyhow::anyhow!("STOMP: expected CONNECTED, got: {line}"));
    }
    // Read remaining headers until empty line
    loop {
        line.clear();
        reader.read_line(&mut line).await?;
        if line.trim().is_empty() {
            break;
        }
    }
    // Read null byte
    let mut null = [0u8; 1];
    let _ = tokio::io::AsyncReadExt::read_exact(&mut reader, &mut null).await;

    info!("USP STOMP: connected to {host}:{port}");

    // Subscribe to agent destination
    let agent_dest = cfg
        .stomp_destination
        .as_deref()
        .map(|d| format!("{d}/{}", agent_id.as_str()))
        .unwrap_or_else(|| format!("/topic/usp.agent.{}", agent_id.as_str()));

    let subscribe_frame = format!(
        "SUBSCRIBE\nid:0\ndestination:{agent_dest}\nack:auto\n\n\0"
    );
    writer.write_all(subscribe_frame.as_bytes()).await?;
    debug!("STOMP: subscribed to {agent_dest}");

    // Send STOMPConnectRecord
    let controller_id = &cfg.controller_id;
    let controller_dest = cfg
        .stomp_destination
        .as_deref()
        .map(|d| format!("{d}/{controller_id}"))
        .unwrap_or_else(|| format!("/topic/usp.controller.{controller_id}"));

    let connect_rec = stomp_connect_record(agent_id.as_str(), controller_id, &agent_dest);
    let connect_bytes = encode_record(&connect_rec)?;
    send_stomp_message(&mut writer, &controller_dest, &connect_bytes).await?;

    // Send GetSupportedProto for version negotiation
    let proto_msg = super::super::message::build_get_supported_proto();
    let proto_bytes = super::super::message::encode_msg(&proto_msg)?;
    let proto_rec = no_session_record(agent_id.as_str(), controller_id, proto_bytes, "1.3");
    let proto_enc = encode_record(&proto_rec)?;
    send_stomp_message(&mut writer, &controller_dest, &proto_enc).await?;

    // Main message loop — read STOMP MESSAGE frames
    let mut buf = Vec::new();
    loop {
        // Check for status messages to send — extract bytes without holding lock across await
        let pending_status = {
            status_rx
                .try_lock()
                .ok()
                .and_then(|mut rx| rx.try_recv().ok())
        };
        if let Some(record_bytes) = pending_status {
            send_stomp_message(&mut writer, &controller_dest, &record_bytes).await?;
        }

        // Read next frame command
        line.clear();
        // Use a timeout so we can check status_rx periodically
        let read_result = tokio::time::timeout(
            Duration::from_millis(100),
            reader.read_line(&mut line),
        )
        .await;

        match read_result {
            Ok(Ok(0)) => return Err(anyhow::anyhow!("STOMP: connection closed")),
            Ok(Ok(_)) => {}
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => continue, // Timeout, check status_rx again
        }

        let command = line.trim().to_string();
        if command.is_empty() || command == "\0" {
            continue; // Heartbeat or null byte
        }

        // Read headers
        let mut content_length: Option<usize> = None;
        loop {
            line.clear();
            reader.read_line(&mut line).await?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }
            if let Some(val) = trimmed.strip_prefix("content-length:") {
                content_length = val.parse().ok();
            }
        }

        // Read body
        buf.clear();
        if let Some(len) = content_length {
            buf.resize(len, 0);
            tokio::io::AsyncReadExt::read_exact(&mut reader, &mut buf).await?;
            // Read trailing null
            let mut null = [0u8; 1];
            let _ = tokio::io::AsyncReadExt::read_exact(&mut reader, &mut null).await;
        } else {
            // Read until null byte
            loop {
                let mut byte = [0u8; 1];
                tokio::io::AsyncReadExt::read_exact(&mut reader, &mut byte).await?;
                if byte[0] == 0 {
                    break;
                }
                buf.push(byte[0]);
            }
        }

        if command != "MESSAGE" {
            trace!("STOMP: ignoring frame type: {command}");
            continue;
        }

        // Decode USP record from body
        let record = match decode_record(&buf) {
            Ok(r) => r,
            Err(e) => {
                error!("STOMP: failed to decode record: {e}");
                continue;
            }
        };

        if !record.to_id.is_empty() && record.to_id != agent_id.as_str() {
            warn!("STOMP: to_id mismatch, discarding");
            continue;
        }

        let msg_bytes = match extract_msg_payload(&record) {
            Some(b) => b.to_vec(),
            None => continue,
        };

        if let Some(resp) = super::super::agent::handle_incoming(
            cfg.clone(),
            agent_id.clone(),
            &msg_bytes,
            Arc::clone(&negotiated_ver),
        )
        .await
        {
            let ver = negotiated_ver.lock().unwrap().clone();
            let resp_rec = no_session_record(agent_id.as_str(), &record.from_id, resp, &ver);
            if let Ok(encoded) = encode_record(&resp_rec) {
                send_stomp_message(&mut writer, &controller_dest, &encoded).await?;
            }
        }
    }
}

async fn send_stomp_message<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    destination: &str,
    payload: &[u8],
) -> anyhow::Result<()> {
    let header = format!(
        "SEND\ndestination:{destination}\ncontent-type:application/vnd.bbf.usp.msg\ncontent-length:{}\n\n",
        payload.len()
    );
    writer.write_all(header.as_bytes()).await?;
    writer.write_all(payload).await?;
    writer.write_all(&[0]).await?; // Null terminator
    writer.flush().await?;
    Ok(())
}
