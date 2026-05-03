use std::collections::HashMap;
use std::time::Instant;

use fabro_dump::RunDump;
use fabro_hooks::{HookContext, HookEvent};
use fabro_types::run_event::{MetadataSnapshotFailureKind, MetadataSnapshotPhase};
use fabro_types::{BilledTokenCounts, EventBody, RunProjection};
use fabro_util::error::collect_causes;
use fabro_util::time::elapsed_ms;

use super::types::{Concluded, FinalizeOptions, Retroed};
use crate::error::Error;
use crate::event::{Event, RunNoticeLevel};
use crate::outcome::{Outcome, OutcomeExt, StageOutcome};
use crate::records::{Checkpoint, Conclusion, StageSummary};
use crate::run_metadata::MetadataSnapshot;
use crate::run_options::RunOptions;
use crate::run_status::{FailureReason, RunStatus, SuccessReason};
use crate::runtime_store::RunStoreHandle;
use crate::sandbox_git::git_diff_with_timeout;
use crate::services::RunServices;

pub fn classify_engine_result(
    engine_result: &Result<Outcome, Error>,
) -> (StageOutcome, Option<String>, RunStatus) {
    match engine_result {
        Ok(outcome) => {
            let status = outcome.status;
            let failure_reason = outcome.failure_reason().map(String::from);
            let run_status = match status {
                StageOutcome::Succeeded | StageOutcome::Skipped => RunStatus::Succeeded {
                    reason: SuccessReason::Completed,
                },
                StageOutcome::PartiallySucceeded => RunStatus::Succeeded {
                    reason: SuccessReason::PartialSuccess,
                },
                StageOutcome::Failed { .. } => RunStatus::Failed {
                    reason: FailureReason::WorkflowError,
                },
            };
            (status, failure_reason, run_status)
        }
        Err(Error::Cancelled) => (
            StageOutcome::Failed {
                retry_requested: false,
            },
            Some("Cancelled".to_string()),
            RunStatus::Failed {
                reason: FailureReason::Cancelled,
            },
        ),
        Err(err) => (
            StageOutcome::Failed {
                retry_requested: false,
            },
            Some(err.display_with_causes()),
            RunStatus::Failed {
                reason: FailureReason::WorkflowError,
            },
        ),
    }
}

pub(crate) async fn build_conclusion_from_store(
    run_store: &RunStoreHandle,
    status: StageOutcome,
    failure_reason: Option<String>,
    run_duration_ms: u64,
    final_git_commit_sha: Option<String>,
) -> Conclusion {
    let (state_result, events_result) = tokio::join!(run_store.state(), run_store.list_events());
    let projection = state_result.ok();
    let projection_order = projection
        .as_ref()
        .map(stage_projection_order)
        .unwrap_or_default();
    let checkpoint = projection
        .as_ref()
        .and_then(|state| state.checkpoint.as_ref());
    let stage_durations = events_result
        .map(|events| crate::extract_stage_durations_from_events(&events))
        .unwrap_or_default();

    build_conclusion_from_parts(
        checkpoint,
        &stage_durations,
        &projection_order,
        status,
        failure_reason,
        run_duration_ms,
        final_git_commit_sha,
    )
}

fn build_conclusion_from_parts(
    checkpoint: Option<&Checkpoint>,
    stage_durations: &HashMap<String, u64>,
    projection_order: &HashMap<String, u32>,
    status: StageOutcome,
    failure_reason: Option<String>,
    run_duration_ms: u64,
    final_git_commit_sha: Option<String>,
) -> Conclusion {
    // Looping workflows revisit nodes; `completed_nodes` accumulates duplicates
    // while the other checkpoint maps are keyed by node_id. Dedupe to one row
    // per node so the stages table matches the deduped billing total.
    let (stages, total_retries) = if let Some(cp) = checkpoint {
        let mut stage_rows = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut retries_sum: u32 = 0;
        let mut stage_order = Vec::new();

        for (original_checkpoint_order, node_id) in cp.completed_nodes.iter().enumerate() {
            if !seen.insert(node_id.as_str()) {
                continue;
            }
            stage_order.push((original_checkpoint_order, node_id.as_str()));
        }
        let mut extra_node_outcomes = cp
            .node_outcomes
            .keys()
            .filter(|node_id| !seen.contains(node_id.as_str()))
            .map(String::as_str)
            .collect::<Vec<_>>();
        extra_node_outcomes.sort_unstable();
        let extra_offset = stage_order.len();
        for (extra_index, node_id) in extra_node_outcomes.into_iter().enumerate() {
            seen.insert(node_id);
            stage_order.push((extra_offset + extra_index, node_id));
        }

        for (original_checkpoint_order, node_id) in stage_order {
            let outcome = cp.node_outcomes.get(node_id);
            let retries = cp
                .node_retries
                .get(node_id)
                .copied()
                .unwrap_or(1)
                .saturating_sub(1);
            retries_sum += retries;

            let summary = StageSummary {
                stage_id: node_id.to_string(),
                stage_label: node_id.to_string(),
                duration_ms: stage_durations.get(node_id).copied().unwrap_or(0),
                billing_usd_micros: outcome
                    .and_then(|o| o.usage.as_ref())
                    .and_then(|usage| usage.total_usd_micros),
                retries,
            };
            stage_rows.push((
                projection_order.get(node_id).copied().unwrap_or(u32::MAX),
                original_checkpoint_order,
                summary,
            ));
        }
        stage_rows.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| left.1.cmp(&right.1))
                .then_with(|| left.2.stage_id.cmp(&right.2.stage_id))
        });
        let stages = stage_rows
            .into_iter()
            .map(|(_, _, summary)| summary)
            .collect();
        (stages, retries_sum)
    } else {
        (vec![], 0)
    };

    Conclusion {
        timestamp: chrono::Utc::now(),
        status,
        duration_ms: run_duration_ms,
        failure_reason,
        final_git_commit_sha,
        stages,
        billing: checkpoint.and_then(billing_from_checkpoint),
        total_retries,
    }
}

fn stage_projection_order(state: &RunProjection) -> HashMap<String, u32> {
    let mut order = HashMap::new();
    for (stage_id, stage) in state.iter_stages() {
        order
            .entry(stage_id.node_id().to_string())
            .and_modify(|first_seq: &mut u32| {
                *first_seq = (*first_seq).min(stage.first_event_seq.get());
            })
            .or_insert_with(|| stage.first_event_seq.get());
    }
    order
}

/// `conclusion` is injected because the terminal event hasn't been emitted
/// yet — the run store's `projection.conclusion` is still `None` at this point.
pub async fn write_finalize_commit(
    run_options: &RunOptions,
    services: &RunServices,
    conclusion: &Conclusion,
) {
    if services.metadata_runtime.metadata_degraded() {
        return;
    }
    let Some(writer) = services.metadata_writer.as_ref() else {
        return;
    };
    let Some(meta_branch) = run_options
        .git
        .as_ref()
        .and_then(|git| git.meta_branch.as_deref())
    else {
        return;
    };

    let phase = MetadataSnapshotPhase::Finalize;
    let started = Instant::now();
    emit_metadata_snapshot_started(services, phase, meta_branch);

    let mut projection = match services.run_store.state().await {
        Ok(state) => state,
        Err(err) => {
            let message = format!("failed to load run state for final metadata snapshot: {err}");
            emit_metadata_snapshot_failed(
                services,
                phase,
                meta_branch,
                started,
                MetadataSnapshotFailureKind::LoadState,
                message.clone(),
                collect_causes(err.as_ref()),
                None,
                None,
                None,
            );
            emit_metadata_warning(services, "checkpoint_metadata_write_failed", message);
            return;
        }
    };
    projection.conclusion = Some(conclusion.clone());
    let dump = match RunDump::from_projection(&projection) {
        Ok(dump) => dump,
        Err(err) => {
            let message = format!("failed to build run dump for final metadata snapshot: {err}");
            emit_metadata_snapshot_failed(
                services,
                phase,
                meta_branch,
                started,
                MetadataSnapshotFailureKind::Write,
                message.clone(),
                collect_causes(err.as_ref()),
                None,
                None,
                None,
            );
            emit_metadata_warning(services, "checkpoint_metadata_write_failed", message);
            return;
        }
    };
    match writer.write_snapshot(&dump, "finalize run").await {
        Ok(snapshot) => {
            if let Some(detail) = snapshot.push_error.as_deref() {
                let message =
                    format!("failed to push metadata ref refs/heads/{meta_branch}: {detail}");
                emit_metadata_snapshot_failed(
                    services,
                    phase,
                    meta_branch,
                    started,
                    MetadataSnapshotFailureKind::Push,
                    message.clone(),
                    Vec::new(),
                    Some(snapshot.commit_sha.clone()),
                    Some(snapshot.entry_count),
                    Some(snapshot.bytes),
                );
                emit_metadata_warning(services, "checkpoint_metadata_push_failed", message);
            } else {
                emit_metadata_snapshot_completed(services, phase, meta_branch, started, &snapshot);
            }
        }
        Err(err) => {
            let message = format!("failed to write final checkpoint metadata: {err}");
            emit_metadata_snapshot_failed(
                services,
                phase,
                meta_branch,
                started,
                MetadataSnapshotFailureKind::Write,
                message.clone(),
                collect_causes(&err),
                None,
                None,
                None,
            );
            emit_metadata_warning(services, "checkpoint_metadata_write_failed", message);
        }
    }
}

fn emit_metadata_snapshot_started(
    services: &RunServices,
    phase: MetadataSnapshotPhase,
    branch: &str,
) {
    services.emitter.emit(&Event::MetadataSnapshotStarted {
        phase,
        branch: branch.to_string(),
    });
}

fn emit_metadata_snapshot_completed(
    services: &RunServices,
    phase: MetadataSnapshotPhase,
    branch: &str,
    started: Instant,
    snapshot: &MetadataSnapshot,
) {
    services.emitter.emit(&Event::MetadataSnapshotCompleted {
        phase,
        branch: branch.to_string(),
        duration_ms: elapsed_ms(started),
        entry_count: snapshot.entry_count,
        bytes: snapshot.bytes,
        commit_sha: snapshot.commit_sha.clone(),
    });
}

#[allow(
    clippy::too_many_arguments,
    reason = "Metadata failure event carries the full event contract explicitly."
)]
fn emit_metadata_snapshot_failed(
    services: &RunServices,
    phase: MetadataSnapshotPhase,
    branch: &str,
    started: Instant,
    failure_kind: MetadataSnapshotFailureKind,
    error: String,
    causes: Vec<String>,
    commit_sha: Option<String>,
    entry_count: Option<usize>,
    bytes: Option<u64>,
) {
    services.emitter.emit(&Event::MetadataSnapshotFailed {
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
    });
}

fn emit_metadata_warning(services: &RunServices, code: &str, message: String) {
    if services.metadata_runtime.mark_metadata_degraded() {
        services.emitter.notice(RunNoticeLevel::Warn, code, message);
    }
}

/// Failed and cancelled runs use a shorter diff timeout so a corrupted
/// workspace can't stall downstream consumers waiting on the terminal event.
async fn compute_final_patch(
    run_options: &RunOptions,
    services: &RunServices,
    status: StageOutcome,
) -> Option<String> {
    let base_sha = run_options.git.as_ref().and_then(|g| g.base_sha.clone())?;
    let timeout_ms = match status {
        StageOutcome::Succeeded | StageOutcome::PartiallySucceeded => 30_000,
        _ => 10_000,
    };
    match git_diff_with_timeout(&*services.sandbox, &base_sha, timeout_ms).await {
        Ok(patch) if !patch.is_empty() => Some(patch),
        Ok(_) => None,
        Err(err) => {
            services.emitter.notice(
                RunNoticeLevel::Warn,
                "git_diff_failed",
                format!("final diff failed: {err}"),
            );
            None
        }
    }
}

/// Iterates `node_outcomes.values()` rather than `completed_nodes` to avoid
/// over-counting the last visit's usage on looping workflows.
pub(crate) fn billing_from_checkpoint(cp: &Checkpoint) -> Option<BilledTokenCounts> {
    let usage: Vec<_> = cp
        .node_outcomes
        .values()
        .filter_map(|o| o.usage.clone())
        .collect();
    (!usage.is_empty()).then(|| BilledTokenCounts::from_billed_usage(&usage))
}

pub(crate) fn build_terminal_event(
    outcome: &Result<Outcome, Error>,
    duration_ms: u64,
    artifact_count: usize,
    final_git_commit_sha: Option<String>,
    final_patch: Option<String>,
    billing: Option<BilledTokenCounts>,
) -> Event {
    if matches!(outcome, Err(Error::Cancelled)) {
        return Event::WorkflowRunFailed {
            error: Error::Cancelled,
            duration_ms,
            reason: FailureReason::Cancelled,
            git_commit_sha: final_git_commit_sha,
            final_patch,
        };
    }

    let outcome_status = outcome.as_ref().map_or(
        StageOutcome::Failed {
            retry_requested: false,
        },
        |o| o.status,
    );

    if outcome_status == StageOutcome::Succeeded
        || outcome_status == StageOutcome::PartiallySucceeded
    {
        let total_usd_micros = billing.as_ref().and_then(|b| b.total_usd_micros);
        return Event::WorkflowRunCompleted {
            duration_ms,
            artifact_count,
            status: outcome_status.to_string(),
            reason: match outcome_status {
                StageOutcome::PartiallySucceeded => SuccessReason::PartialSuccess,
                _ => SuccessReason::Completed,
            },
            total_usd_micros,
            final_git_commit_sha,
            final_patch,
            billing,
        };
    }

    let error = match outcome {
        Err(err) => err.clone(),
        Ok(o) => Error::engine(
            o.failure
                .as_ref()
                .map_or_else(|| "run failed".to_string(), |f| f.message.clone()),
        ),
    };
    Event::WorkflowRunFailed {
        error,
        duration_ms,
        reason: FailureReason::WorkflowError,
        git_commit_sha: final_git_commit_sha,
        final_patch,
    }
}

async fn cleanup_sandbox(
    services: &RunServices,
    run_id: &fabro_types::RunId,
    workflow_name: &str,
    preserve: bool,
) -> std::result::Result<(), String> {
    let hook_ctx = HookContext::new(
        HookEvent::SandboxCleanup,
        *run_id,
        workflow_name.to_string(),
    );
    let _ = services.run_hooks(&hook_ctx).await;
    if !preserve {
        services
            .sandbox
            .cleanup()
            .await
            .map_err(|e| e.display_with_causes())?;
    }
    Ok(())
}

/// FINALIZE phase: build conclusion, write the meta branch, emit the terminal
/// `WorkflowRunCompleted`/`WorkflowRunFailed` event.
///
/// The terminal event is emitted here (not from `on_run_end`) so observers
/// can't act on "done" before the meta branch writes are flushed.
///
/// # Errors
///
/// Returns `Error` if persisting terminal state fails.
pub async fn finalize(retroed: Retroed, options: &FinalizeOptions) -> Result<Concluded, Error> {
    let Retroed {
        graph,
        outcome,
        run_options,
        duration_ms,
        services,
        retro: _,
    } = retroed;

    let (final_status, failure_reason, _run_status) = classify_engine_result(&outcome);

    let events = services.run_store.list_events().await.unwrap_or_default();
    let stage_durations = crate::extract_stage_durations_from_events(&events);
    let artifact_count = events
        .iter()
        .filter(|envelope| matches!(envelope.event.body, EventBody::ArtifactCaptured(_)))
        .count();
    let projection = services.run_store.state().await.ok();
    let projection_order = projection
        .as_ref()
        .map(stage_projection_order)
        .unwrap_or_default();
    let checkpoint = projection
        .as_ref()
        .and_then(|state| state.checkpoint.as_ref());
    let conclusion = build_conclusion_from_parts(
        checkpoint,
        &stage_durations,
        &projection_order,
        final_status,
        failure_reason,
        duration_ms,
        options.last_git_sha.clone(),
    );

    let (final_patch, ()) = tokio::join!(
        compute_final_patch(&run_options, &services, final_status),
        write_finalize_commit(&run_options, &services, &conclusion),
    );

    if services.metadata_runtime.metadata_degraded() {
        services.emitter.notice(
            RunNoticeLevel::Warn,
            "checkpoint_metadata_degraded",
            "checkpoint metadata archive writes were degraded for this run".to_string(),
        );
    }

    let terminal_event = build_terminal_event(
        &outcome,
        duration_ms,
        artifact_count,
        options.last_git_sha.clone(),
        final_patch,
        conclusion.billing.clone(),
    );
    services.emitter.emit(&terminal_event);

    if options.preserve_sandbox {
        let info = services.sandbox.sandbox_info();
        let message = if info.is_empty() {
            "sandbox preserved".to_string()
        } else {
            format!("sandbox preserved: {info}")
        };
        services
            .emitter
            .notice(RunNoticeLevel::Info, "sandbox_preserved", message);
    }
    if let Err(e) = cleanup_sandbox(
        &services,
        &options.run_id,
        &options.workflow_name,
        options.preserve_sandbox,
    )
    .await
    {
        tracing::warn!(error = %e, "Sandbox cleanup failed");
        services.emitter.notice(
            RunNoticeLevel::Warn,
            "sandbox_cleanup_failed",
            format!("sandbox cleanup failed: {e}"),
        );
    }

    Ok(Concluded {
        outcome,
        conclusion,
        graph,
        run_options,
        services,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    use anyhow::Result;
    use async_trait::async_trait;
    use bytes::Bytes;
    use fabro_graphviz::graph::Graph;
    use fabro_store::{Database, EventEnvelope, RunDatabase, RunProjection};
    use fabro_types::run_event::{MetadataSnapshotFailureKind, MetadataSnapshotPhase};
    use fabro_types::{
        EventBody, RunBlobId, RunEvent, RunId, WorkflowSettings, first_event_seq, fixtures,
    };
    use object_store::memory::InMemory;

    use super::*;
    use crate::event::{Emitter, StoreProgressLogger, append_event};
    use crate::pipeline::types::Retroed;
    use crate::run_metadata::{RunMetadataRuntime, RunMetadataWriterHandle};
    use crate::run_options::{GitCheckpointOptions, RunOptions};
    use crate::runtime_store::{RunStoreBackend, RunStoreHandle};
    use crate::sandbox_git_runtime::SandboxGitRuntime;

    fn test_run_id() -> RunId {
        fixtures::RUN_1
    }

    fn test_run_options(run_dir: &std::path::Path) -> RunOptions {
        RunOptions {
            settings:         WorkflowSettings::default(),
            run_dir:          run_dir.to_path_buf(),
            cancel_token:     None,
            run_id:           test_run_id(),
            labels:           HashMap::new(),
            workflow_slug:    None,
            github_app:       None,
            pre_run_git:      None,
            fork_source_ref:  None,
            base_branch:      None,
            display_base_sha: None,
            git:              None,
        }
    }

    fn test_git_run_options(run_dir: &std::path::Path, meta_branch: &str) -> RunOptions {
        let mut options = test_run_options(run_dir);
        options.git = Some(GitCheckpointOptions {
            base_sha:    None,
            run_branch:  None,
            meta_branch: Some(meta_branch.to_string()),
        });
        options
    }

    fn test_store() -> Arc<Database> {
        Arc::new(Database::new(
            Arc::new(InMemory::new()),
            "",
            Duration::from_millis(1),
            None,
        ))
    }

    async fn seeded_run_store() -> RunDatabase {
        let run_store = test_store().create_run(&test_run_id()).await.unwrap();
        append_event(&run_store, &test_run_id(), &Event::RunCreated {
            run_id:           test_run_id(),
            settings:         serde_json::to_value(WorkflowSettings::default()).unwrap(),
            graph:            serde_json::to_value(fabro_types::Graph::new("metadata")).unwrap(),
            workflow_source:  None,
            workflow_config:  None,
            labels:           std::collections::BTreeMap::new(),
            run_dir:          "/tmp/run".to_string(),
            source_directory: Some("/tmp/project".to_string()),
            workflow_slug:    Some("metadata".to_string()),
            db_prefix:        None,
            provenance:       None,
            manifest_blob:    None,
            git:              None,
            fork_source_ref:  None,
            in_place:         false,
            web_url:          None,
        })
        .await
        .unwrap();
        run_store
    }

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

    fn record_events(emitter: &Arc<Emitter>) -> Arc<std::sync::Mutex<Vec<RunEvent>>> {
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = Arc::clone(&events);
        emitter.on_event(move |event| {
            captured.lock().unwrap().push(event.clone());
        });
        events
    }

    fn checkpoint_with(
        completed_nodes: Vec<&str>,
        node_outcomes: HashMap<String, Outcome>,
    ) -> Checkpoint {
        Checkpoint {
            timestamp: chrono::Utc::now(),
            current_node: completed_nodes
                .last()
                .copied()
                .unwrap_or("start")
                .to_string(),
            completed_nodes: completed_nodes.into_iter().map(str::to_string).collect(),
            node_retries: HashMap::new(),
            context_values: HashMap::new(),
            node_outcomes,
            next_node_id: None,
            git_commit_sha: None,
            loop_failure_signatures: HashMap::new(),
            restart_failure_signatures: HashMap::new(),
            node_visits: HashMap::new(),
        }
    }

    #[test]
    fn conclusion_stage_order_follows_projection_first_event_order() {
        let mut projection = RunProjection::default();
        projection.stage_entry("zebra", 1, first_event_seq(1));
        projection.stage_entry("apple", 1, first_event_seq(2));
        let projection_order = stage_projection_order(&projection);
        let checkpoint = checkpoint_with(
            vec!["apple", "zebra"],
            HashMap::from([
                ("apple".to_string(), Outcome::success()),
                ("zebra".to_string(), Outcome::success()),
            ]),
        );

        let conclusion = build_conclusion_from_parts(
            Some(&checkpoint),
            &HashMap::new(),
            &projection_order,
            StageOutcome::Succeeded,
            None,
            10,
            None,
        );

        let stage_ids = conclusion
            .stages
            .iter()
            .map(|stage| stage.stage_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(stage_ids, vec!["zebra", "apple"]);
    }

    #[test]
    fn conclusion_includes_skipped_stage_from_projection_checkpoint_fallback() {
        let mut projection = RunProjection::default();
        projection.stage_entry("skipped", 1, first_event_seq(4));
        projection.stage_entry("finished", 1, first_event_seq(5));
        let projection_order = stage_projection_order(&projection);
        let checkpoint = checkpoint_with(
            vec!["finished"],
            HashMap::from([
                ("finished".to_string(), Outcome::success()),
                (
                    "skipped".to_string(),
                    Outcome::skipped("condition was false"),
                ),
            ]),
        );

        let conclusion = build_conclusion_from_parts(
            Some(&checkpoint),
            &HashMap::new(),
            &projection_order,
            StageOutcome::Succeeded,
            None,
            10,
            None,
        );

        let stage_ids = conclusion
            .stages
            .iter()
            .map(|stage| stage.stage_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(stage_ids, vec!["skipped", "finished"]);
    }

    fn test_services(
        run_store: RunStoreHandle,
        emitter: Arc<Emitter>,
        sandbox: Arc<dyn fabro_agent::Sandbox>,
        metadata_runtime: Arc<RunMetadataRuntime>,
        metadata_writer: Option<RunMetadataWriterHandle>,
    ) -> Arc<RunServices> {
        RunServices::new(
            run_store,
            emitter,
            sandbox,
            None,
            None,
            fabro_model::Provider::Anthropic,
            Arc::new(fabro_auth::EnvCredentialSource::new()),
            Arc::new(SandboxGitRuntime::new()),
            metadata_runtime,
            metadata_writer,
        )
    }

    #[tokio::test]
    async fn finalize_persists_conclusion_in_projection() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let inner_store = test_store().create_run(&test_run_id()).await.unwrap();
        let run_store = inner_store;
        let emitter = Arc::new(Emitter::new(test_run_id()));
        let store_logger = StoreProgressLogger::new(run_store.clone());
        store_logger.register(&emitter);
        let services = RunServices::new(
            run_store.clone().into(),
            Arc::clone(&emitter),
            Arc::new(fabro_agent::LocalSandbox::new(
                std::env::current_dir().unwrap(),
            )),
            None,
            None,
            fabro_model::Provider::Anthropic,
            Arc::new(fabro_auth::EnvCredentialSource::new()),
            Arc::new(SandboxGitRuntime::new()),
            Arc::new(RunMetadataRuntime::new()),
            None,
        );
        let retroed = Retroed {
            graph: Graph::new("test"),
            outcome: Ok(Outcome::success()),
            run_options: test_run_options(&run_dir),
            duration_ms: 5,
            services,
            retro: None,
        };

        let concluded = finalize(retroed, &FinalizeOptions {
            run_dir:          run_dir.clone(),
            run_id:           test_run_id(),
            workflow_name:    "test".to_string(),
            preserve_sandbox: true,
            last_git_sha:     None,
        })
        .await
        .unwrap();
        store_logger.flush().await;

        assert_eq!(concluded.conclusion.status, StageOutcome::Succeeded);
    }

    #[tokio::test]
    async fn finalize_metadata_snapshot_success_emits_started_completed_unscoped() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_git_repo(repo_dir.path());
        let branch = "fabro/metadata/run";
        let run_store = seeded_run_store().await;
        let handle = RunStoreHandle::local(run_store.clone());
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
        let emitter = Arc::new(Emitter::new(test_run_id()));
        let events = record_events(&emitter);
        let services = test_services(
            handle,
            emitter,
            Arc::new(fabro_agent::LocalSandbox::new(
                repo_dir.path().to_path_buf(),
            )),
            Arc::new(RunMetadataRuntime::new()),
            Some(RunMetadataWriterHandle::new_for_test_repo(
                repo_dir.path(),
                branch,
            )),
        );
        let run_options = test_git_run_options(repo_dir.path(), branch);

        write_finalize_commit(&run_options, &services, &conclusion).await;

        let events = events.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_name(), "metadata.snapshot.started");
        assert_eq!(events[1].event_name(), "metadata.snapshot.completed");
        assert!(events[0].node_id.is_none());
        match &events[1].body {
            EventBody::MetadataSnapshotCompleted(props) => {
                assert_eq!(props.phase, MetadataSnapshotPhase::Finalize);
                assert_eq!(props.branch, branch);
                assert!(!props.commit_sha.is_empty());
            }
            other => panic!("expected metadata completed event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn finalize_metadata_load_state_failure_emits_failed_before_notice() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_git_repo(repo_dir.path());
        let emitter = Arc::new(Emitter::new(test_run_id()));
        let events = record_events(&emitter);
        let services = test_services(
            RunStoreHandle::new(Arc::new(FailingStateStore)),
            emitter,
            Arc::new(fabro_agent::LocalSandbox::new(
                repo_dir.path().to_path_buf(),
            )),
            Arc::new(RunMetadataRuntime::new()),
            Some(RunMetadataWriterHandle::new_for_test_repo(
                repo_dir.path(),
                "fabro/metadata/run",
            )),
        );
        let run_options = test_git_run_options(repo_dir.path(), "fabro/metadata/run");
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

        write_finalize_commit(&run_options, &services, &conclusion).await;

        let events = events.lock().unwrap();
        let names = events.iter().map(RunEvent::event_name).collect::<Vec<_>>();
        assert_eq!(names, vec![
            "metadata.snapshot.started",
            "metadata.snapshot.failed",
            "run.notice",
        ]);
        match &events[1].body {
            EventBody::MetadataSnapshotFailed(props) => {
                assert_eq!(props.phase, MetadataSnapshotPhase::Finalize);
                assert_eq!(props.failure_kind, MetadataSnapshotFailureKind::LoadState);
            }
            other => panic!("expected metadata failed event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn degraded_metadata_runtime_skips_finalize_metadata_events() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_git_repo(repo_dir.path());
        let run_store = seeded_run_store().await;
        let emitter = Arc::new(Emitter::new(test_run_id()));
        let events = record_events(&emitter);
        let runtime = Arc::new(RunMetadataRuntime::new());
        runtime.mark_metadata_degraded();
        let services = test_services(
            RunStoreHandle::local(run_store),
            emitter,
            Arc::new(fabro_agent::LocalSandbox::new(
                repo_dir.path().to_path_buf(),
            )),
            runtime,
            Some(RunMetadataWriterHandle::new_for_test_repo(
                repo_dir.path(),
                "fabro/metadata/run",
            )),
        );
        let run_options = test_git_run_options(repo_dir.path(), "fabro/metadata/run");
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

        write_finalize_commit(&run_options, &services, &conclusion).await;

        assert!(events.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn finalize_emits_metadata_snapshot_before_run_completed() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_git_repo(repo_dir.path());
        let run_store = seeded_run_store().await;
        let emitter = Arc::new(Emitter::new(test_run_id()));
        let events = record_events(&emitter);
        let services = test_services(
            RunStoreHandle::local(run_store),
            Arc::clone(&emitter),
            Arc::new(fabro_agent::LocalSandbox::new(
                repo_dir.path().to_path_buf(),
            )),
            Arc::new(RunMetadataRuntime::new()),
            Some(RunMetadataWriterHandle::new_for_test_repo(
                repo_dir.path(),
                "fabro/metadata/run",
            )),
        );
        let retroed = Retroed {
            graph: Graph::new("test"),
            outcome: Ok(Outcome::success()),
            run_options: test_git_run_options(repo_dir.path(), "fabro/metadata/run"),
            duration_ms: 5,
            services,
            retro: None,
        };

        finalize(retroed, &FinalizeOptions {
            run_dir:          repo_dir.path().to_path_buf(),
            run_id:           test_run_id(),
            workflow_name:    "test".to_string(),
            preserve_sandbox: false,
            last_git_sha:     None,
        })
        .await
        .unwrap();

        let names = events
            .lock()
            .unwrap()
            .iter()
            .map(|event| event.event_name().to_string())
            .collect::<Vec<_>>();
        assert_eq!(names, vec![
            "metadata.snapshot.started",
            "metadata.snapshot.completed",
            "run.completed",
        ]);
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
