use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use fabro_core::error::CoreError;
use fabro_core::graph::NodeSpec;
use fabro_core::lifecycle::RunLifecycle;
use fabro_core::outcome::NodeResult;
use fabro_core::state::RunState;

use super::super::graph::WorkflowGraph;
use super::super::WorkflowNode;
use crate::artifact::ArtifactStore;
use crate::engine::{self, RunConfig};
use crate::event::{EventEmitter, RunNoticeLevel, WorkflowRunEvent};
use crate::outcome::{Outcome, StageStatus, StageUsage};

type WfRunState = RunState<Option<StageUsage>>;
type WfNodeResult = NodeResult<Option<StageUsage>>;

/// Result of a git checkpoint operation, shared with EventLifecycle.
#[derive(Debug, Clone)]
pub struct GitCheckpointResult {
    pub commit_sha: Option<String>,
    pub push_results: Vec<(String, bool)>,
}

/// Sub-lifecycle responsible for git operations (checkpoint commits, pushes, diffs).
pub struct GitLifecycle {
    pub sandbox: Arc<dyn fabro_sandbox::Sandbox>,
    pub artifact_store: Arc<Mutex<ArtifactStore>>,
    pub emitter: Arc<EventEmitter>,
    pub run_dir: PathBuf,
    pub run_id: String,
    pub config: Arc<RunConfig>,
    pub start_node_id: Option<String>,
    // Cross-lifecycle data (shared with EventLifecycle)
    pub checkpoint_git_result: Arc<Mutex<Option<GitCheckpointResult>>>,
    pub last_git_sha: Arc<Mutex<Option<String>>>,
}

#[async_trait]
impl RunLifecycle<WorkflowGraph> for GitLifecycle {
    async fn on_run_start(
        &self,
        _graph: &WorkflowGraph,
        _state: &WfRunState,
    ) -> fabro_core::error::Result<()> {
        // Reset last_git_sha (diff base parity)
        *self.last_git_sha.lock().unwrap() = None;
        *self.checkpoint_git_result.lock().unwrap() = None;

        // Init metadata branch (best-effort)
        if let (Some(_), Some(ref repo_path)) =
            (&self.config.meta_branch, &self.config.host_repo_path)
        {
            let store = crate::git::MetadataStore::new(repo_path, &self.config.git_author);
            let manifest_bytes = {
                let manifest_path = self.run_dir.join("manifest.json");
                std::fs::read(&manifest_path).unwrap_or_default()
            };
            let dot_source = std::fs::read(self.run_dir.join("graph.fabro"))
                .or_else(|_| std::fs::read(self.run_dir.join("graph.dot")))
                .unwrap_or_default();
            let sandbox_json = std::fs::read(self.run_dir.join("sandbox.json")).ok();
            let mut extra_files: Vec<(&str, &[u8])> = Vec::new();
            if let Some(ref data) = sandbox_json {
                extra_files.push(("sandbox.json", data));
            }
            if let Err(e) = store.init_run(&self.run_id, &manifest_bytes, &dot_source, &extra_files)
            {
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
        _next_node_id: Option<&str>,
        state: &WfRunState,
    ) -> fabro_core::error::Result<()> {
        let node_id = node.id();

        // Skip git checkpoint for the start node (always empty) or if git disabled
        if self.start_node_id.as_deref() == Some(node_id) || !self.config.git_checkpoint_enabled {
            *self.checkpoint_git_result.lock().unwrap() = None;
            return Ok(());
        }

        // Shadow commit (best-effort, metadata branch)
        let shadow_sha: Option<String> = if let (Some(_), Some(ref repo_path)) =
            (&self.config.meta_branch, &self.config.host_repo_path)
        {
            let store = crate::git::MetadataStore::new(repo_path, &self.config.git_author);
            // Build checkpoint JSON for shadow branch
            let checkpoint_path = self.run_dir.join("checkpoint.json");
            std::fs::read(&checkpoint_path).ok().and_then(|cp_json| {
                let artifact_store = self.artifact_store.lock().unwrap();
                let mut extra_entries: Vec<(String, Vec<u8>)> = artifact_store
                    .list()
                    .iter()
                    .filter_map(|info| {
                        info.file_path.as_ref().and_then(|path| {
                            std::fs::read(path)
                                .ok()
                                .map(|data| (format!("artifacts/{}.json", info.id), data))
                        })
                    })
                    .collect();
                extra_entries.extend(crate::git::scan_node_files(&self.run_dir));
                let extra_refs: Vec<(&str, &[u8])> = extra_entries
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.as_slice()))
                    .collect();
                match store.write_checkpoint(&self.run_id, &cp_json, &extra_refs) {
                    Ok(sha) => Some(sha),
                    Err(e) => {
                        self.emitter.emit(&WorkflowRunEvent::RunNotice {
                            level: RunNoticeLevel::Warn,
                            code: "checkpoint_metadata_write_failed".to_string(),
                            message: format!(
                                "[node: {node_id}] metadata checkpoint write failed: {e}"
                            ),
                        });
                        None
                    }
                }
            })
        } else {
            None
        };

        // Run branch commit via sandbox
        let completed_count = state.completed_nodes.len();
        let commit_result = engine::git_checkpoint(
            &*self.sandbox,
            &self.run_id,
            node_id,
            &result.outcome.status.to_string(),
            completed_count,
            shadow_sha,
            &self.config.checkpoint_exclude_globs,
            &self.config.git_author,
        )
        .await;

        match commit_result {
            Ok(sha) => {
                let mut git_result = GitCheckpointResult {
                    commit_sha: Some(sha.clone()),
                    push_results: Vec::new(),
                };

                // Re-save checkpoint.json with SHA
                let checkpoint_path = self.run_dir.join("checkpoint.json");
                if let Ok(mut cp) = crate::checkpoint::Checkpoint::load(&checkpoint_path) {
                    cp.git_commit_sha = Some(sha.clone());
                    if let Err(e) = cp.save(&checkpoint_path) {
                        self.emitter.emit(&WorkflowRunEvent::RunNotice {
                            level: RunNoticeLevel::Warn,
                            code: "checkpoint_resave_failed".to_string(),
                            message: format!(
                                "[node: {node_id}] checkpoint re-save with SHA failed: {e}"
                            ),
                        });
                    }
                }

                // Push run branch (skip in dry-run mode)
                if !self.config.dry_run {
                    if let Some(ref branch) = self.config.run_branch {
                        let push_ok = if self.sandbox.git_push_branch(branch).await {
                            true
                        } else if let Some(ref repo_path) = self.config.host_repo_path {
                            let refspec = format!("refs/heads/{branch}");
                            engine::git_push_host(
                                repo_path,
                                &refspec,
                                &self.config.github_app,
                                "run branch",
                            )
                            .await
                        } else {
                            false
                        };
                        git_result.push_results.push((branch.clone(), push_ok));
                    }
                    // Push metadata branch (always from host)
                    if let (Some(ref meta_branch), Some(ref repo_path)) =
                        (&self.config.meta_branch, &self.config.host_repo_path)
                    {
                        let refspec = format!("refs/heads/{meta_branch}");
                        let meta_push_ok = engine::git_push_host(
                            repo_path,
                            &refspec,
                            &self.config.github_app,
                            "metadata branch",
                        )
                        .await;
                        git_result
                            .push_results
                            .push((meta_branch.clone(), meta_push_ok));
                    }
                }

                // Save diff.patch
                let visit = state.node_visits.get(node_id).copied().unwrap_or(1);
                let prev = self
                    .last_git_sha
                    .lock()
                    .unwrap()
                    .clone()
                    .or_else(|| self.config.base_sha.clone())
                    .unwrap_or_else(|| sha.clone());
                let diff_dest = engine::node_dir(&self.run_dir, node_id, visit).join("diff.patch");

                match engine::git_diff(&*self.sandbox, &prev).await {
                    Ok(patch) if !patch.is_empty() => {
                        let _ = std::fs::write(&diff_dest, patch);
                    }
                    Ok(_) => {}
                    Err(err) => {
                        self.emitter.emit(&WorkflowRunEvent::RunNotice {
                            level: RunNoticeLevel::Warn,
                            code: "git_diff_failed".to_string(),
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
                self.emitter.emit(&WorkflowRunEvent::CheckpointFailed {
                    node_id: node_id.to_string(),
                    error: e.clone(),
                });
                return Err(CoreError::Other(format!(
                    "git checkpoint commit failed for node '{node_id}': {e}"
                )));
            }
        }

        Ok(())
    }

    async fn on_run_end(&self, outcome: &Outcome, _state: &WfRunState) {
        // Write final.patch on success
        if (outcome.status == StageStatus::Success || outcome.status == StageStatus::PartialSuccess)
            && self.config.git_checkpoint_enabled
        {
            if let Some(ref base_sha) = self.config.base_sha {
                let diff_dest = self.run_dir.join("final.patch");
                match engine::git_diff(&*self.sandbox, base_sha).await {
                    Ok(patch) if !patch.is_empty() => {
                        let _ = std::fs::write(&diff_dest, patch);
                    }
                    Ok(_) => {}
                    Err(err) => {
                        self.emitter.emit(&WorkflowRunEvent::RunNotice {
                            level: RunNoticeLevel::Warn,
                            code: "git_diff_failed".to_string(),
                            message: format!("final diff failed: {err}"),
                        });
                    }
                }
            }
        }
    }
}
