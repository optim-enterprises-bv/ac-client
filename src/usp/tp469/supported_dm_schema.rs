//! TP-469 Supported Data Model Schema
//!
//! This module defines the complete TR-181 data model schema supported by the agent.
//! Used by GetSupportedDM to report capabilities to the controller.

use std::collections::HashMap;

/// Represents a parameter in the data model
#[derive(Debug, Clone)]
pub struct ParameterSchema {
    pub name: String,
    pub param_type: ParamType,
    pub access: Access,
    pub description: String,
}

/// Represents an object in the data model
#[derive(Debug, Clone)]
pub struct ObjectSchema {
    pub name: String,
    pub is_multi_instance: bool,
    pub description: String,
    pub unique_keys: Vec<Vec<String>>,  // Unique key parameter sets
    pub parameters: HashMap<String, ParameterSchema>,
    pub children: HashMap<String, ObjectSchema>,
    pub commands: Vec<CommandSchema>,
    pub events: Vec<EventSchema>,
}

/// Command (OPERATE) definition
#[derive(Debug, Clone)]
pub struct CommandSchema {
    pub name: String,
    pub description: String,
    pub input_args: Vec<ParameterSchema>,
    pub output_args: Vec<ParameterSchema>,
}

/// Event (NOTIFY) definition
#[derive(Debug, Clone)]
pub struct EventSchema {
    pub name: String,
    pub description: String,
    pub params: Vec<ParameterSchema>,
}

#[derive(Debug, Clone, Copy)]
pub enum ParamType {
    String,
    Int,
    Uint,
    Bool,
    DateTime,
    Base64,
    Long,
    Ulong,
    Double,
    HexBinary,
    List,
}

#[derive(Debug, Clone, Copy)]
pub enum Access {
    ReadOnly,
    ReadWrite,
    WriteOnceReadOnly,
}

/// Build the complete TR-181 data model schema
pub fn build_data_model_schema() -> ObjectSchema {
    let mut root = ObjectSchema {
        name: "Device".into(),
        is_multi_instance: false,
        description: "Root device object".into(),
        unique_keys: vec![],
        parameters: HashMap::new(),
        children: HashMap::new(),
        commands: vec![],
        events: vec![],
    };
    
    // Device.DeviceInfo
    root.children.insert("DeviceInfo".into(), build_device_info_schema());
    
    // Device.WiFi
    root.children.insert("WiFi".into(), build_wifi_schema());
    
    // Device.IP
    root.children.insert("IP".into(), build_ip_schema());
    
    // Device.DHCPv4
    root.children.insert("DHCPv4".into(), build_dhcp_schema());
    
    // Device.Hosts
    root.children.insert("Hosts".into(), build_hosts_schema());
    
    // Device.LocalAgent
    root.children.insert("LocalAgent".into(), build_local_agent_schema());
    
    // Device.X_OptimACS extensions
    root.children.insert("X_OptimACS".into(), build_optimacs_schema());
    
    root
}

fn build_device_info_schema() -> ObjectSchema {
    let mut params = HashMap::new();
    
    params.insert("Manufacturer".into(), ParameterSchema {
        name: "Manufacturer".into(),
        param_type: ParamType::String,
        access: Access::ReadOnly,
        description: "Manufacturer of the device".into(),
    });
    
    params.insert("ModelName".into(), ParameterSchema {
        name: "ModelName".into(),
        param_type: ParamType::String,
        access: Access::ReadOnly,
        description: "Model name of the device".into(),
    });
    
    params.insert("ProductClass".into(), ParameterSchema {
        name: "ProductClass".into(),
        param_type: ParamType::String,
        access: Access::ReadOnly,
        description: "Product class of the device".into(),
    });
    
    params.insert("SerialNumber".into(), ParameterSchema {
        name: "SerialNumber".into(),
        param_type: ParamType::String,
        access: Access::ReadOnly,
        description: "Serial number of the device".into(),
    });
    
    params.insert("HardwareVersion".into(), ParameterSchema {
        name: "HardwareVersion".into(),
        param_type: ParamType::String,
        access: Access::ReadOnly,
        description: "Hardware version of the device".into(),
    });
    
    params.insert("SoftwareVersion".into(), ParameterSchema {
        name: "SoftwareVersion".into(),
        param_type: ParamType::String,
        access: Access::ReadOnly,
        description: "Software version currently installed on the device".into(),
    });
    
    params.insert("UpTime".into(), ParameterSchema {
        name: "UpTime".into(),
        param_type: ParamType::Uint,
        access: Access::ReadOnly,
        description: "Time since the device was last restarted".into(),
    });
    
    params.insert("X_OptimACS_LoadAvg".into(), ParameterSchema {
        name: "X_OptimACS_LoadAvg".into(),
        param_type: ParamType::String,
        access: Access::ReadOnly,
        description: "System load average".into(),
    });
    
    params.insert("X_OptimACS_ZoneName".into(), ParameterSchema {
        name: "X_OptimACS_ZoneName".into(),
        param_type: ParamType::String,
        access: Access::ReadWrite,
        description: "Timezone name (e.g., UTC, Europe/London)".into(),
    });
    
    params.insert("X_OptimACS_CompatVersion".into(), ParameterSchema {
        name: "X_OptimACS_CompatVersion".into(),
        param_type: ParamType::String,
        access: Access::ReadOnly,
        description: "OpenWrt compatibility version".into(),
    });
    
    params.insert("X_OptimACS_TTYLogin".into(), ParameterSchema {
        name: "X_OptimACS_TTYLogin".into(),
        param_type: ParamType::Bool,
        access: Access::ReadWrite,
        description: "Enable TTY login".into(),
    });
    
    params.insert("X_OptimACS_LogSize".into(), ParameterSchema {
        name: "X_OptimACS_LogSize".into(),
        param_type: ParamType::Uint,
        access: Access::ReadWrite,
        description: "Log buffer size in KB".into(),
    });
    
    ObjectSchema {
        name: "DeviceInfo".into(),
        is_multi_instance: false,
        description: "Device information".into(),
        unique_keys: vec![],
        parameters: params,
        children: HashMap::new(),
        commands: vec![],
        events: vec![],
    }
}

fn build_wifi_schema() -> ObjectSchema {
    let mut radio = ObjectSchema {
        name: "Radio".into(),
        is_multi_instance: true,
        description: "WiFi radio entry".into(),
        unique_keys: vec![vec!["Name".into()]],
        parameters: HashMap::new(),
        children: HashMap::new(),
        commands: vec![],
        events: vec![],
    };
    
    radio.parameters.insert("Enable".into(), ParameterSchema {
        name: "Enable".into(),
        param_type: ParamType::Bool,
        access: Access::ReadWrite,
        description: "Enable or disable this radio".into(),
    });
    
    radio.parameters.insert("Name".into(), ParameterSchema {
        name: "Name".into(),
        param_type: ParamType::String,
        access: Access::ReadOnly,
        description: "Name of the radio".into(),
    });
    
    radio.parameters.insert("Channel".into(), ParameterSchema {
        name: "Channel".into(),
        param_type: ParamType::Uint,
        access: Access::ReadWrite,
        description: "Current channel".into(),
    });
    
    radio.parameters.insert("OperatingFrequencyBand".into(), ParameterSchema {
        name: "OperatingFrequencyBand".into(),
        param_type: ParamType::String,
        access: Access::ReadOnly,
        description: "Frequency band (2.4GHz or 5GHz)".into(),
    });
    
    radio.parameters.insert("OperatingChannelBandwidth".into(), ParameterSchema {
        name: "OperatingChannelBandwidth".into(),
        param_type: ParamType::String,
        access: Access::ReadWrite,
        description: "Channel bandwidth (20MHz, 40MHz, 80MHz, 160MHz, EHT20, EHT80, EHT320 for WiFi 7)".into(),
    });
    
    radio.parameters.insert("X_OptimACS_CellDensity".into(), ParameterSchema {
        name: "X_OptimACS_CellDensity".into(),
        param_type: ParamType::Int,
        access: Access::ReadWrite,
        description: "Cell density setting (-1, 0, 1, 2, 3)".into(),
    });
    
    let mut ssid = ObjectSchema {
        name: "SSID".into(),
        is_multi_instance: true,
        description: "SSID entry".into(),
        unique_keys: vec![vec!["SSID".into()]],
        parameters: HashMap::new(),
        children: HashMap::new(),
        commands: vec![],
        events: vec![],
    };
    
    ssid.parameters.insert("Enable".into(), ParameterSchema {
        name: "Enable".into(),
        param_type: ParamType::Bool,
        access: Access::ReadWrite,
        description: "Enable or disable this SSID".into(),
    });
    
    ssid.parameters.insert("SSID".into(), ParameterSchema {
        name: "SSID".into(),
        param_type: ParamType::String,
        access: Access::ReadWrite,
        description: "SSID name".into(),
    });
    
    let mut access_point = ObjectSchema {
        name: "AccessPoint".into(),
        is_multi_instance: true,
        description: "Access point entry".into(),
        unique_keys: vec![],
        parameters: HashMap::new(),
        children: HashMap::new(),
        commands: vec![],
        events: vec![],
    };
    
    access_point.parameters.insert("Security.ModeEnabled".into(), ParameterSchema {
        name: "ModeEnabled".into(),
        param_type: ParamType::String,
        access: Access::ReadWrite,
        description: "Security mode (None, WEP, WPA, WPA2, WPA3)".into(),
    });
    
    access_point.parameters.insert("Security.KeyPassphrase".into(), ParameterSchema {
        name: "KeyPassphrase".into(),
        param_type: ParamType::String,
        access: Access::ReadWrite,
        description: "WiFi password".into(),
    });
    
    access_point.parameters.insert("X_OptimACS_OCV".into(), ParameterSchema {
        name: "X_OptimACS_OCV".into(),
        param_type: ParamType::Int,
        access: Access::ReadWrite,
        description: "Operating Channel Validation (0=disabled, 1=enabled)".into(),
    });
    
    let mut wifi = ObjectSchema {
        name: "WiFi".into(),
        is_multi_instance: false,
        description: "WiFi configuration".into(),
        unique_keys: vec![],
        parameters: HashMap::new(),
        children: HashMap::new(),
        commands: vec![],
        events: vec![],
    };
    
    wifi.children.insert("Radio".into(), radio);
    wifi.children.insert("SSID".into(), ssid);
    wifi.children.insert("AccessPoint".into(), access_point);
    
    wifi
}

fn build_ip_schema() -> ObjectSchema {
    let mut ipv4_addr = ObjectSchema {
        name: "IPv4Address".into(),
        is_multi_instance: true,
        description: "IPv4 address entry".into(),
        unique_keys: vec![vec!["IPAddress".into()]],
        parameters: HashMap::new(),
        children: HashMap::new(),
        commands: vec![],
        events: vec![],
    };
    
    ipv4_addr.parameters.insert("IPAddress".into(), ParameterSchema {
        name: "IPAddress".into(),
        param_type: ParamType::List,  // Can be list type in OpenWrt
        access: Access::ReadWrite,
        description: "IPv4 address(es) with CIDR notation (e.g., 192.168.1.1/24)".into(),
    });
    
    ipv4_addr.parameters.insert("IPv6Address".into(), ParameterSchema {
        name: "IPv6Address".into(),
        param_type: ParamType::List,
        access: Access::ReadWrite,
        description: "IPv6 address(es)".into(),
    });
    
    ipv4_addr.parameters.insert("IPv6Prefix".into(), ParameterSchema {
        name: "IPv6Prefix".into(),
        param_type: ParamType::String,
        access: Access::ReadWrite,
        description: "IPv6 ULA prefix (e.g., fd22:240f:a934::/48)".into(),
    });
    
    ipv4_addr.parameters.insert("SubnetMask".into(), ParameterSchema {
        name: "SubnetMask".into(),
        param_type: ParamType::String,
        access: Access::ReadWrite,
        description: "Subnet mask".into(),
    });
    
    ipv4_addr.parameters.insert("AddressingType".into(), ParameterSchema {
        name: "AddressingType".into(),
        param_type: ParamType::String,
        access: Access::ReadWrite,
        description: "Addressing type (Static, DHCP)".into(),
    });
    
    ipv4_addr.parameters.insert("DNSServers".into(), ParameterSchema {
        name: "DNSServers".into(),
        param_type: ParamType::List,
        access: Access::ReadWrite,
        description: "DNS server list".into(),
    });
    
    let mut interface = ObjectSchema {
        name: "Interface".into(),
        is_multi_instance: true,
        description: "IP interface entry".into(),
        unique_keys: vec![vec!["Name".into()]],
        parameters: HashMap::new(),
        children: HashMap::new(),
        commands: vec![],
        events: vec![],
    };
    
    interface.parameters.insert("Name".into(), ParameterSchema {
        name: "Name".into(),
        param_type: ParamType::String,
        access: Access::ReadOnly,
        description: "Interface name".into(),
    });
    
    interface.parameters.insert("X_OptimACS_BridgePorts".into(), ParameterSchema {
        name: "X_OptimACS_BridgePorts".into(),
        param_type: ParamType::List,
        access: Access::ReadWrite,
        description: "Bridge member ports (e.g., lan1 lan2 lan3)".into(),
    });
    
    interface.parameters.insert("X_OptimACS_MACAddress".into(), ParameterSchema {
        name: "X_OptimACS_MACAddress".into(),
        param_type: ParamType::String,
        access: Access::ReadWrite,
        description: "MAC address override for interface".into(),
    });
    
    interface.parameters.insert("Enable".into(), ParameterSchema {
        name: "Enable".into(),
        param_type: ParamType::Bool,
        access: Access::ReadWrite,
        description: "Enable or disable this interface".into(),
    });
    
    interface.children.insert("IPv4Address".into(), ipv4_addr);
    
    let mut ip = ObjectSchema {
        name: "IP".into(),
        is_multi_instance: false,
        description: "IP configuration".into(),
        unique_keys: vec![],
        parameters: HashMap::new(),
        children: HashMap::new(),
        commands: vec![],
        events: vec![],
    };
    
    ip.children.insert("Interface".into(), interface);
    ip
}

fn build_dhcp_schema() -> ObjectSchema {
    let mut static_addr = ObjectSchema {
        name: "StaticAddress".into(),
        is_multi_instance: true,
        description: "Static DHCP lease entry".into(),
        unique_keys: vec![vec!["Chaddr".into()]],
        parameters: HashMap::new(),
        children: HashMap::new(),
        commands: vec![],
        events: vec![],
    };
    
    static_addr.parameters.insert("Chaddr".into(), ParameterSchema {
        name: "Chaddr".into(),
        param_type: ParamType::String,
        access: Access::ReadWrite,
        description: "Client MAC address".into(),
    });
    
    static_addr.parameters.insert("Yiaddr".into(), ParameterSchema {
        name: "Yiaddr".into(),
        param_type: ParamType::String,
        access: Access::ReadWrite,
        description: "Reserved IP address".into(),
    });
    
    static_addr.parameters.insert("X_OptimACS_Hostname".into(), ParameterSchema {
        name: "X_OptimACS_Hostname".into(),
        param_type: ParamType::String,
        access: Access::ReadWrite,
        description: "Hostname for this lease".into(),
    });
    
    let mut pool = ObjectSchema {
        name: "Pool".into(),
        is_multi_instance: true,
        description: "DHCP pool".into(),
        unique_keys: vec![],
        parameters: HashMap::new(),
        children: HashMap::new(),
        commands: vec![],
        events: vec![],
    };
    
    pool.children.insert("StaticAddress".into(), static_addr);
    
    let mut server = ObjectSchema {
        name: "Server".into(),
        is_multi_instance: false,
        description: "DHCP server".into(),
        unique_keys: vec![],
        parameters: HashMap::new(),
        children: HashMap::new(),
        commands: vec![],
        events: vec![],
    };
    
    server.children.insert("Pool".into(), pool);
    
    let mut dhcp = ObjectSchema {
        name: "DHCPv4".into(),
        is_multi_instance: false,
        description: "DHCPv4 configuration".into(),
        unique_keys: vec![],
        parameters: HashMap::new(),
        children: HashMap::new(),
        commands: vec![],
        events: vec![],
    };
    
    dhcp.children.insert("Server".into(), server);
    dhcp
}

fn build_hosts_schema() -> ObjectSchema {
    let mut host = ObjectSchema {
        name: "Host".into(),
        is_multi_instance: true,
        description: "Host entry".into(),
        unique_keys: vec![vec!["HostName".into()], vec!["IPAddress".into()]],
        parameters: HashMap::new(),
        children: HashMap::new(),
        commands: vec![],
        events: vec![],
    };
    
    host.parameters.insert("HostName".into(), ParameterSchema {
        name: "HostName".into(),
        param_type: ParamType::String,
        access: Access::ReadWrite,
        description: "Hostname".into(),
    });
    
    host.parameters.insert("IPAddress".into(), ParameterSchema {
        name: "IPAddress".into(),
        param_type: ParamType::String,
        access: Access::ReadWrite,
        description: "IP address".into(),
    });
    
    host.parameters.insert("Active".into(), ParameterSchema {
        name: "Active".into(),
        param_type: ParamType::Bool,
        access: Access::ReadWrite,
        description: "Whether this entry is active".into(),
    });
    
    let mut hosts = ObjectSchema {
        name: "Hosts".into(),
        is_multi_instance: false,
        description: "Static hosts configuration".into(),
        unique_keys: vec![],
        parameters: HashMap::new(),
        children: HashMap::new(),
        commands: vec![],
        events: vec![],
    };
    
    hosts.children.insert("Host".into(), host);
    hosts
}

fn build_local_agent_schema() -> ObjectSchema {
    let mut controller = ObjectSchema {
        name: "Controller".into(),
        is_multi_instance: true,
        description: "Controller entry".into(),
        unique_keys: vec![vec!["EndpointID".into()]],
        parameters: HashMap::new(),
        children: HashMap::new(),
        commands: vec![],
        events: vec![],
    };
    
    controller.parameters.insert("EndpointID".into(), ParameterSchema {
        name: "EndpointID".into(),
        param_type: ParamType::String,
        access: Access::ReadOnly,
        description: "Controller endpoint ID".into(),
    });
    
    controller.parameters.insert("Enable".into(), ParameterSchema {
        name: "Enable".into(),
        param_type: ParamType::Bool,
        access: Access::ReadWrite,
        description: "Enable this controller".into(),
    });
    
    let mut mtp = ObjectSchema {
        name: "MTP".into(),
        is_multi_instance: true,
        description: "MTP entry".into(),
        unique_keys: vec![],
        parameters: HashMap::new(),
        children: HashMap::new(),
        commands: vec![],
        events: vec![],
    };
    
    mtp.parameters.insert("Protocol".into(), ParameterSchema {
        name: "Protocol".into(),
        param_type: ParamType::String,
        access: Access::ReadWrite,
        description: "Protocol (WebSocket, MQTT)".into(),
    });
    
    controller.children.insert("MTP".into(), mtp);
    
    let mut la = ObjectSchema {
        name: "LocalAgent".into(),
        is_multi_instance: false,
        description: "Local agent configuration".into(),
        unique_keys: vec![],
        parameters: HashMap::new(),
        children: HashMap::new(),
        commands: vec![],
        events: vec![],
    };
    
    la.parameters.insert("EndpointID".into(), ParameterSchema {
        name: "EndpointID".into(),
        param_type: ParamType::String,
        access: Access::ReadOnly,
        description: "Agent endpoint ID".into(),
    });
    
    la.parameters.insert("UpTime".into(), ParameterSchema {
        name: "UpTime".into(),
        param_type: ParamType::Uint,
        access: Access::ReadOnly,
        description: "Agent uptime".into(),
    });
    
    la.children.insert("Controller".into(), controller);
    
    // AddCertificate command
    la.commands.push(CommandSchema {
        name: "AddCertificate()".into(),
        description: "Add a trusted certificate".into(),
        input_args: vec![
            ParameterSchema {
                name: "Certificate".into(),
                param_type: ParamType::String,
                access: Access::ReadOnly,
                description: "PEM-encoded certificate".into(),
            },
        ],
        output_args: vec![],
    });
    
    la
}

fn build_optimacs_schema() -> ObjectSchema {
    let mut camera = ObjectSchema {
        name: "Camera".into(),
        is_multi_instance: true,
        description: "Camera entry".into(),
        unique_keys: vec![vec!["MACAddress".into()]],
        parameters: HashMap::new(),
        children: HashMap::new(),
        commands: vec![],
        events: vec![],
    };
    
    camera.parameters.insert("MACAddress".into(), ParameterSchema {
        name: "MACAddress".into(),
        param_type: ParamType::String,
        access: Access::ReadOnly,
        description: "Camera MAC address".into(),
    });
    
    camera.commands.push(CommandSchema {
        name: "Capture()".into(),
        description: "Capture an image".into(),
        input_args: vec![],
        output_args: vec![
            ParameterSchema {
                name: "ImageURL".into(),
                param_type: ParamType::String,
                access: Access::ReadOnly,
                description: "URL to captured image".into(),
            },
        ],
    });
    
    let mut firmware = ObjectSchema {
        name: "Firmware".into(),
        is_multi_instance: false,
        description: "Firmware management".into(),
        unique_keys: vec![],
        parameters: HashMap::new(),
        children: HashMap::new(),
        commands: vec![],
        events: vec![],
    };
    
    firmware.commands.push(CommandSchema {
        name: "Download()".into(),
        description: "Download firmware".into(),
        input_args: vec![
            ParameterSchema {
                name: "URL".into(),
                param_type: ParamType::String,
                access: Access::ReadOnly,
                description: "Firmware URL".into(),
            },
        ],
        output_args: vec![],
    });
    
    let mut optimacs = ObjectSchema {
        name: "X_OptimACS".into(),
        is_multi_instance: false,
        description: "OptimACS extensions".into(),
        unique_keys: vec![],
        parameters: HashMap::new(),
        children: HashMap::new(),
        commands: vec![],
        events: vec![],
    };
    
    optimacs.children.insert("Camera".into(), camera);
    optimacs.children.insert("Firmware".into(), firmware);
    
    optimacs
}

/// Find an object schema by path
pub fn find_object_schema<'a>(root: &'a ObjectSchema, path: &str) -> Option<&'a ObjectSchema> {
    let parts: Vec<&str> = path.trim_start_matches("Device.").split('.').collect();
    let mut current = root;
    
    for part in parts {
        // Remove instance numbers (e.g., "WiFi.SSID.1" -> "WiFi.SSID")
        let clean_part = part.split('{').next().unwrap_or(part);
        
        match current.children.get(clean_part) {
            Some(child) => current = child,
            None => return None,
        }
    }
    
    Some(current)
}

/// Find a parameter schema by path
pub fn find_parameter_schema<'a>(root: &'a ObjectSchema, path: &str) -> Option<&'a ParameterSchema> {
    // Split path into object path and parameter name
    if let Some(last_dot) = path.rfind('.') {
        let obj_path = &path[..last_dot];
        let param_name = &path[last_dot + 1..];
        
        if let Some(obj) = find_object_schema(root, obj_path) {
            return obj.parameters.get(param_name);
        }
    }
    
    None
}

/// Check if a path represents a multi-instance object
pub fn is_multi_instance(root: &ObjectSchema, path: &str) -> bool {
    if let Some(obj) = find_object_schema(root, path) {
        obj.is_multi_instance
    } else {
        false
    }
}
