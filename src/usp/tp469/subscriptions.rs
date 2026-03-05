//! TP-469 Subscription Management
//!
//! Implements subscription creation/deletion per TR-369 §6.2.2

use std::collections::HashMap;

/// Subscription structure
#[derive(Debug, Clone)]
pub struct Subscription {
    pub id: String,
    pub notif_type: String, // ValueChange, Event, ObjectCreation, etc.
    pub path: String,
    pub enable: bool,
}

/// Subscription manager (stub - full implementation would persist to database)
pub struct SubscriptionManager {
    subscriptions: HashMap<String, Subscription>,
}

impl SubscriptionManager {
    pub fn new() -> Self {
        SubscriptionManager {
            subscriptions: HashMap::new(),
        }
    }
    
    /// Add a subscription
    pub fn add_subscription(&mut self, sub: Subscription) -> Result<(), String> {
        if self.subscriptions.contains_key(&sub.id) {
            return Err("Subscription already exists".into());
        }
        self.subscriptions.insert(sub.id.clone(), sub);
        Ok(())
    }
    
    /// Remove a subscription
    pub fn remove_subscription(&mut self, id: &str) -> Result<(), String> {
        if self.subscriptions.remove(id).is_none() {
            return Err("Subscription not found".into());
        }
        Ok(())
    }
    
    /// Get active subscriptions for a notification type
    pub fn get_active_subscriptions(&self, notif_type: &str) -> Vec<&Subscription> {
        self.subscriptions
            .values()
            .filter(|s| s.notif_type == notif_type && s.enable)
            .collect()
    }
}

impl Default for SubscriptionManager {
    fn default() -> Self {
        Self::new()
    }
}
