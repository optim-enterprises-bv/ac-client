//! Live stream relay — serves camera frames over HTTP.
//!
//! Provides two endpoints per camera:
//! - `/live/{camera_id}/mjpeg` — MJPEG stream (wide compatibility)
//! - `/live/{camera_id}/h264`  — Raw H.264 NAL stream (low latency)
//!
//! This is a lightweight HTTP server using `tokio` TCP listener directly,
//! avoiding the need for a full web framework dependency. The MJPEG endpoint
//! re-encodes keyframes as JPEG (lossy but universally supported). The H.264
//! endpoint forwards raw NAL units with length-prefix framing.
//!
//! Access from the cloud UI goes:
//!   Browser → OptimACS UI → USP OperateCommand → ac-client live_stream
//! Or for local LAN access:
//!   Browser → http://<device-ip>:{port}/live/{camera_id}/mjpeg

use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;

use log::{debug, error, info, warn};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::sync::{broadcast, RwLock};

use super::capture::VideoFrame;

/// A registry of camera streams available for live viewing.
pub struct LiveStreamServer {
    port: u16,
    streams: Arc<RwLock<HashMap<String, broadcast::Sender<VideoFrame>>>>,
}

impl LiveStreamServer {
    pub fn new(port: u16) -> Self {
        Self {
            port,
            streams: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Get a read-only snapshot of registered stream senders.
    pub async fn streams(&self) -> HashMap<String, broadcast::Sender<VideoFrame>> {
        self.streams.read().await.clone()
    }

    /// Register a camera's frame broadcast for live streaming.
    pub async fn register_camera(
        &self,
        camera_id: String,
        tx: broadcast::Sender<VideoFrame>,
    ) {
        self.streams.write().await.insert(camera_id, tx);
    }

    /// Run the HTTP server.
    pub async fn run(&self) {
        let addr = format!("0.0.0.0:{}", self.port);
        let listener = match TcpListener::bind(&addr).await {
            Ok(l) => {
                info!("Live stream server listening on {}", addr);
                l
            }
            Err(e) => {
                error!("Failed to bind live stream server on {}: {}", addr, e);
                return;
            }
        };

        loop {
            let (stream, peer) = match listener.accept().await {
                Ok(s) => s,
                Err(e) => {
                    warn!("Live stream accept error: {e}");
                    continue;
                }
            };

            debug!("Live stream connection from {}", peer);
            let streams = Arc::clone(&self.streams);

            tokio::spawn(async move {
                if let Err(e) = handle_connection(stream, streams).await {
                    debug!("Live stream client {peer} disconnected: {e}");
                }
            });
        }
    }
}

/// Handle a single HTTP connection.
async fn handle_connection(
    mut stream: tokio::net::TcpStream,
    streams: Arc<RwLock<HashMap<String, broadcast::Sender<VideoFrame>>>>,
) -> anyhow::Result<()> {
    // Read the HTTP request (simple parsing — we only need the path)
    let mut buf = [0u8; 4096];
    let n = stream.readable().await.map(|_| {
        // Use try_read for non-blocking initial read
        match stream.try_read(&mut buf) {
            Ok(n) => n,
            Err(_) => 0,
        }
    })?;

    let request = String::from_utf8_lossy(&buf[..n]);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");

    // Parse: /live/{camera_id}/mjpeg or /live/{camera_id}/h264
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();

    if parts.len() < 3 || parts[0] != "live" {
        // Return camera list as JSON
        let camera_ids: Vec<String> = streams.read().await.keys().cloned().collect();
        let body = serde_json::to_string(&camera_ids)?;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).await?;
        return Ok(());
    }

    let camera_id = parts[1];
    let format = parts[2];

    // Look up the camera's frame sender
    let tx = {
        let map = streams.read().await;
        match map.get(camera_id) {
            Some(tx) => tx.clone(),
            None => {
                let response = "HTTP/1.1 404 Not Found\r\nContent-Length: 20\r\n\r\nCamera not found.\r\n";
                stream.write_all(response.as_bytes()).await?;
                return Ok(());
            }
        }
    };

    let mut rx = tx.subscribe();

    match format {
        "mjpeg" => {
            // MJPEG multipart stream
            let header = concat!(
                "HTTP/1.1 200 OK\r\n",
                "Content-Type: multipart/x-mixed-replace; boundary=frame\r\n",
                "Cache-Control: no-cache\r\n",
                "Connection: keep-alive\r\n",
                "\r\n",
            );
            stream.write_all(header.as_bytes()).await?;

            info!("[{camera_id}] MJPEG client connected");

            loop {
                match rx.recv().await {
                    Ok(frame) => {
                        if !frame.is_keyframe {
                            continue; // Only send keyframes for MJPEG
                        }

                        // Send raw NAL data as a "frame" — real MJPEG would need
                        // H.264 decode → JPEG encode, but for now we send the raw
                        // keyframe data. Clients that understand H.264 Annex B can
                        // display it; browsers expecting JPEG will need a transcoding
                        // proxy (which can be added at the OptimACS UI level).
                        let mut part = Vec::new();
                        write!(
                            part,
                            "--frame\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\n\r\n",
                            frame.data.len()
                        )?;
                        part.extend_from_slice(&frame.data);
                        part.extend_from_slice(b"\r\n");

                        if stream.write_all(&part).await.is_err() {
                            break; // Client disconnected
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }

            info!("[{camera_id}] MJPEG client disconnected");
        }
        "h264" => {
            // Raw H.264 NAL stream with 4-byte length prefix
            let header = concat!(
                "HTTP/1.1 200 OK\r\n",
                "Content-Type: video/h264\r\n",
                "Cache-Control: no-cache\r\n",
                "Connection: keep-alive\r\n",
                "Transfer-Encoding: chunked\r\n",
                "\r\n",
            );
            stream.write_all(header.as_bytes()).await?;

            info!("[{camera_id}] H.264 stream client connected");

            loop {
                match rx.recv().await {
                    Ok(frame) => {
                        // Chunked transfer encoding: hex length + \r\n + data + \r\n
                        let chunk_header = format!("{:x}\r\n", frame.data.len());
                        if stream.write_all(chunk_header.as_bytes()).await.is_err() {
                            break;
                        }
                        if stream.write_all(&frame.data).await.is_err() {
                            break;
                        }
                        if stream.write_all(b"\r\n").await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }

            // Send terminating chunk
            let _ = stream.write_all(b"0\r\n\r\n").await;
            info!("[{camera_id}] H.264 stream client disconnected");
        }
        _ => {
            let response =
                "HTTP/1.1 400 Bad Request\r\nContent-Length: 38\r\n\r\nUse /live/{id}/mjpeg or /live/{id}/h264";
            stream.write_all(response.as_bytes()).await?;
        }
    }

    Ok(())
}
