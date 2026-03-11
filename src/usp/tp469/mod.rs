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
mod notifications;
mod search;
mod subscriptions;
pub mod supported_dm_schema;
pub mod uci_backend;

#[cfg(test)]
pub mod tests;

// Re-export commonly used items
pub use add_delete::{AddResult, DeleteResult, handle_add, handle_delete};
pub use get_instances::handle_get_instances;
pub use get_supported_dm::handle_get_supported_dm;
