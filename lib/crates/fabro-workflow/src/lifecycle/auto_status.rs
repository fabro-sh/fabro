use async_trait::async_trait;
use fabro_core::error::Result as CoreResult;
use fabro_core::lifecycle::RunLifecycle;
use fabro_core::outcome::NodeResult;
use fabro_core::state::ExecutionState;

use crate::graph::{WorkflowGraph, WorkflowNode};
use crate::outcome::{BilledModelUsage, StageOutcome};

type WfRunState = ExecutionState<Option<BilledModelUsage>>;
type WfNodeResult = NodeResult<Option<BilledModelUsage>>;

/// Sub-lifecycle responsible for auto-status override on nodes with
/// `auto_status=true`.
pub(crate) struct AutoStatusLifecycle;

#[async_trait]
impl RunLifecycle<WorkflowGraph> for AutoStatusLifecycle {
    async fn after_node(
        &self,
        node: &WorkflowNode,
        result: &mut WfNodeResult,
        _state: &WfRunState,
    ) -> CoreResult<()> {
        let gv = node.inner();
        let outcome = &mut result.outcome;
        if gv.auto_status()
            && outcome.status != StageOutcome::Succeeded
            && outcome.status != StageOutcome::Skipped
        {
            outcome.status = StageOutcome::Succeeded;
            outcome.notes =
                Some("auto-status: handler completed without writing status".to_string());
        }
        Ok(())
    }
}
