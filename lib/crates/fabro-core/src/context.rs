use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use serde_json::Value;

pub trait ContextStore: Send + Sync {
    fn set(&self, key: String, value: Value);
    fn get(&self, key: &str) -> Option<Value>;
    fn snapshot(&self) -> HashMap<String, Value>;
    fn fork(&self) -> Arc<dyn ContextStore>;
}

pub struct InMemoryStore {
    data: RwLock<HashMap<String, Value>>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self {
            data: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for InMemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ContextStore for InMemoryStore {
    fn set(&self, key: String, value: Value) {
        self.data.write().unwrap().insert(key, value);
    }

    fn get(&self, key: &str) -> Option<Value> {
        self.data.read().unwrap().get(key).cloned()
    }

    fn snapshot(&self) -> HashMap<String, Value> {
        self.data.read().unwrap().clone()
    }

    fn fork(&self) -> Arc<dyn ContextStore> {
        let cloned = self.data.read().unwrap().clone();
        Arc::new(InMemoryStore {
            data: RwLock::new(cloned),
        })
    }
}

#[derive(Clone)]
pub struct Context {
    store: Arc<dyn ContextStore>,
}

impl Default for Context {
    fn default() -> Self {
        Self::new()
    }
}

impl Context {
    pub fn new() -> Self {
        Self {
            store: Arc::new(InMemoryStore::new()),
        }
    }

    pub fn with_store(store: Arc<dyn ContextStore>) -> Self {
        Self { store }
    }

    pub fn set(&self, key: impl Into<String>, value: Value) {
        self.store.set(key.into(), value);
    }

    pub fn get(&self, key: &str) -> Option<Value> {
        self.store.get(key)
    }

    pub fn get_string(&self, key: &str, default: &str) -> String {
        self.get(key)
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_else(|| default.to_string())
    }

    pub fn apply_updates(&self, updates: &HashMap<String, Value>) {
        for (k, v) in updates {
            self.store.set(k.clone(), v.clone());
        }
    }

    pub fn snapshot(&self) -> HashMap<String, Value> {
        self.store.snapshot()
    }

    pub fn clone_context(&self) -> Self {
        Self {
            store: self.store.fork(),
        }
    }

    // Core typed accessors
    pub fn current_node_id(&self) -> String {
        self.get_string("current_node", "")
    }

    pub fn node_visit_count(&self) -> usize {
        self.get("internal.node_visit_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn in_memory_store_set_and_get() {
        let store = InMemoryStore::new();
        store.set("k".into(), json!("v"));
        assert_eq!(store.get("k"), Some(json!("v")));
        assert_eq!(store.get("missing"), None);
    }

    #[test]
    fn in_memory_store_snapshot_is_independent() {
        let store = InMemoryStore::new();
        store.set("a".into(), json!(1));
        let snap = store.snapshot();
        store.set("b".into(), json!(2));
        assert!(!snap.contains_key("b"));
        assert_eq!(snap.len(), 1);
    }

    #[test]
    fn in_memory_store_fork() {
        let store = InMemoryStore::new();
        store.set("x".into(), json!(10));
        let forked = store.fork();
        forked.set("y".into(), json!(20));
        assert!(store.get("y").is_none());
        assert_eq!(forked.get("x"), Some(json!(10)));
        assert_eq!(forked.get("y"), Some(json!(20)));
    }

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
    fn context_clone_is_independent() {
        let ctx = Context::new();
        ctx.set("x", json!(1));
        let cloned = ctx.clone_context();
        cloned.set("x", json!(2));
        assert_eq!(ctx.get("x"), Some(json!(1)));
        assert_eq!(cloned.get("x"), Some(json!(2)));
    }

    #[test]
    fn context_with_custom_store() {
        struct CountingStore {
            inner: InMemoryStore,
            set_count: AtomicUsize,
        }
        impl ContextStore for CountingStore {
            fn set(&self, key: String, value: Value) {
                self.set_count.fetch_add(1, Ordering::Relaxed);
                self.inner.set(key, value);
            }
            fn get(&self, key: &str) -> Option<Value> {
                self.inner.get(key)
            }
            fn snapshot(&self) -> HashMap<String, Value> {
                self.inner.snapshot()
            }
            fn fork(&self) -> Arc<dyn ContextStore> {
                self.inner.fork()
            }
        }

        let store = Arc::new(CountingStore {
            inner: InMemoryStore::new(),
            set_count: AtomicUsize::new(0),
        });
        let ctx = Context::with_store(store.clone());
        ctx.set("k", json!(1));
        ctx.set("k2", json!(2));
        assert_eq!(store.set_count.load(Ordering::Relaxed), 2);
        assert_eq!(ctx.get("k"), Some(json!(1)));
    }

    #[test]
    fn context_fork_is_independent() {
        let ctx = Context::new();
        ctx.set("shared", json!("original"));
        let forked = ctx.clone_context();
        forked.set("shared", json!("modified"));
        assert_eq!(ctx.get("shared"), Some(json!("original")));
        assert_eq!(forked.get("shared"), Some(json!("modified")));
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
