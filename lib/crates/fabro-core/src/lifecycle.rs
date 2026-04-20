use std::time::Duration;

use async_trait::async_trait;

use crate::error::Result;
use crate::graph::Graph;
use crate::outcome::{NodeResult, Outcome, OutcomeMeta};
use crate::state::ExecutionState;

#[derive(Debug, Clone)]
pub enum NodeDecision<M: OutcomeMeta = ()> {
    Continue,
    Skip(Box<Outcome<M>>),
    Block(String),
}

#[derive(Debug, Clone)]
pub enum EdgeDecision {
    Continue,
    Override(String),
    Block(String),
}

pub struct AttemptContext<'a, G: Graph> {
    pub node:         &'a G::Node,
    pub attempt:      u32,
    pub max_attempts: u32,
}

pub struct AttemptResultContext<'a, G: Graph> {
    pub node:          &'a G::Node,
    pub result:        &'a NodeResult<G::Meta>,
    pub attempt:       u32,
    pub will_retry:    bool,
    pub backoff_delay: Option<Duration>,
}

pub struct EdgeContext<'a, G: Graph> {
    pub from:    &'a str,
    pub to:      &'a str,
    pub edge:    Option<G::Edge>,
    pub is_jump: bool,
    pub outcome: &'a Outcome<G::Meta>,
    pub reason:  &'a str,
}

#[async_trait]
pub trait RunLifecycle<G: Graph>: Send + Sync {
    async fn on_run_start(&self, _graph: &G, _state: &ExecutionState<G::Meta>) -> Result<()> {
        Ok(())
    }

    async fn on_terminal_reached(
        &self,
        _node: &G::Node,
        _goal_gates_passed: bool,
        _state: &ExecutionState<G::Meta>,
    ) {
    }

    async fn before_node(
        &self,
        _node: &G::Node,
        _state: &ExecutionState<G::Meta>,
    ) -> Result<NodeDecision<G::Meta>> {
        Ok(NodeDecision::Continue)
    }

    async fn before_attempt(
        &self,
        _ctx: &AttemptContext<'_, G>,
        _state: &ExecutionState<G::Meta>,
    ) -> Result<NodeDecision<G::Meta>> {
        Ok(NodeDecision::Continue)
    }

    async fn after_attempt(
        &self,
        _ctx: &AttemptResultContext<'_, G>,
        _state: &ExecutionState<G::Meta>,
    ) -> Result<()> {
        Ok(())
    }

    async fn after_node(
        &self,
        _node: &G::Node,
        _result: &mut NodeResult<G::Meta>,
        _state: &ExecutionState<G::Meta>,
    ) -> Result<()> {
        Ok(())
    }

    async fn after_record(
        &self,
        _node: &G::Node,
        _result: &NodeResult<G::Meta>,
        _state: &ExecutionState<G::Meta>,
    ) -> Result<()> {
        Ok(())
    }

    async fn on_edge_selected(
        &self,
        _ctx: &EdgeContext<'_, G>,
        _state: &ExecutionState<G::Meta>,
    ) -> Result<EdgeDecision> {
        Ok(EdgeDecision::Continue)
    }

    async fn on_checkpoint(
        &self,
        _node: &G::Node,
        _result: &NodeResult<G::Meta>,
        _next_node_id: Option<&str>,
        _state: &ExecutionState<G::Meta>,
    ) -> Result<()> {
        Ok(())
    }

    async fn on_run_end(&self, _outcome: &Outcome<G::Meta>, _state: &ExecutionState<G::Meta>) {}
}

/// No-op lifecycle that passes through everything.
pub struct NoopLifecycle;

#[async_trait]
impl<G: Graph> RunLifecycle<G> for NoopLifecycle {}

/// Composes multiple lifecycles, calling them in order. Useful for testing
/// and simple use cases where fixed ordering suffices.
pub struct CompositeLifecycle<G: Graph> {
    children: Vec<Box<dyn RunLifecycle<G>>>,
}

impl<G: Graph> CompositeLifecycle<G> {
    pub fn new(children: Vec<Box<dyn RunLifecycle<G>>>) -> Self {
        Self { children }
    }
}

#[async_trait]
impl<G: Graph + 'static> RunLifecycle<G> for CompositeLifecycle<G> {
    async fn on_run_start(&self, graph: &G, state: &ExecutionState<G::Meta>) -> Result<()> {
        for child in &self.children {
            child.on_run_start(graph, state).await?;
        }
        Ok(())
    }

    async fn on_terminal_reached(
        &self,
        node: &G::Node,
        goal_gates_passed: bool,
        state: &ExecutionState<G::Meta>,
    ) {
        for child in &self.children {
            child
                .on_terminal_reached(node, goal_gates_passed, state)
                .await;
        }
    }

    async fn before_node(
        &self,
        node: &G::Node,
        state: &ExecutionState<G::Meta>,
    ) -> Result<NodeDecision<G::Meta>> {
        for child in &self.children {
            match child.before_node(node, state).await? {
                NodeDecision::Continue => {}
                decision => return Ok(decision),
            }
        }
        Ok(NodeDecision::Continue)
    }

    async fn before_attempt(
        &self,
        ctx: &AttemptContext<'_, G>,
        state: &ExecutionState<G::Meta>,
    ) -> Result<NodeDecision<G::Meta>> {
        for child in &self.children {
            match child.before_attempt(ctx, state).await? {
                NodeDecision::Continue => {}
                decision => return Ok(decision),
            }
        }
        Ok(NodeDecision::Continue)
    }

    async fn after_attempt(
        &self,
        ctx: &AttemptResultContext<'_, G>,
        state: &ExecutionState<G::Meta>,
    ) -> Result<()> {
        for child in &self.children {
            child.after_attempt(ctx, state).await?;
        }
        Ok(())
    }

    async fn after_node(
        &self,
        node: &G::Node,
        result: &mut NodeResult<G::Meta>,
        state: &ExecutionState<G::Meta>,
    ) -> Result<()> {
        for child in &self.children {
            child.after_node(node, result, state).await?;
        }
        Ok(())
    }

    async fn after_record(
        &self,
        node: &G::Node,
        result: &NodeResult<G::Meta>,
        state: &ExecutionState<G::Meta>,
    ) -> Result<()> {
        for child in &self.children {
            child.after_record(node, result, state).await?;
        }
        Ok(())
    }

    async fn on_edge_selected(
        &self,
        ctx: &EdgeContext<'_, G>,
        state: &ExecutionState<G::Meta>,
    ) -> Result<EdgeDecision> {
        for child in &self.children {
            match child.on_edge_selected(ctx, state).await? {
                EdgeDecision::Continue => {}
                decision => return Ok(decision),
            }
        }
        Ok(EdgeDecision::Continue)
    }

    async fn on_checkpoint(
        &self,
        node: &G::Node,
        result: &NodeResult<G::Meta>,
        next_node_id: Option<&str>,
        state: &ExecutionState<G::Meta>,
    ) -> Result<()> {
        for child in &self.children {
            child
                .on_checkpoint(node, result, next_node_id, state)
                .await?;
        }
        Ok(())
    }

    async fn on_run_end(&self, outcome: &Outcome<G::Meta>, state: &ExecutionState<G::Meta>) {
        for child in &self.children {
            child.on_run_end(outcome, state).await;
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::items_after_statements,
        reason = "Local helper items keep the test setup readable."
    )]

    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::test_fixtures::{TestGraph, TestNode, linear_graph};

    /// A lifecycle that records which callbacks were called.
    struct RecordingLifecycle {
        name:                    String,
        log:                     Arc<Mutex<Vec<String>>>,
        before_node_decision:    Mutex<Option<NodeDecision>>,
        before_attempt_decision: Mutex<Option<NodeDecision>>,
        edge_decision:           Mutex<Option<EdgeDecision>>,
    }

    impl RecordingLifecycle {
        fn new(name: &str, log: Arc<Mutex<Vec<String>>>) -> Self {
            Self {
                name: name.to_string(),
                log,
                before_node_decision: Mutex::new(None),
                before_attempt_decision: Mutex::new(None),
                edge_decision: Mutex::new(None),
            }
        }

        fn with_before_node(self, decision: NodeDecision) -> Self {
            *self.before_node_decision.lock().unwrap() = Some(decision);
            self
        }

        fn with_before_attempt(self, decision: NodeDecision) -> Self {
            *self.before_attempt_decision.lock().unwrap() = Some(decision);
            self
        }

        fn with_edge_decision(self, decision: EdgeDecision) -> Self {
            *self.edge_decision.lock().unwrap() = Some(decision);
            self
        }
    }

    #[async_trait]
    impl RunLifecycle<TestGraph> for RecordingLifecycle {
        async fn on_run_start(&self, _graph: &TestGraph, _state: &ExecutionState) -> Result<()> {
            self.log
                .lock()
                .unwrap()
                .push(format!("{}:on_run_start", self.name));
            Ok(())
        }

        async fn on_terminal_reached(
            &self,
            _node: &TestNode,
            _goal_gates_passed: bool,
            _state: &ExecutionState,
        ) {
            self.log
                .lock()
                .unwrap()
                .push(format!("{}:on_terminal_reached", self.name));
        }

        async fn before_node(
            &self,
            _node: &TestNode,
            _state: &ExecutionState,
        ) -> Result<NodeDecision> {
            self.log
                .lock()
                .unwrap()
                .push(format!("{}:before_node", self.name));
            Ok(self
                .before_node_decision
                .lock()
                .unwrap()
                .take()
                .unwrap_or(NodeDecision::Continue))
        }

        async fn before_attempt(
            &self,
            _ctx: &AttemptContext<'_, TestGraph>,
            _state: &ExecutionState,
        ) -> Result<NodeDecision> {
            self.log
                .lock()
                .unwrap()
                .push(format!("{}:before_attempt", self.name));
            Ok(self
                .before_attempt_decision
                .lock()
                .unwrap()
                .take()
                .unwrap_or(NodeDecision::Continue))
        }

        async fn after_attempt(
            &self,
            _ctx: &AttemptResultContext<'_, TestGraph>,
            _state: &ExecutionState,
        ) -> Result<()> {
            self.log
                .lock()
                .unwrap()
                .push(format!("{}:after_attempt", self.name));
            Ok(())
        }

        async fn after_node(
            &self,
            _node: &TestNode,
            _result: &mut NodeResult,
            _state: &ExecutionState,
        ) -> Result<()> {
            self.log
                .lock()
                .unwrap()
                .push(format!("{}:after_node", self.name));
            Ok(())
        }

        async fn after_record(
            &self,
            _node: &TestNode,
            _result: &NodeResult,
            _state: &ExecutionState,
        ) -> Result<()> {
            self.log
                .lock()
                .unwrap()
                .push(format!("{}:after_record", self.name));
            Ok(())
        }

        async fn on_edge_selected(
            &self,
            _ctx: &EdgeContext<'_, TestGraph>,
            _state: &ExecutionState,
        ) -> Result<EdgeDecision> {
            self.log
                .lock()
                .unwrap()
                .push(format!("{}:on_edge_selected", self.name));
            Ok(self
                .edge_decision
                .lock()
                .unwrap()
                .take()
                .unwrap_or(EdgeDecision::Continue))
        }

        async fn on_checkpoint(
            &self,
            _node: &TestNode,
            _result: &NodeResult,
            _next_node_id: Option<&str>,
            _state: &ExecutionState,
        ) -> Result<()> {
            self.log
                .lock()
                .unwrap()
                .push(format!("{}:on_checkpoint", self.name));
            Ok(())
        }

        async fn on_run_end(&self, _outcome: &Outcome, _state: &ExecutionState) {
            self.log
                .lock()
                .unwrap()
                .push(format!("{}:on_run_end", self.name));
        }
    }

    #[tokio::test]
    async fn default_lifecycle_is_noop() {
        let lc = NoopLifecycle;
        let g = linear_graph(&["start", "end"]);
        let state = ExecutionState::new(&g).unwrap();
        assert!(
            <NoopLifecycle as RunLifecycle<TestGraph>>::on_run_start(&lc, &g, &state)
                .await
                .is_ok()
        );
        let node = g.get_node("start").unwrap();
        assert!(matches!(
            <NoopLifecycle as RunLifecycle<TestGraph>>::before_node(&lc, &node, &state)
                .await
                .unwrap(),
            NodeDecision::Continue
        ));
    }

    #[tokio::test]
    async fn composite_calls_all_children_on_run_start() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let lc = CompositeLifecycle::new(vec![
            Box::new(RecordingLifecycle::new("a", log.clone())),
            Box::new(RecordingLifecycle::new("b", log.clone())),
        ]);
        let g = linear_graph(&["start", "end"]);
        let state = ExecutionState::new(&g).unwrap();
        lc.on_run_start(&g, &state).await.unwrap();
        let calls = log.lock().unwrap().clone();
        assert_eq!(calls, vec!["a:on_run_start", "b:on_run_start"]);
    }

    #[tokio::test]
    async fn composite_before_node_skip_short_circuits() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let lc = CompositeLifecycle::new(vec![
            Box::new(
                RecordingLifecycle::new("a", log.clone())
                    .with_before_node(NodeDecision::Skip(Box::new(Outcome::skipped("hook")))),
            ),
            Box::new(RecordingLifecycle::new("b", log.clone())),
        ]);
        let g = linear_graph(&["start", "end"]);
        let state = ExecutionState::new(&g).unwrap();
        let node = g.get_node("start").unwrap();
        let decision = lc.before_node(&node, &state).await.unwrap();
        assert!(matches!(decision, NodeDecision::Skip(_)));
        let calls = log.lock().unwrap().clone();
        assert_eq!(calls, vec!["a:before_node"]);
        // b was NOT called
    }

    #[tokio::test]
    async fn composite_before_node_block_short_circuits() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let lc = CompositeLifecycle::new(vec![
            Box::new(
                RecordingLifecycle::new("a", log.clone())
                    .with_before_node(NodeDecision::Block("denied".into())),
            ),
            Box::new(RecordingLifecycle::new("b", log.clone())),
        ]);
        let g = linear_graph(&["start", "end"]);
        let state = ExecutionState::new(&g).unwrap();
        let node = g.get_node("start").unwrap();
        let decision = lc.before_node(&node, &state).await.unwrap();
        assert!(matches!(decision, NodeDecision::Block(_)));
        let calls = log.lock().unwrap().clone();
        assert_eq!(calls, vec!["a:before_node"]);
    }

    #[tokio::test]
    async fn composite_before_attempt_skip_short_circuits() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let lc = CompositeLifecycle::new(vec![
            Box::new(
                RecordingLifecycle::new("a", log.clone())
                    .with_before_attempt(NodeDecision::Skip(Box::new(Outcome::skipped("skip")))),
            ),
            Box::new(RecordingLifecycle::new("b", log.clone())),
        ]);
        let g = linear_graph(&["start", "end"]);
        let state = ExecutionState::new(&g).unwrap();
        let node = g.get_node("start").unwrap();
        let ctx = AttemptContext {
            node:         &node,
            attempt:      1,
            max_attempts: 1,
        };
        let decision = lc.before_attempt(&ctx, &state).await.unwrap();
        assert!(matches!(decision, NodeDecision::Skip(_)));
        let calls = log.lock().unwrap().clone();
        assert_eq!(calls, vec!["a:before_attempt"]);
    }

    #[tokio::test]
    async fn composite_before_attempt_block_short_circuits() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let lc = CompositeLifecycle::new(vec![
            Box::new(
                RecordingLifecycle::new("a", log.clone())
                    .with_before_attempt(NodeDecision::Block("nope".into())),
            ),
            Box::new(RecordingLifecycle::new("b", log.clone())),
        ]);
        let g = linear_graph(&["start", "end"]);
        let state = ExecutionState::new(&g).unwrap();
        let node = g.get_node("start").unwrap();
        let ctx = AttemptContext {
            node:         &node,
            attempt:      1,
            max_attempts: 1,
        };
        let decision = lc.before_attempt(&ctx, &state).await.unwrap();
        assert!(matches!(decision, NodeDecision::Block(_)));
    }

    #[tokio::test]
    async fn composite_after_attempt_calls_all() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let lc = CompositeLifecycle::new(vec![
            Box::new(RecordingLifecycle::new("a", log.clone())),
            Box::new(RecordingLifecycle::new("b", log.clone())),
        ]);
        let g = linear_graph(&["start", "end"]);
        let state = ExecutionState::new(&g).unwrap();
        let node = g.get_node("start").unwrap();
        let result = NodeResult::new(Outcome::success(), Duration::ZERO, 1, 1);
        let ctx = AttemptResultContext {
            node:          &node,
            result:        &result,
            attempt:       1,
            will_retry:    false,
            backoff_delay: None,
        };
        lc.after_attempt(&ctx, &state).await.unwrap();
        let calls = log.lock().unwrap().clone();
        assert_eq!(calls, vec!["a:after_attempt", "b:after_attempt"]);
    }

    #[tokio::test]
    async fn composite_on_edge_selected_override_short_circuits() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let lc = CompositeLifecycle::new(vec![
            Box::new(
                RecordingLifecycle::new("a", log.clone())
                    .with_edge_decision(EdgeDecision::Override("other".into())),
            ),
            Box::new(RecordingLifecycle::new("b", log.clone())),
        ]);
        let g = linear_graph(&["start", "end"]);
        let state = ExecutionState::new(&g).unwrap();
        let outcome = Outcome::success();
        let edge = g.outgoing_edges("start").into_iter().next().unwrap();
        let ctx = EdgeContext {
            from:    "start",
            to:      "end",
            edge:    Some(edge),
            is_jump: false,
            outcome: &outcome,
            reason:  "unconditional",
        };
        let decision = lc.on_edge_selected(&ctx, &state).await.unwrap();
        assert!(matches!(decision, EdgeDecision::Override(ref t) if t == "other"));
        let calls = log.lock().unwrap().clone();
        assert_eq!(calls, vec!["a:on_edge_selected"]);
    }

    #[tokio::test]
    async fn composite_on_edge_selected_block_short_circuits() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let lc = CompositeLifecycle::new(vec![
            Box::new(
                RecordingLifecycle::new("a", log.clone())
                    .with_edge_decision(EdgeDecision::Block("blocked".into())),
            ),
            Box::new(RecordingLifecycle::new("b", log.clone())),
        ]);
        let g = linear_graph(&["start", "end"]);
        let state = ExecutionState::new(&g).unwrap();
        let outcome = Outcome::success();
        let ctx = EdgeContext {
            from:    "start",
            to:      "end",
            edge:    None,
            is_jump: false,
            outcome: &outcome,
            reason:  "unconditional",
        };
        let decision = lc.on_edge_selected(&ctx, &state).await.unwrap();
        assert!(matches!(decision, EdgeDecision::Block(_)));
    }

    #[tokio::test]
    async fn composite_on_edge_selected_none_for_jumps() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let lc = CompositeLifecycle::new(vec![Box::new(RecordingLifecycle::new("a", log.clone()))]);
        let g = linear_graph(&["start", "end"]);
        let state = ExecutionState::new(&g).unwrap();
        let outcome = Outcome::success();
        let ctx = EdgeContext::<TestGraph> {
            from:    "start",
            to:      "target",
            edge:    None,
            is_jump: true,
            outcome: &outcome,
            reason:  "jump",
        };
        let decision = lc.on_edge_selected(&ctx, &state).await.unwrap();
        assert!(matches!(decision, EdgeDecision::Continue));
        assert!(ctx.edge.is_none());
        assert!(ctx.is_jump);
    }

    #[tokio::test]
    async fn composite_after_node_calls_all() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let lc = CompositeLifecycle::new(vec![
            Box::new(RecordingLifecycle::new("a", log.clone())),
            Box::new(RecordingLifecycle::new("b", log.clone())),
        ]);
        let g = linear_graph(&["start", "end"]);
        let state = ExecutionState::new(&g).unwrap();
        let node = g.get_node("start").unwrap();
        let mut result = NodeResult::new(Outcome::success(), Duration::ZERO, 1, 1);
        lc.after_node(&node, &mut result, &state).await.unwrap();
        let calls = log.lock().unwrap().clone();
        assert_eq!(calls, vec!["a:after_node", "b:after_node"]);
    }

    #[tokio::test]
    async fn composite_after_record_calls_all() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let lc = CompositeLifecycle::new(vec![
            Box::new(RecordingLifecycle::new("a", log.clone())),
            Box::new(RecordingLifecycle::new("b", log.clone())),
        ]);
        let g = linear_graph(&["start", "end"]);
        let state = ExecutionState::new(&g).unwrap();
        let node = g.get_node("start").unwrap();
        let result = NodeResult::new(Outcome::success(), Duration::ZERO, 1, 1);
        lc.after_record(&node, &result, &state).await.unwrap();
        let calls = log.lock().unwrap().clone();
        assert_eq!(calls, vec!["a:after_record", "b:after_record"]);
    }

    #[tokio::test]
    async fn composite_ordering_is_preserved() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let counter = Arc::new(AtomicU32::new(0));

        struct OrderedLifecycle {
            name:    String,
            log:     Arc<Mutex<Vec<String>>>,
            counter: Arc<AtomicU32>,
        }

        #[async_trait]
        impl RunLifecycle<TestGraph> for OrderedLifecycle {
            async fn on_run_start(&self, _g: &TestGraph, _s: &ExecutionState) -> Result<()> {
                let order = self.counter.fetch_add(1, Ordering::SeqCst);
                self.log
                    .lock()
                    .unwrap()
                    .push(format!("{}:{}", self.name, order));
                Ok(())
            }
        }

        let lc = CompositeLifecycle::new(vec![
            Box::new(OrderedLifecycle {
                name:    "first".into(),
                log:     log.clone(),
                counter: counter.clone(),
            }),
            Box::new(OrderedLifecycle {
                name:    "second".into(),
                log:     log.clone(),
                counter: counter.clone(),
            }),
            Box::new(OrderedLifecycle {
                name:    "third".into(),
                log:     log.clone(),
                counter: counter.clone(),
            }),
        ]);
        let g = linear_graph(&["start", "end"]);
        let state = ExecutionState::new(&g).unwrap();
        lc.on_run_start(&g, &state).await.unwrap();
        let calls = log.lock().unwrap().clone();
        assert_eq!(calls, vec!["first:0", "second:1", "third:2"]);
    }
}
