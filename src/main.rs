//! USP Agent daemon for OpenWrt access-point devices (TR-369).
//!
//! Usage:
//!   ac-client -c /etc/apclient/ac_client.conf
//!   ac-client -c /etc/apclient/ac_client.conf --stderr   # log to stderr

mod apply;
mod cam;
mod config;
mod error;
mod gnss;
mod heartbeat;
mod proto;
mod tls;
mod usp;
mod util;

use std::path::PathBuf;
use std::process;
use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use log::{error, info};

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Debug, Parser)]
#[command(name = "ac-client", about = "USP Agent (TR-369) — OptimACS access-point client")]
struct Cli {
    /// Path to the flat key=value configuration file.
    /// Ignored when --uci is set.
    #[arg(short = 'c', long = "config", default_value = "/etc/apclient/ac_client.conf")]
    config: PathBuf,

    /// Read configuration from UCI (/etc/config/optimacs) instead of the
    /// flat config file.  All options are read from the 'agent' section:
    ///   uci show optimacs.agent
    #[arg(short = 'u', long = "uci")]
    uci: bool,

    /// Log to stderr instead of syslog (useful for debugging).
    #[arg(long)]
    stderr: bool,
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let cfg = if cli.uci {
        match config::load_config_uci() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("ac-client: UCI config error: {e}");
                process::exit(1);
            }
        }
    } else {
        match config::load_config(&cli.config) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("ac-client: config error: {e}");
                process::exit(1);
            }
        }
    };
    if let Err(e) = config::validate_config(&cfg) {
        eprintln!("ac-client: config validation: {e}");
        process::exit(1);
    }

    // Set up logging — prefer syslog, fall back to stderr if the socket is not
    // yet available (can happen early in the boot sequence before logd is ready).
    let use_syslog = cfg.log_syslog && !cli.stderr;
    if use_syslog {
        if let Err(e) = setup_logging(true) {
            eprintln!("ac-client: syslog unavailable ({e}), falling back to stderr");
            setup_logging(false).ok();
        }
    } else {
        setup_logging(false).ok();
    }

    // Install the post-quantum TLS provider (must happen before any TLS use).
    if let Err(e) = rustls_post_quantum::provider().install_default() {
        error!("FATAL: post-quantum TLS provider failed to initialise: {e}");
        error!("Ensure the binary was compiled for the correct CPU architecture.");
        process::exit(1);
    }

    // Write PID file
    if let Err(e) = util::write_pid_file(&cfg.pid_file) {
        error!("cannot write PID file {}: {e}", cfg.pid_file.display());
    }

    // Auto-detect MAC if not configured.
    // detect_mac() tries a broad set of interface names; if none are found
    // the operator must set mac_addr explicitly in UCI or the flat config.
    let cfg = if cfg.mac_addr.is_empty() {
        let mac = util::detect_mac();
        if mac.is_empty() {
            error!("mac_addr not configured and auto-detection failed.");
            error!("Set it explicitly:  uci set optimacs.agent.mac_addr='<mac>'");
            error!("                    uci commit optimacs");
            error!("                    /etc/init.d/ac-client restart");
            process::exit(1);
        }
        info!("auto-detected MAC address: {mac}");
        config::ClientConfig { mac_addr: mac, ..cfg }
    } else {
        cfg
    };

    // Derive ws_url from server_host if not set explicitly
    let cfg = if cfg.ws_url.is_none() && !cfg.server_host.is_empty() {
        config::ClientConfig {
            ws_url: Some(format!("wss://{}:{}/usp", cfg.server_host, cfg.server_port)),
            ..cfg
        }
    } else {
        cfg
    };

    let cfg = Arc::new(cfg);

    info!("ac-client starting (MTP={:?})", cfg.mtp);

    // Start GNSS reader (non-fatal if device not present)
    let gnss_pos = if cfg.gnss_dev.is_empty() {
        std::sync::Arc::new(std::sync::Mutex::new(None))
    } else {
        gnss::spawn_gnss_reader(&cfg.gnss_dev, cfg.gnss_baud)
    };

    // Run the USP agent; restart on error
    loop {
        usp::agent::run(Arc::clone(&cfg), Arc::clone(&gnss_pos)).await;
        error!("USP agent exited; restarting in 30s");
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    }
}

// ── Logging setup ─────────────────────────────────────────────────────────────

fn setup_logging(use_syslog: bool) -> anyhow::Result<()> {
    if use_syslog {
        let formatter = syslog::Formatter3164 {
            facility: syslog::Facility::LOG_DAEMON,
            hostname: None,
            process:  "ac-client".into(),
            pid:      process::id(),
        };
        let logger = syslog::unix(formatter)
            .map_err(|e| anyhow::anyhow!("syslog connect failed: {e}"))?;
        log::set_boxed_logger(Box::new(syslog::BasicLogger::new(logger)))
            .map(|()| log::set_max_level(log::LevelFilter::Info))
            .map_err(|e| anyhow::anyhow!("set_logger: {e}"))?;
    } else {
        env_logger::Builder::from_default_env()
            .filter_level(log::LevelFilter::Debug)
            .init();
    }
    Ok(())
}
