pub mod keys {
    //! Static context key constants and helper functions for dynamic keys.
    //!
    //! All context keys used across the engine, handlers, and preamble are
    //! defined here to prevent typos and improve discoverability.

    // --- Top-level keys ---
    pub const CURRENT_NODE: &str = "current_node";
    pub const OUTCOME: &str = "outcome";
    pub const FAILURE_CLASS: &str = "failure_class";
    pub const FAILURE_SIGNATURE: &str = "failure_signature";
    pub const PREFERRED_LABEL: &str = "preferred_label";
    pub const LAST_STAGE: &str = "last_stage";
    pub const LAST_RESPONSE: &str = "last_response";

    // --- graph.* keys ---
    pub const GRAPH_GOAL: &str = "graph.goal";

    // --- internal.* keys ---
    pub const INTERNAL_RUN_ID: &str = "internal.run_id";
    pub const INTERNAL_WORK_DIR: &str = "internal.work_dir";
    pub const INTERNAL_FIDELITY: &str = "internal.fidelity";
    pub const INTERNAL_THREAD_ID: &str = "internal.thread_id";
    pub const INTERNAL_NODE_VISIT_COUNT: &str = "internal.node_visit_count";
    pub const INTERNAL_PARENT_PREAMBLE: &str = "internal.parent_preamble";
    pub const INTERNAL_PARALLEL_GROUP_ID: &str = "internal.parallel_group_id";
    pub const INTERNAL_PARALLEL_BRANCH_ID: &str = "internal.parallel_branch_id";

    // --- current.* keys ---
    pub const CURRENT_PREAMBLE: &str = "current.preamble";

    // --- command.* keys ---
    pub const COMMAND_OUTPUT: &str = "command.output";
    pub const COMMAND_STDERR: &str = "command.stderr";

    // --- human.gate.* keys ---
    pub const HUMAN_GATE_SELECTED: &str = "human.gate.selected";
    pub const HUMAN_GATE_LABEL: &str = "human.gate.label";
    pub const HUMAN_GATE_TEXT: &str = "human.gate.text";

    // --- parallel.* keys ---
    pub const PARALLEL_RESULTS: &str = "parallel.results";
    pub const PARALLEL_BRANCH_COUNT: &str = "parallel.branch_count";
    pub const PARALLEL_FAN_IN_BEST_ID: &str = "parallel.fan_in.best_id";
    pub const PARALLEL_FAN_IN_BEST_OUTCOME: &str = "parallel.fan_in.best_outcome";
    pub const PARALLEL_FAN_IN_BEST_HEAD_SHA: &str = "parallel.fan_in.best_head_sha";

    // --- Prefix constants (for filtering and dynamic keys) ---
    pub const GRAPH_PREFIX: &str = "graph.";
    pub const INTERNAL_PREFIX: &str = "internal.";
    pub const CURRENT_PREFIX: &str = "current";
    pub const THREAD_PREFIX: &str = "thread.";
    pub const RESPONSE_PREFIX: &str = "response.";
    pub const INTERNAL_RETRY_COUNT_PREFIX: &str = "internal.retry_count.";

    // --- Helper functions for dynamic keys ---

    #[must_use]
    pub fn response_key(node_id: &str) -> String {
        format!("{RESPONSE_PREFIX}{node_id}")
    }

    #[must_use]
    pub fn thread_current_node_key(thread_id: &str) -> String {
        format!("{THREAD_PREFIX}{thread_id}.current_node")
    }

    #[must_use]
    pub fn graph_attr_key(attr: &str) -> String {
        format!("{GRAPH_PREFIX}{attr}")
    }

    #[must_use]
    pub fn retry_count_key(node_id: &str) -> String {
        format!("{INTERNAL_RETRY_COUNT_PREFIX}{node_id}")
    }

    /// Returns `true` for engine-internal keys that should not propagate from child
    /// to parent workflow contexts.
    #[must_use]
    pub fn is_engine_internal_key(key: &str) -> bool {
        key.starts_with(INTERNAL_PREFIX)
            || key.starts_with(GRAPH_PREFIX)
            || key.starts_with(THREAD_PREFIX)
            || key.starts_with(CURRENT_PREFIX)
    }

    pub use fabro_graphviz::Fidelity;

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn response_key_formats_correctly() {
            assert_eq!(response_key("plan"), "response.plan");
        }

        #[test]
        fn thread_current_node_key_formats_correctly() {
            assert_eq!(thread_current_node_key("main"), "thread.main.current_node");
        }

        #[test]
        fn graph_attr_key_formats_correctly() {
            assert_eq!(graph_attr_key("goal"), "graph.goal");
        }

        #[test]
        fn retry_count_key_formats_correctly() {
            assert_eq!(retry_count_key("plan"), "internal.retry_count.plan");
        }

        #[test]
        fn is_engine_internal_key_classifies_correctly() {
            // Keys that ARE engine-internal (should not propagate)
            assert!(is_engine_internal_key("internal.run_id"));
            assert!(is_engine_internal_key("internal.fidelity"));
            assert!(is_engine_internal_key("internal.parent_preamble"));
            assert!(is_engine_internal_key("graph.goal"));
            assert!(is_engine_internal_key("thread.main.current_node"));
            assert!(is_engine_internal_key("current.preamble"));
            assert!(is_engine_internal_key("current_node"));

            // Keys that are NOT engine-internal (should propagate)
            assert!(!is_engine_internal_key("response.plan"));
            assert!(!is_engine_internal_key("command.output"));
            assert!(!is_engine_internal_key("outcome"));
            assert!(!is_engine_internal_key("last_stage"));
            assert!(!is_engine_internal_key("review.result"));
            assert!(!is_engine_internal_key("user.name"));
        }
    }
}

pub use fabro_core::Context;

use fabro_graphviz::Fidelity;

/// Domain-specific typed accessors for workflow context values.
pub trait WorkflowContext {
    fn fidelity(&self) -> Fidelity;
    fn thread_id(&self) -> Option<String>;
    fn preamble(&self) -> String;
    fn run_id(&self) -> String;
    fn parallel_group_id(&self) -> Option<String>;
    fn parallel_branch_id(&self) -> Option<String>;
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

    fn parallel_group_id(&self) -> Option<String> {
        self.get(keys::INTERNAL_PARALLEL_GROUP_ID)
            .and_then(|value| value.as_str().map(String::from))
    }

    fn parallel_branch_id(&self) -> Option<String> {
        self.get(keys::INTERNAL_PARALLEL_BRANCH_ID)
            .and_then(|value| value.as_str().map(String::from))
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
    fn parallel_ids_default() {
        let ctx = Context::new();
        assert_eq!(ctx.parallel_group_id(), None);
        assert_eq!(ctx.parallel_branch_id(), None);
    }

    #[test]
    fn parallel_ids_set() {
        let ctx = Context::new();
        ctx.set(
            keys::INTERNAL_PARALLEL_GROUP_ID,
            serde_json::json!("fanout@2"),
        );
        ctx.set(
            keys::INTERNAL_PARALLEL_BRANCH_ID,
            serde_json::json!("fanout@2:1"),
        );
        assert_eq!(ctx.parallel_group_id(), Some("fanout@2".to_string()));
        assert_eq!(ctx.parallel_branch_id(), Some("fanout@2:1".to_string()));
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
