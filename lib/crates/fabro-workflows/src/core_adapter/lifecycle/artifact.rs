use async_trait::async_trait;

use fabro_core::lifecycle::RunLifecycle;

use super::super::graph::WorkflowGraph;
/// Sub-lifecycle responsible for artifact collection, offloading, and syncing.
/// Currently a stub — artifact operations are not yet wired through the core adapter.
pub struct ArtifactLifecycle;

#[async_trait]
impl RunLifecycle<WorkflowGraph> for ArtifactLifecycle {}
