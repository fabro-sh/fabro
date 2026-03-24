use async_trait::async_trait;

use fabro_core::lifecycle::RunLifecycle;

use super::super::graph::WorkflowGraph;
/// Sub-lifecycle responsible for git operations (checkpoint commits, pushes, diffs).
/// Currently a stub — git operations are not yet wired through the core adapter.
pub struct GitLifecycle;

#[async_trait]
impl RunLifecycle<WorkflowGraph> for GitLifecycle {}
