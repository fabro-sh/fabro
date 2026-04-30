use std::collections::HashMap;

use crate::context::Context;
use crate::error::Result;
use crate::graph::{Graph, NodeSpec};
use crate::outcome::{NodeResult, Outcome, OutcomeMeta};

impl<M: OutcomeMeta> std::fmt::Debug for ExecutionState<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecutionState")
            .field("current_node_id", &self.current_node_id)
            .field("completed_nodes", &self.completed_nodes)
            .field("stage_index", &self.stage_index)
            .field("cancelled", &self.cancelled)
            .finish_non_exhaustive()
    }
}

pub struct ExecutionState<M: OutcomeMeta = ()> {
    pub context:          Context,
    pub current_node_id:  String,
    pub completed_nodes:  Vec<String>,
    pub node_outcomes:    HashMap<String, Outcome<M>>,
    pub node_retries:     HashMap<String, u32>,
    pub node_visits:      HashMap<String, usize>,
    pub stage_index:      usize,
    pub previous_node_id: Option<String>,
    pub cancelled:        bool,
}

impl<M: OutcomeMeta> ExecutionState<M> {
    pub fn new<G: Graph>(graph: &G) -> Result<Self> {
        let start = graph.find_start_node()?;
        Ok(Self {
            context:          Context::new(),
            current_node_id:  start.id().to_string(),
            completed_nodes:  Vec::new(),
            node_outcomes:    HashMap::new(),
            node_retries:     HashMap::new(),
            node_visits:      HashMap::new(),
            stage_index:      0,
            previous_node_id: None,
            cancelled:        false,
        })
    }

    pub fn record(&mut self, node_id: &str, result: &NodeResult<M>) {
        self.completed_nodes.push(node_id.to_string());
        self.node_outcomes
            .insert(node_id.to_string(), result.outcome.clone());
        if result.attempts > 1 {
            self.node_retries
                .insert(node_id.to_string(), result.attempts - 1);
        }
        self.stage_index += 1;
        self.context.apply_updates(&result.outcome.context_updates);
    }

    pub fn advance(&mut self, next_node_id: &str) {
        self.previous_node_id = Some(self.current_node_id.clone());
        self.current_node_id = next_node_id.to_string();
    }

    pub fn restart(&mut self, start_node_id: &str, new_context: Option<Context>) {
        self.current_node_id = start_node_id.to_string();
        self.completed_nodes.clear();
        self.node_outcomes.clear();
        self.node_retries.clear();
        self.stage_index = 0;
        self.previous_node_id = None;
        if let Some(ctx) = new_context {
            self.context = ctx;
        }
        // node_visits is NOT cleared — preserves total visit counts across
        // restarts
    }

    pub fn current_node<G: Graph>(&self, graph: &G) -> Option<G::Node> {
        graph.get_node(&self.current_node_id)
    }

    pub fn increment_visits(&mut self, node_id: &str) -> usize {
        let count = self.node_visits.entry(node_id.to_string()).or_insert(0);
        *count += 1;
        *count
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::*;
    use crate::outcome::{Outcome, StageOutcome};
    use crate::test_fixtures::linear_graph;

    #[test]
    fn run_state_new_from_graph() {
        let g = linear_graph(&["start", "work", "end"]);
        let state = ExecutionState::<()>::new(&g).unwrap();
        assert_eq!(state.current_node_id, "start");
        assert!(state.completed_nodes.is_empty());
        assert!(state.node_outcomes.is_empty());
        assert_eq!(state.stage_index, 0);
        assert!(state.previous_node_id.is_none());
    }

    #[test]
    fn run_state_record_updates_all_fields() {
        let g = linear_graph(&["start", "end"]);
        let mut state = ExecutionState::<()>::new(&g).unwrap();
        let result = NodeResult::new(Outcome::success(), Duration::from_millis(50), 2, 3);
        state.record("start", &result);

        assert_eq!(state.completed_nodes, vec!["start"]);
        assert_eq!(state.node_outcomes["start"].status, StageOutcome::Succeeded);
        assert_eq!(state.node_retries["start"], 1); // 2 attempts - 1
        assert_eq!(state.stage_index, 1);
    }

    #[test]
    fn run_state_record_applies_context_updates() {
        let g = linear_graph(&["start", "end"]);
        let mut state = ExecutionState::<()>::new(&g).unwrap();
        let mut outcome = Outcome::success();
        outcome.context_updates.insert("key".into(), json!("value"));
        let result = NodeResult::new(outcome, Duration::ZERO, 1, 1);
        state.record("start", &result);
        assert_eq!(state.context.get("key"), Some(json!("value")));
    }

    #[test]
    fn run_state_advance_updates_current_and_previous() {
        let g = linear_graph(&["start", "mid", "end"]);
        let mut state = ExecutionState::<()>::new(&g).unwrap();
        assert_eq!(state.current_node_id, "start");
        assert!(state.previous_node_id.is_none());

        state.advance("mid");
        assert_eq!(state.current_node_id, "mid");
        assert_eq!(state.previous_node_id.as_deref(), Some("start"));

        state.advance("end");
        assert_eq!(state.current_node_id, "end");
        assert_eq!(state.previous_node_id.as_deref(), Some("mid"));
    }

    #[test]
    fn run_state_restart_clears_progress_keeps_visits() {
        let g = linear_graph(&["start", "work", "end"]);
        let mut state = ExecutionState::<()>::new(&g).unwrap();
        state.increment_visits("start");
        state.increment_visits("work");
        state.record(
            "start",
            &NodeResult::new(Outcome::success(), Duration::ZERO, 1, 1),
        );
        state.advance("work");

        state.restart("start", None);

        assert_eq!(state.current_node_id, "start");
        assert!(state.completed_nodes.is_empty());
        assert!(state.node_outcomes.is_empty());
        assert!(state.node_retries.is_empty());
        assert_eq!(state.stage_index, 0);
        assert!(state.previous_node_id.is_none());
        // visits preserved
        assert_eq!(state.node_visits["start"], 1);
        assert_eq!(state.node_visits["work"], 1);
    }

    #[test]
    fn run_state_current_node_from_graph() {
        let g = linear_graph(&["start", "end"]);
        let state = ExecutionState::<()>::new(&g).unwrap();
        let node = state.current_node(&g).unwrap();
        assert_eq!(node.id(), "start");
    }

    #[test]
    fn run_state_increment_visits() {
        let g = linear_graph(&["start", "end"]);
        let mut state = ExecutionState::<()>::new(&g).unwrap();
        assert_eq!(state.increment_visits("start"), 1);
        assert_eq!(state.increment_visits("start"), 2);
        assert_eq!(state.increment_visits("other"), 1);
    }

    #[test]
    fn run_state_restart_with_new_context() {
        let g = linear_graph(&["start", "end"]);
        let mut state = ExecutionState::<()>::new(&g).unwrap();
        state.context.set("key", json!("old_value"));
        state.increment_visits("start");

        let new_ctx = Context::new();
        new_ctx.set("fresh", json!(true));
        state.restart("start", Some(new_ctx));

        // Old context key is gone
        assert!(state.context.get("key").is_none());
        // New context key is present
        assert_eq!(state.context.get("fresh"), Some(json!(true)));
        // Visits preserved
        assert_eq!(state.node_visits["start"], 1);
    }

    #[test]
    fn run_state_restart_without_context_preserves() {
        let g = linear_graph(&["start", "end"]);
        let mut state = ExecutionState::<()>::new(&g).unwrap();
        state.context.set("key", json!("value"));

        state.restart("start", None);

        // Context preserved when None passed
        assert_eq!(state.context.get("key"), Some(json!("value")));
    }
}
