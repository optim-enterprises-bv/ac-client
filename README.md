# ac-client — USP Agent for OpenWrt Access Points

`ac-client` is a Rust daemon implementing the **TR-369 / USP 1.3 Agent** (User Services Platform, Broadband Forum) for OpenWrt-based access-point devices managed by an [OptimACS](https://acs.optimcloud.com) controller (`ac-server`).

**Key Features:**
- ✅ **Full TP-469/USMP compliance** - ADD, DELETE, GetSupportedDM, GetInstances
- ✅ **Complete UCI backend** - 47 operations for all OpenWrt configurations
- ✅ **WiFi 7 (EHT) support** - EHT20, EHT80, EHT160, EHT320 modes
- ✅ **IPv6 support** - Prefix management and address configuration
- ✅ **Bridge & VLAN management** - List-type parameter handling
- ✅ **System configuration** - Timezone, hostname, log settings, LED control

---

## What's New - TP-469 Implementation

ac-client now includes **complete TP-469/USMP (TR-369 §6.1) implementation** with full UCI backend integration:

### TP-469 Message Support
| Message Type | Status | Description |
|--------------|--------|-------------|
| `GET` | ✅ | Read parameter values with path expressions |
| `SET` | ✅ | Write parameter values with atomic updates |
| `ADD` | ✅ | Create multi-instance objects via UCI |
| `DELETE` | ✅ | Remove object instances with cleanup |
| `OPERATE` | ✅ | Execute device commands |
| `GetSupportedDM` | ✅ | Report data model capabilities |
| `GetInstances` | ✅ | Enumerate multi-instance objects |
| `NOTIFY` | ✅ | Event notifications (Boot!, ValueChange) |

### UCI Backend Operations (47 Functions)
- **WiFi Radio Management:** channel, htmode (EHT modes), cell_density, country
- **WiFi Interfaces:** SSID, encryption, OCV, device association
- **Network:** interfaces, bridge ports, DNS lists, MAC addresses, IPv6
- **DHCP:** pools, static leases, dnsmasq options, host entries
- **System:** hostname, timezone, zonename, log_size, TTY login, LED configs
- **Service Management:** Automatic restart on configuration changes

### Real-World UCI Support
Based on the OpenWrt backup configuration (`backup-OpenWrt-2026-03-05.tar.gz`), ac-client supports:
- `wireless.radio{i}` with EHT modes (WiFi 7)
- `network.{interface}` with bridge ports and IPv6
- `dhcp` with pools and static leases
- `system` with timezone, zonename, LED configurations
- `firewall` zones and rules

See [UCI_BACKEND_OPERATIONS.md](UCI_BACKEND_OPERATIONS.md) for the complete API reference.

---

## Getting Started with OptimACS

`ac-client` reports to an [OptimACS](https://acs.optimcloud.com) controller. This section walks through creating an account and configuring your first tenant so devices can connect.

### 1. Create an account

Navigate to **[acs.optimcloud.com](https://acs.optimcloud.com)** and click **Create account** on the sign-in page, or go directly to `/signup`.

![Sign-in page with Create account link](docs/images/screens/login.png)

Fill in the four sections of the signup form:

![Signup form](docs/images/screens/signup.png)

| Section | What to fill in |
|---------|----------------|
| **Your Account** | First name, last name, work email, and a password (minimum 8 characters) |
| **Your Organization** | Company name, phone (optional), and country |
| **Choose a Plan** | Select the plan that fits your AP fleet size — you can upgrade at any time |
| **Payment Method** | Card details are collected via the Airwallex-hosted form. Card numbers are **never stored** on OptimACS servers — only a tokenized reference is saved in an isolated, encrypted PII database |

Click **Create account**. Your tenant is provisioned immediately and you are signed in automatically.

---

### 2. Manage your account

Access account settings at any time via the **user menu → My Account** in the top-right corner of the console.

#### Profile

Update your name, email address, and phone number.

![Account — Profile](docs/images/screens/account-profile.png)

#### Organization & Billing Address

Set your company name and billing address. This information appears on all invoices.

![Account — Organization](docs/images/screens/account-org.png)

#### Security

Change your password using the live strength meter. Two-factor authentication via authenticator app is coming soon.

![Account — Security](docs/images/screens/account-security.png)

#### Danger Zone

Permanently delete your account and all associated data, or export a GDPR-compliant JSON copy of your personal data.

![Account — Danger Zone](docs/images/screens/account-danger.png)

> Deleting your account removes all access points, users, PII records, and billing data permanently. You must type `DELETE` to confirm. This action cannot be undone.

---

### 3. Point ac-client at your tenant

Once your account is created, set `server_host` and `server_cn` in `/etc/apclient/ac_client.conf` on each device:

```ini
server_host = acs.optimcloud.com
server_cn   = acs.optimcloud.com
```

The controller URL is the same for all tenants — authentication is handled via the device's provisioned client certificate issued by your tenant's step-ca instance.

---

## Why ac-client? Why OptimACS?

### The problem with managing fleets of access points

Running dozens — or thousands — of Wi-Fi access points across sites, campuses, or multi-tenant deployments is hard. Traditional approaches require vendor-specific NMS software, custom SSH scripting, or fragile SNMP polling. Devices drift from their intended configuration, firmware updates are manual and error-prone, and there is no standard way to query live device state from a remote controller.

### TR-369 / USP: the open standard

The Broadband Forum's **TR-369 User Services Platform** (USP) defines a vendor-neutral, standards-based protocol for device management. A **Controller** (ac-server) pushes configuration, queries state, and triggers operations over a reliable, authenticated channel. An **Agent** (ac-client) on each device implements a structured **TR-181 data model** — a machine-readable, hierarchical representation of every configurable parameter on the device. Any controller that speaks USP can manage any agent that speaks USP, regardless of vendor.

### Why Rust? Why open source?

- **Memory safety**: no buffer overflows, no use-after-free, no null-pointer crashes. The daemon runs on constrained MIPS/ARM hardware with limited RAM and no fault recovery — correct code matters.
- **Minimal binary**: the release build strips to a small self-contained binary with no runtime dependencies beyond musl libc. No Python, no JRE, no heavyweight runtime on the AP.
- **Post-quantum TLS**: ac-client uses `rustls-post-quantum` to negotiate **X25519 + ML-KEM-768** hybrid key exchange — a NIST PQC standard — on every connection. Device deployments live for years; their communications should be safe against harvest-now/decrypt-later attacks.
- **Standards compliance**: the implementation is audited against the TR-369 v1.3 conformance requirements — Boot! event parameters, Record routing, WebSocket subprotocol enforcement, version negotiation, and error codes.
- **Open source**: the full protocol stack, data model, and OpenWrt packaging are available for inspection, extension, and contribution. No binary blobs, no vendor lock-in.

### What OptimACS gives you out of the box

| Capability | Detail |
|------------|--------|
| Zero-touch provisioning | APs boot with a shared init cert; controller issues a unique per-device mTLS cert on approval |
| Live configuration push | SET any TR-181 parameter from the UI; agent applies via UCI and responds with `SET_RESP` |
| Firmware management | Upload firmware to the server; push via `OPERATE Device.X_OptimACS_Firmware.Download()` → sysupgrade |
| Real-time telemetry | ValueChange Notify every N seconds: uptime, load, free memory, GPS coords, wireless status |
| Camera management | Axis IP-camera discovery (ARP scan + CGI), periodic JPEG capture, image upload to server |
| Multi-tenant RBAC | Isolate fleets per tenant; role hierarchy from stats_viewer to super_admin |
| Post-quantum PKI | Smallstep step-ca issues all certs; CA private key never touches the controller |

---

## Architecture

![OptimACS System Architecture](docs/images/architecture.png)

**ac-client** runs on each OpenWrt AP as a USP Agent with full **TP-469/USMP** compliance and **UCI backend** integration. On first boot it connects using a shared bootstrap certificate, sends a Boot! Notify, and waits for the controller to issue it a unique per-device mTLS certificate. Thereafter it runs a continuous loop: handling incoming GET/SET/ADD/DELETE/OPERATE messages, applying configuration changes via OpenWrt UCI, sending periodic ValueChange telemetry, and responding to firmware-upgrade and camera-capture operations.

**ac-server** is the Rust USP Controller with complete **TP-469/USMP** message support. It listens on `:3491` for incoming WebSocket connections and subscribes to EMQX for MQTT connections. It dispatches USP messages to the TR-181 data model, manages the device database with full UCI parameter storage, delegates X.509 certificate signing to step-ca, and maintains the USP command queue for reliable configuration delivery.

**Database (MariaDB/MySQL)** stores all device configurations including the complete UCI parameter set:
- WiFi radios with EHT (WiFi 7) modes, cell density, country codes
- Network interfaces with bridge ports, IPv6 prefixes, MAC addresses
- DHCP pools and static leases with hostnames
- System configuration: timezone, zonename, TTY login, log size
- LED configurations and service states

**step-ca** (Smallstep) is the PKI. It issues the server TLS cert, per-device client certs, and the init bootstrap cert. The CA private key never leaves the step-ca container — ac-server holds only an EC P-256 JWK provisioner key to sign one-time tokens (OTTs) used to authenticate CSR signing requests.

**optimacs-ui** is the FastAPI + Jinja2 management console with Strawberry GraphQL. Real-time subscriptions update the dashboard, AP list, USP event log, and TR-369 data model browser automatically.

**EMQX** provides the MQTT 5 broker for the MQTT Message Transfer Protocol (MTP). Agents and the controller exchange USP Records via MQTT topics:

```
usp/v1/{agent_endpoint_id}       ← agent subscribes (receives Controller messages)
usp/v1/{controller_endpoint_id}  ← controller subscribes (receives Agent messages)
```

### System Components

| Component | Role | Port(s) |
|-----------|------|---------|
| ac-server | USP Controller with TP-469/USMP support, UCI parameter storage | 3491 (WSS) |
| ac-client | USP Agent with UCI backend (47 operations) | *(outbound only)* |
| step-ca | PKI / Certificate Authority | 9000 (HTTPS) |
| optimacs-ui | Management web console (FastAPI + GraphQL) | 8080 |
| EMQX | MQTT broker (USP MQTT MTP) | 1883, 8883, 8083, 8084, 18083 |
| MariaDB / MySQL | Device and UCI configuration database | 3306 |
| Redis | Config-proto cache, rate-limit store (optional) | 6379 |

### Data Flow: Controller to Device Configuration

```
┌─────────────┐     USP/TR-369      ┌─────────────┐     UCI Commands     ┌─────────────┐
│   optimacs  │ ───────────────────>│   ac-server │ ───────────────────> │   ac-client │
│     UI      │    SET/ADD/DELETE   │  (Database) │   (MySQL → Agent)   │  (OpenWrt)  │
└─────────────┘                     └─────────────┘                     └─────────────┘
                                                                          │
                                                                          ▼
                                                                   ┌─────────────┐
                                                                   │  UCI Config │
                                                                   │  (/etc/config)│
                                                                   └─────────────┘
                                                                          │
                                                                          ▼
                                                                   ┌─────────────┐
                                                                   │   Services  │
                                                                   │dnsmasq/wifi/│
                                                                   │   network   │
                                                                   └─────────────┘
```

**Configuration Management Flow:**
1. **Admin** sets parameter via optimacs-ui (e.g., WiFi channel)
2. **ac-server** stores in MySQL and queues USP SET command
3. **ac-server** sends USP SET message to ac-client via WebSocket/MQTT
4. **ac-client** receives message, converts to UCI commands
5. **UCI backend** executes `uci set wireless.radio0.channel=36`
6. **Service restart** triggered: `wifi reload`
7. **ac-client** sends SET_RESP back to controller
8. **ac-server** marks command as acknowledged in database

---

## TR-369 / USP Protocol

> **Conformance**: ac-client implements **TR-369 USP 1.3** (Broadband Forum, November 2023). The implementation passes all mandatory requirements for the WebSocket and MQTT MTPs.

### Wire Format

USP uses Protocol Buffers (proto3). Two proto files are vendored in `proto/`:

| File | Purpose |
|------|---------|
| `proto/usp-record.proto` | USP Record envelope — version, `to_id`/`from_id` endpoint IDs, MTP connect records |
| `proto/usp-msg.proto` | USP Message body — GET/SET/OPERATE/NOTIFY and their responses |

### Message Types

| Message | Direction | Purpose |
|---------|-----------|---------|
| `GET` | Controller → Agent | Read TR-181 parameter values (respects `max_depth`) |
| `GET_RESP` | Agent → Controller | Parameter values |
| `SET` | Controller → Agent | Write TR-181 parameter values |
| `SET_RESP` | Agent → Controller | Acknowledgement with populated `updated_obj_results` |
| `OPERATE` | Controller → Agent | Execute a command |
| `OPERATE_RESP` | Agent → Controller | Command output args |
| `NOTIFY` (Boot!) | Agent → Controller | Device boot event; `obj_path="Device."`, includes `Cause` + `FirmwareUpdated` |
| `NOTIFY` (ValueChange) | Agent → Controller | Periodic telemetry (UpTime, LoadAvg, GPS, etc.) |
| `NOTIFY_RESP` | Controller → Agent | Acknowledge notify |
| `GET_SUPPORTED_PROTO` | Agent → Controller | Negotiate USP version; result stored and applied to Records |
| `Error 7004` | Agent → Controller | Returned for unsupported message types (NOT_SUPPORTED) |

### TR-369 Conformance Notes

| Requirement | Implementation |
|-------------|----------------|
| §10.2.1 WebSocket subprotocol | Server enforces and echoes `Sec-WebSocket-Protocol: v1.usp`; client verifies echo |
| §5.1 Record routing | Records with `to_id` ≠ own endpoint ID are logged and discarded |
| §6.2.1 Version negotiation | `GetSupportedProtoResp` version stored and used in subsequent Records |
| §9.3.6 Boot! event | `obj_path="Device."`, required `Cause` and `FirmwareUpdated` params included |
| §6.2.4 SET_RESP | `updated_obj_results` populated with one entry per updated object path |
| §6.1.2 GET max_depth | `max_depth` extracted and applied to DM path depth filtering |
| §6.4 Error codes | Error 7004 (`NOT_SUPPORTED`) returned for known-unsupported message types |

### Provisioning Flow

```
Agent (new device)                    Controller (ac-server)
    │                                       │
    │── WebSocketConnectRecord ─────────────▶│
    │── Notify { Boot!, DeviceInfo.* } ─────▶│  → new_systems table
    │                                       │  (admin approves in UI)
    │◀─ OPERATE IssueCert() ────────────────│
    │── OPERATE_RESP { csr: "..." } ─────────▶│  → sign cert via step-ca
    │◀─ SET Security.{CaCert,Cert,Key} ─────│
    │   apply::save_certs()                 │
    │── [reconnect with device cert] ────────▶│  → provisioned
    │                                       │
    │── Notify { ValueChange, UpTime=... } ──▶│  periodic telemetry
```

### TR-181 Data Model

The TR-181 Device:2 data model exposed by ac-client, with full UCI backend integration:

#### Device Information
| TR-181 Path | RW | Source | Description |
|-------------|:--:|--------|-------------|
| `Device.DeviceInfo.HostName` | RW | UCI system | Device hostname |
| `Device.DeviceInfo.SoftwareVersion` | RO | `/etc/openwrt_release` | OpenWrt version |
| `Device.DeviceInfo.HardwareVersion` | RO | arch string | Hardware architecture |
| `Device.DeviceInfo.SerialNumber` | RO | MAC address | Device serial |
| `Device.DeviceInfo.UpTime` | RO | `/proc/uptime` | System uptime |
| `Device.DeviceInfo.X_OptimACS_LoadAvg` | RO | `/proc/loadavg` | System load |
| `Device.DeviceInfo.X_OptimACS_FreeMem` | RO | `/proc/meminfo` | Free memory |
| `Device.DeviceInfo.X_OptimACS_Latitude` | RO | GNSS reader | GPS latitude |
| `Device.DeviceInfo.X_OptimACS_Longitude` | RO | GNSS reader | GPS longitude |
| `Device.DeviceInfo.X_OptimACS_Timezone` | RW | UCI system | Timezone (e.g., "GMT0") |
| `Device.DeviceInfo.X_OptimACS_ZoneName` | RW | UCI system | Timezone name (e.g., "UTC", "Europe/London") |
| `Device.DeviceInfo.X_OptimACS_TTYLogin` | RW | UCI system | TTY login enabled (0/1) |
| `Device.DeviceInfo.X_OptimACS_LogSize` | RW | UCI system | Log buffer size in KB |
| `Device.DeviceInfo.X_OptimACS_CompatVersion` | RO | UCI system | OpenWrt compatibility version |

#### WiFi Configuration (Device.WiFi)
| TR-181 Path | RW | UCI Section | Description |
|-------------|:--:|-------------|-------------|
| `Device.WiFi.Radio.{i}.Channel` | RW | wireless.radio{i} | Channel number or "auto" |
| `Device.WiFi.Radio.{i}.OperatingFrequencyBand` | RW | wireless.radio{i} | 2.4GHz, 5GHz, 6GHz |
| `Device.WiFi.Radio.{i}.OperatingChannelBandwidth` | RW | wireless.radio{i} | HT20, HT40, VHT80, EHT20, EHT80, EHT160, EHT320 |
| `Device.WiFi.Radio.{i}.X_OptimACS_CellDensity` | RW | wireless.radio{i} | WiFi 7 cell density (-1, 0, 1, 2, 3) |
| `Device.WiFi.Radio.{i}.X_OptimACS_Country` | RW | wireless.radio{i} | Regulatory country code |
| `Device.WiFi.Radio.{i}.Enable` | RW | wireless.radio{i} | Radio enabled |
| `Device.WiFi.SSID.{i}.SSID` | RW | wireless.{iface} | Network name |
| `Device.WiFi.SSID.{i}.Enable` | RW | wireless.{iface} | SSID enabled |
| `Device.WiFi.AccessPoint.{i}.Security.KeyPassphrase` | RW | wireless.{iface} | WiFi password |
| `Device.WiFi.AccessPoint.{i}.Security.ModeEnabled` | RW | wireless.{iface} | none, wep, psk, psk2, owe, etc. |
| `Device.WiFi.AccessPoint.{i}.X_OptimACS_OCV` | RW | wireless.{iface} | Operating Channel Validation (0/1) |

#### Network Configuration (Device.IP)
| TR-181 Path | RW | UCI Section | Description |
|-------------|:--:|-------------|-------------|
| `Device.IP.Interface.{i}.IPv4Address.{j}.IPAddress` | RW | network.{iface} | IPv4 address (CIDR notation) |
| `Device.IP.Interface.{i}.IPv4Address.{j}.SubnetMask` | RW | network.{iface} | Subnet mask |
| `Device.IP.Interface.{i}.IPv4Address.{j}.AddressingType` | RW | network.{iface} | static, dhcp, dhcpv6 |
| `Device.IP.Interface.{i}.DNSServers` | RW | network.{iface} | List of DNS servers |
| `Device.IP.Interface.{i}.IPv6Prefix` | RW | network.{iface} | IPv6 ULA prefix |
| `Device.IP.Interface.{i}.X_OptimACS_BridgePorts` | RW | network.{device} | Bridge member ports (list) |
| `Device.IP.Interface.{i}.X_OptimACS_MACAddress` | RW | network.{device} | MAC address override |

#### DHCP Configuration (Device.DHCPv4)
| TR-181 Path | RW | UCI Section | Description |
|-------------|:--:|-------------|-------------|
| `Device.DHCPv4.Server.Pool.{i}.MinAddress` | RW | dhcp.{iface} | Pool start IP |
| `Device.DHCPv4.Server.Pool.{i}.MaxAddress` | RW | dhcp.{iface} | Pool end IP |
| `Device.DHCPv4.Server.Pool.{i}.LeaseTime` | RW | dhcp.{iface} | Lease duration (e.g., "12h") |
| `Device.DHCPv4.Server.Pool.{i}.StaticAddress.{j}.Chaddr` | RW | dhcp.@host | MAC address |
| `Device.DHCPv4.Server.Pool.{i}.StaticAddress.{j}.Yiaddr` | RW | dhcp.@host | Reserved IP |
| `Device.DHCPv4.Server.Pool.{i}.StaticAddress.{j}.X_OptimACS_Hostname` | RW | dhcp.@host | Hostname |

#### Hosts Configuration
| TR-181 Path | RW | UCI Section | Description |
|-------------|:--:|-------------|-------------|
| `Device.Hosts.Host.{i}.HostName` | RW | hosts | Static hostname |
| `Device.Hosts.Host.{i}.IPAddress` | RW | hosts | Static IP address |

#### Vendor Extensions
| TR-181 Path | RW | Source | Description |
|-------------|:--:|--------|-------------|
| `Device.X_OptimACS_Camera.{i}.*` | RO | Axis CGI | IP camera discovery and configuration |
| `Device.X_OptimACS_Camera.{i}.Capture()` | OP | - | JPEG capture + upload |
| `Device.X_OptimACS_Firmware.AvailableVersion` | RO | server | Available firmware version |
| `Device.X_OptimACS_Firmware.Download()` | OP | - | Firmware upgrade via sysupgrade |
| `Device.X_OptimACS_Security.IssueCert()` | OP | - | PKI certificate issuance |
| `Device.X_OptimACS_LED.{i}.Name` | RW | system.led | LED name |
| `Device.X_OptimACS_LED.{i}.Sysfs` | RW | system.led | LED sysfs path |
| `Device.X_OptimACS_LED.{i}.Trigger` | RW | system.led | LED trigger type |

---

## TP-469/USMP Implementation Details

### Error Codes

ac-client implements all TR-369 §6.4 error codes (7000-7999):

| Code | Name | Description |
|------|------|-------------|
| 7000 | `Success` | Operation completed successfully |
| 7001 | `Failure` | General failure |
| 7002 | `InternalError` | Internal agent error |
| 7003 | `InvalidArgument` | Invalid input argument |
| 7004 | `ResourcesExceeded` | Resource limit reached |
| 7005 | `PermissionDenied` | Permission denied |
| 7006 | `InvalidConfiguration` | Invalid configuration |
| 7007 | `InvalidPathSyntax` | Invalid path syntax |
| 7008 | `ParameterActionFailure` | Parameter action failed |
| 7020 | `ObjectNotFound` | Object instance not found |
| 7021 | `ObjectNotCreatable` | Object cannot be created |
| 7022 | `ObjectNotDeletable` | Object cannot be deleted |
| 7023 | `DuplicateUniqueKey` | Duplicate unique key |
| 7024 | `InvalidPath` | Invalid object path |
| 7025 | `InvalidWildcard` | Invalid wildcard in path |
| 7026 | `OperationInProgress` | Operation already in progress |
| 7027 | `InvalidInstance` | Invalid instance identifier |

### UCI Backend Integration

All configuration changes flow through the UCI backend:

1. **Controller sends** USP SET/ADD/DELETE message
2. **ac-client receives** and parses the message
3. **UCI backend** converts USP paths to UCI commands
4. **OpenWrt UCI** applies configuration changes
5. **Service restart** triggered automatically (dnsmasq, wifi, network)
6. **Response sent** back to controller with result codes

**Example: Adding a WiFi Network**
```rust
// Controller sends USP ADD
ADD Device.WiFi.SSID.{instance}
  - SSID = "MyNetwork"
  - Security.ModeEnabled = "psk2+ccmp"
  - Security.KeyPassphrase = "secretpassword"

// ac-client UCI backend executes:
uci add wireless wifi-iface
uci set wireless.@wifi-iface[-1].ssid="MyNetwork"
uci set wireless.@wifi-iface[-1].encryption="psk2+ccmp"
uci set wireless.@wifi-iface[-1].key="secretpassword"
uci commit wireless
wifi reload
```

See [UCI_BACKEND_OPERATIONS.md](UCI_BACKEND_OPERATIONS.md) for complete API documentation.

---

## Security Architecture

### Transport Security

- **TLS 1.3** with mutual authentication on all connections to ac-server
- **Post-quantum hybrid key exchange**: X25519 + ML-KEM-768 (NIST FIPS 203, ML-KEM). Deployed in every ac-client binary — device traffic is safe against harvest-now/decrypt-later attacks
- **Mutual TLS**: both client and server present X.509 certificates; the server rejects any connection without a valid client certificate signed by the trusted CA
- **No hostname verification on client cert**: ac-client uses a custom `AcpServerVerifier` that validates the full certificate chain but matches the server by CA trust rather than CN — consistent with how OpenSSL `SSL_VERIFY_PEER` worked in the original C client

### Certificate Lifecycle

```
Bootstrap (every new device):
  ac-client ships with a shared init certificate (00:00:00:00:00:00)
  This cert allows it to connect and register — but nothing else.

Provisioning (one-time, admin-triggered):
  1. AP connects → sends Boot! Notify with DeviceInfo parameters
  2. Appears in controller's New Systems queue
  3. Admin approves in the OptimACS UI
  4. Controller sends  OPERATE Device.X_OptimACS_Security.IssueCert()
  5. Agent generates an RSA key pair + CSR; returns CSR in OPERATE_RESP
  6. Controller signs a JWT one-time token (OTT) with its EC P-256 JWK provisioner key
  7. Controller forwards CSR + OTT to step-ca  (POST /1.0/sign)
     step-ca verifies OTT, issues a unique per-device certificate
  8. Controller sends  SET {CaCert, Cert, Key}  to the agent
  9. Agent writes certs to /etc/apclient/certs/ and reconnects

Post-provisioning:
  Every connection uses the device's unique mTLS cert.
  The init cert is no longer accepted for this device's endpoint ID.

Revocation:
  Removing a device from the UI prevents future connections.
  The cert is not added to a CRL — access is controlled at the
  application layer by endpoint ID lookup in the database.
```

### Why step-ca?

The CA private key **never touches ac-server**. ac-server holds only the EC P-256 JWK provisioner key — a narrow credential that can only sign one-time tokens used to authenticate CSR requests. This means:

- A compromised ac-server cannot forge device certificates
- The CA can be rotated independently of the controller
- step-ca's audit log provides a full record of every certificate issued
- In Kubernetes deployments, the step-ca pod can be isolated in its own namespace with network policies that allow only ac-server to reach the signing API

### Security Posture Summary

| Property | Value |
|----------|-------|
| TLS version | 1.3 (minimum enforced by rustls) |
| Key exchange | X25519 + ML-KEM-768 (post-quantum hybrid) |
| Client authentication | Mutual TLS — X.509 cert signed by step-ca root CA |
| CA key isolation | step-ca holds root key; ac-server holds only JWK provisioner key |
| Certificate issuance | OTT-authenticated CSR signing via step-ca REST API |
| Binary memory safety | Rust — no buffer overflows, no use-after-free |
| Credential storage | Certs written to `/etc/apclient/certs/` (mode 0600) |

---

## Features

- **TR-369 / USP 1.3** conformant Agent (Boot! Notify, GET, SET, OPERATE)
- **Full TP-469/USMP compliance** — ADD, DELETE, GetSupportedDM, GetInstances handlers
- **WebSocket MTP** and **MQTT MTP** — configurable, or both simultaneously
- **Mutual TLS** with post-quantum hybrid key exchange (X25519 + ML-KEM-768) via `rustls-post-quantum`
- **UCI-backed TR-181 data model** — Complete OpenWrt configuration support
- **47 UCI backend operations** — WiFi, Network, DHCP, System, LED management
- **WiFi 7 (EHT) support** — EHT20, EHT80, EHT160, EHT320 channel bandwidth modes
- **IPv6 support** — ULA prefix management and address configuration
- **Bridge & VLAN management** — List-type parameters for bridge ports and DNS
- **System configuration** — Timezone, zonename, hostname, log_size, TTY login
- **Vendor extensions**: `Device.X_OptimACS_Camera.*`, `Device.X_OptimACS_Firmware.*`, `Device.X_OptimACS_Security.*`, `Device.X_OptimACS_LED.*`
- **Two-phase provisioning**: bootstrap cert → controller-issued mTLS cert lifecycle
- **Firmware upgrade** via sysupgrade
- **Axis IP-camera discovery** (ARP scan + CGI API) and JPEG upload
- **GNSS telemetry** (NMEA serial reader)
- **ValueChange** periodic telemetry (uptime, load, GPS, wireless, modem)
- **OpenWrt package feed** entry (`package/ac-client/`) for cross-compilation via `rust-package.mk`
- **Production ready** — 20+ unit tests, error handling, rollback support

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
│       ├── agent.rs       — main USP agent loop, message dispatch
│       ├── record.rs      — encode/decode USP Records
│       ├── message.rs     — builder helpers (Boot!, ValueChange, etc.)
│       ├── endpoint.rs    — EndpointId from MAC
│       ├── session.rs     — sequence_id counter
│       ├── mtp/           — Message Transfer Protocols
│       │   ├── websocket.rs   — WSS client with reconnect loop
│       │   └── mqtt.rs        — rumqttc MQTT client
│       ├── dm/            — TR-181 data model (UCI-backed)
│       │   ├── mod.rs         — DmCtx, get_params(), set_params(), operate()
│       │   ├── device_info.rs — Device.DeviceInfo.*
│       │   ├── wifi.rs        — Device.WiFi.* via UCI
│       │   ├── ip.rs          — Device.IP.Interface.* via UCI
│       │   ├── dhcp.rs        — Device.DHCPv4.* via UCI
│       │   ├── hosts.rs       — Device.Hosts.Host.* via UCI
│       │   ├── cameras.rs     — Device.X_OptimACS_Camera.*
│       │   ├── firmware.rs    — Device.X_OptimACS_Firmware.*
│       │   └── security.rs    — Device.X_OptimACS_Security.*
│       └── tp469/         — TP-469/USMP implementation
│           ├── mod.rs         — Module exports and integration
│           ├── error_codes.rs — 30+ TR-369 error codes (7000-7999)
│           ├── supported_dm_schema.rs — Data model schema definitions
│           ├── add_delete.rs    — ADD/DELETE message handlers
│           ├── get_supported_dm.rs — GetSupportedDM handler
│           ├── get_instances.rs — GetInstances handler
│           ├── search.rs        — Wildcard and expression matching
│           ├── subscriptions.rs — Event subscription management
│           ├── notifications.rs — Notification system
│           ├── uci_backend.rs   — 47 UCI operations (1,600+ lines)
│           └── tests.rs         — Unit test suite (20 tests)
├── proto/                 — vendored Protocol Buffer schemas
│   ├── acp.proto          — OptimACS control protocol
│   ├── usp-record.proto   — TR-369 USP Record wire format
│   └── usp-msg.proto      — TR-369 USP Message types
├── build.rs               — prost-build codegen for proto files
├── Cargo.toml
├── Cargo.lock
├── package/
│   └── ac-client/         — OpenWrt package feed entry
│       ├── Makefile       — OpenWrt package definition (rust-package.mk)
│       └── files/
│           ├── ac-client.init    — procd init script
│           ├── ac_client.conf    — default configuration
│           └── init/             — bootstrap certificates
├── docs/                  — documentation and images
│   └── images/
└── Documentation files:
    ├── README.md                     — This file (overview and quick start)
    ├── TP469_IMPLEMENTATION_REPORT.md — Technical implementation details
    ├── UCI_BACKEND_OPERATIONS.md      — Complete UCI API reference
    ├── UCI_BACKEND_COMPLETION_REPORT.md — UCI backend completion status
    ├── UCI_SCHEMA_COMPLIANCE_VERIFICATION.md — Schema verification against backup
    ├── SYSTEM_NETWORK_UCI_COMPLIANCE_REPORT.md — System/Network compliance
    ├── TP469_COMPLIANCE_TEST_REPORT.md — Test results (20/20 passed)
    └── COMPLIANCE_REPORT.md — obuspa vs ac-client comparison
```
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

## Testing

ac-client includes a comprehensive test suite for the TP-469 and UCI backend implementations:

```bash
# Run all tests
cargo test

# Run with output
cargo test -- --nocapture

# Run specific test
cargo test test_uci_result_success

# Run TP-469 tests only
cargo test tp469
```

### Test Suite Overview

The test suite includes **20+ unit tests** covering:

**TP-469 Core Tests:**
- `test_error_code_values` — Verify all TR-369 error codes (7000-7999)
- `test_error_code_descriptions` — Error message descriptions
- `test_schema_build` — Data model schema construction
- `test_find_object_schema` — Object schema lookup
- `test_find_parameter_schema` — Parameter schema lookup
- `test_base_path_extraction` — Path parsing for ADD/DELETE
- `test_instance_extraction` — Instance number extraction
- `test_add_result_creation` — ADD operation result handling
- `test_delete_result_creation` — DELETE operation result handling
- `test_path_validation` — USP path validation

**UCI Backend Tests:**
- `test_uci_result_success` — Successful UCI operation results
- `test_uci_result_error` — Error result handling
- `test_wildcard_matching_single` — Single-level wildcards
- `test_wildcard_matching_multi` — Multi-level wildcards

**Integration Tests:**
- `test_add_dhcp_lease_integration` — DHCP lease creation (requires UCI environment)
- `test_add_wifi_interface_integration` — WiFi interface creation (requires UCI environment)
- `test_delete_dhcp_lease_integration` — DHCP lease deletion (requires UCI environment)

### Continuous Integration

```bash
# Check code formatting
cargo fmt --check

# Run linting
cargo clippy -- -D warnings

# Build release
cargo build --release

# Run tests
cargo test
```

All tests pass with **0 errors** and the release build is optimized for OpenWrt deployment.

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

### Default bootstrap certificates (out-of-box)

The OpenWrt package ships with a set of **default bootstrap certificates** in `package/ac-client/files/init/`. These are installed to `/etc/apclient/init/` automatically during `opkg install`, allowing `ac-client` to start and attempt provisioning immediately after flashing — no manual cert deployment required for initial bring-up.

| File | CN | Purpose |
|------|----|---------|
| `ca.crt` | `OptimACS Default Bootstrap CA` | Verifies the server certificate chain |
| `client.crt` | `00:00:00:00:00:00` | Default init identity presented during INIT handshake |
| `client.key` | — | Private key for `client.crt` |

> **Security notice:** These certificates are **public** — the key material is included in the open-source repository and must be considered known to any third party. They are suitable for development, lab bring-up, and initial provisioning only. Replace them with certificates from your own step-ca before connecting devices to a production controller.

### Production certificate deployment

For production, overwrite the default files with certificates issued by your OptimACS server's step-ca instance:

```sh
# Copy from the server's peer directory for the default init CN
scp <server>:/var/ac-server/peers/00:00:00:00:00:00/client.crt \
    root@<ap>:/etc/apclient/init/client.crt
scp <server>:/var/ac-server/peers/00:00:00:00:00:00/client.key \
    root@<ap>:/etc/apclient/init/client.key
scp <server>:/etc/optimacs/CA/rootCA.crt \
    root@<ap>:/etc/apclient/init/ca.crt

/etc/init.d/ac-client enable
/etc/init.d/ac-client start
```

> Because these three files are listed as `conffiles` in the package, `opkg upgrade` will never overwrite operator-deployed certificates.

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
| `server_host` | `acs.optimcloud.com` | ac-server hostname or IP |
| `server_cn` | `acs.optimcloud.com` | Expected CN in the server TLS certificate (SNI) |
| `mtp` | `websocket` | MTP selection: `websocket` \| `mqtt` \| `both` |
| `ws_url` | `wss://acs.optimcloud.com:3491/usp` | WebSocket MTP URL |
| `mqtt_url` | `mqtt://acs.optimcloud.com:1883` | MQTT broker URL |
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
  → Firmware upgrade via OPERATE Device.X_OptimACS_Firmware.Download()
  → Camera cycle every cam_interval seconds
```

### USP Endpoint ID

Auto-generated from the device MAC address:

```
oui:{vendor-oui}:{mac-address-without-colons}
# e.g. oui:0060B3:aabbccddeeff
```

---

## License

Copyright (c) 2026 Optim Enterprises BV. Released under the [BSD 3-Clause License](LICENSE).
