use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use fabro_core::graph::NodeSpec;
use fabro_core::lifecycle::{AttemptContext, AttemptResultContext, RunLifecycle};
use fabro_core::outcome::NodeResult;
use fabro_core::state::RunState;

use super::super::graph::WorkflowGraph;
use super::super::WorkflowNode;
use crate::artifact::{offload_large_values, sync_artifacts_to_env, ArtifactStore};
use crate::engine;
use crate::event::{EventEmitter, RunNoticeLevel, WorkflowRunEvent};
use crate::outcome::StageUsage;
use fabro_core::lifecycle::NodeDecision;

type WfRunState = RunState<Option<StageUsage>>;
type WfNodeResult = NodeResult<Option<StageUsage>>;
type WfNodeDecision = NodeDecision<Option<StageUsage>>;

/// Sub-lifecycle responsible for artifact collection, offloading, and syncing.
pub struct ArtifactLifecycle {
    pub sandbox: Arc<dyn fabro_sandbox::Sandbox>,
    pub artifact_store: Arc<Mutex<ArtifactStore>>,
    pub artifact_base_dir: Option<PathBuf>,
    pub emitter: Arc<EventEmitter>,
    pub run_dir: PathBuf,
    pub asset_globs: Vec<String>,
    /// Per-attempt state: epoch seconds when the attempt started.
    attempt_start_epoch: Mutex<Option<f64>>,
}

impl ArtifactLifecycle {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        sandbox: Arc<dyn fabro_sandbox::Sandbox>,
        artifact_store: Arc<Mutex<ArtifactStore>>,
        artifact_base_dir: Option<PathBuf>,
        emitter: Arc<EventEmitter>,
        run_dir: PathBuf,
        asset_globs: Vec<String>,
    ) -> Self {
        Self {
            sandbox,
            artifact_store,
            artifact_base_dir,
            emitter,
            run_dir,
            asset_globs,
            attempt_start_epoch: Mutex::new(None),
        }
    }
}

#[async_trait]
impl RunLifecycle<WorkflowGraph> for ArtifactLifecycle {
    async fn on_run_start(
        &self,
        _graph: &WorkflowGraph,
        _state: &WfRunState,
    ) -> fabro_core::error::Result<()> {
        // Swap in a fresh artifact store on restart (don't call clear() — preserves files on disk)
        let mut store = self.artifact_store.lock().unwrap();
        *store = ArtifactStore::new(self.artifact_base_dir.clone());
        Ok(())
    }

    async fn before_attempt(
        &self,
        _ctx: &AttemptContext<'_, WorkflowGraph>,
        _state: &WfRunState,
    ) -> fabro_core::error::Result<WfNodeDecision> {
        // Record epoch seconds (floored to integer for macOS stat mtime parity)
        let epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as f64)
            .unwrap_or(0.0);
        *self.attempt_start_epoch.lock().unwrap() = Some(epoch);
        Ok(NodeDecision::Continue)
    }

    async fn after_attempt(
        &self,
        ctx: &AttemptResultContext<'_, WorkflowGraph>,
        state: &WfRunState,
    ) -> fabro_core::error::Result<()> {
        if self.asset_globs.is_empty() {
            return Ok(());
        }
        let epoch = self.attempt_start_epoch.lock().unwrap().unwrap_or(0.0);
        let node_id = ctx.node.id();
        let visit = state.node_visits.get(node_id).copied().unwrap_or(1);
        let stage_dir = engine::node_dir(&self.run_dir, node_id, visit);
        let _ = std::fs::create_dir_all(&stage_dir);

        match crate::asset_snapshot::collect_assets(
            &*self.sandbox,
            &stage_dir,
            &self.asset_globs,
            epoch,
        )
        .await
        {
            Ok(summary) if summary.files_copied > 0 => {
                self.emitter.emit(&WorkflowRunEvent::AssetsCaptured {
                    node_id: node_id.to_string(),
                    files_copied: summary.files_copied,
                    total_bytes: summary.total_bytes,
                    files_skipped: summary.files_skipped,
                });
            }
            Ok(_) => {} // no files collected
            Err(e) => {
                self.emitter.emit(&WorkflowRunEvent::RunNotice {
                    level: RunNoticeLevel::Warn,
                    code: "asset_collection_failed".to_string(),
                    message: format!("[node: {node_id}] asset collection failed: {e}"),
                });
            }
        }

        Ok(())
    }

    async fn after_node(
        &self,
        node: &WorkflowNode,
        result: &mut WfNodeResult,
        _state: &WfRunState,
    ) -> fabro_core::error::Result<()> {
        let node_id = node.id();

        // Offload large context_updates values to artifact store
        {
            let store = self.artifact_store.lock().unwrap();
            if let Err(e) = offload_large_values(&mut result.outcome.context_updates, &store) {
                self.emitter.emit(&WorkflowRunEvent::RunNotice {
                    level: RunNoticeLevel::Warn,
                    code: "artifact_offload_failed".to_string(),
                    message: format!("[node: {node_id}] artifact offload failed: {e}"),
                });
            }
        }

        // Sync file-backed artifacts to sandbox environment
        if let Err(e) =
            sync_artifacts_to_env(&mut result.outcome.context_updates, &*self.sandbox).await
        {
            self.emitter.emit(&WorkflowRunEvent::RunNotice {
                level: RunNoticeLevel::Warn,
                code: "artifact_sync_failed".to_string(),
                message: format!("[node: {node_id}] artifact sync failed: {e}"),
            });
        }

        Ok(())
    }
}
