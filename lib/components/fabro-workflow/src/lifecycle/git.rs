use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fabro_core::error::{Error as CoreError, Result as CoreResult};
use fabro_core::graph::NodeSpec;
use fabro_core::lifecycle::RunLifecycle;
use fabro_core::outcome::NodeResult;
use fabro_core::state::ExecutionState;
use fabro_types::{DiffSummary, RunId};

use crate::event::{Emitter, Event, RunNoticeCode, RunNoticeLevel};
use crate::graph::{WorkflowGraph, WorkflowNode};
use crate::lifecycle::event::stage_scope_for;
use crate::outcome::BilledModelUsage;
use crate::run_options::RunOptions;
use crate::sandbox_git::{
    checked_git_checkpoint, git_diff, list_diff_numstat, summarize_diff_numstat,
};
use crate::sandbox_git_runtime::SandboxGitRuntime;
use crate::stage_execution::StageExecutionTracker;

type WfRunState = ExecutionState<Option<BilledModelUsage>>;
type WfNodeResult = NodeResult<Option<BilledModelUsage>>;

/// Result of a git checkpoint operation, shared with EventLifecycle.
#[derive(Debug, Clone)]
pub(crate) struct GitCheckpointResult {
    pub commit_sha:   Option<String>,
    pub push_results: Vec<PushResult>,
    pub diff:         Option<String>,
    pub diff_summary: Option<DiffSummary>,
}

#[derive(Debug, Clone)]
pub(crate) struct PushResult {
    pub branch:           String,
    pub success:          bool,
    pub exec_output_tail: Option<fabro_types::ExecOutputTail>,
    pub attempts:         Vec<fabro_sandbox::PushAttempt>,
}

/// Push a run branch to its remote counterpart.
///
/// Owns the refspec convention so the checkpoint push and the terminal publish
/// push cannot drift apart. The caller picks the retry budget: cheap for
/// checkpoint pushes (the next checkpoint re-pushes the same branch anyway),
/// generous for the terminal publish push.
pub(crate) async fn push_run_branch(
    sandbox: &dyn fabro_sandbox::Sandbox,
    branch: &str,
    plan: &fabro_sandbox::RetryPlan,
) -> Result<fabro_sandbox::PushReport, fabro_sandbox::PushError> {
    sandbox
        .git_push_ref(&format!("refs/heads/{branch}:refs/heads/{branch}"), plan)
        .await
}

/// Sub-lifecycle responsible for git operations (checkpoint commits, pushes,
/// diffs).
pub(crate) struct GitLifecycle {
    pub sandbox:               Arc<dyn fabro_sandbox::Sandbox>,
    pub emitter:               Arc<Emitter>,
    pub run_id:                RunId,
    pub run_options:           Arc<RunOptions>,
    pub sandbox_git:           Arc<SandboxGitRuntime>,
    pub start_node_id:         Option<String>,
    // Cross-lifecycle data (shared with EventLifecycle)
    pub checkpoint_git_result: Arc<Mutex<Option<GitCheckpointResult>>>,
    pub last_git_sha:          Arc<Mutex<Option<String>>>,
    /// Run-scoped stage execution allocator shared with `RunServices`.
    pub stage_executions:      StageExecutionTracker,
}

#[async_trait]
impl RunLifecycle<WorkflowGraph> for GitLifecycle {
    async fn on_run_start(&self, _graph: &WorkflowGraph, _state: &WfRunState) -> CoreResult<()> {
        // Reset last_git_sha (diff base parity)
        *self.last_git_sha.lock().expect(
            "git lifecycle mutex should not be poisoned: no code panics while holding this lock",
        ) = None;
        *self.checkpoint_git_result.lock().expect(
            "git lifecycle mutex should not be poisoned: no code panics while holding this lock",
        ) = None;

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
            *self.checkpoint_git_result.lock()
                .expect("git lifecycle mutex should not be poisoned: no code panics while holding this lock") = None;
            return Ok(());
        }

        // Run branch commit via sandbox
        let completed_count = state.completed_nodes.len();
        let git_author = self.run_options.git_author();
        let commit_result = checked_git_checkpoint(
            &self.sandbox_git,
            &*self.sandbox,
            &self.run_id.to_string(),
            node_id,
            &result.outcome.status.to_string(),
            completed_count,
            self.run_options.checkpoint(),
            &git_author,
        )
        .await;

        match commit_result {
            Ok(sha) => {
                let mut git_result = GitCheckpointResult {
                    commit_sha:   Some(sha.clone()),
                    push_results: Vec::new(),
                    diff:         None,
                    diff_summary: None,
                };

                // Push run branch (skip in dry-run mode)
                if !self.run_options.dry_run_enabled()
                    && self.run_options.settings.run.run_branch.push
                {
                    if let Some(branch) = self
                        .run_options
                        .git
                        .as_ref()
                        .and_then(|g| g.run_branch.as_ref())
                    {
                        let plan = fabro_sandbox::RetryPlan::checkpoint_push();
                        let (push_ok, exec_output_tail, attempts) =
                            match push_run_branch(self.sandbox.as_ref(), branch, &plan).await {
                                Ok(report) => {
                                    self.sandbox_git.record_successful_push();
                                    (true, None, report.attempts)
                                }
                                Err(push_error) => {
                                    let exec_output_tail =
                                        fabro_sandbox::default_redacted_output_tail(
                                            &push_error.error,
                                        );
                                    tracing::warn!(
                                        branch = %branch,
                                        attempts = push_error.report.attempts.len(),
                                        error = %fabro_sandbox::display_for_log(&push_error.error),
                                        "git push from run lifecycle failed"
                                    );
                                    self.emitter.notice_with_tail(
                                        RunNoticeLevel::Warn,
                                        RunNoticeCode::GitPushFailed,
                                        format!(
                                            "Failed to push run branch {branch}: {}",
                                            push_error.error
                                        ),
                                        exec_output_tail.clone(),
                                    );
                                    (false, exec_output_tail, push_error.report.attempts)
                                }
                            };
                        git_result.push_results.push(PushResult {
                            branch: branch.clone(),
                            success: push_ok,
                            exec_output_tail,
                            attempts,
                        });
                    }
                }

                // Save diff.patch
                let prev = self.last_git_sha.lock()
                    .expect("git lifecycle mutex should not be poisoned: no code panics while holding this lock")
                    .clone().or_else(|| {
                    self.run_options
                        .git
                        .as_ref()
                        .and_then(|g| g.base_sha.clone())
                });
                if let Some(prev) = prev.filter(|p| p != &sha) {
                    let summary_base = self
                        .run_options
                        .git
                        .as_ref()
                        .and_then(|git| git.base_sha.clone());
                    let (patch_result, numstat_result) =
                        tokio::join!(git_diff(&*self.sandbox, &prev), async {
                            match summary_base.as_deref() {
                                Some(base) if base != sha => {
                                    Some(list_diff_numstat(&*self.sandbox, base, &sha).await)
                                }
                                _ => None,
                            }
                        },);
                    match patch_result {
                        Ok(patch) if !patch.is_empty() => {
                            git_result.diff = Some(patch);
                        }
                        Ok(_) => {}
                        Err(err) => {
                            let exec_output_tail =
                                fabro_sandbox::default_redacted_output_tail(&err);
                            self.emitter.notice_with_tail(
                                RunNoticeLevel::Warn,
                                RunNoticeCode::GitDiffFailed,
                                format!("[node: {node_id}] git diff failed: {err}"),
                                exec_output_tail,
                            );
                        }
                    }
                    match numstat_result {
                        Some(Ok(numstat)) => {
                            git_result.diff_summary = Some(summarize_diff_numstat(&numstat));
                        }
                        Some(Err(err)) => {
                            let exec_output_tail =
                                fabro_sandbox::default_redacted_output_tail(&err);
                            self.emitter.notice_with_tail(
                                RunNoticeLevel::Warn,
                                RunNoticeCode::GitDiffFailed,
                                format!("[node: {node_id}] git diff stats failed: {err}"),
                                exec_output_tail,
                            );
                        }
                        None => {}
                    }
                }

                // Update shared state
                *self.last_git_sha.lock()
                    .expect("git lifecycle mutex should not be poisoned: no code panics while holding this lock") = Some(sha);
                *self.checkpoint_git_result.lock()
                    .expect("git lifecycle mutex should not be poisoned: no code panics while holding this lock") = Some(git_result);
            }
            Err(e) => {
                let exec_output_tail = fabro_sandbox::default_redacted_output_tail(&e);
                let error = e.to_string();
                // Emit CheckpointFailed and return error
                let scope = stage_scope_for(&self.stage_executions, state, node_id);
                self.emitter.emit_scoped(
                    &Event::CheckpointFailed {
                        node_id: node_id.to_string(),
                        error: error.clone(),
                        exec_output_tail,
                    },
                    &scope,
                );
                return Err(CoreError::Other(format!(
                    "git checkpoint commit failed for node '{node_id}': {error}"
                )));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    use fabro_core::graph::Graph as CoreGraph;
    use fabro_core::lifecycle::RunLifecycle;
    use fabro_core::state::ExecutionState;
    use fabro_graphviz::graph::types::{AttrValue, Edge, Graph, Node};
    use fabro_types::{WorkflowSettings, fixtures};

    use super::*;
    use crate::outcome::Outcome;
    use crate::run_options::GitCheckpointOptions;

    #[expect(
        clippy::disallowed_methods,
        reason = "checkpoint tests use synchronous git commands to set up temporary repositories"
    )]
    fn init_git_repo(repo: &Path) {
        let init = std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(init.status.success());
        for (key, value) in [("user.name", "Test"), ("user.email", "test@test.com")] {
            let config = std::process::Command::new("git")
                .args(["config", key, value])
                .current_dir(repo)
                .output()
                .unwrap();
            assert!(config.status.success());
        }
        let commit = std::process::Command::new("git")
            .args(["commit", "--allow-empty", "-m", "initial"])
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(commit.status.success());
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "checkpoint tests use synchronous git commands to set up temporary repositories"
    )]
    fn git_commit_all(repo: &Path, msg: &str) -> String {
        let add = std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(add.status.success());
        let commit = std::process::Command::new("git")
            .args(["commit", "-m", msg])
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(
            commit.status.success(),
            "git commit failed: {}",
            String::from_utf8_lossy(&commit.stderr)
        );
        let rev_parse = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(rev_parse.status.success());
        String::from_utf8(rev_parse.stdout)
            .unwrap()
            .trim()
            .to_string()
    }

    fn workflow_graph() -> WorkflowGraph {
        let mut graph = Graph::new("checkpoint");
        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        graph.nodes.insert("start".to_string(), start);
        let mut build = Node::new("build");
        build
            .attrs
            .insert("shape".to_string(), AttrValue::String("box".to_string()));
        graph.nodes.insert("build".to_string(), build);
        let mut exit = Node::new("exit");
        exit.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Msquare".to_string()),
        );
        graph.nodes.insert("exit".to_string(), exit);
        graph.edges.push(Edge::new("start", "build"));
        graph.edges.push(Edge::new("build", "exit"));
        WorkflowGraph(Arc::new(graph))
    }

    fn run_options(run_dir: &Path) -> Arc<RunOptions> {
        Arc::new(RunOptions {
            settings:         WorkflowSettings::default(),
            run_dir:          run_dir.to_path_buf(),
            cancel_token:     tokio_util::sync::CancellationToken::new(),
            run_id:           fixtures::RUN_1,
            labels:           HashMap::new(),
            workflow_slug:    Some("checkpoint".to_string()),
            github_app:       None,
            pre_run_git:      None,
            fork_source_ref:  None,
            base_branch:      None,
            display_base_sha: None,
            git:              Some(GitCheckpointOptions {
                base_sha:   None,
                run_branch: None,
            }),
        })
    }

    fn git_lifecycle(
        repo: &Path,
        emitter: Arc<Emitter>,
        run_options: Arc<RunOptions>,
    ) -> GitLifecycle {
        GitLifecycle {
            stage_executions: StageExecutionTracker::default(),
            sandbox: Arc::new(fabro_agent::LocalSandbox::new(repo.to_path_buf())),
            emitter,
            run_id: fixtures::RUN_1,
            run_options,
            sandbox_git: Arc::new(SandboxGitRuntime::new()),
            start_node_id: Some("start".to_string()),
            checkpoint_git_result: Arc::new(Mutex::new(None)),
            last_git_sha: Arc::new(Mutex::new(None)),
        }
    }

    #[tokio::test]
    async fn checkpoint_git_result_includes_diff_summary() {
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = repo_dir.path();
        init_git_repo(repo);
        tokio::fs::write(repo.join("notes.txt"), "one\n")
            .await
            .unwrap();
        let base = git_commit_all(repo, "base");
        tokio::fs::write(repo.join("notes.txt"), "one\ntwo\n")
            .await
            .unwrap();

        let mut options = run_options(repo).as_ref().clone();
        options.git = Some(GitCheckpointOptions {
            base_sha:   Some(base),
            run_branch: None,
        });
        let lifecycle = git_lifecycle(
            repo,
            Arc::new(Emitter::new(fixtures::RUN_1)),
            Arc::new(options),
        );
        let graph = workflow_graph();
        let node = graph.get_node("build").unwrap();
        let mut state = ExecutionState::new(&graph).unwrap();
        state.increment_visits("build");
        let result = WfNodeResult::new(
            Outcome::success(),
            Duration::from_millis(10),
            Duration::ZERO,
            Duration::ZERO,
            1,
            1,
        );

        lifecycle
            .on_checkpoint(&node, &result, Some("exit"), &state)
            .await
            .unwrap();

        let git_result = lifecycle
            .checkpoint_git_result
            .lock()
            .unwrap()
            .clone()
            .unwrap();
        let diff_summary = git_result.diff_summary.expect("diff summary");
        assert_eq!(diff_summary.files_changed, 1);
        assert_eq!(diff_summary.additions, 1);
        assert_eq!(diff_summary.deletions, 0);

        tokio::fs::write(repo.join("notes.txt"), "one\ntwo\nthree\n")
            .await
            .unwrap();
        state.increment_visits("build");
        lifecycle
            .on_checkpoint(&node, &result, Some("exit"), &state)
            .await
            .unwrap();

        let git_result = lifecycle
            .checkpoint_git_result
            .lock()
            .unwrap()
            .clone()
            .unwrap();
        let diff_summary = git_result.diff_summary.expect("diff summary");
        assert_eq!(diff_summary.files_changed, 1);
        assert_eq!(diff_summary.additions, 2);
        assert_eq!(diff_summary.deletions, 0);
    }

    #[tokio::test]
    async fn checkpoint_git_result_omits_push_when_run_branch_push_disabled() {
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = repo_dir.path();
        init_git_repo(repo);
        tokio::fs::write(repo.join("notes.txt"), "checkpoint\n")
            .await
            .unwrap();

        let mut options = run_options(repo).as_ref().clone();
        options.settings.run.run_branch.push = false;
        options.git = Some(GitCheckpointOptions {
            base_sha:   None,
            run_branch: Some("fabro/run/test".to_string()),
        });
        let lifecycle = git_lifecycle(
            repo,
            Arc::new(Emitter::new(fixtures::RUN_1)),
            Arc::new(options),
        );
        let graph = workflow_graph();
        let node = graph.get_node("build").unwrap();
        let mut state = ExecutionState::new(&graph).unwrap();
        state.increment_visits("build");
        let result = WfNodeResult::new(
            Outcome::success(),
            Duration::from_millis(10),
            Duration::ZERO,
            Duration::ZERO,
            1,
            1,
        );

        lifecycle
            .on_checkpoint(&node, &result, Some("exit"), &state)
            .await
            .unwrap();

        let git_result = lifecycle
            .checkpoint_git_result
            .lock()
            .unwrap()
            .clone()
            .unwrap();
        assert!(git_result.commit_sha.is_some());
        assert!(git_result.push_results.is_empty());
    }
}
