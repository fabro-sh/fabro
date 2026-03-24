use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use fabro_core::context::{Context as CoreContext, ContextStore};
use serde_json::Value;

use crate::context::keys;
use crate::context::Context as WfContext;

/// A ContextStore implementation that delegates to a wf::Context's internal values map.
struct WfContextStore {
    values: Arc<RwLock<HashMap<String, Value>>>,
}

impl ContextStore for WfContextStore {
    fn set(&self, key: String, value: Value) {
        self.values
            .write()
            .expect("context lock poisoned")
            .insert(key, value);
    }

    fn get(&self, key: &str) -> Option<Value> {
        self.values
            .read()
            .expect("context lock poisoned")
            .get(key)
            .cloned()
    }

    fn snapshot(&self) -> HashMap<String, Value> {
        self.values.read().expect("context lock poisoned").clone()
    }

    fn fork(&self) -> Arc<dyn ContextStore> {
        let cloned = self.values.read().expect("context lock poisoned").clone();
        Arc::new(WfContextStore {
            values: Arc::new(RwLock::new(cloned)),
        })
    }
}

/// Create a fabro_core::Context that shares the same underlying values
/// as the given wf::Context. Writes through either are visible to both.
pub fn bridge_context(wf_ctx: &WfContext) -> CoreContext {
    let store = Arc::new(WfContextStore {
        values: wf_ctx.values_arc(),
    });
    CoreContext::with_store(store)
}

/// Extension trait providing typed domain accessors on a fabro_core::Context.
pub trait WorkflowContextExt {
    fn run_id(&self) -> String;
    fn fidelity(&self) -> keys::Fidelity;
    fn preamble(&self) -> String;
    fn thread_id(&self) -> Option<String>;
}

impl WorkflowContextExt for CoreContext {
    fn run_id(&self) -> String {
        self.get_string(keys::INTERNAL_RUN_ID, "unknown")
    }

    fn fidelity(&self) -> keys::Fidelity {
        self.get_string(keys::INTERNAL_FIDELITY, "")
            .parse()
            .unwrap_or_default()
    }

    fn preamble(&self) -> String {
        self.get_string(keys::CURRENT_PREAMBLE, "")
    }

    fn thread_id(&self) -> Option<String> {
        self.get(keys::INTERNAL_THREAD_ID)
            .and_then(|v| v.as_str().map(String::from))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bridge_shares_values() {
        let wf = WfContext::new();
        let core = bridge_context(&wf);

        // Set via wf, read via core
        wf.set("key1", json!("from_wf"));
        assert_eq!(core.get("key1"), Some(json!("from_wf")));

        // Set via core, read via wf
        core.set("key2", json!("from_core"));
        assert_eq!(wf.get("key2"), Some(json!("from_core")));
    }

    #[test]
    fn bridge_fork_is_independent() {
        let wf = WfContext::new();
        wf.set("shared", json!("original"));
        let core = bridge_context(&wf);
        let forked = core.clone_context();

        // Write to fork should not affect original
        forked.set("shared", json!("modified"));
        assert_eq!(wf.get("shared"), Some(json!("original")));
        assert_eq!(core.get("shared"), Some(json!("original")));
        assert_eq!(forked.get("shared"), Some(json!("modified")));
    }

    #[test]
    fn workflow_context_ext_accessors() {
        let wf = WfContext::new();
        wf.set(keys::INTERNAL_RUN_ID, json!("run-42"));
        wf.set(keys::INTERNAL_FIDELITY, json!("full"));
        wf.set(keys::CURRENT_PREAMBLE, json!("You are a helpful assistant"));
        wf.set(keys::INTERNAL_THREAD_ID, json!("thread-1"));

        let core = bridge_context(&wf);
        assert_eq!(core.run_id(), "run-42");
        assert_eq!(core.fidelity(), keys::Fidelity::Full);
        assert_eq!(core.preamble(), "You are a helpful assistant");
        assert_eq!(core.thread_id(), Some("thread-1".to_string()));
    }

    #[test]
    fn workflow_context_ext_defaults() {
        let core = CoreContext::new();
        assert_eq!(core.run_id(), "unknown");
        assert_eq!(core.fidelity(), keys::Fidelity::Compact);
        assert_eq!(core.preamble(), "");
        assert_eq!(core.thread_id(), None);
    }
}
