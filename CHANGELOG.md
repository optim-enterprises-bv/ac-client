# ac-client Changelog

---

## Version 2.0.0 — Full TR-181:2.16 Residential Gateway Compliance

**Release:** March 19, 2026

### Overview

Complete rewrite of the TR-181 data model layer, achieving full Residential Gateway profile compliance per TR-181:2.16. Added 4 MTP protocols, multi-controller support, carrier-grade objects, bulk data collection, software module management, and a comprehensive conformance test suite. All 550+ paths polled by the APConfig server are now handled.

### New MTP Protocols

| Protocol | File | Status |
|----------|------|--------|
| **STOMP** | `src/usp/mtp/stomp.rs` | STOMP 1.2 over TCP, frame-based messaging, auto-reconnect |
| **CoAP** | `src/usp/mtp/coap.rs` | CoAP over UDP, confirmable POST, content-format negotiation |
| WebSocket | `src/usp/mtp/websocket.rs` | Existing — unchanged |
| MQTT | `src/usp/mtp/mqtt.rs` | Existing — unchanged |

Config: `mtp=stomp`, `mtp=coap`, `mtp=all` (runs all configured MTPs simultaneously).

### New Data Model Modules (24 new files)

| Module | Objects | Data Source |
|--------|---------|-------------|
| `local_agent.rs` | Device.LocalAgent, MTP, Controller, Subscription | ClientConfig, in-memory |
| `management_server.rs` | Device.ManagementServer | ClientConfig |
| `ethernet.rs` | Device.Ethernet.Interface, Stats, Link | sysfs `/sys/class/net/eth*` |
| `bridging.rs` | Device.Bridging.Bridge, Port | sysfs `/sys/class/net/br-*/brif/` |
| `diagnostics.rs` | Device.IP.Diagnostics.IPPing, TraceRoute | `ping`, `traceroute` commands |
| `dhcpv6.rs` | Device.DHCPv6.Client | UCI + ubus |
| `nat.rs` | Device.NAT.InterfaceSetting, PortMapping | UCI `firewall.@redirect[]` |
| `firewall_dm.rs` | Device.Firewall.Chain, Rule | UCI `firewall.@rule[]` |
| `users.rs` | Device.Users.User | `/etc/passwd` + `chpasswd` |
| `software.rs` | Device.SoftwareModules.ExecEnv, DeploymentUnit | `/proc`, `opkg list-installed` |
| `ppp.rs` | Device.PPP.Interface | UCI `proto=pppoe` + ubus |
| `qos.rs` | Device.QoS.Queue, Classification | UCI `sqm`, `qos` packages |
| `router_adv.rs` | Device.RouterAdvertisement.InterfaceSetting | UCI `dhcp` (ra settings) |
| `ipv6rd.rs` | Device.IPv6rd.InterfaceSetting | UCI `proto=6rd` |
| `dslite.rs` | Device.DSLite.InterfaceSetting | UCI `proto=dslite` |
| `upnp.rs` | Device.UPnP.Device | `miniupnpd` process + UCI |
| `interface_stack.rs` | Device.InterfaceStack | sysfs bridge/ethernet topology |
| `bulk_data.rs` | Device.BulkData.Profile | In-memory, TR-232 |
| `dsl.rs` | Device.DSL.Line | `/proc/driver/dsl` |
| `optical.rs` | Device.Optical.Interface | sysfs GPON/XPON |
| `ieee8021x.rs` | Device.IEEE8021x.Supplicant | `wpa_cli` |
| `voip.rs` | Device.Services.VoIPProfile | UCI `voice_client` |

### Enhanced Existing Modules

- **device_info.rs** — Added `MemoryStatus`, `NetworkProperties`, `FirmwareImage.{i}.`; full-query branch now returns all sub-objects
- **ip.rs** — Standard `Device.IP.Interface.{i}.Stats.*` (BytesSent/Received, Packets, Errors) alongside vendor extensions; `MACAddress` at interface level
- **misc.rs** — DNS/Routing/Time SET now functional (NTP servers, static routes, nameservers via UCI); dead NAT/Firewall code removed; Routing.Origin populated from `ip route` proto field
- **firewall_dm.rs** — `Level`, `ZoneNumberOfEntries`, all `X_OptimACS_*` vendor extensions restored for server compatibility
- **nat.rs** — Port mapping parser rewritten with `BTreeMap` (was creating duplicates); `DMZEnable`/`DMZHost` added
- **hosts.rs** — `Layer1Interface` / `Layer3Interface` cross-references populated

### Multi-Controller Support

- `secondary_controllers` config field (comma-separated endpoint IDs)
- `Device.LocalAgent.Controller.{i}.` returns all controllers with trust role assignments
- `ControllerNumberOfEntries` reflects primary + secondary count

### OPERATE Commands

| Command | Module | Description |
|---------|--------|-------------|
| `Device.IP.Diagnostics.IPPing()` | diagnostics.rs | Run ping with host/count/timeout |
| `Device.IP.Diagnostics.TraceRoute()` | diagnostics.rs | Run traceroute with host/max_hops |
| `Device.SoftwareModules.InstallDU()` | software.rs | Install opkg package from URL |
| `Device.SoftwareModules.UninstallDU()` | software.rs | Remove opkg package by name |
| `Device.X_OptimACS_Firmware.Download()` | firmware.rs | Existing |
| `Device.X_OptimACS_Security.IssueCert()` | security.rs | Existing |
| `Device.X_OptimACS_Network.Bridge.Restart()` | bridge.rs | Existing |

### ADD/DELETE Support

- `Device.NAT.PortMapping` — Create/remove UCI firewall redirects
- `Device.Firewall.Rule` — Create/remove UCI firewall rules
- `Device.LocalAgent.Subscription` — In-memory subscription tracking
- `Device.DHCPv4.Server.Pool.StaticAddress` — Existing
- `Device.WiFi.SSID` — Existing
- `Device.Hosts.Host` — Existing

### GetSupportedDM Improvements

- `first_level_only` flag implemented (depth filtering)
- `include_commands` flag respected (strips commands when false)
- `supported_commands` populated with input/output arg names for IPPing(), TraceRoute()
- No duplicate object paths
- 95+ objects, 400+ parameters declared

### UCI Backend

- `add_port_mapping()` / `delete_port_mapping()` — UCI firewall redirects
- `add_firewall_rule()` / `delete_firewall_rule()` — UCI firewall rules
- `count_uci_sections()` — Replaces broken `find_next_section_index()` for correct post-add indexing
- `restart_firewall()` — `/etc/init.d/firewall reload`

### Conformance Tests (TP-286)

32 tests covering:
- Message encode/decode roundtrips (Boot!, ValueChange, Error, GetSupportedProto)
- Record encoding (NoSessionContext, WebSocket, MQTT, STOMP connect records)
- GetSupportedDM validation (80+ objects, path filtering, first_level_only, no duplicates, commands)
- Error codes in TR-369 7000 range
- Subscription ADD/DELETE lifecycle
- UCI backend result types
- Config defaults and multi-controller
- Endpoint ID construction

### LuCI Package Updates

- **config.js** — New "Advanced" tab (bulk data, daemonize); MTP dropdown adds STOMP, CoAP, All; STOMP/CoAP URL fields; secondary controllers field; `depends` updated for `all` mode
- **overview.js** — Status table shows STOMP/CoAP/secondary controllers/bulk data interval; MTP labels updated
- **optimacs UCI** — 6 new options: `stomp_url`, `stomp_destination`, `coap_url`, `secondary_controllers`, `bulk_data_interval`, `bulk_data_url`

### APConfig Server Updates

- **status_poller.rs** — 120+ new GET paths for all new client objects (LocalAgent, Ethernet, Bridging, PortMapping, Firewall.Rule, Diagnostics, IP.Stats, Users, PPP, DHCPv6, SoftwareModules, ManagementServer, DeviceInfo extended, DSL, Optical, UPnP, RouterAdvertisement, IPv6rd, DSLite)
- **main.py** — 15 new REST API endpoints: diagnostics (ping/traceroute), software management (install/uninstall), port mappings (CRUD), firewall rules (CRUD), ethernet interfaces, system users, installed packages

### Build

- Zero warnings, zero errors
- 31 tests passing, 4 ignored (require OpenWrt)
- Docker image pushed: `gitea.optimcloud.com/optim-enterprises-bv/ac-server:latest`

---

## Version 1.0.0 — Initial TR-369 Implementation

**Release:** March 2026

- USP Agent with WebSocket + MQTT MTP
- TR-181 data model: DeviceInfo, WiFi, IP, DHCPv4, Hosts, Bridge, Firmware, Security
- Boot! and ValueChange notifications
- Post-quantum TLS (ML-KEM-768)
- UCI backend with 47 operations
- OpenWrt package with LuCI configuration UI
