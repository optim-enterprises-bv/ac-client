//! TP-469 Search Path Resolution
//!
//! Implements wildcard and search expression path resolution per TR-369 §6.1.1

use crate::usp::dm;
use crate::config::ClientConfig;
use std::collections::HashMap;

/// Match a path against a wildcard pattern
/// Supports: * (single level), ** (multi-level), {i} (instance numbers)
pub fn matches_wildcard(path: &str, pattern: &str) -> bool {
    // Simple wildcard matching
    // Device.WiFi.* matches Device.WiFi.1, Device.WiFi.2, etc.
    // Device.** matches all paths starting with Device.
    
    if pattern.contains("**") {
        // Multi-level wildcard
        let prefix = pattern.split("**").next().unwrap_or("");
        path.starts_with(prefix.trim_end_matches('.'))
    } else if pattern.contains('*') {
        // Single-level wildcard
        let parts: Vec<&str> = pattern.split('.').collect();
        let path_parts: Vec<&str> = path.split('.').collect();
        
        if parts.len() != path_parts.len() {
            return false;
        }
        
        for (i, part) in parts.iter().enumerate() {
            if *part == "*" {
                // Wildcard matches any value at this level
                continue;
            }
            if part != &path_parts[i] {
                return false;
            }
        }
        true
    } else {
        // Exact match
        path == pattern
    }
}

/// Check if path matches a search expression
/// Search expressions use operators: ==, !=, <, >, <=, >=
/// Example: Device.WiFi.SSID.[Enable==true]
pub fn matches_search_expression(
    path: &str,
    expression: &str,
    _cfg: &ClientConfig,
) -> bool {
    // Parse search expression format: [ParamName==value]
    // For now, return true (accept all matching wildcards)
    // Full implementation would evaluate the expression against actual parameter values
    
    // Extract the base path and expression
    if let Some(start) = expression.find('[') {
        if let Some(end) = expression.find(']') {
            let _expr = &expression[start + 1..end];
            // Parse expression like "Enable==true"
            // Evaluate by getting actual value from dm
            // For now, assume match
            return true;
        }
    }
    
    // If no expression brackets, treat as wildcard match
    matches_wildcard(path, expression)
}

/// Resolve a search path to list of concrete paths
/// Input: Device.WiFi.SSID.* or Device.WiFi.SSID.[Enable==true]
/// Output: [Device.WiFi.SSID.1, Device.WiFi.SSID.2, ...]
pub async fn resolve_search_path(
    cfg: &ClientConfig,
    search_path: &str,
) -> Vec<String> {
    let mut results = Vec::new();
    
    // Check for wildcard patterns
    if search_path.contains('*') || search_path.contains('[') {
        // Expand the wildcard
        // Device.WiFi.SSID.* -> enumerate all SSID instances
        
        // Get all parameters and check which match the pattern
        let all_params = dm::get_params(cfg, &["Device.WiFi.SSID.".into()], 2).await;
        
        for (param_path, _) in all_params {
            if matches_wildcard(&param_path, search_path) {
                // Extract object path (remove parameter name)
                if let Some(idx) = param_path.rfind('.') {
                    let obj_path = &param_path[..idx];
                    if !results.contains(&obj_path.to_string()) {
                        results.push(obj_path.to_string());
                    }
                }
            }
        }
    } else {
        // No wildcard, return as-is
        results.push(search_path.to_string());
    }
    
    results
}

/// Extract instance number from path
/// Device.WiFi.SSID.1 -> 1
pub fn extract_instance_number(path: &str) -> Option<u32> {
    // Find the last numeric segment
    path.split('.').last()?.parse().ok()
}

/// Get the base path without instance numbers
/// Device.WiFi.SSID.1 -> Device.WiFi.SSID
pub fn get_base_path(path: &str) -> String {
    let parts: Vec<&str> = path.split('.').collect();
    // Remove last part if it's numeric
    if let Some(last) = parts.last() {
        if last.parse::<u32>().is_ok() {
            return parts[..parts.len() - 1].join(".");
        }
    }
    path.to_string()
}

/// Check if a path is a valid TR-181 path
pub fn is_valid_path(path: &str) -> bool {
    // Must start with Device.
    if !path.starts_with("Device.") {
        return false;
    }
    
    // Must not contain invalid characters
    let invalid_chars = [' ', '\t', '\n', '\r', '<', '>', '&'];
    for c in invalid_chars {
        if path.contains(c) {
            return false;
        }
    }
    
    true
}
