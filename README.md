# ac-client — USP Agent for OpenWrt Access Points

`ac-client` is a Rust daemon implementing the **TR-369 / USP 1.3 Agent** (User Services Platform, Broadband Forum) for OpenWrt-based access-point devices managed by an [OptimACS](https://github.com/optim-enterprises-bv/APConfig) controller (`ac-server`).

---

## Features

- **TR-369 / USP 1.3** conformant Agent (Boot! Notify, GET, SET, OPERATE)
- **WebSocket MTP** and **MQTT MTP** — configurable, or both simultaneously
- **Mutual TLS** with post-quantum hybrid key exchange (X25519 + ML-KEM-768) via `rustls-post-quantum`
- **UCI-backed TR-181 data model** — Device.DeviceInfo, Device.WiFi, Device.IP, Device.Hosts, Device.DHCPv4
- **Vendor extensions**: `Device.X_OptimACS_Camera.*`, `Device.X_OptimACS_Firmware.*`, `Device.X_OptimACS_Security.*`
- **Two-phase provisioning**: bootstrap cert → controller-issued mTLS cert lifecycle
- **Firmware upgrade** via sysupgrade
- **Axis IP-camera discovery** (ARP scan + CGI API) and JPEG upload
- **GNSS telemetry** (NMEA serial reader)
- **ValueChange** periodic telemetry (uptime, load, GPS, wireless, modem)
- **OpenWrt package feed** entry (`package/ac-client/`) for cross-compilation via `rust-package.mk`

---

## Repository Layout

```
ac-client/
├── src/
│   ├── main.rs            — tokio runtime, load config, spawn agent
│   ├── config.rs          — parse ac_client.conf + MtpType enum
│   ├── apply.rs           — apply_config(), save_certs(), apply_firmware()
│   ├── cam.rs             — Axis camera discovery + JPEG capture
│   ├── gnss.rs            — GNSS position reader (NMEA serial)
│   ├── tls.rs             — mutual TLS client connector
│   ├── util.rs            — read_uptime(), read_fw_version(), MAC detection, etc.
│   └── usp/
│       ├── mod.rs         — UspError, proto includes
│       ├── agent.rs       — main USP agent loop
│       ├── record.rs      — encode/decode USP Records
│       ├── message.rs     — builder helpers (Boot!, ValueChange, etc.)
│       ├── endpoint.rs    — EndpointId from MAC
│       ├── session.rs     — sequence_id counter
│       ├── dm/            — TR-181 data model (UCI-backed)
│       │   ├── mod.rs         — DmCtx, get_params(), set_params(), operate()
│       │   ├── device_info.rs — Device.DeviceInfo.*
│       │   ├── wifi.rs        — Device.WiFi.* via UCI
│       │   ├── ip.rs          — Device.IP.Interface.*
│       │   ├── dhcp.rs        — Device.DHCPv4.*
│       │   ├── hosts.rs       — Device.Hosts.Host.*
│       │   ├── cameras.rs     — Device.X_OptimACS_Camera.*
│       │   ├── firmware.rs    — Device.X_OptimACS_Firmware.*
│       │   └── security.rs    — Device.X_OptimACS_Security.*
│       └── mtp/
│           ├── websocket.rs   — WSS client with reconnect loop
│           └── mqtt.rs        — rumqttc MQTT client
├── proto/                 — vendored Protocol Buffer schemas
│   ├── acp.proto          — OptimACS control protocol
│   ├── usp-record.proto   — TR-369 USP Record wire format
│   └── usp-msg.proto      — TR-369 USP Message types
├── build.rs               — prost-build codegen for proto files
├── Cargo.toml
├── Cargo.lock
└── package/
    └── ac-client/         — OpenWrt package feed entry
        ├── Makefile       — OpenWrt package definition (rust-package.mk)
        └── files/
            ├── ac-client.init    — procd init script
            └── ac_client.conf    — default configuration
```

---

## Building

### Native (host) build

**Requirements:** Rust stable ≥ 1.75, `cmake`, `clang`

```sh
cargo build --release
cargo test
```

Output: `target/release/ac-client`

### Cross-compile for OpenWrt

Use the OpenWrt buildroot with the `package/ac-client/` feed entry (see [OpenWrt Package](#openwrt-package) below).

---

## OpenWrt Package

The `package/ac-client/` directory is an OpenWrt package feed entry that cross-compiles `ac-client` for any OpenWrt target architecture (MIPS, ARM, AArch64, x86_64).

### Requirements

- OpenWrt 22.03 or later (musl 1.2 + kernel headers)
- Rust host toolchain from `packages/lang/rust` in the packages feed
- `cmake` on the build host — pulled in automatically via `HOST_BUILD_DEPENDS`

### Add to your buildroot

```sh
# 1. Register the feed in feeds.conf
echo "src-git-full  ac-client  git@github.com:optim-enterprises-bv/ac-client.git" >> feeds.conf

# 2. Update and install the feed
./scripts/feeds update ac-client
./scripts/feeds install ac-client

# 3. Select the package
make menuconfig
#    Network → Management → ac-client  [*]

# 4. Build
make package/ac-client/compile V=s
```

> **Note:** Use `src-git-full` (not `src-git`) so the full repo history is cloned — required for subdir-based builds.

### Installed files

| Path | Description |
|------|-------------|
| `/usr/sbin/ac-client` | Daemon binary |
| `/etc/apclient/ac_client.conf` | Default configuration (preserved across upgrades) |
| `/etc/init.d/ac-client` | procd init script (respawning, logs to syslog) |
| `/etc/apclient/init/` | Directory for the bootstrap certificate |
| `/etc/apclient/certs/` | Directory for the provisioned client certificate |

---

## Certificate Deployment

Before starting `ac-client`, deploy the bootstrap (init) certificate issued by the OptimACS server:

```sh
scp <server>:/var/ac-server/peers/00:00:00:00:00:00/client.crt \
    root@<ap>:/etc/apclient/init/client.crt
scp <server>:/var/ac-server/peers/00:00:00:00:00:00/client.key \
    root@<ap>:/etc/apclient/init/client.key
scp <server>:/etc/optimacs/CA/rootCA.crt \
    root@<ap>:/etc/apclient/init/ca.crt

/etc/init.d/ac-client enable
/etc/init.d/ac-client start
```

---

## Configuration

`ac-client` reads `/etc/apclient/ac_client.conf` (`key = value` format, `#` comments).

```sh
/etc/init.d/ac-client restart   # after config changes
```

### TLS / Certificates

| Key | Default | Description |
|-----|---------|-------------|
| `init_cert` | `/etc/apclient/init/client.crt` | Bootstrap certificate (pre-provisioning) |
| `init_key` | `/etc/apclient/init/client.key` | Bootstrap private key |
| `ca_file` | `/etc/apclient/init/ca.crt` | CA certificate for server verification |
| `cert_file` | `/etc/apclient/certs/client.crt` | Provisioned client certificate |
| `key_file` | `/etc/apclient/certs/client.key` | Provisioned client private key |
| `cert_dir` | `/etc/apclient/certs` | Directory where provisioned certs are saved |

### Connection

| Key | Default | Description |
|-----|---------|-------------|
| `server_host` | `0.0.0.0` | ac-server hostname or IP |
| `server_cn` | `ac-server` | Expected CN in the server TLS certificate (SNI) |
| `mtp` | `websocket` | MTP selection: `websocket` \| `mqtt` \| `both` |
| `ws_url` | `wss://0.0.0.0:3491/usp` | WebSocket MTP URL |
| `mqtt_url` | `mqtt://0.0.0.0:1883` | MQTT broker URL |
| `mqtt_client_id` | *(auto)* | MQTT client identifier |

### Device Identity

| Key | Default | Description |
|-----|---------|-------------|
| `mac_addr` | *(auto)* | MAC address — auto-detected from `br-lan`/`eth0`/`wlan0` |
| `usp_endpoint_id` | *(auto)* | USP Endpoint ID — auto-generated as `oui:{oui}:{mac}` |
| `controller_id` | `oui:00005A:OptimACS-Controller-1` | Controller endpoint ID |

### Telemetry

| Key | Default | Description |
|-----|---------|-------------|
| `status_interval` | `300` | Seconds between ValueChange Notify messages |
| `gnss_dev` | *(disabled)* | Serial device for NMEA GPS (e.g. `/dev/ttyUSB0`) |
| `gnss_baud` | `9600` | GNSS baud rate |

### Storage Paths

| Key | Default | Description |
|-----|---------|-------------|
| `fw_dir` | `/tmp/apclient/firmware` | Scratch directory for downloaded firmware |
| `img_dir` | `/var/apclient/images` | Directory for saved camera snapshots |
| `pid_file` | `/var/run/apclient.pid` | PID file path |

### Process Behaviour

| Key | Default | Description |
|-----|---------|-------------|
| `daemonize` | `false` | Background daemon mode (leave `false` under procd) |
| `log_syslog` | `true` | Log to syslog (`true`) or stderr (`false`) |

---

## Protocol Details

### Device Lifecycle

```
Phase 1 — Provisioning
  Connect with init cert → Boot! Notify → controller approves → receive CERT
  → save to cert_dir → reconnect with provisioned cert

Phase 2 — Operation
  Boot! Notify → GET/SET/OPERATE dispatch loop
  → ValueChange Notify every status_interval seconds
  → Firmware upgrade via OPERATE Device.X_OptimACS_Firmware.Update()
  → Camera cycle every cam_interval seconds
```

### TLS

All connections use **TLS 1.3** with the `rustls-post-quantum` provider, which negotiates an **X25519 + ML-KEM-768 hybrid key exchange** matching the ac-server configuration.

### USP Endpoint ID

Auto-generated from the device MAC address:

```
oui:{vendor-oui}:{mac-address-without-colons}
# e.g. oui:0060B3:aabbccddeeff
```

---

## Related Repositories

| Repository | Description |
|------------|-------------|
| [optim-enterprises-bv/APConfig](https://github.com/optim-enterprises-bv/APConfig) | Full OptimACS stack — ac-server, management UI, Docker Compose, docs |
| [optim-enterprises-bv/helm-charts](https://github.com/optim-enterprises-bv/helm-charts) | Helm chart for Kubernetes deployment |

---

## License

See [APConfig](https://github.com/optim-enterprises-bv/APConfig) for license information.
