//! USP Endpoint ID management for the agent side.

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EndpointId(pub String);

impl EndpointId {
    pub fn new(s: impl Into<String>) -> Self { EndpointId(s.into()) }

    /// Build agent endpoint ID from OUI and MAC: `oui:{oui}:{mac}`
    pub fn from_mac(oui: &str, mac: &str) -> Self {
        EndpointId(format!("oui:{}:{}", oui, mac))
    }

    pub fn as_str(&self) -> &str { &self.0 }
}

impl std::fmt::Display for EndpointId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
