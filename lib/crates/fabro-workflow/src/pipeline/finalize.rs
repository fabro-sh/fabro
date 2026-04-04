use std::sync::Arc;

use crate::error::FabroError;
use crate::event::{EventEmitter, RunNoticeLevel, WorkflowRunEvent};
use crate::git::MetadataStore;
use crate::outcome::{Outcome, OutcomeExt, StageStatus};
use crate::records::{Checkpoint, Conclusion, StageSummary};
use crate::run_dump::RunDump;
use crate::run_options::RunOptions;
use crate::run_status::{RunStatus, StatusReason};
use crate::sandbox_git::git_push_host;
use fabro_hooks::{HookContext, HookEvent, HookRunner};
use fabro_store::SlateRunStore;

use super::types::{Concluded, FinalizeOptions, Retroed};

fn emit_run_notice(
    emitter: &EventEmitter,
    level: RunNoticeLevel,
    code: impl Into<String>,
    message: impl Into<String>,
) {
    emitter.emit(&WorkflowRunEvent::RunNotice {
        level,
        code: code.into(),
        message: message.into(),
    });
}

pub fn classify_engine_result(
    engine_result: &Result<Outcome, FabroError>,
) -> (StageStatus, Option<String>, RunStatus, Option<StatusReason>) {
    match engine_result {
        Ok(outcome) => {
            let status = outcome.status.clone();
            let failure_reason = outcome.failure_reason().map(String::from);
            let (run_status, status_reason) = match status {
                StageStatus::Success | StageStatus::Skipped => {
                    (RunStatus::Succeeded, Some(StatusReason::Completed))
                }
                StageStatus::PartialSuccess => {
                    (RunStatus::Succeeded, Some(StatusReason::PartialSuccess))
                }
                StageStatus::Fail | StageStatus::Retry => {
                    (RunStatus::Failed, Some(StatusReason::WorkflowError))
                }
            };
            (status, failure_reason, run_status, status_reason)
        }
        Err(FabroError::Cancelled) => (
            StageStatus::Fail,
            Some("Cancelled".to_string()),
            RunStatus::Failed,
            Some(StatusReason::Cancelled),
        ),
        Err(err) => (
            StageStatus::Fail,
            Some(err.to_string()),
            RunStatus::Failed,
            Some(StatusReason::WorkflowError),
        ),
    }
}

pub(crate) async fn build_conclusion_from_store(
    run_store: &SlateRunStore,
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
    let mut total_input_tokens: i64 = 0;
    let mut total_output_tokens: i64 = 0;
    let mut total_cache_read_tokens: i64 = 0;
    let mut total_cache_write_tokens: i64 = 0;
    let mut total_reasoning_tokens: i64 = 0;
    let mut has_pricing = false;

    let (stages, total_cost, total_retries) = if let Some(cp) = checkpoint {
        let mut stages = Vec::new();
        let mut cost_sum: Option<f64> = None;
        let mut retries_sum: u32 = 0;

        for node_id in &cp.completed_nodes {
            let outcome = cp.node_outcomes.get(node_id);
            let retries = cp
                .node_retries
                .get(node_id)
                .copied()
                .unwrap_or(1)
                .saturating_sub(1);
            retries_sum += retries;

            let cost = outcome.and_then(|o| o.usage.as_ref()).and_then(|u| u.cost);
            if let Some(c) = cost {
                *cost_sum.get_or_insert(0.0) += c;
                has_pricing = true;
            }

            if let Some(usage) = outcome.and_then(|o| o.usage.as_ref()) {
                total_input_tokens += usage.input_tokens;
                total_output_tokens += usage.output_tokens;
                total_cache_read_tokens += usage.cache_read_tokens.unwrap_or(0);
                total_cache_write_tokens += usage.cache_write_tokens.unwrap_or(0);
                total_reasoning_tokens += usage.reasoning_tokens.unwrap_or(0);
            }

            stages.push(StageSummary {
                stage_id: node_id.clone(),
                stage_label: node_id.clone(),
                duration_ms: stage_durations.get(node_id).copied().unwrap_or(0),
                cost,
                retries,
            });
        }
        (stages, cost_sum, retries_sum)
    } else {
        (vec![], None, 0)
    };

    Conclusion {
        timestamp: chrono::Utc::now(),
        status,
        duration_ms: run_duration_ms,
        failure_reason,
        final_git_commit_sha,
        stages,
        total_cost,
        total_retries,
        total_input_tokens,
        total_output_tokens,
        total_cache_read_tokens,
        total_cache_write_tokens,
        total_reasoning_tokens,
        has_pricing,
    }
}

/// Write a finalize commit to the shadow branch with retro.json and final node files.
///
/// This captures the last diff.patch (written after the final checkpoint) and retro.json.
/// Best-effort: errors are logged as warnings.
pub async fn write_finalize_commit(run_options: &RunOptions, run_store: &SlateRunStore) {
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
    let Ok(store_state) = run_store.state().await else {
        return;
    };
    let dump = RunDump::metadata_finalize(&store_state);
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

async fn run_hooks(
    hook_runner: Option<&HookRunner>,
    hook_context: &HookContext,
    sandbox: Arc<dyn fabro_agent::Sandbox>,
) {
    let Some(runner) = hook_runner else {
        return;
    };
    let _ = runner.run(hook_context, sandbox, None).await;
}

async fn cleanup_sandbox(
    hook_runner: Option<Arc<HookRunner>>,
    sandbox: Arc<dyn fabro_agent::Sandbox>,
    run_id: &fabro_types::RunId,
    workflow_name: &str,
    preserve: bool,
) -> std::result::Result<(), String> {
    let hook_ctx = HookContext::new(
        HookEvent::SandboxCleanup,
        *run_id,
        workflow_name.to_string(),
    );
    run_hooks(hook_runner.as_deref(), &hook_ctx, Arc::clone(&sandbox)).await;
    if !preserve {
        sandbox.cleanup().await?;
    }
    Ok(())
}

/// FINALIZE phase: classify outcome, build conclusion, persist terminal state.
///
/// # Errors
///
/// Returns `FabroError` if persisting terminal state fails.
pub async fn finalize(
    retroed: Retroed,
    options: &FinalizeOptions,
) -> Result<Concluded, FabroError> {
    let Retroed {
        graph,
        outcome,
        run_options,
        run_store: _run_store,
        hook_runner,
        emitter,
        sandbox,
        duration_ms,
        retro: _,
    } = retroed;

    let (final_status, failure_reason, _run_status, _status_reason) =
        classify_engine_result(&outcome);
    let conclusion = build_conclusion_from_store(
        &options.run_store,
        final_status,
        failure_reason,
        duration_ms,
        options.last_git_sha.clone(),
    )
    .await;

    write_finalize_commit(&run_options, &options.run_store).await;

    if options.preserve_sandbox {
        let info = sandbox.sandbox_info();
        if info.is_empty() {
            emit_run_notice(
                &emitter,
                RunNoticeLevel::Info,
                "sandbox_preserved",
                "sandbox preserved",
            );
        } else {
            emit_run_notice(
                &emitter,
                RunNoticeLevel::Info,
                "sandbox_preserved",
                format!("sandbox preserved: {info}"),
            );
        }
    }
    if let Err(e) = cleanup_sandbox(
        options.hook_runner.clone().or(hook_runner),
        sandbox,
        &options.run_id,
        &options.workflow_name,
        options.preserve_sandbox,
    )
    .await
    {
        tracing::warn!(error = %e, "Sandbox cleanup failed");
        emit_run_notice(
            &emitter,
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
        emitter,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    use fabro_graphviz::graph::Graph;
    use fabro_store::SlateStore;
    use fabro_types::{RunId, Settings, fixtures};
    use object_store::memory::InMemory;

    use super::*;
    use crate::event::StoreProgressLogger;
    use crate::pipeline::types::Retroed;
    use crate::run_options::RunOptions;

    fn test_run_id() -> RunId {
        fixtures::RUN_1
    }

    fn test_run_options(run_dir: &std::path::Path) -> RunOptions {
        RunOptions {
            settings: Settings::default(),
            run_dir: run_dir.to_path_buf(),
            cancel_token: None,
            run_id: test_run_id(),
            labels: HashMap::new(),
            workflow_slug: None,
            github_app: None,
            host_repo_path: None,
            base_branch: None,
            display_base_sha: None,
            git: None,
        }
    }

    fn test_store() -> Arc<SlateStore> {
        Arc::new(SlateStore::new(
            Arc::new(InMemory::new()),
            "",
            Duration::from_millis(1),
        ))
    }

    #[tokio::test]
    async fn finalize_writes_conclusion_json() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let inner_store = test_store().create_run(&test_run_id()).await.unwrap();
        let run_store = inner_store;
        let emitter = Arc::new(EventEmitter::new(test_run_id()));
        let store_logger = StoreProgressLogger::new(run_store.clone());
        store_logger.register(&emitter);
        let retroed = Retroed {
            graph: Graph::new("test"),
            outcome: Ok(Outcome::success()),
            run_options: test_run_options(&run_dir),
            run_store: run_store.clone(),
            hook_runner: None,
            emitter,
            sandbox: Arc::new(fabro_agent::LocalSandbox::new(
                std::env::current_dir().unwrap(),
            )),
            duration_ms: 5,
            retro: None,
        };

        let concluded = finalize(
            retroed,
            &FinalizeOptions {
                run_dir: run_dir.clone(),
                run_id: test_run_id(),
                run_store: run_store.clone(),
                workflow_name: "test".to_string(),
                hook_runner: None,
                preserve_sandbox: true,
                last_git_sha: None,
            },
        )
        .await
        .unwrap();
        store_logger.flush().await;

        assert_eq!(concluded.conclusion.status, StageStatus::Success);
    }
}
