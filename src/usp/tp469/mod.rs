//! TP-469 Compliance Module - USP/TR-369 Protocol Conformance
//!
//! This module implements all TP-469 test requirements for USP Agent conformance:
//! - Message types: GET, SET, ADD, DELETE, OPERATE, NOTIFY
//! - Protocol features: Version negotiation, Subscriptions, Notifications, Permissions
//! - Data model operations: GetSupportedDM, GetInstances, Search paths, Wildcards
//! - Error handling: All TP-469 error codes and scenarios

pub mod add_delete;
pub mod error_codes;
pub mod get_instances;
pub mod get_supported_dm;
pub mod notifications;
pub mod search;
pub mod subscriptions;
pub mod supported_dm_schema;
pub mod uci_backend;

#[cfg(test)]
pub mod tests;

// Re-export commonly used items
pub use add_delete::{AddResult, DeleteResult, handle_add, handle_delete};
pub use error_codes::ErrorCode;
pub use get_instances::handle_get_instances;
pub use get_supported_dm::handle_get_supported_dm;
pub use notifications::{NotificationManager, NotificationType};
pub use search::{matches_wildcard, matches_search_expression, resolve_search_path};
pub use subscriptions::{Subscription, SubscriptionManager};
pub use uci_backend::{UciResult, 
    add_dhcp_lease, delete_dhcp_lease, 
    add_wifi_interface, delete_wifi_interface, 
    add_static_host, delete_static_host,
    set_system_hostname, get_system_hostname,
    add_network_interface, delete_network_interface, update_network_interface_param,
    set_system_timezone, set_system_log_size
};
