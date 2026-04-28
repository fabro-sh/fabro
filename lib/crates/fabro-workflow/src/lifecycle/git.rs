use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fabro_core::error::{Error as CoreError, Result as CoreResult};
use fabro_core::graph::NodeSpec;
use fabro_core::lifecycle::RunLifecycle;
use fabro_core::outcome::NodeResult;
use fabro_core::state::ExecutionState;
use fabro_types::RunId;

use crate::artifact;
use crate::event::{Emitter, Event, RunNoticeLevel};
use crate::graph::{WorkflowGraph, WorkflowNode};
use crate::lifecycle::event::stage_scope_for;
use crate::outcome::BilledModelUsage;
use crate::run_dump::RunDump;
use crate::run_options::RunOptions;
use crate::runtime_store::RunStoreHandle;
use crate::sandbox_git::{git_checkpoint, git_diff};
use crate::sandbox_metadata::{SandboxGitRuntime, SandboxMetadataWriter};

type WfRunState = ExecutionState<Option<BilledModelUsage>>;
type WfNodeResult = NodeResult<Option<BilledModelUsage>>;

fn build_checkpoint(
    node: &WorkflowNode,
    result: &WfNodeResult,
    next_node_id: Option<&str>,
    state: &WfRunState,
    loop_failure_signatures: std::collections::HashMap<fabro_types::FailureSignature, usize>,
    restart_failure_signatures: std::collections::HashMap<fabro_types::FailureSignature, usize>,
    git_commit_sha: Option<String>,
) -> fabro_types::Checkpoint {
    let mut node_outcomes = state.node_outcomes.clone();
    node_outcomes.insert(node.id().to_string(), result.outcome.clone());
    artifact::normalize_durable_outcomes(&mut node_outcomes);

    fabro_types::Checkpoint {
        timestamp: chrono::Utc::now(),
        current_node: node.id().to_string(),
        completed_nodes: state.completed_nodes.clone(),
        node_outcomes,
        node_retries: state.node_retries.clone(),
        context_values: artifact::durable_context_snapshot(&state.context),
        next_node_id: next_node_id.map(String::from),
        git_commit_sha,
        node_visits: state.node_visits.clone(),
        loop_failure_signatures,
        restart_failure_signatures,
    }
}

/// Result of a git checkpoint operation, shared with EventLifecycle.
#[derive(Debug, Clone)]
pub(crate) struct GitCheckpointResult {
    pub commit_sha:   Option<String>,
    pub push_results: Vec<(String, bool)>,
    pub diff:         Option<String>,
}

/// Sub-lifecycle responsible for git operations (checkpoint commits, pushes,
/// diffs).
pub(crate) struct GitLifecycle {
    pub sandbox:               Arc<dyn fabro_sandbox::Sandbox>,
    pub emitter:               Arc<Emitter>,
    pub run_id:                RunId,
    pub run_store:             RunStoreHandle,
    pub run_options:           Arc<RunOptions>,
    pub metadata_runtime:      Arc<SandboxGitRuntime>,
    pub start_node_id:         Option<String>,
    // Cross-lifecycle data (shared with EventLifecycle)
    pub checkpoint_git_result: Arc<Mutex<Option<GitCheckpointResult>>>,
    pub last_git_sha:          Arc<Mutex<Option<String>>>,
}

#[async_trait]
impl RunLifecycle<WorkflowGraph> for GitLifecycle {
    async fn on_run_start(&self, _graph: &WorkflowGraph, _state: &WfRunState) -> CoreResult<()> {
        // Reset last_git_sha (diff base parity)
        *self.last_git_sha.lock().unwrap() = None;
        *self.checkpoint_git_result.lock().unwrap() = None;
        if self
            .run_options
            .git
            .as_ref()
            .and_then(|g| g.meta_branch.as_ref())
            .is_some()
        {
            match self.run_store.state().await {
                Ok(state) => {
                    let dump = RunDump::from_projection(&state);
                    let _ = self.write_metadata_snapshot(&dump, "init run").await;
                }
                Err(err) => {
                    self.emit_metadata_warning(
                        "checkpoint_metadata_write_failed",
                        format!("failed to load run state for metadata init: {err}"),
                    );
                }
            }
        }

        Ok(())
    }

    async fn on_checkpoint(
        &self,
        node: &WorkflowNode,
        result: &WfNodeResult,
        next_node_id: Option<&str>,
        state: &WfRunState,
    ) -> CoreResult<()> {
        let node_id = node.id();

        // Skip git checkpoint for the start node (always empty) or if git disabled
        if self.start_node_id.as_deref() == Some(node_id) || self.run_options.git.is_none() {
            *self.checkpoint_git_result.lock().unwrap() = None;
            return Ok(());
        }

        let checkpoint = build_checkpoint(
            node,
            result,
            next_node_id,
            state,
            std::collections::HashMap::new(),
            std::collections::HashMap::new(),
            None,
        );
        let shadow_sha = match self.run_store.state().await {
            Ok(mut projection) => {
                projection.checkpoint = Some(checkpoint);
                let dump = RunDump::from_projection(&projection);
                self.write_metadata_snapshot(&dump, "checkpoint").await
            }
            Err(err) => {
                self.emit_metadata_warning(
                    "checkpoint_metadata_write_failed",
                    format!("failed to load run state for metadata checkpoint: {err}"),
                );
                None
            }
        };

        // Run branch commit via sandbox
        let completed_count = state.completed_nodes.len();
        let git_author = self.run_options.git_author();
        let commit_result = git_checkpoint(
            &*self.sandbox,
            &self.run_id.to_string(),
            node_id,
            &result.outcome.status.to_string(),
            completed_count,
            shadow_sha,
            &self.run_options.checkpoint_exclude_globs(),
            &git_author,
        )
        .await;

        match commit_result {
            Ok(sha) => {
                let mut git_result = GitCheckpointResult {
                    commit_sha:   Some(sha.clone()),
                    push_results: Vec::new(),
                    diff:         None,
                };

                // Push run branch (skip in dry-run mode)
                if !self.run_options.dry_run_enabled() {
                    if let Some(branch) = self
                        .run_options
                        .git
                        .as_ref()
                        .and_then(|g| g.run_branch.as_ref())
                    {
                        let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
                        let push_ok = self.sandbox.git_push_ref(&refspec).await;
                        git_result.push_results.push((refspec, push_ok));
                    }
                }

                // Save diff.patch
                let prev = self.last_git_sha.lock().unwrap().clone().or_else(|| {
                    self.run_options
                        .git
                        .as_ref()
                        .and_then(|g| g.base_sha.clone())
                });
                if let Some(prev) = prev.filter(|p| p != &sha) {
                    match git_diff(&*self.sandbox, &prev).await {
                        Ok(patch) if !patch.is_empty() => {
                            git_result.diff = Some(patch);
                        }
                        Ok(_) => {}
                        Err(err) => {
                            self.emitter.emit(&Event::RunNotice {
                                level:   RunNoticeLevel::Warn,
                                code:    "git_diff_failed".to_string(),
                                message: format!("[node: {node_id}] git diff failed: {err}"),
                            });
                        }
                    }
                }

                // Update shared state
                *self.last_git_sha.lock().unwrap() = Some(sha);
                *self.checkpoint_git_result.lock().unwrap() = Some(git_result);
            }
            Err(e) => {
                // Emit CheckpointFailed and return error
                let scope = stage_scope_for(state, node_id);
                self.emitter.emit_scoped(
                    &Event::CheckpointFailed {
                        node_id: node_id.to_string(),
                        error:   e.clone(),
                    },
                    &scope,
                );
                return Err(CoreError::Other(format!(
                    "git checkpoint commit failed for node '{node_id}': {e}"
                )));
            }
        }

        Ok(())
    }
}

impl GitLifecycle {
    async fn write_metadata_snapshot(&self, dump: &RunDump, message: &str) -> Option<String> {
        if self.metadata_runtime.metadata_degraded() {
            return None;
        }
        let meta_branch = self
            .run_options
            .git
            .as_ref()
            .and_then(|git| git.meta_branch.as_deref())?;

        let run_id = self.run_id.to_string();
        let writer = SandboxMetadataWriter::new(
            &*self.sandbox,
            &self.metadata_runtime,
            &run_id,
            meta_branch,
            self.run_options.git_author(),
        );
        match writer.write_snapshot(dump, message).await {
            Ok(snapshot) => {
                if !snapshot.pushed {
                    self.emit_metadata_warning(
                        "checkpoint_metadata_push_failed",
                        format!("failed to push metadata ref refs/heads/{meta_branch}"),
                    );
                }
                Some(snapshot.commit_sha)
            }
            Err(err) => {
                self.emit_metadata_warning(
                    "checkpoint_metadata_write_failed",
                    format!("failed to write checkpoint metadata: {err}"),
                );
                None
            }
        }
    }

    fn emit_metadata_warning(&self, code: &str, message: String) {
        if self.metadata_runtime.mark_metadata_degraded() {
            self.emitter.emit(&Event::RunNotice {
                level: RunNoticeLevel::Warn,
                code: code.to_string(),
                message,
            });
        }
    }
}
