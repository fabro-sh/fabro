use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fabro_store::RunStore;

use fabro_core::error::{CoreError, Result as CoreResult};
use fabro_core::graph::NodeSpec;
use fabro_core::lifecycle::RunLifecycle;
use fabro_core::outcome::NodeResult;
use fabro_core::state::RunState;

use crate::artifact::ArtifactStore;
use crate::event::{EventEmitter, RunNoticeLevel, WorkflowRunEvent};
use crate::git::MetadataStore;
use crate::git::scan_node_files;
use crate::graph::WorkflowGraph;
use crate::graph::WorkflowNode;
use crate::outcome::{Outcome, StageStatus, StageUsage};
use crate::records::{Checkpoint, CheckpointExt};
use crate::run_dir::node_dir;
use crate::run_options::RunOptions;
use crate::sandbox_git::{git_checkpoint, git_diff, git_push_host};

type WfRunState = RunState<Option<StageUsage>>;
type WfNodeResult = NodeResult<Option<StageUsage>>;

/// Result of a git checkpoint operation, shared with EventLifecycle.
#[derive(Debug, Clone)]
pub(crate) struct GitCheckpointResult {
    pub commit_sha: Option<String>,
    pub push_results: Vec<(String, bool)>,
}

/// Sub-lifecycle responsible for git operations (checkpoint commits, pushes, diffs).
pub(crate) struct GitLifecycle {
    pub sandbox: Arc<dyn fabro_sandbox::Sandbox>,
    pub artifact_store: Arc<Mutex<ArtifactStore>>,
    pub emitter: Arc<EventEmitter>,
    pub run_dir: PathBuf,
    pub run_id: String,
    pub run_store: Arc<dyn RunStore>,
    pub run_options: Arc<RunOptions>,
    pub start_node_id: Option<String>,
    // Cross-lifecycle data (shared with EventLifecycle)
    pub checkpoint_git_result: Arc<Mutex<Option<GitCheckpointResult>>>,
    pub last_git_sha: Arc<Mutex<Option<String>>>,
}

#[async_trait]
impl RunLifecycle<WorkflowGraph> for GitLifecycle {
    async fn on_run_start(&self, _graph: &WorkflowGraph, _state: &WfRunState) -> CoreResult<()> {
        // Reset last_git_sha (diff base parity)
        *self.last_git_sha.lock().unwrap() = None;
        *self.checkpoint_git_result.lock().unwrap() = None;

        // Init metadata branch (best-effort)
        if let (Some(_), Some(repo_path)) = (
            self.run_options
                .git
                .as_ref()
                .and_then(|g| g.meta_branch.as_ref()),
            self.run_options.host_repo_path.as_ref(),
        ) {
            let store = MetadataStore::new(repo_path, &self.run_options.git_author);
            let run_json = self
                .run_store
                .get_run()
                .await
                .ok()
                .flatten()
                .and_then(|record| serde_json::to_vec_pretty(&record).ok())
                .or_else(|| std::fs::read(self.run_dir.join("run.json")).ok());
            let start_json = self
                .run_store
                .get_start()
                .await
                .ok()
                .flatten()
                .and_then(|record| serde_json::to_vec_pretty(&record).ok())
                .or_else(|| std::fs::read(self.run_dir.join("start.json")).ok());
            let sandbox_json = self
                .run_store
                .get_sandbox()
                .await
                .ok()
                .flatten()
                .and_then(|record| serde_json::to_vec_pretty(&record).ok())
                .or_else(|| std::fs::read(self.run_dir.join("sandbox.json")).ok());
            let mut files: Vec<(&str, &[u8])> = Vec::new();
            if let Some(ref data) = run_json {
                files.push(("run.json", data));
            }
            if let Some(ref data) = start_json {
                files.push(("start.json", data));
            }
            if let Some(ref data) = sandbox_json {
                files.push(("sandbox.json", data));
            }
            if let Err(e) = store.init_run(&self.run_id, &files) {
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
            let store = MetadataStore::new(repo_path, &self.run_options.git_author);
            // Build checkpoint JSON for shadow branch
            self.run_store
                .get_checkpoint()
                .await
                .ok()
                .flatten()
                .and_then(|checkpoint| serde_json::to_vec_pretty(&checkpoint).ok())
                .or_else(|| std::fs::read(self.run_dir.join("checkpoint.json")).ok())
                .and_then(|cp_json| {
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
                    extra_entries.extend(scan_node_files(&self.run_dir));
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
        let commit_result = git_checkpoint(
            &*self.sandbox,
            &self.run_id,
            node_id,
            &result.outcome.status.to_string(),
            completed_count,
            shadow_sha,
            self.run_options.checkpoint_exclude_globs(),
            &self.run_options.git_author,
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
                if let Ok(mut cp) = Checkpoint::load(&checkpoint_path) {
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
                match self.run_store.get_checkpoint().await {
                    Ok(Some(mut checkpoint)) => {
                        checkpoint.git_commit_sha = Some(sha.clone());
                        if let Err(err) = self.run_store.put_checkpoint(&checkpoint).await {
                            self.emitter.emit(&WorkflowRunEvent::RunNotice {
                                level: RunNoticeLevel::Warn,
                                code: "checkpoint_store_resave_failed".to_string(),
                                message: format!(
                                    "[node: {node_id}] checkpoint store re-save with SHA failed: {err}"
                                ),
                            });
                        }
                    }
                    Ok(None) => {}
                    Err(err) => {
                        self.emitter.emit(&WorkflowRunEvent::RunNotice {
                            level: RunNoticeLevel::Warn,
                            code: "checkpoint_store_load_failed".to_string(),
                            message: format!(
                                "[node: {node_id}] checkpoint store load failed: {err}"
                            ),
                        });
                    }
                }

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
                let visit = state.node_visits.get(node_id).copied().unwrap_or(1);
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
                let diff_dest = node_dir(&self.run_dir, node_id, visit).join("diff.patch");

                match git_diff(&*self.sandbox, &prev).await {
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
            && self.run_options.git.is_some()
        {
            if let Some(base_sha) = self
                .run_options
                .git
                .as_ref()
                .and_then(|g| g.base_sha.clone())
            {
                let diff_dest = self.run_dir.join("final.patch");
                match git_diff(&*self.sandbox, &base_sha).await {
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
