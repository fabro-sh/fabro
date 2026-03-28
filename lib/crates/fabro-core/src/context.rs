use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use serde_json::Value;

#[derive(Clone, Default)]
pub struct Context {
    values: Arc<RwLock<HashMap<String, Value>>>,
}

impl Context {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_values(values: HashMap<String, Value>) -> Self {
        Self {
            values: Arc::new(RwLock::new(values)),
        }
    }

    pub fn set(&self, key: impl Into<String>, value: Value) {
        self.values.write().unwrap().insert(key.into(), value);
    }

    pub fn get(&self, key: &str) -> Option<Value> {
        self.values.read().unwrap().get(key).cloned()
    }

    pub fn get_string(&self, key: &str, default: &str) -> String {
        self.get(key)
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_else(|| default.to_string())
    }

    pub fn apply_updates(&self, updates: &HashMap<String, Value>) {
        let mut values = self.values.write().unwrap();
        for (k, v) in updates {
            values.insert(k.clone(), v.clone());
        }
    }

    pub fn snapshot(&self) -> HashMap<String, Value> {
        self.values.read().unwrap().clone()
    }

    /// Deep copy for parallel branch isolation.
    /// `.clone()` shares state (Arc clone); `.fork()` creates an independent copy.
    #[must_use]
    pub fn fork(&self) -> Self {
        Self {
            values: Arc::new(RwLock::new(self.snapshot())),
        }
    }

    // Core typed accessors
    pub fn current_node_id(&self) -> String {
        self.get_string("current_node", "")
    }

    /// Returns the raw stored node visit count.
    ///
    /// This is `0` when the workflow lifecycle has not yet seeded
    /// `internal.node_visit_count` into the context.
    pub fn node_visit_count(&self) -> usize {
        self.get("internal.node_visit_count")
            .and_then(|v| v.as_u64())
            .map_or(0, |v| usize::try_from(v).unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn context_set_and_get() {
        let ctx = Context::new();
        ctx.set("name", json!("test"));
        assert_eq!(ctx.get("name"), Some(json!("test")));
    }

    #[test]
    fn context_get_missing_returns_none() {
        let ctx = Context::new();
        assert_eq!(ctx.get("nope"), None);
    }

    #[test]
    fn context_get_string_with_default() {
        let ctx = Context::new();
        assert_eq!(ctx.get_string("missing", "fallback"), "fallback");
        ctx.set("present", json!("value"));
        assert_eq!(ctx.get_string("present", "fallback"), "value");
    }

    #[test]
    fn context_apply_updates() {
        let ctx = Context::new();
        let mut updates = HashMap::new();
        updates.insert("a".into(), json!(1));
        updates.insert("b".into(), json!(2));
        ctx.apply_updates(&updates);
        assert_eq!(ctx.get("a"), Some(json!(1)));
        assert_eq!(ctx.get("b"), Some(json!(2)));
    }

    #[test]
    fn context_fork_is_independent() {
        let ctx = Context::new();
        ctx.set("shared", json!("original"));
        let forked = ctx.fork();
        forked.set("shared", json!("modified"));
        assert_eq!(ctx.get("shared"), Some(json!("original")));
        assert_eq!(forked.get("shared"), Some(json!("modified")));
    }

    #[test]
    fn context_from_values() {
        let mut vals = HashMap::new();
        vals.insert("k".into(), json!("v"));
        let ctx = Context::from_values(vals);
        assert_eq!(ctx.get("k"), Some(json!("v")));
    }

    #[test]
    fn context_current_node_id() {
        let ctx = Context::new();
        assert_eq!(ctx.current_node_id(), "");
        ctx.set("current_node", json!("node_5"));
        assert_eq!(ctx.current_node_id(), "node_5");
    }

    #[test]
    fn context_node_visit_count() {
        let ctx = Context::new();
        assert_eq!(ctx.node_visit_count(), 0);
        ctx.set("internal.node_visit_count", json!(3));
        assert_eq!(ctx.node_visit_count(), 3);
    }
}
