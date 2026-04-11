use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use fabro_core::error::{Error as CoreError, Result as CoreResult};
use fabro_core::lifecycle::{
    AttemptContext, EdgeContext, EdgeDecision, NodeDecision, RunLifecycle,
};
use fabro_core::outcome::NodeResult;
use fabro_core::state::ExecutionState;
use fabro_hooks::{HookContext, HookDecision, HookEvent, HookRunner};
use fabro_sandbox::Sandbox;
use fabro_types::RunId;

use crate::graph::{WorkflowGraph, WorkflowNode};
use crate::hook_context::set_hook_node;
use crate::outcome::{BilledModelUsage, Outcome, OutcomeExt, StageStatus};

type WfRunState = ExecutionState<Option<BilledModelUsage>>;
type WfNodeResult = NodeResult<Option<BilledModelUsage>>;
type WfNodeDecision = NodeDecision<Option<BilledModelUsage>>;

/// Sub-lifecycle responsible for running workflow hooks.
pub(crate) struct HookLifecycle {
    pub hook_runner:   Option<Arc<HookRunner>>,
    pub sandbox:       Arc<dyn Sandbox>,
    pub hook_work_dir: Option<PathBuf>,
    pub run_id:        RunId,
    pub graph_name:    String,
}

impl HookLifecycle {
    async fn run_hook(&self, hook_ctx: &HookContext) -> HookDecision {
        let Some(ref runner) = self.hook_runner else {
            return HookDecision::Proceed;
        };
        runner
            .run(
                hook_ctx,
                self.sandbox.clone(),
                self.hook_work_dir.as_deref(),
            )
            .await
    }
}

#[async_trait]
impl RunLifecycle<WorkflowGraph> for HookLifecycle {
    async fn on_run_start(&self, _graph: &WorkflowGraph, _state: &WfRunState) -> CoreResult<()> {
        let hook_ctx = HookContext::new(HookEvent::RunStart, self.run_id, self.graph_name.clone());
        let decision = self.run_hook(&hook_ctx).await;
        if let HookDecision::Block { reason } = decision {
            let msg = reason.unwrap_or_else(|| "blocked by RunStart hook".into());
            return Err(CoreError::blocked(msg));
        }
        Ok(())
    }

    async fn before_attempt(
        &self,
        ctx: &AttemptContext<'_, WorkflowGraph>,
        _state: &WfRunState,
    ) -> CoreResult<WfNodeDecision> {
        let gv = ctx.node.inner();
        let mut hook_ctx =
            HookContext::new(HookEvent::StageStart, self.run_id, self.graph_name.clone());
        hook_ctx.cwd = self
            .hook_work_dir
            .as_ref()
            .map(|path| path.display().to_string());
        set_hook_node(&mut hook_ctx, gv);
        hook_ctx.attempt = Some(ctx.attempt as usize);
        hook_ctx.max_attempts = Some(ctx.max_attempts as usize);
        let decision = self.run_hook(&hook_ctx).await;
        match decision {
            HookDecision::Skip { reason } => {
                let msg = reason.unwrap_or_else(|| "skipped by StageStart hook".into());
                Ok(NodeDecision::Skip(Box::new(Outcome::skipped(&msg))))
            }
            HookDecision::Block { reason } => {
                let msg = reason.unwrap_or_else(|| "blocked by StageStart hook".into());
                Err(CoreError::blocked(msg))
            }
            _ => Ok(NodeDecision::Continue),
        }
    }

    async fn after_node(
        &self,
        node: &WorkflowNode,
        result: &mut WfNodeResult,
        _state: &WfRunState,
    ) -> CoreResult<()> {
        let outcome = &result.outcome;
        // Skipped nodes had no StageStarted, so skip hooks (engine.rs:2080)
        if outcome.status == StageStatus::Skipped {
            return Ok(());
        }
        let hook_event = if outcome.status == StageStatus::Fail {
            HookEvent::StageFailed
        } else {
            HookEvent::StageComplete
        };
        let mut hook_ctx = HookContext::new(hook_event, self.run_id, self.graph_name.clone());
        set_hook_node(&mut hook_ctx, node.inner());
        hook_ctx.status = Some(outcome.status.to_string());
        hook_ctx.failure_reason = outcome.failure_reason().map(String::from);
        let _ = self.run_hook(&hook_ctx).await;
        Ok(())
    }

    async fn on_edge_selected(
        &self,
        ctx: &EdgeContext<'_, WorkflowGraph>,
        _state: &WfRunState,
    ) -> CoreResult<EdgeDecision> {
        let mut hook_ctx = HookContext::new(
            HookEvent::EdgeSelected,
            self.run_id,
            self.graph_name.clone(),
        );
        hook_ctx.edge_from = Some(ctx.from.to_string());
        hook_ctx.edge_to = Some(ctx.to.to_string());
        hook_ctx.edge_label = ctx
            .edge
            .as_ref()
            .and_then(|edge| edge.inner().label().map(String::from));
        let decision = self.run_hook(&hook_ctx).await;
        match decision {
            HookDecision::Override { edge_to } => Ok(EdgeDecision::Override(edge_to)),
            HookDecision::Block { reason } => {
                let msg = reason.unwrap_or_else(|| "blocked by EdgeSelected hook".into());
                Err(CoreError::blocked(msg))
            }
            _ => Ok(EdgeDecision::Continue),
        }
    }

    async fn on_checkpoint(
        &self,
        node: &WorkflowNode,
        _result: &WfNodeResult,
        _next_node_id: Option<&str>,
        _state: &WfRunState,
    ) -> CoreResult<()> {
        let mut hook_ctx = HookContext::new(
            HookEvent::CheckpointSaved,
            self.run_id,
            self.graph_name.clone(),
        );
        hook_ctx.node_id = Some(node.inner().id.clone());
        let _ = self.run_hook(&hook_ctx).await;
        Ok(())
    }

    async fn on_run_end(&self, outcome: &Outcome, state: &WfRunState) {
        if state.cancelled {
            return;
        }
        if outcome.status == StageStatus::Success || outcome.status == StageStatus::PartialSuccess {
            let hook_ctx =
                HookContext::new(HookEvent::RunComplete, self.run_id, self.graph_name.clone());
            let _ = self.run_hook(&hook_ctx).await;
        } else {
            let error_msg = outcome
                .failure
                .as_ref()
                .map_or_else(|| "run failed".to_string(), |f| f.message.clone());
            let mut hook_ctx =
                HookContext::new(HookEvent::RunFailed, self.run_id, self.graph_name.clone());
            hook_ctx.failure_reason = Some(error_msg);
            let _ = self.run_hook(&hook_ctx).await;
        }
    }
}
