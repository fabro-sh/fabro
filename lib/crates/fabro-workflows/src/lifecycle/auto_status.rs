use async_trait::async_trait;

use fabro_core::lifecycle::RunLifecycle;
use fabro_core::outcome::NodeResult;
use fabro_core::state::RunState;

use crate::graph::WorkflowGraph;
use crate::graph::WorkflowNode;
use crate::outcome::{StageStatus, StageUsage};

type WfRunState = RunState<Option<StageUsage>>;
type WfNodeResult = NodeResult<Option<StageUsage>>;

/// Sub-lifecycle responsible for auto-status override on nodes with `auto_status=true`.
pub struct AutoStatusLifecycle;

#[async_trait]
impl RunLifecycle<WorkflowGraph> for AutoStatusLifecycle {
    async fn after_node(
        &self,
        node: &WorkflowNode,
        result: &mut WfNodeResult,
        _state: &WfRunState,
    ) -> fabro_core::error::Result<()> {
        let gv = node.inner();
        let outcome = &mut result.outcome;
        if gv.auto_status()
            && outcome.status != StageStatus::Success
            && outcome.status != StageStatus::Skipped
        {
            outcome.status = StageStatus::Success;
            outcome.notes =
                Some("auto-status: handler completed without writing status".to_string());
        }
        Ok(())
    }
}
