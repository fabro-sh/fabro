pub mod keys;

pub use fabro_core::Context;

use fabro_graphviz::Fidelity;

/// Domain-specific typed accessors for workflow context values.
pub trait WorkflowContext {
    fn fidelity(&self) -> Fidelity;
    fn thread_id(&self) -> Option<String>;
    fn preamble(&self) -> String;
    fn run_id(&self) -> String;
}

impl WorkflowContext for Context {
    fn fidelity(&self) -> Fidelity {
        self.get_string(keys::INTERNAL_FIDELITY, "")
            .parse()
            .unwrap_or_default()
    }

    fn thread_id(&self) -> Option<String> {
        self.get(keys::INTERNAL_THREAD_ID)
            .and_then(|v| v.as_str().map(String::from))
    }

    fn preamble(&self) -> String {
        self.get_string(keys::CURRENT_PREAMBLE, "")
    }

    fn run_id(&self) -> String {
        self.get_string(keys::INTERNAL_RUN_ID, "unknown")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn new_context_is_empty() {
        let ctx = Context::new();
        assert!(ctx.snapshot().is_empty());
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
    fn snapshot_is_independent() {
        let ctx = Context::new();
        ctx.set("a", serde_json::json!(1));
        let snap = ctx.snapshot();
        ctx.set("b", serde_json::json!(2));
        assert!(snap.contains_key("a"));
        assert!(!snap.contains_key("b"));
    }

    #[test]
    fn fork_is_independent() {
        let ctx = Context::new();
        ctx.set("shared", serde_json::json!("original"));

        let forked = ctx.fork();
        forked.set("shared", serde_json::json!("modified"));

        assert_eq!(ctx.get("shared"), Some(serde_json::json!("original")));
        assert_eq!(forked.get("shared"), Some(serde_json::json!("modified")));
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

    #[test]
    fn run_id_default() {
        let ctx = Context::new();
        assert_eq!(ctx.run_id(), "unknown");
    }

    #[test]
    fn run_id_set() {
        let ctx = Context::new();
        ctx.set(keys::INTERNAL_RUN_ID, serde_json::json!("abc-123"));
        assert_eq!(ctx.run_id(), "abc-123");
    }

    #[test]
    fn fidelity_default() {
        let ctx = Context::new();
        assert_eq!(ctx.fidelity(), keys::Fidelity::Compact);
    }

    #[test]
    fn fidelity_set() {
        let ctx = Context::new();
        ctx.set(keys::INTERNAL_FIDELITY, serde_json::json!("full"));
        assert_eq!(ctx.fidelity(), keys::Fidelity::Full);
    }

    #[test]
    fn preamble_default() {
        let ctx = Context::new();
        assert_eq!(ctx.preamble(), "");
    }

    #[test]
    fn preamble_set() {
        let ctx = Context::new();
        ctx.set(keys::CURRENT_PREAMBLE, serde_json::json!("hello"));
        assert_eq!(ctx.preamble(), "hello");
    }

    #[test]
    fn thread_id_default() {
        let ctx = Context::new();
        assert_eq!(ctx.thread_id(), None);
    }

    #[test]
    fn thread_id_null() {
        let ctx = Context::new();
        ctx.set(keys::INTERNAL_THREAD_ID, serde_json::Value::Null);
        assert_eq!(ctx.thread_id(), None);
    }

    #[test]
    fn thread_id_set() {
        let ctx = Context::new();
        ctx.set(keys::INTERNAL_THREAD_ID, serde_json::json!("main"));
        assert_eq!(ctx.thread_id(), Some("main".to_string()));
    }

    #[test]
    fn node_visit_count_default() {
        let ctx = Context::new();
        // fabro-core returns 0 for missing; workflow code expects 1 as default
        // when used in workflow context. The raw core accessor returns 0.
        assert_eq!(ctx.node_visit_count(), 0);
    }

    #[test]
    fn node_visit_count_set() {
        let ctx = Context::new();
        ctx.set(keys::INTERNAL_NODE_VISIT_COUNT, serde_json::json!(3));
        assert_eq!(ctx.node_visit_count(), 3);
    }

    #[test]
    fn current_node_id_default() {
        let ctx = Context::new();
        assert_eq!(ctx.current_node_id(), "");
    }

    #[test]
    fn current_node_id_set() {
        let ctx = Context::new();
        ctx.set(keys::CURRENT_NODE, serde_json::json!("plan"));
        assert_eq!(ctx.current_node_id(), "plan");
    }
}
