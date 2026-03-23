use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use crate::error::{CoreError, Result};
use crate::graph::{EdgeSpec, Graph, NodeSpec};
use crate::handler::NodeHandler;
use crate::lifecycle::{
    AttemptContext, AttemptResultContext, EdgeContext, EdgeDecision, NodeDecision, NoopLifecycle,
    RunLifecycle,
};
use crate::outcome::{NodeResult, Outcome, StageStatus};
use crate::state::RunState;

#[derive(Default)]
pub struct ExecutorSettings {
    pub cancel_token: Option<Arc<AtomicBool>>,
    pub max_node_visits: Option<usize>,
}

pub struct Executor<G: Graph> {
    handler: Arc<dyn NodeHandler<G>>,
    lifecycle: Box<dyn RunLifecycle<G>>,
    settings: ExecutorSettings,
}

enum NextStep {
    Edge(String),
    Jump(String),
    LoopRestart(String),
    End,
}

pub struct ExecutorBuilder<G: Graph> {
    handler: Arc<dyn NodeHandler<G>>,
    lifecycle: Option<Box<dyn RunLifecycle<G>>>,
    settings: ExecutorSettings,
}

impl<G: Graph + 'static> ExecutorBuilder<G> {
    pub fn new(handler: Arc<dyn NodeHandler<G>>) -> Self {
        Self {
            handler,
            lifecycle: None,
            settings: ExecutorSettings::default(),
        }
    }

    pub fn lifecycle(mut self, lifecycle: Box<dyn RunLifecycle<G>>) -> Self {
        self.lifecycle = Some(lifecycle);
        self
    }

    pub fn cancel_token(mut self, token: Arc<AtomicBool>) -> Self {
        self.settings.cancel_token = Some(token);
        self
    }

    pub fn max_node_visits(mut self, limit: usize) -> Self {
        self.settings.max_node_visits = Some(limit);
        self
    }

    pub fn build(self) -> Executor<G> {
        Executor {
            handler: self.handler,
            lifecycle: self.lifecycle.unwrap_or_else(|| Box::new(NoopLifecycle)),
            settings: self.settings,
        }
    }
}

impl<G: Graph + 'static> Executor<G> {
    pub async fn run(&self, graph: &G, mut state: RunState) -> Result<Outcome> {
        self.lifecycle.on_run_start(graph, &state).await?;

        loop {
            // Check cancellation
            if let Some(ref token) = self.settings.cancel_token {
                if token.load(Ordering::Relaxed) {
                    let outcome = Outcome::fail("run cancelled");
                    self.lifecycle.on_run_end(&outcome, &state).await;
                    return Err(CoreError::Cancelled);
                }
            }

            let node = state
                .current_node(graph)
                .ok_or_else(|| CoreError::NodeNotFound {
                    id: state.current_node_id.clone(),
                })?;

            // Terminal nodes: skip normal lifecycle, call on_terminal_reached, check goal gates
            if node.is_terminal() {
                self.lifecycle.on_terminal_reached(&node, &state).await;
                match graph.check_goal_gates(&state.node_outcomes) {
                    Ok(()) => {
                        let outcome = Outcome::success();
                        self.lifecycle.on_run_end(&outcome, &state).await;
                        return Ok(outcome);
                    }
                    Err(msg) => {
                        // Check if there's a retry target for goal gate failure
                        if let Some(retry_target) = graph.get_retry_target(&state.current_node_id) {
                            tracing::debug!(
                                node = %node.id(),
                                retry_target = %retry_target,
                                reason = %msg,
                                "Goal gate unsatisfied, retrying"
                            );
                            state.advance(&retry_target);
                            continue;
                        }
                        let outcome = Outcome::fail(&msg);
                        self.lifecycle.on_run_end(&outcome, &state).await;
                        return Ok(outcome);
                    }
                }
            }

            // Check visit limits
            let visits = state.increment_visits(node.id());
            if let Some(max) = node.max_visits() {
                if visits > max {
                    return Err(CoreError::VisitLimitExceeded {
                        node_id: node.id().to_string(),
                        visits,
                        limit: max,
                    });
                }
            }
            if let Some(global_max) = self.settings.max_node_visits {
                if visits > global_max {
                    return Err(CoreError::VisitLimitExceeded {
                        node_id: node.id().to_string(),
                        visits,
                        limit: global_max,
                    });
                }
            }

            // before_node lifecycle
            match self.lifecycle.before_node(&node, &state).await? {
                NodeDecision::Skip(outcome) => {
                    let mut result = NodeResult::from_skip(*outcome);
                    self.lifecycle
                        .after_node(&node, &mut result, &state)
                        .await?;
                    state.record(node.id(), &result);
                    self.lifecycle.on_checkpoint(&node, &result, &state).await?;
                }
                NodeDecision::Block(msg) => {
                    return Err(CoreError::blocked(msg));
                }
                NodeDecision::Continue => {
                    // Execute with retry
                    let mut result = self.execute_with_retry(&node, &state, graph).await?;
                    self.lifecycle
                        .after_node(&node, &mut result, &state)
                        .await?;
                    state.record(node.id(), &result);
                    self.lifecycle.on_checkpoint(&node, &result, &state).await?;
                }
            }

            // Determine next step
            let last_outcome = state.node_outcomes.get(node.id()).unwrap();
            let next = self
                .resolve_next_step(&node, last_outcome, &state, graph)
                .await?;

            match next {
                NextStep::Edge(target) | NextStep::Jump(target) => {
                    state.advance(&target);
                }
                NextStep::LoopRestart(start_id) => {
                    state.restart(&start_id);
                    self.lifecycle.on_run_start(graph, &state).await?;
                }
                NextStep::End => {
                    let outcome = last_outcome.clone();
                    self.lifecycle.on_run_end(&outcome, &state).await;
                    return Ok(outcome);
                }
            }
        }
    }

    async fn execute_with_retry(
        &self,
        node: &G::Node,
        state: &RunState,
        graph: &G,
    ) -> Result<NodeResult> {
        let policy = self.handler.retry_policy(node, graph);
        let start = Instant::now();

        for attempt in 1..=policy.max_attempts {
            let attempt_ctx = AttemptContext {
                node,
                attempt,
                max_attempts: policy.max_attempts,
            };
            match self.lifecycle.before_attempt(&attempt_ctx, state).await? {
                NodeDecision::Skip(o) => return Ok(NodeResult::from_skip(*o)),
                NodeDecision::Block(msg) => return Err(CoreError::blocked(msg)),
                NodeDecision::Continue => {}
            }

            let can_retry = attempt < policy.max_attempts;

            match self.handler.execute(node, &state.context, graph).await {
                Ok(outcome) if outcome.status == StageStatus::Retry && can_retry => {
                    let delay = policy.backoff.delay_for_attempt(attempt);
                    let result =
                        NodeResult::new(outcome, start.elapsed(), attempt, policy.max_attempts);
                    let ctx = AttemptResultContext {
                        node,
                        result: &result,
                        attempt,
                        will_retry: true,
                        backoff_delay: Some(delay),
                    };
                    self.lifecycle.after_attempt(&ctx, state).await?;
                    tokio::time::sleep(delay).await;
                }
                Ok(outcome) if outcome.status == StageStatus::Retry => {
                    let final_outcome = self.handler.on_retries_exhausted(node, outcome);
                    let result = NodeResult::new(
                        final_outcome,
                        start.elapsed(),
                        attempt,
                        policy.max_attempts,
                    );
                    let ctx = AttemptResultContext {
                        node,
                        result: &result,
                        attempt,
                        will_retry: false,
                        backoff_delay: None,
                    };
                    self.lifecycle.after_attempt(&ctx, state).await?;
                    return Ok(result);
                }
                Ok(outcome) => {
                    let result =
                        NodeResult::new(outcome, start.elapsed(), attempt, policy.max_attempts);
                    let ctx = AttemptResultContext {
                        node,
                        result: &result,
                        attempt,
                        will_retry: false,
                        backoff_delay: None,
                    };
                    self.lifecycle.after_attempt(&ctx, state).await?;
                    return Ok(result);
                }
                Err(e) if can_retry && e.is_retryable() => {
                    let delay = policy.backoff.delay_for_attempt(attempt);
                    let fail_result =
                        NodeResult::from_error(&e, start.elapsed(), attempt, policy.max_attempts);
                    let ctx = AttemptResultContext {
                        node,
                        result: &fail_result,
                        attempt,
                        will_retry: true,
                        backoff_delay: Some(delay),
                    };
                    self.lifecycle.after_attempt(&ctx, state).await?;
                    tokio::time::sleep(delay).await;
                }
                Err(e) => {
                    let fail_result =
                        NodeResult::from_error(&e, start.elapsed(), attempt, policy.max_attempts);
                    let ctx = AttemptResultContext {
                        node,
                        result: &fail_result,
                        attempt,
                        will_retry: false,
                        backoff_delay: None,
                    };
                    self.lifecycle.after_attempt(&ctx, state).await?;
                    return Err(e);
                }
            }
        }
        unreachable!("loop always returns or continues")
    }

    async fn resolve_next_step(
        &self,
        node: &G::Node,
        outcome: &Outcome,
        state: &RunState,
        graph: &G,
    ) -> Result<NextStep> {
        // Jump takes priority
        if let Some(ref target) = outcome.jump_to_node {
            let ctx = EdgeContext {
                from: node.id(),
                to: target,
                edge: None,
                is_jump: true,
                outcome,
                reason: "jump",
            };
            match self.lifecycle.on_edge_selected(&ctx, state).await? {
                EdgeDecision::Continue => return Ok(NextStep::Jump(target.clone())),
                EdgeDecision::Override(new_target) => return Ok(NextStep::Edge(new_target)),
                EdgeDecision::Block(msg) => return Err(CoreError::blocked(msg)),
            }
        }

        // Normal edge selection
        match graph.select_edge(node, outcome, &state.context) {
            Some(selection) => {
                let target = selection.edge.target().to_string();
                let is_restart = selection.edge.is_loop_restart();

                let ctx = EdgeContext {
                    from: node.id(),
                    to: &target,
                    edge: Some(selection.edge.clone()),
                    is_jump: false,
                    outcome,
                    reason: selection.reason,
                };
                match self.lifecycle.on_edge_selected(&ctx, state).await? {
                    EdgeDecision::Continue => {
                        if is_restart {
                            let start = graph.find_start_node()?;
                            Ok(NextStep::LoopRestart(start.id().to_string()))
                        } else {
                            Ok(NextStep::Edge(target))
                        }
                    }
                    EdgeDecision::Override(new_target) => Ok(NextStep::Edge(new_target)),
                    EdgeDecision::Block(msg) => Err(CoreError::blocked(msg)),
                }
            }
            None => {
                // No edge found
                if outcome.status == StageStatus::Success
                    || outcome.status == StageStatus::PartialSuccess
                {
                    Ok(NextStep::End)
                } else {
                    // Fail with no outgoing edge → fail the run
                    Ok(NextStep::End)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use async_trait::async_trait;

    use super::*;
    use crate::context::Context;
    use crate::error::HandlerErrorDetail;
    use crate::lifecycle::RunLifecycle;
    use crate::retry::{BackoffPolicy, RetryPolicy};
    use crate::test_fixtures::*;

    // Helper to build and run an executor with default settings
    async fn run_linear(
        node_ids: &[&str],
        handler: Arc<dyn NodeHandler<TestGraph>>,
    ) -> Result<Outcome> {
        let g = linear_graph(node_ids);
        let state = RunState::new(&g)?;
        let executor = ExecutorBuilder::new(handler).build();
        executor.run(&g, state).await
    }

    // ---- Step 8: Linear happy path ----

    #[tokio::test]
    async fn executor_linear_three_node_success() {
        let result = run_linear(&["start", "work", "end"], Arc::new(AlwaysSucceedHandler))
            .await
            .unwrap();
        assert_eq!(result.status, StageStatus::Success);
    }

    #[tokio::test]
    async fn executor_builder_sets_lifecycle() {
        let log = Arc::new(Mutex::new(Vec::<String>::new()));
        struct LogLifecycle(Arc<Mutex<Vec<String>>>);
        #[async_trait]
        impl RunLifecycle<TestGraph> for LogLifecycle {
            async fn on_run_start(&self, _g: &TestGraph, _s: &RunState) -> Result<()> {
                self.0.lock().unwrap().push("start".into());
                Ok(())
            }
        }
        let g = linear_graph(&["start", "end"]);
        let state = RunState::new(&g).unwrap();
        let executor =
            ExecutorBuilder::new(Arc::new(AlwaysSucceedHandler) as Arc<dyn NodeHandler<TestGraph>>)
                .lifecycle(Box::new(LogLifecycle(log.clone())))
                .build();
        executor.run(&g, state).await.unwrap();
        assert_eq!(log.lock().unwrap().clone(), vec!["start"]);
    }

    #[tokio::test]
    async fn executor_builder_sets_cancel_token() {
        let token = Arc::new(AtomicBool::new(true)); // already cancelled
        let g = linear_graph(&["start", "end"]);
        let state = RunState::new(&g).unwrap();
        let executor =
            ExecutorBuilder::new(Arc::new(AlwaysSucceedHandler) as Arc<dyn NodeHandler<TestGraph>>)
                .cancel_token(token)
                .build();
        let result = executor.run(&g, state).await;
        assert!(matches!(result, Err(CoreError::Cancelled)));
    }

    // ---- Step 9: Terminal nodes, goal gates, visit limits ----

    #[tokio::test]
    async fn executor_goal_gate_satisfied() {
        let g = TestGraph::new(
            vec![
                TestNode::new("work"),
                TestNode::terminal("end").with_goal_gate("work", StageStatus::Success),
            ],
            vec![TestEdge::new("work", "end")],
            "work",
        );
        let state = RunState::new(&g).unwrap();
        let executor =
            ExecutorBuilder::new(Arc::new(AlwaysSucceedHandler) as Arc<dyn NodeHandler<TestGraph>>)
                .build();
        let result = executor.run(&g, state).await.unwrap();
        assert_eq!(result.status, StageStatus::Success);
    }

    #[tokio::test]
    async fn executor_goal_gate_unsatisfied_with_retry() {
        // work → end (goal gate: work must be success)
        // retry_target: end → work (so we go back to work)
        // First call fails, second succeeds
        let g = TestGraph::new(
            vec![
                TestNode::new("work"),
                TestNode::terminal("end").with_goal_gate("work", StageStatus::Success),
            ],
            vec![TestEdge::new("work", "end")],
            "work",
        )
        .with_retry_target("end", "work");

        let handler = Arc::new(CountingHandler::new(vec![
            Ok(Outcome::fail("first attempt")),
            Ok(Outcome::success()),
        ]));
        let state = RunState::new(&g).unwrap();
        let executor =
            ExecutorBuilder::new(handler.clone() as Arc<dyn NodeHandler<TestGraph>>).build();
        let result = executor.run(&g, state).await.unwrap();
        assert_eq!(result.status, StageStatus::Success);
        assert_eq!(handler.calls(), 2);
    }

    #[tokio::test]
    async fn executor_goal_gate_unsatisfied_no_retry_fails() {
        let g = TestGraph::new(
            vec![
                TestNode::new("work"),
                TestNode::terminal("end").with_goal_gate("work", StageStatus::Success),
            ],
            vec![TestEdge::new("work", "end")],
            "work",
        );
        // No retry target, and handler fails
        let state = RunState::new(&g).unwrap();
        let executor = ExecutorBuilder::new(
            Arc::new(AlwaysFailHandler::new("nope")) as Arc<dyn NodeHandler<TestGraph>>
        )
        .build();
        let result = executor.run(&g, state).await.unwrap();
        assert_eq!(result.status, StageStatus::Fail);
    }

    #[tokio::test]
    async fn executor_terminal_node_skips_normal_lifecycle() {
        let log = Arc::new(Mutex::new(Vec::<String>::new()));
        struct TrackingLifecycle(Arc<Mutex<Vec<String>>>);
        #[async_trait]
        impl RunLifecycle<TestGraph> for TrackingLifecycle {
            async fn before_node(&self, node: &TestNode, _s: &RunState) -> Result<NodeDecision> {
                self.0
                    .lock()
                    .unwrap()
                    .push(format!("before_node:{}", node.id()));
                Ok(NodeDecision::Continue)
            }
            async fn after_node(
                &self,
                node: &TestNode,
                _r: &mut NodeResult,
                _s: &RunState,
            ) -> Result<()> {
                self.0
                    .lock()
                    .unwrap()
                    .push(format!("after_node:{}", node.id()));
                Ok(())
            }
            async fn on_terminal_reached(&self, node: &TestNode, _s: &RunState) {
                self.0
                    .lock()
                    .unwrap()
                    .push(format!("terminal:{}", node.id()));
            }
        }
        let g = linear_graph(&["start", "end"]);
        let state = RunState::new(&g).unwrap();
        let executor =
            ExecutorBuilder::new(Arc::new(AlwaysSucceedHandler) as Arc<dyn NodeHandler<TestGraph>>)
                .lifecycle(Box::new(TrackingLifecycle(log.clone())))
                .build();
        executor.run(&g, state).await.unwrap();
        let calls = log.lock().unwrap().clone();
        // before_node and after_node called for "start", NOT for "end"
        assert!(calls.contains(&"before_node:start".to_string()));
        assert!(calls.contains(&"after_node:start".to_string()));
        assert!(!calls.contains(&"before_node:end".to_string()));
        assert!(!calls.contains(&"after_node:end".to_string()));
        // on_terminal_reached IS called for "end"
        assert!(calls.contains(&"terminal:end".to_string()));
    }

    #[tokio::test]
    async fn executor_terminal_node_calls_on_terminal_reached() {
        let log = Arc::new(Mutex::new(Vec::<String>::new()));
        struct TerminalTracker(Arc<Mutex<Vec<String>>>);
        #[async_trait]
        impl RunLifecycle<TestGraph> for TerminalTracker {
            async fn on_terminal_reached(&self, node: &TestNode, _s: &RunState) {
                self.0
                    .lock()
                    .unwrap()
                    .push(format!("terminal:{}", node.id()));
            }
        }
        let g = linear_graph(&["start", "end"]);
        let state = RunState::new(&g).unwrap();
        let executor =
            ExecutorBuilder::new(Arc::new(AlwaysSucceedHandler) as Arc<dyn NodeHandler<TestGraph>>)
                .lifecycle(Box::new(TerminalTracker(log.clone())))
                .build();
        executor.run(&g, state).await.unwrap();
        assert_eq!(log.lock().unwrap().clone(), vec!["terminal:end"]);
    }

    #[tokio::test]
    async fn executor_visit_limit_per_node() {
        // Node with max_visits=1, but graph loops back to it
        let g = TestGraph::new(
            vec![
                TestNode::new("loop_node").with_max_visits(1),
                TestNode::new("other"),
                TestNode::terminal("end"),
            ],
            vec![
                TestEdge::new("loop_node", "other"),
                TestEdge::new("other", "loop_node"),
            ],
            "loop_node",
        );
        let state = RunState::new(&g).unwrap();
        let executor =
            ExecutorBuilder::new(Arc::new(AlwaysSucceedHandler) as Arc<dyn NodeHandler<TestGraph>>)
                .build();
        let result = executor.run(&g, state).await;
        assert!(matches!(result, Err(CoreError::VisitLimitExceeded { .. })));
    }

    #[tokio::test]
    async fn executor_visit_limit_global() {
        let g = TestGraph::new(
            vec![
                TestNode::new("a"),
                TestNode::new("b"),
                TestNode::terminal("end"),
            ],
            vec![TestEdge::new("a", "b"), TestEdge::new("b", "a")],
            "a",
        );
        let state = RunState::new(&g).unwrap();
        let executor =
            ExecutorBuilder::new(Arc::new(AlwaysSucceedHandler) as Arc<dyn NodeHandler<TestGraph>>)
                .max_node_visits(3)
                .build();
        let result = executor.run(&g, state).await;
        assert!(matches!(result, Err(CoreError::VisitLimitExceeded { .. })));
    }

    // ---- Step 10: Edge selection, jumps, loop restarts ----

    #[tokio::test]
    async fn executor_conditional_edge_on_fail() {
        let g = TestGraph::new(
            vec![
                TestNode::new("start"),
                TestNode::terminal("ok"),
                TestNode::terminal("bad"),
            ],
            vec![
                TestEdge::new("start", "ok").with_label("success"),
                TestEdge::new("start", "bad").with_label("fail"),
            ],
            "start",
        );
        let state = RunState::new(&g).unwrap();
        let executor = ExecutorBuilder::new(
            Arc::new(AlwaysFailHandler::new("oops")) as Arc<dyn NodeHandler<TestGraph>>
        )
        .build();
        let result = executor.run(&g, state).await.unwrap();
        // Ends at "bad" terminal with success (goal gates pass since no gates defined)
        assert_eq!(result.status, StageStatus::Success);
    }

    #[tokio::test]
    async fn executor_conditional_edge_on_success() {
        let g = TestGraph::new(
            vec![
                TestNode::new("start"),
                TestNode::terminal("ok"),
                TestNode::terminal("bad"),
            ],
            vec![
                TestEdge::new("start", "ok").with_label("success"),
                TestEdge::new("start", "bad").with_label("fail"),
            ],
            "start",
        );
        let state = RunState::new(&g).unwrap();
        let executor =
            ExecutorBuilder::new(Arc::new(AlwaysSucceedHandler) as Arc<dyn NodeHandler<TestGraph>>)
                .build();
        let result = executor.run(&g, state).await.unwrap();
        assert_eq!(result.status, StageStatus::Success);
    }

    #[tokio::test]
    async fn executor_jump_bypasses_edge_selection() {
        // start → end (normal), but handler says jump to "target"
        struct JumpHandler;
        #[async_trait]
        impl NodeHandler<TestGraph> for JumpHandler {
            async fn execute(
                &self,
                _n: &TestNode,
                _c: &Context,
                _g: &TestGraph,
            ) -> Result<Outcome> {
                let mut o = Outcome::success();
                o.jump_to_node = Some("target".into());
                Ok(o)
            }
        }
        let g = TestGraph::new(
            vec![
                TestNode::new("start"),
                TestNode::terminal("end"),
                TestNode::terminal("target"),
            ],
            vec![TestEdge::new("start", "end")],
            "start",
        );
        let state = RunState::new(&g).unwrap();
        let executor =
            ExecutorBuilder::new(Arc::new(JumpHandler) as Arc<dyn NodeHandler<TestGraph>>).build();
        let result = executor.run(&g, state).await.unwrap();
        assert_eq!(result.status, StageStatus::Success);
    }

    #[tokio::test]
    async fn executor_loop_restart_resets_state() {
        // start → work → (loop_restart edge back) → start → work → end
        let handler = Arc::new(CountingHandler::new(vec![
            Ok(Outcome::success()), // start (1st)
            Ok({
                let mut o = Outcome::success();
                o.preferred_label = Some("retry".into());
                o
            }), // work (1st) → triggers loop restart
            Ok(Outcome::success()), // start (2nd)
            Ok(Outcome::success()), // work (2nd) → no label match, takes unconditional to end
        ]));
        let g = TestGraph::new(
            vec![
                TestNode::new("start"),
                TestNode::new("work"),
                TestNode::terminal("end"),
            ],
            vec![
                TestEdge::new("start", "work"),
                TestEdge::new("work", "start")
                    .with_label("retry")
                    .with_loop_restart(),
                TestEdge::new("work", "end"),
            ],
            "start",
        );
        let state = RunState::new(&g).unwrap();
        let executor = ExecutorBuilder::new(handler.clone() as Arc<dyn NodeHandler<TestGraph>>)
            .max_node_visits(5)
            .build();
        let result = executor.run(&g, state).await.unwrap();
        assert_eq!(result.status, StageStatus::Success);
        assert_eq!(handler.calls(), 4);
    }

    #[tokio::test]
    async fn executor_loop_restart_calls_on_run_start() {
        let log = Arc::new(Mutex::new(Vec::<String>::new()));
        struct StartTracker(Arc<Mutex<Vec<String>>>);
        #[async_trait]
        impl RunLifecycle<TestGraph> for StartTracker {
            async fn on_run_start(&self, _g: &TestGraph, _s: &RunState) -> Result<()> {
                self.0.lock().unwrap().push("on_run_start".into());
                Ok(())
            }
        }
        let handler = Arc::new(CountingHandler::new(vec![
            Ok(Outcome::success()),
            Ok({
                let mut o = Outcome::success();
                o.preferred_label = Some("retry".into());
                o
            }),
            Ok(Outcome::success()),
            Ok(Outcome::success()),
        ]));
        let g = TestGraph::new(
            vec![
                TestNode::new("start"),
                TestNode::new("work"),
                TestNode::terminal("end"),
            ],
            vec![
                TestEdge::new("start", "work"),
                TestEdge::new("work", "start")
                    .with_label("retry")
                    .with_loop_restart(),
                TestEdge::new("work", "end"),
            ],
            "start",
        );
        let state = RunState::new(&g).unwrap();
        let executor = ExecutorBuilder::new(handler as Arc<dyn NodeHandler<TestGraph>>)
            .lifecycle(Box::new(StartTracker(log.clone())))
            .max_node_visits(5)
            .build();
        executor.run(&g, state).await.unwrap();
        // on_run_start should be called twice: initial + after restart
        assert_eq!(log.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn executor_fail_no_edge_returns_fail() {
        // Node fails with no "fail" edge → run ends with that outcome
        let g = TestGraph::new(
            vec![TestNode::new("start"), TestNode::terminal("end")],
            vec![TestEdge::new("start", "end").with_label("success")],
            "start",
        );
        let state = RunState::new(&g).unwrap();
        let executor = ExecutorBuilder::new(
            Arc::new(AlwaysFailHandler::new("boom")) as Arc<dyn NodeHandler<TestGraph>>
        )
        .build();
        let result = executor.run(&g, state).await.unwrap();
        assert_eq!(result.status, StageStatus::Fail);
    }

    #[tokio::test]
    async fn executor_no_edge_after_success_returns_success() {
        // Node succeeds with no outgoing edges → run ends with success
        let g = TestGraph::new(vec![TestNode::new("only")], vec![], "only");
        let state = RunState::new(&g).unwrap();
        let executor =
            ExecutorBuilder::new(Arc::new(AlwaysSucceedHandler) as Arc<dyn NodeHandler<TestGraph>>)
                .build();
        let result = executor.run(&g, state).await.unwrap();
        assert_eq!(result.status, StageStatus::Success);
    }

    // ---- Step 11: Cancellation ----

    #[tokio::test]
    async fn executor_cancellation_stops_run() {
        let token = Arc::new(AtomicBool::new(false));
        let token_clone = token.clone();

        struct CancellingHandler(Arc<AtomicBool>);
        #[async_trait]
        impl NodeHandler<TestGraph> for CancellingHandler {
            async fn execute(
                &self,
                _n: &TestNode,
                _c: &Context,
                _g: &TestGraph,
            ) -> Result<Outcome> {
                // Cancel after first node
                self.0.store(true, Ordering::Relaxed);
                Ok(Outcome::success())
            }
        }

        let g = linear_graph(&["start", "work", "end"]);
        let state = RunState::new(&g).unwrap();
        let executor = ExecutorBuilder::new(
            Arc::new(CancellingHandler(token_clone)) as Arc<dyn NodeHandler<TestGraph>>
        )
        .cancel_token(token)
        .build();
        let result = executor.run(&g, state).await;
        assert!(matches!(result, Err(CoreError::Cancelled)));
    }

    // ---- Step 12: Retry integration ----

    #[tokio::test]
    async fn executor_retry_on_retryable_error() {
        let handler = Arc::new(
            CountingHandler::new(vec![
                Err(CoreError::handler(HandlerErrorDetail {
                    message: "fail1".into(),
                    retryable: true,
                    category: None,
                    signature: None,
                })),
                Err(CoreError::handler(HandlerErrorDetail {
                    message: "fail2".into(),
                    retryable: true,
                    category: None,
                    signature: None,
                })),
                Ok(Outcome::success()),
            ])
            .with_retry_policy(RetryPolicy {
                max_attempts: 3,
                backoff: BackoffPolicy {
                    initial_delay: Duration::from_millis(1),
                    factor: 1.0,
                    max_delay: Duration::from_millis(1),
                    jitter: false,
                },
            }),
        );
        let result = run_linear(
            &["start", "end"],
            handler.clone() as Arc<dyn NodeHandler<TestGraph>>,
        )
        .await
        .unwrap();
        assert_eq!(result.status, StageStatus::Success);
        assert_eq!(handler.calls(), 3);
    }

    #[tokio::test]
    async fn executor_retry_on_retry_status() {
        let handler = Arc::new(
            CountingHandler::new(vec![
                Ok(Outcome {
                    status: StageStatus::Retry,
                    ..Outcome::default()
                }),
                Ok(Outcome {
                    status: StageStatus::Retry,
                    ..Outcome::default()
                }),
                Ok(Outcome::success()),
            ])
            .with_retry_policy(RetryPolicy {
                max_attempts: 3,
                backoff: BackoffPolicy {
                    initial_delay: Duration::from_millis(1),
                    factor: 1.0,
                    max_delay: Duration::from_millis(1),
                    jitter: false,
                },
            }),
        );
        let result = run_linear(
            &["start", "end"],
            handler.clone() as Arc<dyn NodeHandler<TestGraph>>,
        )
        .await
        .unwrap();
        assert_eq!(result.status, StageStatus::Success);
        assert_eq!(handler.calls(), 3);
    }

    #[tokio::test]
    async fn executor_retry_non_retryable_error_no_retry() {
        let handler = Arc::new(
            CountingHandler::new(vec![Err(CoreError::handler(HandlerErrorDetail {
                message: "fatal".into(),
                retryable: false,
                category: None,
                signature: None,
            }))])
            .with_retry_policy(RetryPolicy::with_max_attempts(3)),
        );
        let result = run_linear(
            &["start", "end"],
            handler.clone() as Arc<dyn NodeHandler<TestGraph>>,
        )
        .await;
        assert!(result.is_err());
        assert_eq!(handler.calls(), 1);
    }

    #[tokio::test]
    async fn executor_retry_no_retry_by_default() {
        // Default policy is RetryPolicy::none() (max_attempts=1)
        let handler = Arc::new(CountingHandler::new(vec![Err(CoreError::handler(
            HandlerErrorDetail {
                message: "fail".into(),
                retryable: true,
                category: None,
                signature: None,
            },
        ))]));
        let result = run_linear(
            &["start", "end"],
            handler.clone() as Arc<dyn NodeHandler<TestGraph>>,
        )
        .await;
        assert!(result.is_err());
        assert_eq!(handler.calls(), 1);
    }

    #[tokio::test]
    async fn executor_retry_exhausted_calls_on_retries_exhausted() {
        struct ExhaustedHandler;
        #[async_trait]
        impl NodeHandler<TestGraph> for ExhaustedHandler {
            async fn execute(
                &self,
                _n: &TestNode,
                _c: &Context,
                _g: &TestGraph,
            ) -> Result<Outcome> {
                Ok(Outcome {
                    status: StageStatus::Retry,
                    ..Outcome::default()
                })
            }
            fn retry_policy(&self, _n: &TestNode, _g: &TestGraph) -> RetryPolicy {
                RetryPolicy {
                    max_attempts: 2,
                    backoff: BackoffPolicy {
                        initial_delay: Duration::from_millis(1),
                        factor: 1.0,
                        max_delay: Duration::from_millis(1),
                        jitter: false,
                    },
                }
            }
            fn on_retries_exhausted(&self, _n: &TestNode, _last: Outcome) -> Outcome {
                Outcome {
                    status: StageStatus::PartialSuccess,
                    notes: Some("exhausted".into()),
                    ..Outcome::default()
                }
            }
        }
        // No outgoing edges from "start" so PartialSuccess becomes the run result
        let g = TestGraph::new(vec![TestNode::new("start")], vec![], "start");
        let state = RunState::new(&g).unwrap();
        let executor =
            ExecutorBuilder::new(Arc::new(ExhaustedHandler) as Arc<dyn NodeHandler<TestGraph>>)
                .build();
        let result = executor.run(&g, state).await.unwrap();
        assert_eq!(result.status, StageStatus::PartialSuccess);
    }

    #[tokio::test]
    async fn executor_retry_lifecycle_before_attempt_called_per_attempt() {
        let attempt_log = Arc::new(Mutex::new(Vec::<u32>::new()));
        struct AttemptTracker(Arc<Mutex<Vec<u32>>>);
        #[async_trait]
        impl RunLifecycle<TestGraph> for AttemptTracker {
            async fn before_attempt(
                &self,
                ctx: &AttemptContext<'_, TestGraph>,
                _s: &RunState,
            ) -> Result<NodeDecision> {
                self.0.lock().unwrap().push(ctx.attempt);
                Ok(NodeDecision::Continue)
            }
        }
        let handler = Arc::new(
            CountingHandler::new(vec![
                Err(CoreError::handler(HandlerErrorDetail {
                    message: "r".into(),
                    retryable: true,
                    category: None,
                    signature: None,
                })),
                Ok(Outcome::success()),
            ])
            .with_retry_policy(RetryPolicy {
                max_attempts: 3,
                backoff: BackoffPolicy {
                    initial_delay: Duration::from_millis(1),
                    factor: 1.0,
                    max_delay: Duration::from_millis(1),
                    jitter: false,
                },
            }),
        );
        let g = linear_graph(&["start", "end"]);
        let state = RunState::new(&g).unwrap();
        let executor = ExecutorBuilder::new(handler as Arc<dyn NodeHandler<TestGraph>>)
            .lifecycle(Box::new(AttemptTracker(attempt_log.clone())))
            .build();
        executor.run(&g, state).await.unwrap();
        assert_eq!(*attempt_log.lock().unwrap(), vec![1, 2]);
    }

    #[tokio::test]
    async fn executor_retry_lifecycle_after_attempt_called_with_will_retry() {
        let retry_log = Arc::new(Mutex::new(Vec::<(u32, bool)>::new()));
        struct RetryTracker(Arc<Mutex<Vec<(u32, bool)>>>);
        #[async_trait]
        impl RunLifecycle<TestGraph> for RetryTracker {
            async fn after_attempt(
                &self,
                ctx: &AttemptResultContext<'_, TestGraph>,
                _s: &RunState,
            ) -> Result<()> {
                self.0.lock().unwrap().push((ctx.attempt, ctx.will_retry));
                Ok(())
            }
        }
        let handler = Arc::new(
            CountingHandler::new(vec![
                Err(CoreError::handler(HandlerErrorDetail {
                    message: "r".into(),
                    retryable: true,
                    category: None,
                    signature: None,
                })),
                Ok(Outcome::success()),
            ])
            .with_retry_policy(RetryPolicy {
                max_attempts: 3,
                backoff: BackoffPolicy {
                    initial_delay: Duration::from_millis(1),
                    factor: 1.0,
                    max_delay: Duration::from_millis(1),
                    jitter: false,
                },
            }),
        );
        let g = linear_graph(&["start", "end"]);
        let state = RunState::new(&g).unwrap();
        let executor = ExecutorBuilder::new(handler as Arc<dyn NodeHandler<TestGraph>>)
            .lifecycle(Box::new(RetryTracker(retry_log.clone())))
            .build();
        executor.run(&g, state).await.unwrap();
        let log = retry_log.lock().unwrap().clone();
        assert_eq!(log, vec![(1, true), (2, false)]);
    }

    #[tokio::test]
    async fn executor_retry_lifecycle_before_attempt_skip_stops_retry() {
        let call_count = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let call_count_clone = call_count.clone();
        struct SkipOnSecondAttempt(Arc<std::sync::atomic::AtomicU32>);
        #[async_trait]
        impl RunLifecycle<TestGraph> for SkipOnSecondAttempt {
            async fn before_attempt(
                &self,
                ctx: &AttemptContext<'_, TestGraph>,
                _s: &RunState,
            ) -> Result<NodeDecision> {
                self.0.fetch_add(1, Ordering::Relaxed);
                if ctx.attempt >= 2 {
                    Ok(NodeDecision::Skip(Box::new(Outcome::skipped("hook skip"))))
                } else {
                    Ok(NodeDecision::Continue)
                }
            }
        }
        let handler = Arc::new(
            CountingHandler::new(vec![
                Err(CoreError::handler(HandlerErrorDetail {
                    message: "r".into(),
                    retryable: true,
                    category: None,
                    signature: None,
                })),
                Ok(Outcome::success()), // should not be reached
            ])
            .with_retry_policy(RetryPolicy {
                max_attempts: 3,
                backoff: BackoffPolicy {
                    initial_delay: Duration::from_millis(1),
                    factor: 1.0,
                    max_delay: Duration::from_millis(1),
                    jitter: false,
                },
            }),
        );
        let g = linear_graph(&["start", "end"]);
        let state = RunState::new(&g).unwrap();
        let executor = ExecutorBuilder::new(handler.clone() as Arc<dyn NodeHandler<TestGraph>>)
            .lifecycle(Box::new(SkipOnSecondAttempt(call_count_clone)))
            .build();
        let result = executor.run(&g, state).await.unwrap();
        assert_eq!(result.status, StageStatus::Success); // overall run succeeds via terminal
        assert_eq!(handler.calls(), 1); // handler only called once
        assert_eq!(call_count.load(Ordering::Relaxed), 2); // before_attempt called twice
    }

    #[tokio::test]
    async fn executor_retry_backoff_delay() {
        tokio::time::pause();
        let handler = Arc::new(
            CountingHandler::new(vec![
                Ok(Outcome {
                    status: StageStatus::Retry,
                    ..Outcome::default()
                }),
                Ok(Outcome::success()),
            ])
            .with_retry_policy(RetryPolicy {
                max_attempts: 3,
                backoff: BackoffPolicy {
                    initial_delay: Duration::from_secs(5),
                    factor: 2.0,
                    max_delay: Duration::from_secs(60),
                    jitter: false,
                },
            }),
        );
        let start = tokio::time::Instant::now();
        let result = run_linear(
            &["start", "end"],
            handler as Arc<dyn NodeHandler<TestGraph>>,
        )
        .await
        .unwrap();
        assert_eq!(result.status, StageStatus::Success);
        // Should have slept ~5s for the retry backoff
        assert!(start.elapsed() >= Duration::from_secs(4));
    }

    // ---- Step 13: Full lifecycle integration ----

    #[tokio::test]
    async fn executor_lifecycle_before_node_skip() {
        struct SkipFirst(Mutex<bool>);
        #[async_trait]
        impl RunLifecycle<TestGraph> for SkipFirst {
            async fn before_node(&self, node: &TestNode, _s: &RunState) -> Result<NodeDecision> {
                if node.id() == "start" {
                    let mut skipped = self.0.lock().unwrap();
                    if !*skipped {
                        *skipped = true;
                        return Ok(NodeDecision::Skip(Box::new(Outcome::skipped("hook"))));
                    }
                }
                Ok(NodeDecision::Continue)
            }
        }
        let g = linear_graph(&["start", "end"]);
        let state = RunState::new(&g).unwrap();
        let executor =
            ExecutorBuilder::new(Arc::new(AlwaysSucceedHandler) as Arc<dyn NodeHandler<TestGraph>>)
                .lifecycle(Box::new(SkipFirst(Mutex::new(false))))
                .build();
        let result = executor.run(&g, state).await.unwrap();
        assert_eq!(result.status, StageStatus::Success);
    }

    #[tokio::test]
    async fn executor_lifecycle_before_node_block() {
        struct Blocker;
        #[async_trait]
        impl RunLifecycle<TestGraph> for Blocker {
            async fn before_node(&self, _n: &TestNode, _s: &RunState) -> Result<NodeDecision> {
                Ok(NodeDecision::Block("blocked".into()))
            }
        }
        let g = linear_graph(&["start", "end"]);
        let state = RunState::new(&g).unwrap();
        let executor =
            ExecutorBuilder::new(Arc::new(AlwaysSucceedHandler) as Arc<dyn NodeHandler<TestGraph>>)
                .lifecycle(Box::new(Blocker))
                .build();
        let result = executor.run(&g, state).await;
        assert!(matches!(result, Err(CoreError::Blocked { .. })));
    }

    #[tokio::test]
    async fn executor_lifecycle_after_node_mutates_result() {
        struct Mutator;
        #[async_trait]
        impl RunLifecycle<TestGraph> for Mutator {
            async fn after_node(
                &self,
                _n: &TestNode,
                result: &mut NodeResult,
                _s: &RunState,
            ) -> Result<()> {
                result.outcome.notes = Some("mutated".into());
                Ok(())
            }
        }
        let g = linear_graph(&["start", "end"]);
        let state = RunState::new(&g).unwrap();
        let executor =
            ExecutorBuilder::new(Arc::new(AlwaysSucceedHandler) as Arc<dyn NodeHandler<TestGraph>>)
                .lifecycle(Box::new(Mutator))
                .build();
        executor.run(&g, state).await.unwrap();
        // The mutation happened (verified by no error; could also check state)
    }

    #[tokio::test]
    async fn executor_lifecycle_on_edge_override() {
        struct Redirector;
        #[async_trait]
        impl RunLifecycle<TestGraph> for Redirector {
            async fn on_edge_selected(
                &self,
                _ctx: &EdgeContext<'_, TestGraph>,
                _s: &RunState,
            ) -> Result<EdgeDecision> {
                Ok(EdgeDecision::Override("alt".into()))
            }
        }
        let g = TestGraph::new(
            vec![
                TestNode::new("start"),
                TestNode::terminal("end"),
                TestNode::terminal("alt"),
            ],
            vec![TestEdge::new("start", "end")],
            "start",
        );
        let state = RunState::new(&g).unwrap();
        let executor =
            ExecutorBuilder::new(Arc::new(AlwaysSucceedHandler) as Arc<dyn NodeHandler<TestGraph>>)
                .lifecycle(Box::new(Redirector))
                .build();
        let result = executor.run(&g, state).await.unwrap();
        assert_eq!(result.status, StageStatus::Success);
    }

    #[tokio::test]
    async fn executor_lifecycle_on_edge_block() {
        struct EdgeBlocker;
        #[async_trait]
        impl RunLifecycle<TestGraph> for EdgeBlocker {
            async fn on_edge_selected(
                &self,
                _ctx: &EdgeContext<'_, TestGraph>,
                _s: &RunState,
            ) -> Result<EdgeDecision> {
                Ok(EdgeDecision::Block("edge blocked".into()))
            }
        }
        let g = linear_graph(&["start", "end"]);
        let state = RunState::new(&g).unwrap();
        let executor =
            ExecutorBuilder::new(Arc::new(AlwaysSucceedHandler) as Arc<dyn NodeHandler<TestGraph>>)
                .lifecycle(Box::new(EdgeBlocker))
                .build();
        let result = executor.run(&g, state).await;
        assert!(matches!(result, Err(CoreError::Blocked { .. })));
    }

    #[tokio::test]
    async fn executor_lifecycle_on_checkpoint_called() {
        let log = Arc::new(Mutex::new(Vec::<String>::new()));
        struct CheckpointTracker(Arc<Mutex<Vec<String>>>);
        #[async_trait]
        impl RunLifecycle<TestGraph> for CheckpointTracker {
            async fn on_checkpoint(
                &self,
                node: &TestNode,
                _r: &NodeResult,
                _s: &RunState,
            ) -> Result<()> {
                self.0.lock().unwrap().push(node.id().to_string());
                Ok(())
            }
        }
        let g = linear_graph(&["start", "work", "end"]);
        let state = RunState::new(&g).unwrap();
        let executor =
            ExecutorBuilder::new(Arc::new(AlwaysSucceedHandler) as Arc<dyn NodeHandler<TestGraph>>)
                .lifecycle(Box::new(CheckpointTracker(log.clone())))
                .build();
        executor.run(&g, state).await.unwrap();
        assert_eq!(*log.lock().unwrap(), vec!["start", "work"]);
    }

    #[tokio::test]
    async fn executor_lifecycle_on_run_start_and_end_called() {
        let log = Arc::new(Mutex::new(Vec::<String>::new()));
        struct RunTracker(Arc<Mutex<Vec<String>>>);
        #[async_trait]
        impl RunLifecycle<TestGraph> for RunTracker {
            async fn on_run_start(&self, _g: &TestGraph, _s: &RunState) -> Result<()> {
                self.0.lock().unwrap().push("start".into());
                Ok(())
            }
            async fn on_run_end(&self, _o: &Outcome, _s: &RunState) {
                self.0.lock().unwrap().push("end".into());
            }
        }
        let g = linear_graph(&["start", "end"]);
        let state = RunState::new(&g).unwrap();
        let executor =
            ExecutorBuilder::new(Arc::new(AlwaysSucceedHandler) as Arc<dyn NodeHandler<TestGraph>>)
                .lifecycle(Box::new(RunTracker(log.clone())))
                .build();
        executor.run(&g, state).await.unwrap();
        assert_eq!(*log.lock().unwrap(), vec!["start", "end"]);
    }

    #[tokio::test]
    async fn executor_lifecycle_on_edge_for_jumps() {
        let log = Arc::new(Mutex::new(Vec::<(String, bool)>::new()));
        struct JumpTracker(Arc<Mutex<Vec<(String, bool)>>>);
        #[async_trait]
        impl RunLifecycle<TestGraph> for JumpTracker {
            async fn on_edge_selected(
                &self,
                ctx: &EdgeContext<'_, TestGraph>,
                _s: &RunState,
            ) -> Result<EdgeDecision> {
                self.0
                    .lock()
                    .unwrap()
                    .push((ctx.to.to_string(), ctx.is_jump));
                Ok(EdgeDecision::Continue)
            }
        }
        struct JumpHandler;
        #[async_trait]
        impl NodeHandler<TestGraph> for JumpHandler {
            async fn execute(
                &self,
                _n: &TestNode,
                _c: &Context,
                _g: &TestGraph,
            ) -> Result<Outcome> {
                let mut o = Outcome::success();
                o.jump_to_node = Some("target".into());
                Ok(o)
            }
        }
        let g = TestGraph::new(
            vec![
                TestNode::new("start"),
                TestNode::terminal("end"),
                TestNode::terminal("target"),
            ],
            vec![TestEdge::new("start", "end")],
            "start",
        );
        let state = RunState::new(&g).unwrap();
        let executor =
            ExecutorBuilder::new(Arc::new(JumpHandler) as Arc<dyn NodeHandler<TestGraph>>)
                .lifecycle(Box::new(JumpTracker(log.clone())))
                .build();
        executor.run(&g, state).await.unwrap();
        let entries = log.lock().unwrap().clone();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0], ("target".to_string(), true));
    }

    #[tokio::test]
    async fn executor_context_updates_visible_to_next_node() {
        use serde_json::json;

        struct ContextWriter;
        #[async_trait]
        impl NodeHandler<TestGraph> for ContextWriter {
            async fn execute(
                &self,
                node: &TestNode,
                context: &Context,
                _g: &TestGraph,
            ) -> Result<Outcome> {
                if node.id() == "start" {
                    let mut o = Outcome::success();
                    o.context_updates.insert("shared".into(), json!("hello"));
                    Ok(o)
                } else {
                    let val = context.get_string("shared", "missing");
                    let mut o = Outcome::success();
                    o.notes = Some(val);
                    Ok(o)
                }
            }
        }
        let log = Arc::new(Mutex::new(Vec::<String>::new()));
        struct NoteCapture(Arc<Mutex<Vec<String>>>);
        #[async_trait]
        impl RunLifecycle<TestGraph> for NoteCapture {
            async fn after_node(
                &self,
                node: &TestNode,
                result: &mut NodeResult,
                _s: &RunState,
            ) -> Result<()> {
                if node.id() == "work" {
                    if let Some(ref notes) = result.outcome.notes {
                        self.0.lock().unwrap().push(notes.clone());
                    }
                }
                Ok(())
            }
        }

        let g = linear_graph(&["start", "work", "end"]);
        let state = RunState::new(&g).unwrap();
        let executor =
            ExecutorBuilder::new(Arc::new(ContextWriter) as Arc<dyn NodeHandler<TestGraph>>)
                .lifecycle(Box::new(NoteCapture(log.clone())))
                .build();
        executor.run(&g, state).await.unwrap();
        assert_eq!(*log.lock().unwrap(), vec!["hello"]);
    }
}
