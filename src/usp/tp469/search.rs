//! TP-469 Search Path Resolution
//!
//! Implements path utilities per TR-369 §6.1.1

/// Extract instance number from path
/// Device.WiFi.SSID.1 -> 1
pub fn extract_instance_number(path: &str) -> Option<u32> {
    // Find the last numeric segment
    path.split('.').next_back()?.parse().ok()
}
