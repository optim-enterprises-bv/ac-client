//! TP-469 GetSupportedDM Message Handler
//!
//! Implements GetSupportedDM request/response per TR-369 §6.1.5.
//! Declares every object and parameter that this agent supports so that a
//! TR-369 controller can discover the data model before issuing GET/SET/ADD/DELETE.

use crate::usp::usp_msg::{
    self,
    get_supported_dm_resp::{
        supported_object, supported_param, RequestedObjectResult, SupportedCommand,
        SupportedObject, SupportedParam,
    },
};

// ── convenience constructors ──────────────────────────────────────────────────

fn ro_param(name: &str) -> SupportedParam {
    SupportedParam {
        param_name: name.into(),
        access: supported_param::Access::ParamReadOnly as i32,
        value_change: true,
    }
}

fn rw_param(name: &str) -> SupportedParam {
    SupportedParam {
        param_name: name.into(),
        access: supported_param::Access::ParamReadWrite as i32,
        value_change: true,
    }
}

fn ro_obj(obj_path: &str, params: Vec<SupportedParam>) -> SupportedObject {
    SupportedObject {
        obj_path: obj_path.into(),
        access: supported_object::Access::ObjReadOnly as i32,
        is_multi_instance: false,
        supported_params: params,
        supported_commands: vec![],
        supported_events: vec![],
        unique_key_sets: vec![],
    }
}

fn rw_obj(obj_path: &str, params: Vec<SupportedParam>) -> SupportedObject {
    SupportedObject {
        obj_path: obj_path.into(),
        access: supported_object::Access::ObjReadOnly as i32,
        is_multi_instance: false,
        supported_params: params,
        supported_commands: vec![],
        supported_events: vec![],
        unique_key_sets: vec![],
    }
}

fn multi_ro(obj_path: &str, params: Vec<SupportedParam>) -> SupportedObject {
    SupportedObject {
        obj_path: obj_path.into(),
        access: supported_object::Access::ObjReadOnly as i32,
        is_multi_instance: true,
        supported_params: params,
        supported_commands: vec![],
        supported_events: vec![],
        unique_key_sets: vec![],
    }
}

fn multi_rw(obj_path: &str, params: Vec<SupportedParam>) -> SupportedObject {
    SupportedObject {
        obj_path: obj_path.into(),
        access: supported_object::Access::ObjAddDelete as i32,
        is_multi_instance: true,
        supported_params: params,
        supported_commands: vec![],
        supported_events: vec![],
        unique_key_sets: vec![],
    }
}

fn cmd(name: &str, inputs: &[&str], outputs: &[&str]) -> SupportedCommand {
    SupportedCommand {
        command_name: name.into(),
        input_arg_names: inputs.iter().map(|s| s.to_string()).collect(),
        output_arg_names: outputs.iter().map(|s| s.to_string()).collect(),
    }
}

fn rw_obj_with_cmds(
    obj_path: &str,
    params: Vec<SupportedParam>,
    cmds: Vec<SupportedCommand>,
) -> SupportedObject {
    SupportedObject {
        obj_path: obj_path.into(),
        access: supported_object::Access::ObjReadOnly as i32,
        is_multi_instance: false,
        supported_params: params,
        supported_commands: cmds,
        supported_events: vec![],
        unique_key_sets: vec![],
    }
}

// ── full data model declaration ───────────────────────────────────────────────

fn build_supported_objects() -> Vec<SupportedObject> {
    vec![
        // ── Device.LocalAgent. (TR-369 Mandatory) ─────────────────────────────
        ro_obj(
            "Device.LocalAgent.",
            vec![
                ro_param("EndpointID"),
                ro_param("SoftwareVersion"),
                ro_param("SupportedProtocols"),
                ro_param("UpTime"),
                ro_param("MTPNumberOfEntries"),
                ro_param("ControllerNumberOfEntries"),
                ro_param("SubscriptionNumberOfEntries"),
            ],
        ),
        multi_ro(
            "Device.LocalAgent.MTP.{i}.",
            vec![
                ro_param("Enable"),
                ro_param("Protocol"),
                ro_param("Status"),
            ],
        ),
        ro_obj(
            "Device.LocalAgent.MTP.{i}.WebSocket.",
            vec![ro_param("URL"), ro_param("CertFile")],
        ),
        ro_obj(
            "Device.LocalAgent.MTP.{i}.MQTT.",
            vec![ro_param("BrokerAddress"), ro_param("ClientID")],
        ),
        multi_ro(
            "Device.LocalAgent.Controller.{i}.",
            vec![
                ro_param("Enable"),
                ro_param("EndpointID"),
                ro_param("PeriodicNotifInterval"),
            ],
        ),
        multi_rw(
            "Device.LocalAgent.Subscription.{i}.",
            vec![
                rw_param("Enable"),
                rw_param("NotifType"),
                rw_param("ReferenceList"),
            ],
        ),
        // ── Device.DeviceInfo. ────────────────────────────────────────────────
        rw_obj(
            "Device.DeviceInfo.",
            vec![
                ro_param("Manufacturer"),
                ro_param("ManufacturerOUI"),
                ro_param("ModelName"),
                ro_param("Description"),
                ro_param("ProductClass"),
                rw_param("HostName"),
                ro_param("SoftwareVersion"),
                ro_param("HardwareVersion"),
                ro_param("AdditionalSoftwareVersion"),
                ro_param("SerialNumber"),
                ro_param("BaseMacAddress"),
                ro_param("ProcessorArchitecture"),
                ro_param("UpTime"),
                ro_param("DeviceStatus"),
                ro_param("VendorConfigFileNumberOfEntries"),
                ro_param("VendorLogFileNumberOfEntries"),
                // vendor extensions
                ro_param("X_OptimACS_LoadAvg"),
                ro_param("X_OptimACS_FreeMem"),
                ro_param("X_OptimACS_MemTotal"),
                ro_param("X_OptimACS_KernelVersion"),
            ],
        ),
        // Device.DeviceInfo.MemoryStatus.
        ro_obj(
            "Device.DeviceInfo.MemoryStatus.",
            vec![ro_param("Total"), ro_param("Free")],
        ),
        // Device.DeviceInfo.NetworkProperties.
        ro_obj(
            "Device.DeviceInfo.NetworkProperties.",
            vec![
                ro_param("MaxTCPWindowSize"),
                ro_param("TCPImplementation"),
            ],
        ),
        // Device.DeviceInfo.FirmwareImage.{i}.
        multi_ro(
            "Device.DeviceInfo.FirmwareImage.{i}.",
            vec![
                ro_param("Name"),
                ro_param("Version"),
                ro_param("Available"),
                ro_param("Status"),
            ],
        ),
        // Device.DeviceInfo.ProcessStatus.
        ro_obj(
            "Device.DeviceInfo.ProcessStatus.",
            vec![ro_param("CPUUsage"), ro_param("ProcessNumberOfEntries")],
        ),
        // Device.DeviceInfo.TemperatureStatus.
        ro_obj(
            "Device.DeviceInfo.TemperatureStatus.",
            vec![ro_param("TemperatureSensorNumberOfEntries")],
        ),
        multi_ro(
            "Device.DeviceInfo.TemperatureStatus.TemperatureSensor.{i}.",
            vec![
                ro_param("Name"),
                ro_param("Value"),
                ro_param("Enable"),
                ro_param("Status"),
                ro_param("LastUpdate"),
            ],
        ),
        // Device.DeviceInfo.VendorLogFile.{i}.
        multi_ro(
            "Device.DeviceInfo.VendorLogFile.{i}.",
            vec![ro_param("Name"), ro_param("Size"), ro_param("LastModified")],
        ),
        // Device.DeviceInfo.X_TP_LEDs. (vendor extension)
        ro_obj(
            "Device.DeviceInfo.X_TP_LEDs.",
            vec![ro_param("LEDNumberOfEntries")],
        ),
        multi_ro(
            "Device.DeviceInfo.X_TP_LEDs.LED.{i}.",
            vec![ro_param("Name"), ro_param("Status"), ro_param("Enable")],
        ),
        // ── Device.WiFi. ──────────────────────────────────────────────────────
        ro_obj(
            "Device.WiFi.",
            vec![
                ro_param("RadioNumberOfEntries"),
                ro_param("SSIDNumberOfEntries"),
                ro_param("AccessPointNumberOfEntries"),
            ],
        ),
        multi_rw(
            "Device.WiFi.Radio.{i}.",
            vec![
                rw_param("Enable"),
                ro_param("Status"),
                ro_param("Name"),
                ro_param("OperatingFrequencyBand"),
                rw_param("Channel"),
                rw_param("OperatingChannelBandwidth"),
                rw_param("TransmitPower"),
                rw_param("BeaconPeriod"),
                rw_param("DTIMPeriod"),
                rw_param("RTSThreshold"),
                rw_param("GuardInterval"),
                rw_param("IEEE80211hEnabled"),
                rw_param("MaxAssociatedDevices"),
                ro_param("MaxBitRate"),
                // vendor extension
                ro_param("X_OptimACS_BSSID"),
                ro_param("X_OptimACS_Bitrate"),
            ],
        ),
        multi_rw(
            "Device.WiFi.SSID.{i}.",
            vec![
                rw_param("Enable"),
                ro_param("Status"),
                ro_param("BSSID"),
                rw_param("SSID"),
            ],
        ),
        multi_rw(
            "Device.WiFi.AccessPoint.{i}.",
            vec![
                rw_param("Enable"),
                ro_param("Status"),
                rw_param("SSIDAdvertisementEnabled"),
                rw_param("MaxAssociatedDevices"),
                rw_param("WMMEnable"),
                rw_param("IsolationEnable"),
                ro_param("AssociatedDeviceNumberOfEntries"),
            ],
        ),
        rw_obj(
            "Device.WiFi.AccessPoint.{i}.Security.",
            vec![
                rw_param("ModeEnabled"),
                ro_param("ModesSupported"),
                rw_param("MFPConfig"),
                rw_param("KeyPassphrase"),
            ],
        ),
        multi_ro(
            "Device.WiFi.AccessPoint.{i}.AssociatedDevice.{i}.",
            vec![
                ro_param("MACAddress"),
                ro_param("IPAddress"),
                ro_param("SignalStrength"),
                ro_param("LastDataDownlinkRate"),
                ro_param("LastDataUplinkRate"),
                ro_param("BytesSent"),
                ro_param("BytesReceived"),
            ],
        ),
        // ── Device.IP. ────────────────────────────────────────────────────────
        ro_obj("Device.IP.", vec![ro_param("InterfaceNumberOfEntries")]),
        multi_rw(
            "Device.IP.Interface.{i}.",
            vec![
                ro_param("Status"),
                ro_param("Name"),
                ro_param("Type"),
                // vendor extensions
                ro_param("X_OptimACS_Name"),
                ro_param("X_OptimACS_Upstream"),
                ro_param("X_OptimACS_Protocol"),
                ro_param("X_OptimACS_Gateway"),
                ro_param("X_OptimACS_GatewayIPv4"),
                ro_param("X_OptimACS_DNS"),
                ro_param("X_OptimACS_RXBytes"),
                ro_param("X_OptimACS_TXBytes"),
                ro_param("X_OptimACS_RXPackets"),
                ro_param("X_OptimACS_TXPackets"),
                ro_param("X_OptimACS_Uptime"),
            ],
        ),
        multi_rw(
            "Device.IP.Interface.{i}.IPv4Address.{i}.",
            vec![
                rw_param("IPAddress"),
                rw_param("SubnetMask"),
                rw_param("AddressingType"),
            ],
        ),
        multi_ro(
            "Device.IP.Interface.{i}.IPv6Address.{i}.",
            vec![ro_param("IPAddress"), ro_param("PrefixLength")],
        ),
        // ── Device.DHCPv4. ────────────────────────────────────────────────────
        ro_obj(
            "Device.DHCPv4.Server.",
            vec![ro_param("PoolNumberOfEntries")],
        ),
        multi_rw(
            "Device.DHCPv4.Server.Pool.{i}.",
            vec![
                rw_param("Enable"),
                ro_param("Status"),
                rw_param("MinAddress"),
                rw_param("MaxAddress"),
                rw_param("LeaseTime"),
                ro_param("ClientNumberOfEntries"),
            ],
        ),
        multi_ro(
            "Device.DHCPv4.Server.Pool.{i}.Client.{i}.",
            vec![ro_param("Chaddr")],
        ),
        multi_ro(
            "Device.DHCPv4.Server.Pool.{i}.Client.{i}.IPv4Address.{i}.",
            vec![ro_param("IPAddress"), ro_param("LeaseTimeRemaining")],
        ),
        multi_rw(
            "Device.DHCPv4.Server.Pool.{i}.StaticAddress.{i}.",
            vec![
                rw_param("Enable"),
                rw_param("Chaddr"),
                rw_param("Yiaddr"),
            ],
        ),
        // ── Device.Hosts. ─────────────────────────────────────────────────────
        ro_obj("Device.Hosts.", vec![ro_param("HostNumberOfEntries")]),
        multi_ro(
            "Device.Hosts.Host.{i}.",
            vec![
                ro_param("IPAddress"),
                ro_param("MACAddress"),
                ro_param("HostName"),
                ro_param("Active"),
                ro_param("AddressSource"),
                ro_param("Layer1Interface"),
                ro_param("Layer3Interface"),
            ],
        ),
        // ── Device.IP.Interface.{i}.Stats. ────────────────────────────────────
        ro_obj(
            "Device.IP.Interface.{i}.Stats.",
            vec![
                ro_param("BytesSent"),
                ro_param("BytesReceived"),
                ro_param("PacketsSent"),
                ro_param("PacketsReceived"),
                ro_param("ErrorsSent"),
                ro_param("ErrorsReceived"),
            ],
        ),
        // ── Device.IP.Diagnostics. ───────────────────────────────────────────
        rw_obj_with_cmds(
            "Device.IP.Diagnostics.IPPing.",
            vec![
                rw_param("DiagnosticsState"),
                rw_param("Host"),
                rw_param("NumberOfRepetitions"),
                rw_param("Timeout"),
                ro_param("SuccessCount"),
                ro_param("FailureCount"),
                ro_param("AverageResponseTime"),
                ro_param("MinimumResponseTime"),
                ro_param("MaximumResponseTime"),
            ],
            vec![cmd(
                "IPPing()",
                &["Host", "NumberOfRepetitions", "Timeout"],
                &["Status", "SuccessCount", "FailureCount", "AverageResponseTime", "MinimumResponseTime", "MaximumResponseTime"],
            )],
        ),
        rw_obj_with_cmds(
            "Device.IP.Diagnostics.TraceRoute.",
            vec![
                rw_param("DiagnosticsState"),
                rw_param("Host"),
                rw_param("MaxHopCount"),
                rw_param("Timeout"),
                ro_param("RouteHopsNumberOfEntries"),
            ],
            vec![cmd(
                "TraceRoute()",
                &["Host", "MaxHopCount", "Timeout"],
                &["Status", "NumberOfHops"],
            )],
        ),
        multi_ro(
            "Device.IP.Diagnostics.TraceRoute.RouteHops.{i}.",
            vec![
                ro_param("Host"),
                ro_param("HostAddress"),
                ro_param("RTTimes"),
            ],
        ),
        // ── Device.Ethernet. ─────────────────────────────────────────────────
        ro_obj(
            "Device.Ethernet.",
            vec![
                ro_param("InterfaceNumberOfEntries"),
                ro_param("LinkNumberOfEntries"),
            ],
        ),
        multi_rw(
            "Device.Ethernet.Interface.{i}.",
            vec![
                rw_param("Enable"),
                ro_param("Status"),
                ro_param("Name"),
                ro_param("MACAddress"),
                ro_param("MaxBitRate"),
                ro_param("DuplexMode"),
            ],
        ),
        ro_obj(
            "Device.Ethernet.Interface.{i}.Stats.",
            vec![
                ro_param("BytesSent"),
                ro_param("BytesReceived"),
                ro_param("PacketsSent"),
                ro_param("PacketsReceived"),
                ro_param("ErrorsSent"),
                ro_param("ErrorsReceived"),
                ro_param("DiscardPacketsSent"),
                ro_param("DiscardPacketsReceived"),
            ],
        ),
        multi_ro(
            "Device.Ethernet.Link.{i}.",
            vec![
                ro_param("Enable"),
                ro_param("Status"),
                ro_param("Name"),
                ro_param("MACAddress"),
            ],
        ),
        // ── Device.Bridging. ─────────────────────────────────────────────────
        ro_obj(
            "Device.Bridging.",
            vec![ro_param("BridgeNumberOfEntries")],
        ),
        multi_ro(
            "Device.Bridging.Bridge.{i}.",
            vec![
                ro_param("Enable"),
                ro_param("Status"),
                ro_param("Alias"),
                ro_param("PortNumberOfEntries"),
            ],
        ),
        multi_ro(
            "Device.Bridging.Bridge.{i}.Port.{i}.",
            vec![
                ro_param("Enable"),
                ro_param("Status"),
                ro_param("Name"),
                ro_param("ManagementPort"),
                ro_param("PortState"),
            ],
        ),
        // ── Device.DHCPv6. ───────────────────────────────────────────────────
        ro_obj(
            "Device.DHCPv6.",
            vec![ro_param("ClientNumberOfEntries")],
        ),
        multi_ro(
            "Device.DHCPv6.Client.{i}.",
            vec![
                ro_param("Enable"),
                ro_param("Status"),
                ro_param("Interface"),
            ],
        ),
        // ── Device.NAT. ───────────────────────────────────────────────────────
        ro_obj(
            "Device.NAT.",
            vec![
                ro_param("InterfaceSettingNumberOfEntries"),
                ro_param("PortMappingNumberOfEntries"),
            ],
        ),
        multi_ro(
            "Device.NAT.InterfaceSetting.{i}.",
            vec![
                ro_param("Enable"),
                ro_param("Status"),
                ro_param("Alias"),
                ro_param("Interface"),
            ],
        ),
        multi_rw(
            "Device.NAT.PortMapping.{i}.",
            vec![
                rw_param("Enable"),
                ro_param("Status"),
                rw_param("Protocol"),
                rw_param("ExternalPort"),
                rw_param("InternalPort"),
                rw_param("InternalClient"),
                rw_param("Description"),
                rw_param("RemoteHost"),
            ],
        ),
        // ── Device.Firewall. ──────────────────────────────────────────────────
        rw_obj(
            "Device.Firewall.",
            vec![
                rw_param("Config"),
                ro_param("AdvancedLevel"),
                ro_param("ChainNumberOfEntries"),
                ro_param("RuleNumberOfEntries"),
            ],
        ),
        multi_ro(
            "Device.Firewall.Chain.{i}.",
            vec![
                ro_param("Enable"),
                ro_param("Name"),
                ro_param("Alias"),
                ro_param("RuleNumberOfEntries"),
            ],
        ),
        multi_rw(
            "Device.Firewall.Rule.{i}.",
            vec![
                rw_param("Enable"),
                ro_param("Status"),
                rw_param("Description"),
                rw_param("Target"),
                rw_param("Protocol"),
                rw_param("SourcePort"),
                rw_param("DestPort"),
                rw_param("SourceIP"),
                rw_param("DestIP"),
            ],
        ),
        // ── Device.Routing. ───────────────────────────────────────────────────
        ro_obj("Device.Routing.", vec![ro_param("RouterNumberOfEntries")]),
        multi_ro(
            "Device.Routing.Router.{i}.",
            vec![ro_param("IPv4ForwardingNumberOfEntries")],
        ),
        multi_rw(
            "Device.Routing.Router.{i}.IPv4Forwarding.{i}.",
            vec![
                rw_param("DestIPAddress"),
                rw_param("DestSubnetMask"),
                rw_param("GatewayIPAddress"),
                rw_param("Interface"),
                ro_param("Origin"),
            ],
        ),
        // ── Device.DNS. ───────────────────────────────────────────────────────
        ro_obj(
            "Device.DNS.Client.",
            vec![ro_param("ServerNumberOfEntries")],
        ),
        multi_rw(
            "Device.DNS.Client.Server.{i}.",
            vec![rw_param("DNSServer"), ro_param("Type")],
        ),
        // ── Device.Time. ──────────────────────────────────────────────────────
        rw_obj(
            "Device.Time.",
            vec![
                ro_param("CurrentLocalTime"),
                ro_param("LocalTimeZone"),
                rw_param("NTPServer1"),
                rw_param("NTPServer2"),
            ],
        ),
        // ── Device.Users. ─────────────────────────────────────────────────────
        ro_obj(
            "Device.Users.",
            vec![ro_param("UserNumberOfEntries")],
        ),
        multi_rw(
            "Device.Users.User.{i}.",
            vec![
                ro_param("Alias"),
                ro_param("Username"),
                ro_param("Enable"),
                rw_param("Password"),
                ro_param("RemoteAccessCapable"),
                ro_param("Language"),
            ],
        ),
        // ── Device.SoftwareModules. ──────────────────────────────────────────
        ro_obj(
            "Device.SoftwareModules.",
            vec![ro_param("ExecEnvNumberOfEntries")],
        ),
        multi_ro(
            "Device.SoftwareModules.ExecEnv.{i}.",
            vec![
                ro_param("Enable"),
                ro_param("Status"),
                ro_param("Name"),
                ro_param("Type"),
                ro_param("Vendor"),
                ro_param("Version"),
                ro_param("AvailableMemory"),
                ro_param("ProcessorArchitecture"),
            ],
        ),
        // ── Device.PPP. ──────────────────────────────────────────────────────
        ro_obj(
            "Device.PPP.",
            vec![ro_param("InterfaceNumberOfEntries")],
        ),
        multi_rw(
            "Device.PPP.Interface.{i}.",
            vec![
                ro_param("Enable"),
                ro_param("Status"),
                ro_param("Name"),
                rw_param("Username"),
                rw_param("Password"),
                ro_param("ConnectionStatus"),
                ro_param("IPCPLocalIPAddress"),
            ],
        ),
        // ── Device.ManagementServer. ──────────────────────────────────────────
        ro_obj(
            "Device.ManagementServer.",
            vec![
                ro_param("URL"),
                ro_param("EnableCWMP"),
                ro_param("ConnectionRequestURL"),
                ro_param("PeriodicInformEnable"),
                ro_param("PeriodicInformInterval"),
                ro_param("PeriodicInformTime"),
                ro_param("ParameterKey"),
                ro_param("UpgradesManaged"),
            ],
        ),
        // ── Device.InterfaceStack. ───────────────────────────────────────────
        ro_obj(
            "Device.",
            vec![ro_param("InterfaceStackNumberOfEntries")],
        ),
        multi_ro(
            "Device.InterfaceStack.{i}.",
            vec![
                ro_param("HigherLayer"),
                ro_param("LowerLayer"),
                ro_param("HigherAlias"),
                ro_param("LowerAlias"),
            ],
        ),
        // ── Device.RouterAdvertisement. ──────────────────────────────────────
        ro_obj(
            "Device.RouterAdvertisement.",
            vec![
                ro_param("Enable"),
                ro_param("InterfaceSettingNumberOfEntries"),
            ],
        ),
        multi_ro(
            "Device.RouterAdvertisement.InterfaceSetting.{i}.",
            vec![
                ro_param("Enable"),
                ro_param("Status"),
                ro_param("Interface"),
                ro_param("MaxRtrAdvInterval"),
                ro_param("MinRtrAdvInterval"),
                ro_param("AdvManagedFlag"),
                ro_param("AdvOtherConfigFlag"),
            ],
        ),
        // ── Device.IPv6rd. ───────────────────────────────────────────────────
        ro_obj(
            "Device.IPv6rd.",
            vec![
                ro_param("Enable"),
                ro_param("InterfaceSettingNumberOfEntries"),
            ],
        ),
        multi_ro(
            "Device.IPv6rd.InterfaceSetting.{i}.",
            vec![
                ro_param("Enable"),
                ro_param("Status"),
                ro_param("Alias"),
                ro_param("BorderRelayIPv4Addresses"),
                ro_param("SPIPv6Prefix"),
                ro_param("IPv4MaskLength"),
            ],
        ),
        // ── Device.DSLite. ───────────────────────────────────────────────────
        ro_obj(
            "Device.DSLite.",
            vec![
                ro_param("Enable"),
                ro_param("InterfaceSettingNumberOfEntries"),
            ],
        ),
        multi_ro(
            "Device.DSLite.InterfaceSetting.{i}.",
            vec![
                ro_param("Enable"),
                ro_param("Status"),
                ro_param("Alias"),
                ro_param("EndpointAssignmentPrecedence"),
                ro_param("EndpointAddress"),
                ro_param("Origin"),
            ],
        ),
        // ── Device.QoS. ─────────────────────────────────────────────────────
        ro_obj(
            "Device.QoS.",
            vec![
                ro_param("QueueNumberOfEntries"),
                ro_param("ClassificationNumberOfEntries"),
            ],
        ),
        multi_ro(
            "Device.QoS.Queue.{i}.",
            vec![
                ro_param("Enable"),
                ro_param("Status"),
                ro_param("Interface"),
                ro_param("ShapingRate"),
                ro_param("Alias"),
            ],
        ),
        multi_ro(
            "Device.QoS.Classification.{i}.",
            vec![
                ro_param("Enable"),
                ro_param("Alias"),
                ro_param("Order"),
                ro_param("Interface"),
                ro_param("Protocol"),
                ro_param("DestPort"),
                ro_param("SourcePort"),
                ro_param("DSCPMark"),
            ],
        ),
        // ── Device.UPnP. ────────────────────────────────────────────────────
        ro_obj(
            "Device.UPnP.Device.",
            vec![
                ro_param("Enable"),
                ro_param("Status"),
                ro_param("UPnPIGD"),
                ro_param("UPnPMediaServer"),
                ro_param("UPnPMediaRenderer"),
                ro_param("UPnPWLANAccessPoint"),
            ],
        ),
        // ── Device.BulkData. (TR-232) ────────────────────────────────────────
        rw_obj(
            "Device.BulkData.",
            vec![
                ro_param("Enable"),
                ro_param("Status"),
                ro_param("MinReportingInterval"),
                ro_param("Protocols"),
                ro_param("EncodingTypes"),
                ro_param("ProfileNumberOfEntries"),
            ],
        ),
        multi_rw(
            "Device.BulkData.Profile.{i}.",
            vec![
                rw_param("Enable"),
                rw_param("Protocol"),
                rw_param("ReportingInterval"),
            ],
        ),
        rw_obj(
            "Device.BulkData.Profile.{i}.HTTP.",
            vec![rw_param("URL")],
        ),
        // ── Device.SoftwareModules.DeploymentUnit. ───────────────────────────
        multi_ro(
            "Device.SoftwareModules.DeploymentUnit.{i}.",
            vec![
                ro_param("Name"),
                ro_param("Version"),
                ro_param("Status"),
                ro_param("Resolved"),
                ro_param("ExecutionEnvRef"),
            ],
        ),
        // ── Device.DSL. ─────────────────────────────────────────────────────
        ro_obj(
            "Device.DSL.",
            vec![
                ro_param("LineNumberOfEntries"),
                ro_param("ChannelNumberOfEntries"),
            ],
        ),
        multi_ro(
            "Device.DSL.Line.{i}.",
            vec![
                ro_param("Enable"),
                ro_param("Status"),
                ro_param("StandardUsed"),
                ro_param("UpstreamMaxBitRate"),
                ro_param("DownstreamMaxBitRate"),
                ro_param("UpstreamAttenuation"),
                ro_param("DownstreamAttenuation"),
                ro_param("UpstreamNoiseMargin"),
                ro_param("DownstreamNoiseMargin"),
                ro_param("UpstreamPower"),
                ro_param("DownstreamPower"),
            ],
        ),
        // ── Device.Optical. ──────────────────────────────────────────────────
        ro_obj(
            "Device.Optical.",
            vec![ro_param("InterfaceNumberOfEntries")],
        ),
        multi_ro(
            "Device.Optical.Interface.{i}.",
            vec![
                ro_param("Enable"),
                ro_param("Status"),
                ro_param("Name"),
                ro_param("OpticalSignalLevel"),
                ro_param("TransmitOpticalLevel"),
                ro_param("LowerLayers"),
            ],
        ),
        // ── Device.IEEE8021x. ────────────────────────────────────────────────
        ro_obj(
            "Device.IEEE8021x.",
            vec![ro_param("SupplicantNumberOfEntries")],
        ),
        multi_ro(
            "Device.IEEE8021x.Supplicant.{i}.",
            vec![
                ro_param("Enable"),
                ro_param("Status"),
                ro_param("Interface"),
                ro_param("PAEState"),
                ro_param("EAPIdentity"),
            ],
        ),
        // ── Device.Services.VoIPProfile. ─────────────────────────────────────
        ro_obj(
            "Device.Services.",
            vec![ro_param("VoIPProfileNumberOfEntries")],
        ),
        multi_ro(
            "Device.Services.VoIPProfile.{i}.",
            vec![
                ro_param("Enable"),
                ro_param("Name"),
                ro_param("SignallingProtocol"),
            ],
        ),
        ro_obj(
            "Device.Services.VoIPProfile.{i}.SIP.",
            vec![
                ro_param("ProxyServer"),
                ro_param("RegistrarServer"),
                ro_param("UserAgentDomain"),
                ro_param("OutboundProxy"),
            ],
        ),
        // ── Device.X_OptimACS_Firmware. (vendor extension) ───────────────────
        ro_obj(
            "Device.X_OptimACS_Firmware.",
            vec![ro_param("CurrentVersion"), ro_param("AvailableVersion")],
        ),
        // ── Device.X_OptimACS_Security. (vendor extension) ───────────────────
        rw_obj(
            "Device.X_OptimACS_Security.",
            vec![rw_param("DevicePassword")],
        ),
        // ── Device.X_OptimACS. (vendor extension — agent identity) ───────────
        ro_obj(
            "Device.X_OptimACS.",
            vec![ro_param("ClaimToken"), ro_param("AgentEndpointID")],
        ),
        // ── Device.X_OptimACS_Network. (vendor extension — bridge/vlan) ──────
        ro_obj(
            "Device.X_OptimACS_Network.",
            vec![ro_param("BridgeNumberOfEntries")],
        ),
        multi_rw(
            "Device.X_OptimACS_Network.Bridge.{i}.",
            vec![ro_param("Name"), ro_param("Status"), ro_param("Members")],
        ),
    ]
}

/// Handle GetSupportedDM request and return a response declaring all supported
/// objects and parameters per TR-369 §6.1.5.
pub fn handle_get_supported_dm(
    msg_id: &str,
    obj_paths: &[String],
    first_level_only: bool,
    include_commands: bool,
    _include_events: bool,
) -> Option<usp_msg::Msg> {
    let all_objects = build_supported_objects();

    let req_path = if obj_paths.is_empty() {
        "Device.".to_string()
    } else {
        obj_paths
            .first()
            .cloned()
            .unwrap_or_else(|| "Device.".into())
    };

    // Filter to requested subtrees
    let mut filtered: Vec<SupportedObject> =
        if obj_paths.is_empty() || obj_paths.iter().any(|p| p == "Device." || p == "Device") {
            all_objects
        } else {
            all_objects
                .into_iter()
                .filter(|o| {
                    obj_paths.iter().any(|req| {
                        o.obj_path.starts_with(req.as_str())
                            || req.starts_with(o.obj_path.as_str())
                    })
                })
                .collect()
        };

    // first_level_only: only return direct children of the requested path
    if first_level_only {
        let base_depth = req_path.matches('.').count();
        filtered.retain(|o| {
            let obj_depth = o.obj_path.trim_end_matches('.').matches('.').count() + 1;
            obj_depth <= base_depth + 1
        });
    }

    // Strip commands if not requested
    if !include_commands {
        for obj in &mut filtered {
            obj.supported_commands.clear();
        }
    }

    let path_results = vec![RequestedObjectResult {
        req_obj_path: req_path,
        err_code: 0,
        err_msg: String::new(),
        data_model_inst_uri: "urn:broadband-forum-org:tr-181-2-16-0".into(),
        supported_objs: filtered,
    }];

    Some(usp_msg::Msg {
        header: Some(usp_msg::Header {
            msg_id: msg_id.into(),
            msg_type: usp_msg::header::MessageType::GetSupportedDmResp as i32,
        }),
        body: Some(usp_msg::Body {
            msg_body: Some(usp_msg::body::MsgBody::Response(usp_msg::Response {
                resp_type: Some(usp_msg::response::RespType::GetSupportedDmResp(
                    usp_msg::GetSupportedDmResp {
                        req_obj_results: path_results,
                    },
                )),
            })),
        }),
    })
}
