use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use serde_json::Value;

/// Thread-safe key-value context shared across pipeline stages.
#[derive(Debug, Clone)]
pub struct Context {
    values: Arc<RwLock<HashMap<String, Value>>>,
    logs: Arc<RwLock<Vec<String>>>,
}

impl Default for Context {
    fn default() -> Self {
        Self::new()
    }
}

impl Context {
    #[must_use]
    pub fn new() -> Self {
        Self {
            values: Arc::new(RwLock::new(HashMap::new())),
            logs: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Set a key-value pair in the context.
    ///
    /// # Panics
    ///
    /// Panics if the internal lock is poisoned.
    pub fn set(&self, key: impl Into<String>, value: Value) {
        self.values
            .write()
            .expect("context lock poisoned")
            .insert(key.into(), value);
    }

    /// Get a value by key, returning None if not present.
    ///
    /// # Panics
    ///
    /// Panics if the internal lock is poisoned.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<Value> {
        self.values
            .read()
            .expect("context lock poisoned")
            .get(key)
            .cloned()
    }

    /// Get a value as a string, returning the default if not present or not a string.
    #[must_use]
    pub fn get_string(&self, key: &str, default: &str) -> String {
        self.get(key)
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_else(|| default.to_string())
    }

    /// Append a log entry.
    ///
    /// # Panics
    ///
    /// Panics if the internal lock is poisoned.
    pub fn append_log(&self, entry: impl Into<String>) {
        self.logs
            .write()
            .expect("context lock poisoned")
            .push(entry.into());
    }

    /// Return a snapshot (clone) of all current context values.
    ///
    /// # Panics
    ///
    /// Panics if the internal lock is poisoned.
    #[must_use]
    pub fn snapshot(&self) -> HashMap<String, Value> {
        self.values.read().expect("context lock poisoned").clone()
    }

    /// Return a snapshot of the logs.
    ///
    /// # Panics
    ///
    /// Panics if the internal lock is poisoned.
    #[must_use]
    pub fn logs_snapshot(&self) -> Vec<String> {
        self.logs.read().expect("context lock poisoned").clone()
    }

    /// Deep copy for parallel branch isolation.
    #[must_use]
    pub fn clone_context(&self) -> Self {
        let values = self.snapshot();
        let logs = self.logs_snapshot();
        Self {
            values: Arc::new(RwLock::new(values)),
            logs: Arc::new(RwLock::new(logs)),
        }
    }

    /// Merge a map of updates into the context.
    ///
    /// # Panics
    ///
    /// Panics if the internal lock is poisoned.
    pub fn apply_updates(&self, updates: &HashMap<String, Value>) {
        let mut values = self.values.write().expect("context lock poisoned");
        for (key, value) in updates {
            values.insert(key.clone(), value.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_context_is_empty() {
        let ctx = Context::new();
        assert!(ctx.snapshot().is_empty());
        assert!(ctx.logs_snapshot().is_empty());
    }

    #[test]
    fn set_and_get() {
        let ctx = Context::new();
        ctx.set("key", serde_json::json!("value"));
        assert_eq!(ctx.get("key"), Some(serde_json::json!("value")));
    }

    #[test]
    fn get_missing_key() {
        let ctx = Context::new();
        assert_eq!(ctx.get("missing"), None);
    }

    #[test]
    fn get_string_with_value() {
        let ctx = Context::new();
        ctx.set("name", serde_json::json!("alice"));
        assert_eq!(ctx.get_string("name", "default"), "alice");
    }

    #[test]
    fn get_string_missing_key() {
        let ctx = Context::new();
        assert_eq!(ctx.get_string("missing", "fallback"), "fallback");
    }

    #[test]
    fn get_string_non_string_value() {
        let ctx = Context::new();
        ctx.set("num", serde_json::json!(42));
        assert_eq!(ctx.get_string("num", "default"), "default");
    }

    #[test]
    fn append_and_snapshot_logs() {
        let ctx = Context::new();
        ctx.append_log("first entry");
        ctx.append_log("second entry");
        let logs = ctx.logs_snapshot();
        assert_eq!(logs.len(), 2);
        assert_eq!(logs[0], "first entry");
        assert_eq!(logs[1], "second entry");
    }

    #[test]
    fn snapshot_is_independent() {
        let ctx = Context::new();
        ctx.set("a", serde_json::json!(1));
        let snap = ctx.snapshot();
        ctx.set("b", serde_json::json!(2));
        // snapshot should not contain "b"
        assert!(snap.contains_key("a"));
        assert!(!snap.contains_key("b"));
    }

    #[test]
    fn clone_context_is_independent() {
        let ctx = Context::new();
        ctx.set("shared", serde_json::json!("original"));
        ctx.append_log("log1");

        let cloned = ctx.clone_context();
        cloned.set("shared", serde_json::json!("modified"));
        cloned.append_log("log2");

        // original should be unchanged
        assert_eq!(ctx.get("shared"), Some(serde_json::json!("original")));
        assert_eq!(ctx.logs_snapshot().len(), 1);

        // cloned has the modification
        assert_eq!(cloned.get("shared"), Some(serde_json::json!("modified")));
        assert_eq!(cloned.logs_snapshot().len(), 2);
    }

    #[test]
    fn apply_updates() {
        let ctx = Context::new();
        ctx.set("existing", serde_json::json!("old"));

        let mut updates = HashMap::new();
        updates.insert("existing".to_string(), serde_json::json!("new"));
        updates.insert("added".to_string(), serde_json::json!(true));
        ctx.apply_updates(&updates);

        assert_eq!(ctx.get("existing"), Some(serde_json::json!("new")));
        assert_eq!(ctx.get("added"), Some(serde_json::json!(true)));
    }

    #[test]
    fn default_creates_empty_context() {
        let ctx = Context::default();
        assert!(ctx.snapshot().is_empty());
    }
}
