//! Kerberos Vault / S3-compatible upload for recordings.
//!
//! Watches the per-camera recording directory for completed `.mp4` files
//! and uploads them to the configured Vault endpoint. Successfully uploaded
//! files are deleted locally.

use std::path::{Path, PathBuf};
use std::time::Duration;

use log::{debug, error, info, warn};
use reqwest::Client;
use tokio::fs;

/// Uploads completed recordings to a Kerberos Vault instance.
pub struct VaultUploader {
    camera_id: String,
    vault_uri: String,
    access_key: String,
    secret_key: String,
    watch_dir: PathBuf,
    http: Client,
}

impl VaultUploader {
    pub fn new(
        camera_id: String,
        vault_uri: String,
        access_key: String,
        secret_key: String,
        recording_dir: String,
    ) -> Self {
        let watch_dir = PathBuf::from(recording_dir).join(&camera_id);
        let http = Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .expect("Failed to build HTTP client");

        Self {
            camera_id,
            vault_uri,
            access_key,
            secret_key,
            watch_dir,
            http,
        }
    }

    /// Run the upload loop — polls for new recordings and uploads them.
    pub async fn run(&self) {
        info!(
            "[{}] Vault uploader started (uri={}, dir={})",
            self.camera_id, self.vault_uri, self.watch_dir.display()
        );

        let poll_interval = Duration::from_secs(10);

        loop {
            if let Err(e) = self.upload_pending().await {
                warn!("[{}] Upload scan error: {}", self.camera_id, e);
            }
            tokio::time::sleep(poll_interval).await;
        }
    }

    /// Scan for completed recordings and upload them.
    async fn upload_pending(&self) -> anyhow::Result<()> {
        let mut dir = match fs::read_dir(&self.watch_dir).await {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };

        while let Some(entry) = dir.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("mp4") {
                continue;
            }

            // Skip files still being written (modified in last 5 seconds)
            if let Ok(meta) = entry.metadata().await {
                if let Ok(modified) = meta.modified() {
                    if modified.elapsed().unwrap_or_default() < Duration::from_secs(5) {
                        continue;
                    }
                }
            }

            match self.upload_file(&path).await {
                Ok(()) => {
                    info!("[{}] Uploaded: {}", self.camera_id, path.display());
                    if let Err(e) = fs::remove_file(&path).await {
                        warn!(
                            "[{}] Cannot remove uploaded file {}: {}",
                            self.camera_id,
                            path.display(),
                            e
                        );
                    }
                }
                Err(e) => {
                    error!(
                        "[{}] Upload failed for {}: {}",
                        self.camera_id,
                        path.display(),
                        e
                    );
                }
            }
        }

        Ok(())
    }

    /// Upload a single recording file to Kerberos Vault.
    async fn upload_file(&self, path: &Path) -> anyhow::Result<()> {
        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown.mp4");

        let data = fs::read(path).await?;
        let size = data.len();

        debug!(
            "[{}] Uploading {} ({} bytes) to Vault",
            self.camera_id, filename, size
        );

        let upload_url = format!("{}/api/storage/upload", self.vault_uri.trim_end_matches('/'));

        let form = reqwest::multipart::Form::new()
            .text("camera_id", self.camera_id.clone())
            .text("filename", filename.to_string())
            .part(
                "file",
                reqwest::multipart::Part::bytes(data)
                    .file_name(filename.to_string())
                    .mime_str("video/mp4")?,
            );

        let resp = self
            .http
            .post(&upload_url)
            .header("X-Kerberos-Storage-AccessKey", &self.access_key)
            .header("X-Kerberos-Storage-SecretAccessKey", &self.secret_key)
            .multipart(form)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Vault returned {status}: {body}");
        }

        info!(
            "[{}] Uploaded {} ({:.1} KB)",
            self.camera_id,
            filename,
            size as f64 / 1024.0
        );

        Ok(())
    }
}
