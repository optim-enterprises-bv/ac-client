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
pub mod search;
pub mod uci_backend;

#[cfg(test)]
pub mod tests;

// Re-export commonly used items
pub use add_delete::{handle_add, handle_delete, AddResult, DeleteResult};
pub use get_instances::handle_get_instances;
pub use get_supported_dm::handle_get_supported_dm;
