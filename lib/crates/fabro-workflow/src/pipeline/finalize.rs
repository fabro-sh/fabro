use fabro_hooks::{HookContext, HookEvent};
use fabro_types::{BilledTokenCounts, EventBody};

use super::types::{Concluded, FinalizeOptions, Retroed};
use crate::error::Error;
use crate::event::{Event, RunNoticeLevel};
use crate::git::MetadataStore;
use crate::outcome::{Outcome, OutcomeExt, StageStatus};
use crate::records::{Checkpoint, Conclusion, StageSummary};
use crate::run_dump::RunDump;
use crate::run_options::RunOptions;
use crate::run_status::{FailureReason, RunStatus, SuccessReason};
use crate::runtime_store::RunStoreHandle;
use crate::sandbox_git::{git_diff_with_timeout, git_push_host};
use crate::services::RunServices;

pub fn classify_engine_result(
    engine_result: &Result<Outcome, Error>,
) -> (StageStatus, Option<String>, RunStatus) {
    match engine_result {
        Ok(outcome) => {
            let status = outcome.status.clone();
            let failure_reason = outcome.failure_reason().map(String::from);
            let run_status = match status {
                StageStatus::Success | StageStatus::Skipped => RunStatus::Succeeded {
                    reason: SuccessReason::Completed,
                },
                StageStatus::PartialSuccess => RunStatus::Succeeded {
                    reason: SuccessReason::PartialSuccess,
                },
                StageStatus::Fail | StageStatus::Retry => RunStatus::Failed {
                    reason: FailureReason::WorkflowError,
                },
            };
            (status, failure_reason, run_status)
        }
        Err(Error::Cancelled) => (
            StageStatus::Fail,
            Some("Cancelled".to_string()),
            RunStatus::Failed {
                reason: FailureReason::Cancelled,
            },
        ),
        Err(err) => (
            StageStatus::Fail,
            Some(err.to_string()),
            RunStatus::Failed {
                reason: FailureReason::WorkflowError,
            },
        ),
    }
}

pub(crate) async fn build_conclusion_from_store(
    run_store: &RunStoreHandle,
    status: StageStatus,
    failure_reason: Option<String>,
    run_duration_ms: u64,
    final_git_commit_sha: Option<String>,
) -> Conclusion {
    let checkpoint = run_store
        .state()
        .await
        .ok()
        .and_then(|state| state.checkpoint);
    let stage_durations = run_store
        .list_events()
        .await
        .map(|events| crate::extract_stage_durations_from_events(&events))
        .unwrap_or_default();

    build_conclusion_from_parts(
        checkpoint.as_ref(),
        &stage_durations,
        status,
        failure_reason,
        run_duration_ms,
        final_git_commit_sha,
    )
}

fn build_conclusion_from_parts(
    checkpoint: Option<&Checkpoint>,
    stage_durations: &std::collections::HashMap<String, u64>,
    status: StageStatus,
    failure_reason: Option<String>,
    run_duration_ms: u64,
    final_git_commit_sha: Option<String>,
) -> Conclusion {
    // Looping workflows revisit nodes; `completed_nodes` accumulates duplicates
    // while the other checkpoint maps are keyed by node_id. Dedupe to one row
    // per node so the stages table matches the deduped billing total.
    let (stages, total_retries) = if let Some(cp) = checkpoint {
        let mut stages = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut retries_sum: u32 = 0;

        for node_id in &cp.completed_nodes {
            if !seen.insert(node_id.as_str()) {
                continue;
            }
            let outcome = cp.node_outcomes.get(node_id);
            let retries = cp
                .node_retries
                .get(node_id)
                .copied()
                .unwrap_or(1)
                .saturating_sub(1);
            retries_sum += retries;

            stages.push(StageSummary {
                stage_id: node_id.clone(),
                stage_label: node_id.clone(),
                duration_ms: stage_durations.get(node_id).copied().unwrap_or(0),
                billing_usd_micros: outcome
                    .and_then(|o| o.usage.as_ref())
                    .and_then(|usage| usage.total_usd_micros),
                retries,
            });
        }
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

/// `conclusion` is injected because the terminal event hasn't been emitted
/// yet — the run store's `projection.conclusion` is still `None` at this point.
pub async fn write_finalize_commit(
    run_options: &RunOptions,
    run_store: &RunStoreHandle,
    conclusion: &Conclusion,
) {
    let (Some(meta_branch), Some(repo_path)) = (
        run_options
            .git
            .as_ref()
            .and_then(|g| g.meta_branch.as_ref()),
        run_options.host_repo_path.as_ref(),
    ) else {
        return;
    };

    let git_author = run_options.git_author();
    let store = MetadataStore::new(repo_path, &git_author);
    let Ok(mut store_state) = run_store.state().await else {
        return;
    };
    if store_state.conclusion.is_none() {
        store_state.conclusion = Some(conclusion.clone());
    }
    let dump = RunDump::from_projection(&store_state);
    if let Err(e) =
        dump.write_to_metadata_store(&store, &run_options.run_id.to_string(), "finalize run")
    {
        tracing::warn!(error = %e, "Failed to write finalize commit to metadata branch");
        return;
    }

    let refspec = format!("refs/heads/{meta_branch}");
    git_push_host(
        repo_path,
        &refspec,
        &run_options.github_app,
        "finalize metadata",
    )
    .await;
}

/// Failed and cancelled runs use a shorter diff timeout so a corrupted
/// workspace can't stall downstream consumers waiting on the terminal event.
async fn compute_final_patch(
    run_options: &RunOptions,
    services: &RunServices,
    status: StageStatus,
) -> Option<String> {
    let base_sha = run_options.git.as_ref().and_then(|g| g.base_sha.clone())?;
    let timeout_ms = match status {
        StageStatus::Success | StageStatus::PartialSuccess => 30_000,
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

    let outcome_status = outcome
        .as_ref()
        .map_or(StageStatus::Fail, |o| o.status.clone());

    if outcome_status == StageStatus::Success || outcome_status == StageStatus::PartialSuccess {
        let total_usd_micros = billing.as_ref().and_then(|b| b.total_usd_micros);
        return Event::WorkflowRunCompleted {
            duration_ms,
            artifact_count,
            status: outcome_status.to_string(),
            reason: match outcome_status {
                StageStatus::PartialSuccess => SuccessReason::PartialSuccess,
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
        services.sandbox.cleanup().await?;
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
    let checkpoint = services
        .run_store
        .state()
        .await
        .ok()
        .and_then(|state| state.checkpoint);
    let conclusion = build_conclusion_from_parts(
        checkpoint.as_ref(),
        &stage_durations,
        final_status.clone(),
        failure_reason,
        duration_ms,
        options.last_git_sha.clone(),
    );

    let final_patch = compute_final_patch(&run_options, &services, final_status).await;

    write_finalize_commit(&run_options, &services.run_store, &conclusion).await;

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
        run_id: run_options.run_id,
        outcome,
        conclusion,
        pushed_branch: run_options.git.as_ref().and_then(|g| g.run_branch.clone()),
        graph,
        run_options,
        services,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    use fabro_graphviz::graph::Graph;
    use fabro_store::Database;
    use fabro_types::settings::SettingsLayer;
    use fabro_types::{RunId, fixtures};
    use object_store::memory::InMemory;

    use super::*;
    use crate::event::{Emitter, StoreProgressLogger};
    use crate::pipeline::types::Retroed;
    use crate::run_options::RunOptions;

    fn test_run_id() -> RunId {
        fixtures::RUN_1
    }

    fn test_run_options(run_dir: &std::path::Path) -> RunOptions {
        RunOptions {
            settings:         SettingsLayer::default(),
            run_dir:          run_dir.to_path_buf(),
            cancel_token:     None,
            run_id:           test_run_id(),
            labels:           HashMap::new(),
            workflow_slug:    None,
            github_app:       None,
            host_repo_path:   None,
            base_branch:      None,
            display_base_sha: None,
            git:              None,
        }
    }

    fn test_store() -> Arc<Database> {
        Arc::new(Database::new(
            Arc::new(InMemory::new()),
            "",
            Duration::from_millis(1),
            None,
        ))
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

        assert_eq!(concluded.conclusion.status, StageStatus::Success);
    }
}
