use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use async_trait::async_trait;
use fabro_core::error::{Error as CoreError, Result as CoreResult};
use fabro_core::graph::NodeSpec;
use fabro_core::lifecycle::{AttemptContext, AttemptResultContext, NodeDecision, RunLifecycle};
use fabro_core::outcome::NodeResult;
use fabro_core::state::ExecutionState;
use fabro_store::{ArtifactKey, ArtifactStore};
use fabro_types::{ArtifactUpload, RunId, StageId};
use tokio::fs;
use tokio::time::sleep;

use crate::artifact::{normalize_durable_updates, offload_large_values, sync_artifacts_to_env};
use crate::artifact_snapshot::collect_artifacts;
use crate::artifact_upload::ArtifactSink;
use crate::event::{Emitter, Event, RunNoticeCode, RunNoticeLevel};
use crate::graph::{WorkflowGraph, WorkflowNode};
use crate::lifecycle::event::{stage_scope_for, stage_visit};
use crate::outcome::BilledModelUsage;
use crate::runtime_store::RunStoreHandle;

type WfRunState = ExecutionState<Option<BilledModelUsage>>;
type WfNodeResult = NodeResult<Option<BilledModelUsage>>;
type WfNodeDecision = NodeDecision<Option<BilledModelUsage>>;

const ARTIFACT_UPLOAD_RETRY_DELAYS: [Duration; 3] = [
    Duration::from_millis(100),
    Duration::from_millis(250),
    Duration::from_millis(500),
];

/// Sub-lifecycle responsible for artifact collection, offloading, and syncing.
pub(crate) struct ArtifactLifecycle {
    pub sandbox:         Arc<dyn fabro_sandbox::Sandbox>,
    pub run_store:       RunStoreHandle,
    pub emitter:         Arc<Emitter>,
    pub run_id:          RunId,
    pub artifact_globs:  Vec<String>,
    pub artifact_sink:   Option<ArtifactSink>,
    /// Per-attempt state: epoch seconds when the attempt started.
    attempt_start_epoch: std::sync::Mutex<Option<f64>>,
}

impl ArtifactLifecycle {
    pub(crate) fn new(
        sandbox: Arc<dyn fabro_sandbox::Sandbox>,
        run_store: RunStoreHandle,
        emitter: Arc<Emitter>,
        run_id: RunId,
        artifact_globs: Vec<String>,
        artifact_sink: Option<ArtifactSink>,
    ) -> Self {
        Self {
            sandbox,
            run_store,
            emitter,
            run_id,
            artifact_globs,
            artifact_sink,
            attempt_start_epoch: std::sync::Mutex::new(None),
        }
    }
}

#[async_trait]
impl RunLifecycle<WorkflowGraph> for ArtifactLifecycle {
    async fn on_run_start(&self, _graph: &WorkflowGraph, _state: &WfRunState) -> CoreResult<()> {
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
            .map_or(0.0, |d| d.as_secs() as f64);
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
        let visit = stage_visit(state, node_id);
        let node_slug = if visit <= 1 {
            node_id.to_string()
        } else {
            format!("{node_id}-visit_{visit}")
        };
        let artifact_capture_dir =
            tempfile::tempdir().map_err(|err| CoreError::Other(err.to_string()))?;

        match collect_artifacts(
            &*self.sandbox,
            artifact_capture_dir.path(),
            &self.artifact_globs,
            epoch,
        )
        .await
        {
            Ok(summary) if summary.files_copied > 0 => {
                let stage_id = StageId::new(node_id.to_string(), visit);
                if let Err(err) = self
                    .persist_artifacts(
                        &stage_id,
                        ctx.attempt,
                        artifact_capture_dir.path(),
                        &summary.captured_assets,
                    )
                    .await
                {
                    self.emitter.notice(
                        RunNoticeLevel::Warn,
                        RunNoticeCode::ArtifactUploadFailed,
                        format!("[node: {node_id}] artifact upload failed: {err}"),
                    );
                    return Ok(());
                }
                let scope = stage_scope_for(state, node_id);
                for asset in &summary.captured_assets {
                    self.emitter.emit_scoped(
                        &Event::ArtifactCaptured {
                            node_id:        node_id.to_string(),
                            attempt:        ctx.attempt,
                            node_slug:      node_slug.clone(),
                            path:           asset.path.clone(),
                            mime:           asset.mime.clone(),
                            content_md5:    asset.content_md5.clone(),
                            content_sha256: asset.content_sha256.clone(),
                            bytes:          asset.bytes,
                        },
                        &scope,
                    );
                }
            }
            Ok(_) => {} // no files collected
            Err(e) => {
                self.emitter.notice(
                    RunNoticeLevel::Warn,
                    RunNoticeCode::ArtifactCollectionFailed,
                    format!("[node: {node_id}] artifact collection failed: {e}"),
                );
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
        if let Err(e) =
            offload_large_values(&mut result.outcome.context_updates, &self.run_store).await
        {
            self.emitter.notice(
                RunNoticeLevel::Warn,
                RunNoticeCode::ArtifactOffloadFailed,
                format!("[node: {node_id}] artifact offload failed: {e}"),
            );
        }

        normalize_durable_updates(&mut result.outcome.context_updates);

        // Sync file-backed artifacts to sandbox environment
        if let Err(e) =
            sync_artifacts_to_env(&mut result.outcome.context_updates, &*self.sandbox).await
        {
            self.emitter.notice(
                RunNoticeLevel::Warn,
                RunNoticeCode::ArtifactSyncFailed,
                format!("[node: {node_id}] artifact sync failed: {e}"),
            );
        }

        Ok(())
    }
}

impl ArtifactLifecycle {
    async fn persist_artifacts(
        &self,
        stage_id: &StageId,
        retry: u32,
        artifact_capture_dir: &std::path::Path,
        artifacts: &[ArtifactUpload],
    ) -> Result<()> {
        let Some(sink) = self.artifact_sink.as_ref() else {
            return Err(anyhow!("artifact sink is not configured"));
        };

        let mut last_error = None;
        for attempt in 0..=ARTIFACT_UPLOAD_RETRY_DELAYS.len() {
            match self
                .persist_artifacts_once(sink, stage_id, retry, artifact_capture_dir, artifacts)
                .await
            {
                Ok(()) => return Ok(()),
                Err(err) => last_error = Some(err),
            }

            if let Some(delay) = ARTIFACT_UPLOAD_RETRY_DELAYS.get(attempt) {
                sleep(*delay).await;
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow!("artifact upload failed")))
    }

    async fn persist_artifacts_once(
        &self,
        sink: &ArtifactSink,
        stage_id: &StageId,
        retry: u32,
        artifact_capture_dir: &std::path::Path,
        artifacts: &[ArtifactUpload],
    ) -> Result<()> {
        match sink {
            ArtifactSink::Store(store) => {
                self.store_artifacts(store, stage_id, retry, artifact_capture_dir, artifacts)
                    .await
            }
            ArtifactSink::Uploader(uploader) => {
                uploader
                    .upload_stage_artifacts(stage_id, retry, artifact_capture_dir, artifacts)
                    .await
            }
        }
    }

    async fn store_artifacts(
        &self,
        store: &ArtifactStore,
        stage_id: &StageId,
        retry: u32,
        artifact_capture_dir: &std::path::Path,
        artifacts: &[ArtifactUpload],
    ) -> Result<()> {
        for artifact in artifacts {
            let local_path = artifact_capture_dir.join(&artifact.path);
            let bytes = fs::read(&local_path)
                .await
                .with_context(|| format!("failed to read artifact {}", local_path.display()))?;
            store
                .put(
                    &self.run_id,
                    &ArtifactKey::new(stage_id.clone(), retry, artifact.path.clone()),
                    &bytes,
                )
                .await
                .map_err(anyhow::Error::new)?;
        }
        Ok(())
    }
}
