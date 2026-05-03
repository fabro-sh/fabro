use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use fabro_core::error::{Error as CoreError, Result as CoreResult};
use fabro_core::graph::NodeSpec;
use fabro_core::lifecycle::RunLifecycle;
use fabro_core::outcome::NodeResult;
use fabro_core::state::ExecutionState;
use fabro_dump::RunDump;
use fabro_types::RunId;
use fabro_types::run_event::{MetadataSnapshotFailureKind, MetadataSnapshotPhase};
use fabro_util::error::{collect_causes, render_with_causes};
use fabro_util::time::elapsed_ms;

use crate::artifact;
use crate::event::{Emitter, Event, RunNoticeLevel, StageScope};
use crate::graph::{WorkflowGraph, WorkflowNode};
use crate::lifecycle::event::stage_scope_for;
use crate::outcome::BilledModelUsage;
use crate::run_metadata::{MetadataSnapshot, RunMetadataRuntime, RunMetadataWriterHandle};
use crate::run_options::RunOptions;
use crate::runtime_store::RunStoreHandle;
use crate::sandbox_git::{checked_git_checkpoint, git_diff};
use crate::sandbox_git_runtime::SandboxGitRuntime;

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
    pub sandbox_git:           Arc<SandboxGitRuntime>,
    pub metadata_runtime:      Arc<RunMetadataRuntime>,
    pub metadata_writer:       Option<RunMetadataWriterHandle>,
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
        if let Some(meta_branch) = self.metadata_branch().map(str::to_string) {
            if self.metadata_writer.is_none() || self.metadata_runtime.metadata_degraded() {
                return Ok(());
            }
            let phase = MetadataSnapshotPhase::Init;
            let started = Instant::now();
            self.emit_metadata_snapshot_started(phase, &meta_branch, None);
            match self.run_store.state().await {
                Ok(state) => match RunDump::from_projection(&state) {
                    Ok(dump) => {
                        let _ = self
                            .write_metadata_snapshot(
                                phase,
                                &meta_branch,
                                started,
                                &dump,
                                "init run",
                                None,
                            )
                            .await;
                    }
                    Err(err) => {
                        let message = format!("failed to build run dump for metadata init: {err}");
                        self.emit_metadata_snapshot_failed(
                            phase,
                            &meta_branch,
                            started,
                            MetadataSnapshotFailureKind::Write,
                            message.clone(),
                            collect_causes(err.as_ref()),
                            None,
                            None,
                            None,
                            None,
                        );
                        self.emit_metadata_warning("checkpoint_metadata_write_failed", message);
                    }
                },
                Err(err) => {
                    let message = format!("failed to load run state for metadata init: {err}");
                    self.emit_metadata_snapshot_failed(
                        phase,
                        &meta_branch,
                        started,
                        MetadataSnapshotFailureKind::LoadState,
                        message.clone(),
                        collect_causes(err.as_ref()),
                        None,
                        None,
                        None,
                        None,
                    );
                    self.emit_metadata_warning("checkpoint_metadata_write_failed", message);
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
        let shadow_sha = if let Some(meta_branch) = self.metadata_branch().map(str::to_string) {
            if self.metadata_writer.is_none() || self.metadata_runtime.metadata_degraded() {
                None
            } else {
                let phase = MetadataSnapshotPhase::Checkpoint;
                let started = Instant::now();
                let scope = stage_scope_for(state, node_id);
                self.emit_metadata_snapshot_started(phase, &meta_branch, Some(&scope));
                match self.run_store.state().await {
                    Ok(mut projection) => {
                        projection.checkpoint = Some(checkpoint);
                        match RunDump::from_projection(&projection) {
                            Ok(dump) => {
                                self.write_metadata_snapshot(
                                    phase,
                                    &meta_branch,
                                    started,
                                    &dump,
                                    "checkpoint",
                                    Some(&scope),
                                )
                                .await
                            }
                            Err(err) => {
                                let message = format!(
                                    "failed to build run dump for metadata checkpoint: {err}"
                                );
                                self.emit_metadata_snapshot_failed(
                                    phase,
                                    &meta_branch,
                                    started,
                                    MetadataSnapshotFailureKind::Write,
                                    message.clone(),
                                    collect_causes(err.as_ref()),
                                    None,
                                    None,
                                    None,
                                    Some(&scope),
                                );
                                self.emit_metadata_warning(
                                    "checkpoint_metadata_write_failed",
                                    message,
                                );
                                None
                            }
                        }
                    }
                    Err(err) => {
                        let message =
                            format!("failed to load run state for metadata checkpoint: {err}");
                        self.emit_metadata_snapshot_failed(
                            phase,
                            &meta_branch,
                            started,
                            MetadataSnapshotFailureKind::LoadState,
                            message.clone(),
                            collect_causes(err.as_ref()),
                            None,
                            None,
                            None,
                            Some(&scope),
                        );
                        self.emit_metadata_warning("checkpoint_metadata_write_failed", message);
                        None
                    }
                }
            }
        } else {
            None
        };

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
                        let push_ok = match self.sandbox.git_push_ref(&refspec).await {
                            Ok(()) => true,
                            Err(err) => {
                                tracing::warn!(
                                    refspec = %refspec,
                                    error = %err,
                                    "git push from run lifecycle failed"
                                );
                                false
                            }
                        };
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
                let error = render_with_causes(&e.to_string(), &collect_causes(&e));
                // Emit CheckpointFailed and return error
                let scope = stage_scope_for(state, node_id);
                self.emitter.emit_scoped(
                    &Event::CheckpointFailed {
                        node_id: node_id.to_string(),
                        error:   error.clone(),
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

impl GitLifecycle {
    fn metadata_branch(&self) -> Option<&str> {
        self.run_options
            .git
            .as_ref()
            .and_then(|git| git.meta_branch.as_deref())
    }

    async fn write_metadata_snapshot(
        &self,
        phase: MetadataSnapshotPhase,
        meta_branch: &str,
        started: Instant,
        dump: &RunDump,
        message: &str,
        scope: Option<&StageScope>,
    ) -> Option<String> {
        if self.metadata_runtime.metadata_degraded() {
            return None;
        }
        let writer = self.metadata_writer.as_ref()?;

        match writer.write_snapshot(dump, message).await {
            Ok(snapshot) => {
                if let Some(detail) = snapshot.push_error.as_deref() {
                    let message =
                        format!("failed to push metadata ref refs/heads/{meta_branch}: {detail}");
                    self.emit_metadata_snapshot_failed(
                        phase,
                        meta_branch,
                        started,
                        MetadataSnapshotFailureKind::Push,
                        message.clone(),
                        Vec::new(),
                        Some(snapshot.commit_sha.clone()),
                        Some(snapshot.entry_count),
                        Some(snapshot.bytes),
                        scope,
                    );
                    self.emit_metadata_warning("checkpoint_metadata_push_failed", message);
                } else {
                    self.emit_metadata_snapshot_completed(
                        phase,
                        meta_branch,
                        started,
                        &snapshot,
                        scope,
                    );
                }
                Some(snapshot.commit_sha)
            }
            Err(err) => {
                let message = format!("failed to write checkpoint metadata: {err}");
                self.emit_metadata_snapshot_failed(
                    phase,
                    meta_branch,
                    started,
                    MetadataSnapshotFailureKind::Write,
                    message.clone(),
                    collect_causes(&err),
                    None,
                    None,
                    None,
                    scope,
                );
                self.emit_metadata_warning("checkpoint_metadata_write_failed", message);
                None
            }
        }
    }

    fn emit_metadata_snapshot_started(
        &self,
        phase: MetadataSnapshotPhase,
        branch: &str,
        scope: Option<&StageScope>,
    ) {
        self.emit_metadata_snapshot_event(
            &Event::MetadataSnapshotStarted {
                phase,
                branch: branch.to_string(),
            },
            scope,
        );
    }

    fn emit_metadata_snapshot_completed(
        &self,
        phase: MetadataSnapshotPhase,
        branch: &str,
        started: Instant,
        snapshot: &MetadataSnapshot,
        scope: Option<&StageScope>,
    ) {
        self.emit_metadata_snapshot_event(
            &Event::MetadataSnapshotCompleted {
                phase,
                branch: branch.to_string(),
                duration_ms: elapsed_ms(started),
                entry_count: snapshot.entry_count,
                bytes: snapshot.bytes,
                commit_sha: snapshot.commit_sha.clone(),
            },
            scope,
        );
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "Metadata failure event carries the full event contract explicitly."
    )]
    fn emit_metadata_snapshot_failed(
        &self,
        phase: MetadataSnapshotPhase,
        branch: &str,
        started: Instant,
        failure_kind: MetadataSnapshotFailureKind,
        error: String,
        causes: Vec<String>,
        commit_sha: Option<String>,
        entry_count: Option<usize>,
        bytes: Option<u64>,
        scope: Option<&StageScope>,
    ) {
        self.emit_metadata_snapshot_event(
            &Event::MetadataSnapshotFailed {
                phase,
                branch: branch.to_string(),
                duration_ms: elapsed_ms(started),
                failure_kind,
                error,
                causes,
                commit_sha,
                entry_count,
                bytes,
                exec_output_tail: None,
            },
            scope,
        );
    }

    fn emit_metadata_snapshot_event(&self, event: &Event, scope: Option<&StageScope>) {
        if let Some(scope) = scope {
            self.emitter.emit_scoped(event, scope);
        } else {
            self.emitter.emit(event);
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

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    use anyhow::Result;
    use async_trait::async_trait;
    use bytes::Bytes;
    use fabro_core::graph::Graph as CoreGraph;
    use fabro_core::lifecycle::RunLifecycle;
    use fabro_core::state::ExecutionState;
    use fabro_graphviz::graph::types::{AttrValue, Edge, Graph, Node};
    use fabro_store::{Database, EventEnvelope, RunDatabase, RunProjection};
    use fabro_types::run_event::{MetadataSnapshotFailureKind, MetadataSnapshotPhase};
    use fabro_types::{EventBody, RunBlobId, RunEvent, WorkflowSettings, fixtures};
    use object_store::memory::InMemory;

    use super::*;
    use crate::event::append_event;
    use crate::outcome::{Outcome, StageOutcome};
    use crate::pipeline::write_finalize_commit;
    use crate::records::Conclusion;
    use crate::run_options::GitCheckpointOptions;
    use crate::runtime_store::{RunStoreBackend, RunStoreHandle};
    use crate::services::RunServices;

    #[expect(
        clippy::disallowed_methods,
        reason = "metadata event tests use synchronous git commands to set up temporary repositories"
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

    fn workflow_graph() -> WorkflowGraph {
        let mut graph = Graph::new("metadata");
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

    fn run_options(run_dir: &Path, meta_branch: &str) -> Arc<RunOptions> {
        Arc::new(RunOptions {
            settings:         WorkflowSettings::default(),
            run_dir:          run_dir.to_path_buf(),
            cancel_token:     None,
            run_id:           fixtures::RUN_1,
            labels:           HashMap::new(),
            workflow_slug:    Some("metadata".to_string()),
            github_app:       None,
            pre_run_git:      None,
            fork_source_ref:  None,
            base_branch:      None,
            display_base_sha: None,
            git:              Some(GitCheckpointOptions {
                base_sha:    None,
                run_branch:  None,
                meta_branch: Some(meta_branch.to_string()),
            }),
        })
    }

    async fn run_store(run_id: fabro_types::RunId) -> RunDatabase {
        let store = Arc::new(Database::new(
            Arc::new(InMemory::new()),
            "",
            Duration::from_millis(1),
            None,
        ));
        let run_store = store.create_run(&run_id).await.unwrap();
        append_event(&run_store, &run_id, &Event::RunCreated {
            run_id,
            settings: serde_json::to_value(WorkflowSettings::default()).unwrap(),
            graph: serde_json::to_value(fabro_types::Graph::new("metadata")).unwrap(),
            workflow_source: None,
            workflow_config: None,
            labels: BTreeMap::new(),
            run_dir: "/tmp/run".to_string(),
            source_directory: Some("/tmp/project".to_string()),
            workflow_slug: Some("metadata".to_string()),
            db_prefix: None,
            provenance: None,
            manifest_blob: None,
            git: None,
            fork_source_ref: None,
            in_place: false,
            web_url: None,
        })
        .await
        .unwrap();
        run_store
    }

    fn record_events(emitter: &Arc<Emitter>) -> Arc<std::sync::Mutex<Vec<RunEvent>>> {
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = Arc::clone(&events);
        emitter.on_event(move |event| {
            captured.lock().unwrap().push(event.clone());
        });
        events
    }

    fn git_lifecycle(
        repo: &Path,
        emitter: Arc<Emitter>,
        run_store: RunStoreHandle,
        run_options: Arc<RunOptions>,
        metadata_runtime: Arc<RunMetadataRuntime>,
    ) -> GitLifecycle {
        let metadata_writer = run_options
            .git
            .as_ref()
            .and_then(|git| git.meta_branch.as_deref())
            .map(|branch| RunMetadataWriterHandle::new_for_test_repo(repo, branch));
        git_lifecycle_with_writer(
            repo,
            emitter,
            run_store,
            run_options,
            metadata_runtime,
            metadata_writer,
        )
    }

    fn git_lifecycle_with_writer(
        repo: &Path,
        emitter: Arc<Emitter>,
        run_store: RunStoreHandle,
        run_options: Arc<RunOptions>,
        metadata_runtime: Arc<RunMetadataRuntime>,
        metadata_writer: Option<RunMetadataWriterHandle>,
    ) -> GitLifecycle {
        GitLifecycle {
            sandbox: Arc::new(fabro_agent::LocalSandbox::new(repo.to_path_buf())),
            emitter,
            run_id: fixtures::RUN_1,
            run_store,
            run_options,
            sandbox_git: Arc::new(SandboxGitRuntime::new()),
            metadata_runtime,
            metadata_writer,
            start_node_id: Some("start".to_string()),
            checkpoint_git_result: Arc::new(Mutex::new(None)),
            last_git_sha: Arc::new(Mutex::new(None)),
        }
    }

    #[tokio::test]
    async fn init_metadata_snapshot_success_emits_started_completed_unscoped() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_git_repo(repo_dir.path());
        let branch = "fabro/metadata/run";
        let run_store = run_store(fixtures::RUN_1).await;
        let handle = RunStoreHandle::local(run_store.clone());
        let state = handle.state().await.unwrap();
        let expected_entries = RunDump::from_projection(&state)
            .unwrap()
            .git_entries()
            .unwrap();
        let expected_entry_count = expected_entries.len();
        let expected_bytes = expected_entries
            .iter()
            .map(|(_, bytes)| u64::try_from(bytes.len()).unwrap_or(u64::MAX))
            .sum::<u64>();
        let emitter = Arc::new(Emitter::new(fixtures::RUN_1));
        let events = record_events(&emitter);
        let lifecycle = git_lifecycle(
            repo_dir.path(),
            emitter,
            handle,
            run_options(repo_dir.path(), branch),
            Arc::new(RunMetadataRuntime::new()),
        );
        let graph = workflow_graph();
        let state = ExecutionState::new(&graph).unwrap();

        lifecycle.on_run_start(&graph, &state).await.unwrap();

        let events = events.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_name(), "metadata.snapshot.started");
        assert_eq!(events[1].event_name(), "metadata.snapshot.completed");
        assert!(events[0].node_id.is_none());
        assert!(events[1].node_id.is_none());
        match &events[1].body {
            EventBody::MetadataSnapshotCompleted(props) => {
                assert_eq!(props.phase, MetadataSnapshotPhase::Init);
                assert_eq!(props.branch, branch);
                assert_eq!(props.entry_count, expected_entry_count);
                assert_eq!(props.bytes, expected_bytes);
                assert!(!props.commit_sha.is_empty());
            }
            other => panic!("expected metadata completed event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn init_metadata_load_state_failure_emits_failed_before_notice() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_git_repo(repo_dir.path());
        let branch = "fabro/metadata/run";
        let emitter = Arc::new(Emitter::new(fixtures::RUN_1));
        let events = record_events(&emitter);
        let lifecycle = git_lifecycle(
            repo_dir.path(),
            emitter,
            RunStoreHandle::new(Arc::new(FailingStateStore)),
            run_options(repo_dir.path(), branch),
            Arc::new(RunMetadataRuntime::new()),
        );
        let graph = workflow_graph();
        let state = ExecutionState::new(&graph).unwrap();

        lifecycle.on_run_start(&graph, &state).await.unwrap();

        let events = events.lock().unwrap();
        let names = events.iter().map(RunEvent::event_name).collect::<Vec<_>>();
        assert_eq!(names, vec![
            "metadata.snapshot.started",
            "metadata.snapshot.failed",
            "run.notice",
        ]);
        match &events[1].body {
            EventBody::MetadataSnapshotFailed(props) => {
                assert_eq!(props.phase, MetadataSnapshotPhase::Init);
                assert_eq!(props.failure_kind, MetadataSnapshotFailureKind::LoadState);
                assert_eq!(props.commit_sha, None);
                assert_eq!(props.entry_count, None);
                assert_eq!(props.bytes, None);
            }
            other => panic!("expected metadata failed event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn init_metadata_push_failure_emits_failed_with_snapshot_accounting() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_git_repo(repo_dir.path());
        let branch = "fabro/metadata/run";
        let run_store = run_store(fixtures::RUN_1).await;
        let handle = RunStoreHandle::local(run_store.clone());
        let state = handle.state().await.unwrap();
        let expected_entries = RunDump::from_projection(&state)
            .unwrap()
            .git_entries()
            .unwrap();
        let expected_entry_count = expected_entries.len();
        let expected_bytes = expected_entries
            .iter()
            .map(|(_, bytes)| u64::try_from(bytes.len()).unwrap_or(u64::MAX))
            .sum::<u64>();
        let emitter = Arc::new(Emitter::new(fixtures::RUN_1));
        let events = record_events(&emitter);
        let runtime = Arc::new(RunMetadataRuntime::new());
        let metadata_writer = RunMetadataWriterHandle::new_for_test(
            format!("file://{}", repo_dir.path().display()),
            branch.to_string(),
            crate::git::GitAuthor::default(),
            None,
        )
        .unwrap();
        let lifecycle = git_lifecycle_with_writer(
            repo_dir.path(),
            emitter,
            handle,
            run_options(repo_dir.path(), branch),
            Arc::clone(&runtime),
            Some(metadata_writer),
        );
        let graph = workflow_graph();
        let state = ExecutionState::new(&graph).unwrap();

        lifecycle.on_run_start(&graph, &state).await.unwrap();

        assert!(runtime.metadata_degraded());
        let events = events.lock().unwrap();
        let names = events.iter().map(RunEvent::event_name).collect::<Vec<_>>();
        assert_eq!(names, vec![
            "metadata.snapshot.started",
            "metadata.snapshot.failed",
            "run.notice",
        ]);
        match &events[1].body {
            EventBody::MetadataSnapshotFailed(props) => {
                assert_eq!(props.phase, MetadataSnapshotPhase::Init);
                assert_eq!(props.failure_kind, MetadataSnapshotFailureKind::Push);
                assert!(props.commit_sha.as_ref().is_some_and(|sha| !sha.is_empty()));
                assert_eq!(props.entry_count, Some(expected_entry_count));
                assert_eq!(props.bytes, Some(expected_bytes));
            }
            other => panic!("expected metadata failed event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn checkpoint_metadata_load_state_failure_emits_scoped_failed_before_notice() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_git_repo(repo_dir.path());
        let branch = "fabro/metadata/run";
        let emitter = Arc::new(Emitter::new(fixtures::RUN_1));
        let events = record_events(&emitter);
        let lifecycle = git_lifecycle(
            repo_dir.path(),
            emitter,
            RunStoreHandle::new(Arc::new(FailingStateStore)),
            run_options(repo_dir.path(), branch),
            Arc::new(RunMetadataRuntime::new()),
        );
        let graph = workflow_graph();
        let node = graph.get_node("build").unwrap();
        let mut state = ExecutionState::new(&graph).unwrap();
        state.increment_visits("build");
        let result = WfNodeResult::new(Outcome::success(), Duration::from_millis(10), 1, 1);

        lifecycle
            .on_checkpoint(&node, &result, Some("exit"), &state)
            .await
            .unwrap();

        let events = events.lock().unwrap();
        let names = events.iter().map(RunEvent::event_name).collect::<Vec<_>>();
        assert_eq!(names, vec![
            "metadata.snapshot.started",
            "metadata.snapshot.failed",
            "run.notice",
        ]);
        assert_eq!(events[1].node_id.as_deref(), Some("build"));
        match &events[1].body {
            EventBody::MetadataSnapshotFailed(props) => {
                assert_eq!(props.phase, MetadataSnapshotPhase::Checkpoint);
                assert_eq!(props.failure_kind, MetadataSnapshotFailureKind::LoadState);
            }
            other => panic!("expected metadata failed event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn checkpoint_metadata_snapshot_success_emits_scoped_events() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_git_repo(repo_dir.path());
        let branch = "fabro/metadata/run";
        let run_store = run_store(fixtures::RUN_1).await;
        let emitter = Arc::new(Emitter::new(fixtures::RUN_1));
        let events = record_events(&emitter);
        let lifecycle = git_lifecycle(
            repo_dir.path(),
            emitter,
            RunStoreHandle::local(run_store),
            run_options(repo_dir.path(), branch),
            Arc::new(RunMetadataRuntime::new()),
        );
        let graph = workflow_graph();
        let node = graph.get_node("build").unwrap();
        let mut state = ExecutionState::new(&graph).unwrap();
        state.increment_visits("build");
        let result = WfNodeResult::new(Outcome::success(), Duration::from_millis(10), 1, 1);

        lifecycle
            .on_checkpoint(&node, &result, Some("exit"), &state)
            .await
            .unwrap();

        let events = events.lock().unwrap();
        assert_eq!(events[0].event_name(), "metadata.snapshot.started");
        assert_eq!(events[1].event_name(), "metadata.snapshot.completed");
        assert_eq!(events[0].node_id.as_deref(), Some("build"));
        assert_eq!(
            events[0]
                .stage_id
                .as_ref()
                .map(ToString::to_string)
                .as_deref(),
            Some("build@1")
        );
        assert_eq!(events[1].node_id.as_deref(), Some("build"));
        assert_eq!(
            events[1]
                .stage_id
                .as_ref()
                .map(ToString::to_string)
                .as_deref(),
            Some("build@1")
        );
    }

    #[tokio::test]
    async fn degraded_metadata_runtime_skips_snapshot_events() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_git_repo(repo_dir.path());
        let runtime = Arc::new(RunMetadataRuntime::new());
        runtime.mark_metadata_degraded();
        let emitter = Arc::new(Emitter::new(fixtures::RUN_1));
        let events = record_events(&emitter);
        let lifecycle = git_lifecycle(
            repo_dir.path(),
            emitter,
            RunStoreHandle::local(run_store(fixtures::RUN_1).await),
            run_options(repo_dir.path(), "fabro/metadata/run"),
            runtime,
        );
        let graph = workflow_graph();
        let state = ExecutionState::new(&graph).unwrap();

        lifecycle.on_run_start(&graph, &state).await.unwrap();

        assert!(events.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn degraded_after_init_failure_skips_later_checkpoint_and_finalize_metadata_events() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_git_repo(repo_dir.path());
        let emitter = Arc::new(Emitter::new(fixtures::RUN_1));
        let events = record_events(&emitter);
        let runtime = Arc::new(RunMetadataRuntime::new());
        let lifecycle = git_lifecycle(
            repo_dir.path(),
            emitter,
            RunStoreHandle::new(Arc::new(FailingStateStore)),
            run_options(repo_dir.path(), "fabro/metadata/run"),
            runtime,
        );
        let graph = workflow_graph();
        let state = ExecutionState::new(&graph).unwrap();

        lifecycle.on_run_start(&graph, &state).await.unwrap();
        let after_init = events.lock().unwrap().len();
        let node = graph.get_node("build").unwrap();
        let mut checkpoint_state = ExecutionState::new(&graph).unwrap();
        checkpoint_state.increment_visits("build");
        let result = WfNodeResult::new(Outcome::success(), Duration::from_millis(10), 1, 1);
        lifecycle
            .on_checkpoint(&node, &result, Some("exit"), &checkpoint_state)
            .await
            .unwrap();
        let finalize_services = RunServices::new(
            RunStoreHandle::new(Arc::new(FailingStateStore)),
            Arc::clone(&lifecycle.emitter),
            Arc::new(fabro_agent::LocalSandbox::new(
                repo_dir.path().to_path_buf(),
            )),
            None,
            None,
            fabro_model::Provider::Anthropic,
            Arc::new(fabro_auth::EnvCredentialSource::new()),
            Arc::new(SandboxGitRuntime::new()),
            Arc::clone(&lifecycle.metadata_runtime),
            lifecycle.metadata_writer.clone(),
        );
        let conclusion = Conclusion {
            timestamp:            chrono::Utc::now(),
            status:               StageOutcome::Succeeded,
            duration_ms:          10,
            failure_reason:       None,
            final_git_commit_sha: None,
            stages:               Vec::new(),
            billing:              None,
            total_retries:        0,
        };
        write_finalize_commit(
            lifecycle.run_options.as_ref(),
            &finalize_services,
            &conclusion,
        )
        .await;

        let events = events.lock().unwrap();
        assert_eq!(events.len(), after_init);
        assert_eq!(
            events.iter().map(RunEvent::event_name).collect::<Vec<_>>(),
            vec![
                "metadata.snapshot.started",
                "metadata.snapshot.failed",
                "run.notice",
            ]
        );
    }

    struct FailingStateStore;

    #[async_trait]
    impl RunStoreBackend for FailingStateStore {
        async fn load_state(&self) -> Result<RunProjection> {
            Err(anyhow::anyhow!("state unavailable"))
        }

        async fn list_events(&self) -> Result<Vec<EventEnvelope>> {
            Ok(Vec::new())
        }

        async fn append_run_event(&self, _event: &RunEvent) -> Result<()> {
            Ok(())
        }

        async fn write_blob(&self, data: &[u8]) -> Result<RunBlobId> {
            Ok(RunBlobId::new(data))
        }

        async fn read_blob(&self, _id: &RunBlobId) -> Result<Option<Bytes>> {
            Ok(None)
        }

        async fn read_run_log(&self) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
    }
}
