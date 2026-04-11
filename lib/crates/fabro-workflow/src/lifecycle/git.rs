use std::collections::HashMap;
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
use crate::git::MetadataStore;
use crate::graph::{WorkflowGraph, WorkflowNode};
use crate::lifecycle::event::stage_scope_for;
use crate::outcome::{BilledModelUsage, Outcome, StageStatus};
use crate::run_dump::RunDump;
use crate::run_options::RunOptions;
use crate::runtime_store::RunStoreHandle;
use crate::sandbox_git::{git_checkpoint, git_diff, git_push_host};

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
    pub start_node_id:         Option<String>,
    // Cross-lifecycle data (shared with EventLifecycle)
    pub checkpoint_git_result: Arc<Mutex<Option<GitCheckpointResult>>>,
    pub last_git_sha:          Arc<Mutex<Option<String>>>,
    pub final_patch:           Arc<Mutex<Option<String>>>,
}

#[async_trait]
impl RunLifecycle<WorkflowGraph> for GitLifecycle {
    async fn on_run_start(&self, _graph: &WorkflowGraph, _state: &WfRunState) -> CoreResult<()> {
        // Reset last_git_sha (diff base parity)
        *self.last_git_sha.lock().unwrap() = None;
        *self.checkpoint_git_result.lock().unwrap() = None;
        *self.final_patch.lock().unwrap() = None;

        // Init metadata branch (best-effort)
        if let (Some(_), Some(repo_path)) = (
            self.run_options
                .git
                .as_ref()
                .and_then(|g| g.meta_branch.as_ref()),
            self.run_options.host_repo_path.as_ref(),
        ) {
            let git_author = self.run_options.git_author();
            let store = MetadataStore::new(repo_path, &git_author);
            let state = self.run_store.state().await.ok();
            let init_dump = state.as_ref().map(RunDump::metadata_init);
            let init_entries = init_dump
                .as_ref()
                .and_then(|dump| dump.git_entries().ok())
                .unwrap_or_default();
            let refs: Vec<(&str, &[u8])> = init_entries
                .iter()
                .map(|(path, bytes)| (path.as_str(), bytes.as_slice()))
                .collect();
            if let Err(e) = store.init_run(&self.run_id.to_string(), &refs) {
                tracing::warn!(
                    run_id = %self.run_id,
                    error = %e,
                    "Metadata branch init failed"
                );
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

        // Shadow commit (best-effort, metadata branch)
        let shadow_sha: Option<String> = if let (Some(_), Some(repo_path)) = (
            self.run_options
                .git
                .as_ref()
                .and_then(|g| g.meta_branch.as_ref()),
            self.run_options.host_repo_path.as_ref(),
        ) {
            let git_author = self.run_options.git_author();
            let store = MetadataStore::new(repo_path, &git_author);
            let checkpoint = build_checkpoint(
                node,
                result,
                next_node_id,
                state,
                HashMap::new(),
                HashMap::new(),
                None,
            );
            if let Ok(cp_json) = serde_json::to_vec_pretty(&checkpoint) {
                let mut extra_entries: Vec<(String, Vec<u8>)> = Vec::new();
                if let Ok(store_state) = self.run_store.state().await {
                    if let Ok(mut dump_entries) =
                        RunDump::metadata_checkpoint(&store_state).git_entries()
                    {
                        extra_entries.append(&mut dump_entries);
                    }
                }
                let extra_refs: Vec<(&str, &[u8])> = extra_entries
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.as_slice()))
                    .collect();
                match store.write_checkpoint(&self.run_id.to_string(), &cp_json, &extra_refs) {
                    Ok(sha) => Some(sha),
                    Err(e) => {
                        self.emitter.emit(&Event::RunNotice {
                            level:   RunNoticeLevel::Warn,
                            code:    "checkpoint_metadata_write_failed".to_string(),
                            message: format!(
                                "[node: {node_id}] metadata checkpoint write failed: {e}"
                            ),
                        });
                        None
                    }
                }
            } else {
                None
            }
        } else {
            None
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
                        let push_ok = if self.sandbox.git_push_branch(branch).await {
                            true
                        } else if let Some(repo_path) = self.run_options.host_repo_path.as_ref() {
                            let refspec = format!("refs/heads/{branch}");
                            git_push_host(
                                repo_path,
                                &refspec,
                                &self.run_options.github_app,
                                "run branch",
                            )
                            .await
                        } else {
                            false
                        };
                        git_result.push_results.push((branch.clone(), push_ok));
                    }
                    // Push metadata branch (always from host)
                    if let (Some(meta_branch), Some(repo_path)) = (
                        self.run_options
                            .git
                            .as_ref()
                            .and_then(|g| g.meta_branch.as_ref()),
                        self.run_options.host_repo_path.as_ref(),
                    ) {
                        let refspec = format!("refs/heads/{meta_branch}");
                        let meta_push_ok = git_push_host(
                            repo_path,
                            &refspec,
                            &self.run_options.github_app,
                            "metadata branch",
                        )
                        .await;
                        git_result
                            .push_results
                            .push((meta_branch.clone(), meta_push_ok));
                    }
                }

                // Save diff.patch
                let prev = self
                    .last_git_sha
                    .lock()
                    .unwrap()
                    .clone()
                    .or_else(|| {
                        self.run_options
                            .git
                            .as_ref()
                            .and_then(|g| g.base_sha.clone())
                    })
                    .unwrap_or_else(|| sha.clone());
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

    async fn on_run_end(&self, outcome: &Outcome, _state: &WfRunState) {
        // Capture the final diff on success for event/store projection.
        if (outcome.status == StageStatus::Success || outcome.status == StageStatus::PartialSuccess)
            && self.run_options.git.is_some()
        {
            if let Some(base_sha) = self
                .run_options
                .git
                .as_ref()
                .and_then(|g| g.base_sha.clone())
            {
                match git_diff(&*self.sandbox, &base_sha).await {
                    Ok(patch) if !patch.is_empty() => {
                        *self.final_patch.lock().unwrap() = Some(patch.clone());
                    }
                    Ok(_) => {
                        *self.final_patch.lock().unwrap() = None;
                    }
                    Err(err) => {
                        *self.final_patch.lock().unwrap() = None;
                        self.emitter.emit(&Event::RunNotice {
                            level:   RunNoticeLevel::Warn,
                            code:    "git_diff_failed".to_string(),
                            message: format!("final diff failed: {err}"),
                        });
                    }
                }
            }
        }
    }
}
