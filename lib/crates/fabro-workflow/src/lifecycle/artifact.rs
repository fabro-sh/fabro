use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use fabro_store::SlateRunStore;

use fabro_core::graph::NodeSpec;
use fabro_core::lifecycle::{AttemptContext, AttemptResultContext, RunLifecycle};
use fabro_core::outcome::NodeResult;
use fabro_core::state::ExecutionState;

use crate::artifact::{offload_large_values, sync_artifacts_to_env};
use crate::artifact_snapshot::collect_artifacts;
use crate::event::{EventEmitter, RunNoticeLevel, WorkflowRunEvent};
use crate::graph::WorkflowGraph;
use crate::graph::WorkflowNode;
use crate::outcome::StageUsage;
use fabro_core::error::Result as CoreResult;
use fabro_core::lifecycle::NodeDecision;

type WfRunState = ExecutionState<Option<StageUsage>>;
type WfNodeResult = NodeResult<Option<StageUsage>>;
type WfNodeDecision = NodeDecision<Option<StageUsage>>;

/// Sub-lifecycle responsible for artifact collection, offloading, and syncing.
pub(crate) struct ArtifactLifecycle {
    pub sandbox: Arc<dyn fabro_sandbox::Sandbox>,
    pub run_store: SlateRunStore,
    pub blob_cache_dir: PathBuf,
    pub emitter: Arc<EventEmitter>,
    pub artifacts_dir: PathBuf,
    pub artifact_globs: Vec<String>,
    pub captured_artifact_count: Arc<AtomicUsize>,
    /// Per-attempt state: epoch seconds when the attempt started.
    attempt_start_epoch: std::sync::Mutex<Option<f64>>,
}

impl ArtifactLifecycle {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        sandbox: Arc<dyn fabro_sandbox::Sandbox>,
        run_store: SlateRunStore,
        blob_cache_dir: PathBuf,
        emitter: Arc<EventEmitter>,
        artifacts_dir: PathBuf,
        artifact_globs: Vec<String>,
        captured_artifact_count: Arc<AtomicUsize>,
    ) -> Self {
        Self {
            sandbox,
            run_store,
            blob_cache_dir,
            emitter,
            artifacts_dir,
            artifact_globs,
            captured_artifact_count,
            attempt_start_epoch: std::sync::Mutex::new(None),
        }
    }
}

#[async_trait]
impl RunLifecycle<WorkflowGraph> for ArtifactLifecycle {
    async fn on_run_start(&self, _graph: &WorkflowGraph, _state: &WfRunState) -> CoreResult<()> {
        self.captured_artifact_count.store(0, Ordering::Relaxed);
        *self.attempt_start_epoch.lock().unwrap() = None;
        Ok(())
    }

    async fn before_attempt(
        &self,
        _ctx: &AttemptContext<'_, WorkflowGraph>,
        _state: &WfRunState,
    ) -> CoreResult<WfNodeDecision> {
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
    ) -> CoreResult<()> {
        if self.artifact_globs.is_empty() {
            return Ok(());
        }
        let epoch = self.attempt_start_epoch.lock().unwrap().unwrap_or(0.0);
        let node_id = ctx.node.id();
        let visit = state.node_visits.get(node_id).copied().unwrap_or(1);
        let node_slug = if visit <= 1 {
            node_id.to_string()
        } else {
            format!("{node_id}-visit_{visit}")
        };
        let artifact_capture_dir = self
            .artifacts_dir
            .join(&node_slug)
            .join(format!("retry_{}", ctx.attempt));
        let _ = std::fs::create_dir_all(&artifact_capture_dir);

        match collect_artifacts(
            &*self.sandbox,
            &artifact_capture_dir,
            &self.artifact_globs,
            epoch,
        )
        .await
        {
            Ok(summary) if summary.files_copied > 0 => {
                for asset in &summary.captured_assets {
                    self.captured_artifact_count.fetch_add(1, Ordering::Relaxed);
                    self.emitter.emit(&WorkflowRunEvent::ArtifactCaptured {
                        node_id: node_id.to_string(),
                        attempt: ctx.attempt,
                        node_slug: node_slug.clone(),
                        path: asset.path.clone(),
                        mime: asset.mime.clone(),
                        content_md5: asset.content_md5.clone(),
                        content_sha256: asset.content_sha256.clone(),
                        bytes: asset.bytes,
                    });
                }
            }
            Ok(_) => {} // no files collected
            Err(e) => {
                self.emitter.emit(&WorkflowRunEvent::RunNotice {
                    level: RunNoticeLevel::Warn,
                    code: "artifact_collection_failed".to_string(),
                    message: format!("[node: {node_id}] artifact collection failed: {e}"),
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
    ) -> CoreResult<()> {
        let node_id = node.id();

        // Offload large context_updates values to artifact store
        if let Err(e) = offload_large_values(
            &mut result.outcome.context_updates,
            &self.run_store,
            &self.blob_cache_dir,
        )
        .await
        {
            self.emitter.emit(&WorkflowRunEvent::RunNotice {
                level: RunNoticeLevel::Warn,
                code: "artifact_offload_failed".to_string(),
                message: format!("[node: {node_id}] artifact offload failed: {e}"),
            });
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
