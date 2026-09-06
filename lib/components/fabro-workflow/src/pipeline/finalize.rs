use std::sync::Arc;

use fabro_hooks::{HookContext, HookEvent};
use fabro_types::{BilledTokenCounts, DiffSummary, EventBody, RunFailure, RunProjection};

use super::types::{Concluded, Executed, FinalizeOptions, Finalized, PublishOutcome, Published};
use crate::billing_rollup;
use crate::error::{Error, run_failure_from_error, run_failure_from_outcome_failure};
use crate::event::{Event, RunNoticeCode, RunNoticeLevel};
use crate::outcome::{Outcome, StageOutcome};
use crate::records::Conclusion;
use crate::run_options::RunOptions;
use crate::run_status::{FailureReason, RunStatus, SuccessReason};
use crate::runtime_store::RunStoreHandle;
use crate::sandbox_git::{git_diff_with_timeout, list_diff_numstat, summarize_diff_numstat};
use crate::services::RunServices;

pub fn classify_engine_result(
    engine_result: &Result<Outcome, Error>,
) -> (StageOutcome, Option<RunFailure>, RunStatus) {
    match engine_result {
        Ok(outcome) => {
            let status = outcome.status;
            let failure = outcome.failure.as_ref().map(|failure| {
                run_failure_from_outcome_failure(failure, FailureReason::WorkflowError)
            });
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
            (status, failure, run_status)
        }
        Err(err) => {
            let reason = err.failure_reason();
            (
                StageOutcome::Failed {
                    retry_requested: false,
                },
                Some(run_failure_from_error(err, reason)),
                RunStatus::Failed { reason },
            )
        }
    }
}

pub(crate) async fn build_conclusion_from_store(
    run_store: &RunStoreHandle,
    status: StageOutcome,
    failure: Option<RunFailure>,
    run_wall_time_ms: u64,
    final_git_commit_sha: Option<String>,
) -> Conclusion {
    let projection = run_store.state().await.ok();
    build_conclusion_from_projection(
        projection.as_ref(),
        status,
        failure,
        run_wall_time_ms,
        final_git_commit_sha,
    )
}

fn build_conclusion_from_projection(
    projection: Option<&RunProjection>,
    status: StageOutcome,
    failure: Option<RunFailure>,
    run_wall_time_ms: u64,
    final_git_commit_sha: Option<String>,
) -> Conclusion {
    let billing = projection
        .map(|projection| billing_rollup::billing_rollup_from_projection(projection, None))
        .unwrap_or_default();
    let (stages, total_retries) = projection
        .map(|projection| billing.conclusion_stages(projection))
        .unwrap_or_default();
    Conclusion {
        timestamp: chrono::Utc::now(),
        status,
        timing: billing.timing.with_wall_time(run_wall_time_ms),
        failure,
        final_git_commit_sha,
        stages,
        billing: billing.billing_if_present(),
        total_retries,
        diff: fabro_types::RunDiff::default(),
    }
}

/// Failed and cancelled runs use a shorter diff timeout so a corrupted
/// workspace cannot stall consumers waiting on the terminal event.
async fn compute_final_patch(
    run_options: &RunOptions,
    services: &RunServices,
    status: StageOutcome,
) -> (Option<String>, Option<DiffSummary>) {
    let Some(base_sha) = run_options.git.as_ref().and_then(|g| g.base_sha.clone()) else {
        return (None, None);
    };
    let timeout_ms = match status {
        StageOutcome::Succeeded | StageOutcome::PartiallySucceeded => 30_000,
        _ => 10_000,
    };
    let to_sha = "HEAD";
    let (patch_result, numstat_result) = tokio::join!(
        git_diff_with_timeout(&*services.sandbox, &base_sha, timeout_ms),
        list_diff_numstat(&*services.sandbox, &base_sha, to_sha),
    );
    let final_patch = match patch_result {
        Ok(patch) if !patch.is_empty() => Some(patch),
        Ok(_) => None,
        Err(err) => {
            services.emitter.notice(
                RunNoticeLevel::Warn,
                RunNoticeCode::GitDiffFailed,
                format!("final diff failed: {err}"),
            );
            None
        }
    };
    let diff_summary = match numstat_result {
        Ok(numstat) => Some(summarize_diff_numstat(&numstat)),
        Err(err) => {
            services.emitter.notice(
                RunNoticeLevel::Warn,
                RunNoticeCode::GitDiffFailed,
                format!("final diff stats failed: {err}"),
            );
            None
        }
    };
    (final_patch, diff_summary)
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn billing_from_projection(projection: &RunProjection) -> Option<BilledTokenCounts> {
    billing_rollup::billing_rollup_from_projection(projection, None).billing_if_present()
}

pub(crate) fn build_terminal_event(
    outcome: &Result<Outcome, Error>,
    timing: fabro_types::RunTiming,
    artifact_count: usize,
    final_git_commit_sha: Option<String>,
    final_patch: Option<String>,
    diff_summary: Option<DiffSummary>,
    billing: Option<BilledTokenCounts>,
) -> Event {
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
            timing,
            artifact_count,
            status: outcome_status.to_string(),
            reason: match outcome_status {
                StageOutcome::PartiallySucceeded => SuccessReason::PartialSuccess,
                _ => SuccessReason::Completed,
            },
            total_usd_micros,
            final_git_commit_sha,
            final_patch,
            diff_summary,
            billing,
        };
    }

    let failure = match outcome {
        Err(err) => run_failure_from_error(err, err.failure_reason()),
        Ok(outcome) => {
            if let Some(failure) = outcome.failure.as_ref() {
                run_failure_from_outcome_failure(failure, FailureReason::WorkflowError)
            } else {
                let fallback = Error::engine("run failed");
                run_failure_from_error(&fallback, FailureReason::WorkflowError)
            }
        }
    };
    Event::WorkflowRunFailed {
        failure,
        timing,
        final_git_commit_sha,
        final_patch,
        diff_summary,
        billing,
    }
}

async fn stop_sandbox_on_terminal(
    services: &RunServices,
    run_id: &fabro_types::RunId,
    workflow_name: &str,
    stop_on_terminal: bool,
) -> fabro_sandbox::Result<()> {
    let hook_ctx = HookContext::new(
        HookEvent::SandboxCleanup,
        *run_id,
        workflow_name.to_string(),
    );
    let _ = services.run_hooks(&hook_ctx).await;
    if stop_on_terminal {
        services.sandbox.stop().await?;
    }
    Ok(())
}

/// CONCLUDE phase: collect the execution result, final commit, and diff.
///
/// # Errors
///
/// Returns `Error` if the run state needed to build the conclusion cannot be
/// collected.
pub async fn conclude(executed: Executed, options: &FinalizeOptions) -> Result<Concluded, Error> {
    let Executed {
        graph,
        outcome,
        run_options,
        wall_time_ms,
        final_context: _,
        engine,
        model: _,
    } = executed;
    let services = Arc::clone(&engine.run);

    let (final_status, failure_reason, _run_status) = classify_engine_result(&outcome);

    let events = services.run_store.list_events().await.unwrap_or_default();
    let artifact_count = events
        .iter()
        .filter(|envelope| matches!(envelope.event.body, EventBody::ArtifactCaptured(_)))
        .count();
    let projection = services.run_store.state().await.ok();
    let mut conclusion = build_conclusion_from_projection(
        projection.as_ref(),
        final_status,
        failure_reason,
        wall_time_ms,
        options.last_git_sha.clone(),
    );

    let (final_patch, diff_summary) =
        compute_final_patch(&run_options, &services, final_status).await;
    conclusion.diff = fabro_types::RunDiff {
        patch:   final_patch,
        summary: diff_summary,
    };

    Ok(Concluded {
        outcome,
        conclusion,
        artifact_count,
        graph,
        run_options,
        services,
    })
}

/// FINALIZE phase: persist the final conclusion, emit the terminal event, and
/// clean up the sandbox.
///
/// This runs after PUBLISH so a required push or pull-request failure becomes
/// the terminal run result.
///
/// # Errors
///
/// Returns `Error` if persisting terminal state fails.
pub async fn finalize(published: Published, options: &FinalizeOptions) -> Result<Finalized, Error> {
    let Published {
        execution_outcome,
        publish_outcome,
        publish_error,
        mut conclusion,
        artifact_count,
        run_options,
        services,
    } = published;

    let PublishOutcome {
        pushed_branch,
        pr_url,
    } = publish_outcome;
    // An execution failure outranks a publish failure: publish only runs after
    // a successful execution, so the two are never both set.
    let outcome = match (execution_outcome, publish_error) {
        (Err(error), _) | (Ok(_), Some(error)) => Err(error),
        (Ok(outcome), None) => Ok(outcome),
    };

    let (final_status, failure, _run_status) = classify_engine_result(&outcome);
    conclusion.status = final_status;
    conclusion.failure = failure;

    let terminal_event = build_terminal_event(
        &outcome,
        conclusion.timing,
        artifact_count,
        conclusion.final_git_commit_sha.clone(),
        conclusion.diff.patch.clone(),
        conclusion.diff.summary,
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
        services.emitter.notice(
            RunNoticeLevel::Info,
            RunNoticeCode::SandboxPreserved,
            message,
        );
    }
    if let Err(e) = stop_sandbox_on_terminal(
        &services,
        &options.run_id,
        &options.workflow_name,
        options.stop_on_terminal,
    )
    .await
    {
        tracing::warn!(error = %fabro_sandbox::display_for_log(&e), "Sandbox stop failed");
        let exec_output_tail = fabro_sandbox::default_redacted_output_tail(&e);
        services.emitter.notice_with_tail(
            RunNoticeLevel::Warn,
            RunNoticeCode::SandboxCleanupFailed,
            format!("sandbox stop failed: {}", e.display_with_causes()),
            exec_output_tail,
        );
    }

    Ok(Finalized {
        run_id: run_options.run_id,
        outcome,
        conclusion,
        pushed_branch,
        pr_url,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    use anyhow::Result;
    use fabro_auth::test_support as auth_test_support;
    use fabro_graphviz::graph::Graph;
    use fabro_model::Catalog;
    use fabro_sandbox::test_support::MockSandbox;
    use fabro_store::{Database, RunDatabase, RunProjection};
    use fabro_types::{
        BilledTokenCounts, EventBody, RunEvent, RunId, RunSpec, StageCompletion, WorkflowSettings,
        first_event_seq, fixtures, test_support,
    };
    use object_store::memory::InMemory;

    use super::*;
    use crate::context::Context;
    use crate::error::ErrorStage;
    use crate::event::{Emitter, StoreProgressLogger, append_event};
    use crate::records::Checkpoint;
    use crate::run_options::{GitCheckpointOptions, RunOptions};
    use crate::runtime_store::RunStoreHandle;
    use crate::sandbox_git_runtime::SandboxGitRuntime;
    use crate::services::EngineServices;

    fn test_run_id() -> RunId {
        fixtures::RUN_1
    }

    fn test_run_options(run_dir: &std::path::Path) -> RunOptions {
        RunOptions {
            settings:         WorkflowSettings::default(),
            run_dir:          run_dir.to_path_buf(),
            cancel_token:     tokio_util::sync::CancellationToken::new(),
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

    fn test_executed(
        graph: Graph,
        outcome: Result<Outcome, Error>,
        run_options: RunOptions,
        wall_time_ms: u64,
        services: Arc<RunServices>,
    ) -> Executed {
        let mut engine = EngineServices::test_default();
        engine.run = services;
        Executed {
            graph,
            outcome,
            run_options,
            wall_time_ms,
            final_context: Context::new(),
            engine: Arc::new(engine),
            model: "test-model".to_string(),
        }
    }

    async fn finalize_executed(
        executed: Executed,
        options: &FinalizeOptions,
    ) -> Result<Finalized, Error> {
        let concluded = conclude(executed, options).await?;
        let published = crate::pipeline::publish(concluded, &crate::pipeline::PublishOptions {
            pr_config:  None,
            github_app: None,
            origin_url: None,
            model:      "test-model".to_string(),
        })
        .await;
        finalize(published, options).await
    }

    fn test_store() -> Arc<Database> {
        Arc::new(fabro_store::test_support::test_database(
            Arc::new(InMemory::new()),
            "",
            Duration::from_millis(1),
            None,
        ))
    }

    async fn seeded_run_store() -> RunDatabase {
        let run_store = test_store().create_run(&test_run_id()).await.unwrap();
        append_event(&run_store, &test_run_id(), &Event::RunCreated {
            run_id:              test_run_id(),
            title:               None,
            settings:            serde_json::to_value(WorkflowSettings::default()).unwrap(),
            graph:               serde_json::to_value(fabro_types::Graph::new("checkpoint"))
                .unwrap(),
            workflow_source:     None,
            labels:              std::collections::BTreeMap::new(),
            source_directory:    Some("/tmp/project".to_string()),
            workflow_slug:       Some("checkpoint".to_string()),
            workflow_version_id: None,
            target:              None,
            automation:          None,
            provenance:          test_support::test_run_provenance(),
            manifest_blob:       None,
            spec_blob:           None,
            git:                 None,
            fork_source_ref:     None,
            retried_from:        None,
            parent_id:           None,
            web_url:             None,
        })
        .await
        .unwrap();
        run_store
    }

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

    fn test_projection() -> RunProjection {
        RunProjection::new(
            "Test run".to_string(),
            RunSpec {
                run_id:              test_run_id(),
                settings:            WorkflowSettings::default(),
                graph:               Graph::new("test"),
                graph_source:        None,
                workflow_slug:       None,
                workflow_version_id: None,
                target:              None,
                automation:          None,
                source_directory:    None,
                labels:              HashMap::new(),
                provenance:          test_support::test_run_provenance(),
                manifest_blob:       None,
                definition_blob:     None,
                spec_blob:           None,
                git:                 None,
                fork_source_ref:     None,
            },
            chrono::Utc::now(),
        )
    }

    use crate::test_support::test_usage;

    #[test]
    fn publish_error_builds_publish_failed_terminal_event() {
        let event = build_terminal_event(
            &Err(Error::publish("GitHub rejected pull request creation")),
            fabro_types::RunTiming::wall_only(10),
            0,
            Some("final-sha".to_string()),
            Some("diff".to_string()),
            None,
            None,
        );

        match event {
            Event::WorkflowRunFailed { failure, .. } => {
                assert_eq!(failure.reason, FailureReason::PublishFailed);
            }
            other => panic!("expected run failure, got {other:?}"),
        }
    }

    #[test]
    fn conclusion_stage_order_follows_projection_first_event_order() {
        let mut projection = test_projection();
        projection.stage_entry("zebra", 1, first_event_seq(1));
        projection.stage_entry("apple", 1, first_event_seq(2));
        let checkpoint = checkpoint_with(
            vec!["apple", "zebra"],
            HashMap::from([
                ("apple".to_string(), Outcome::success()),
                ("zebra".to_string(), Outcome::success()),
            ]),
        );

        projection.checkpoints.push(fabro_types::CheckpointRecord {
            seq: 10,
            checkpoint,
            diff: fabro_types::RunDiff::default(),
        });
        let conclusion = build_conclusion_from_projection(
            Some(&projection),
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
        let mut projection = test_projection();
        projection.stage_entry("skipped", 1, first_event_seq(4));
        projection.stage_entry("finished", 1, first_event_seq(5));
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

        projection.checkpoints.push(fabro_types::CheckpointRecord {
            seq: 10,
            checkpoint,
            diff: fabro_types::RunDiff::default(),
        });
        let conclusion = build_conclusion_from_projection(
            Some(&projection),
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

    #[test]
    fn conclusion_billing_sums_retry_visit_usage_from_projection() {
        let mut projection = test_projection();
        let failed_usage = test_usage("gpt-old", 100, 10);
        let success_usage = test_usage("gpt-new", 200, 20);
        let failed = projection.stage_entry("verify", 1, first_event_seq(1));
        failed.timing = Some(fabro_types::StageTiming::wall_only(1200));
        failed.usage = BilledTokenCounts::from_billed_usage(std::slice::from_ref(&failed_usage));
        failed.model = Some(failed_usage.model().clone());
        failed.completion = Some(StageCompletion {
            outcome:        StageOutcome::Failed {
                retry_requested: true,
            },
            notes:          None,
            failure_reason: Some("try again".to_string()),
            timestamp:      chrono::Utc::now(),
        });
        let succeeded = projection.stage_entry("verify", 2, first_event_seq(2));
        succeeded.timing = Some(fabro_types::StageTiming::wall_only(800));
        succeeded.usage =
            BilledTokenCounts::from_billed_usage(std::slice::from_ref(&success_usage));
        succeeded.model = Some(success_usage.model().clone());
        succeeded.completion = Some(StageCompletion {
            outcome:        StageOutcome::Succeeded,
            notes:          None,
            failure_reason: None,
            timestamp:      chrono::Utc::now(),
        });

        let mut latest_outcome = Outcome::success();
        latest_outcome.usage = Some(success_usage);
        latest_outcome.timing = Some(fabro_types::StageTiming::wall_only(800));
        let mut checkpoint = checkpoint_with(
            vec!["verify", "verify"],
            HashMap::from([("verify".to_string(), latest_outcome)]),
        );
        checkpoint.node_retries.insert("verify".to_string(), 2);

        projection.checkpoints.push(fabro_types::CheckpointRecord {
            seq: 10,
            checkpoint,
            diff: fabro_types::RunDiff::default(),
        });
        let conclusion = build_conclusion_from_projection(
            Some(&projection),
            StageOutcome::Succeeded,
            None,
            10,
            None,
        );

        assert_eq!(conclusion.billing.as_ref().unwrap().input_tokens, 300);
        assert_eq!(conclusion.billing.as_ref().unwrap().output_tokens, 30);
        assert_eq!(
            conclusion.billing.as_ref().unwrap().total_usd_micros,
            Some(330)
        );
        assert_eq!(conclusion.stages.len(), 1);
        assert_eq!(conclusion.stages[0].stage_id, "verify");
        assert_eq!(conclusion.stages[0].timing.wall_time_ms, 2000);
        assert_eq!(conclusion.stages[0].billing_usd_micros, Some(330));
        assert_eq!(conclusion.stages[0].retries, 1);
    }

    fn test_services(
        run_store: RunStoreHandle,
        emitter: Arc<Emitter>,
        sandbox: Arc<dyn fabro_agent::Sandbox>,
    ) -> Arc<RunServices> {
        let locations = crate::services::RunLocations::for_sandbox(
            None,
            sandbox.as_ref(),
            Path::new(".").to_path_buf(),
        );
        RunServices::new(
            run_store,
            emitter,
            sandbox,
            None,
            locations,
            tokio_util::sync::CancellationToken::new(),
            fabro_model::ProviderId::anthropic(),
            "claude-sonnet-4-6".to_string(),
            auth_test_support::vault_only_credential_source(),
            Arc::new(Catalog::from_builtin().expect("default catalog should build")),
            Arc::new(SandboxGitRuntime::new()),
            crate::stage_execution::StageExecutionTracker::default(),
        )
    }

    #[tokio::test]
    async fn finalize_persists_conclusion_in_projection() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let run_store = seeded_run_store().await;
        crate::test_support::mark_run_running(&run_store, &test_run_id()).await;
        let emitter = Arc::new(Emitter::new(test_run_id()));
        let store_logger = StoreProgressLogger::new(run_store.clone());
        store_logger.register(&emitter);
        let sandbox: Arc<dyn fabro_agent::Sandbox> = Arc::new(fabro_agent::LocalSandbox::new(
            std::env::current_dir().unwrap(),
        ));
        let locations =
            crate::services::RunLocations::for_sandbox(None, sandbox.as_ref(), run_dir.clone());
        let services = RunServices::new(
            run_store.clone().into(),
            Arc::clone(&emitter),
            sandbox,
            None,
            locations,
            tokio_util::sync::CancellationToken::new(),
            fabro_model::ProviderId::anthropic(),
            "claude-sonnet-4-6".to_string(),
            auth_test_support::vault_only_credential_source(),
            Arc::new(Catalog::from_builtin().expect("default catalog should build")),
            Arc::new(SandboxGitRuntime::new()),
            crate::stage_execution::StageExecutionTracker::default(),
        );
        let executed = test_executed(
            Graph::new("test"),
            Ok(Outcome::success()),
            test_run_options(&run_dir),
            5,
            services,
        );

        let concluded = finalize_executed(executed, &FinalizeOptions {
            run_dir:          run_dir.clone(),
            run_id:           test_run_id(),
            workflow_name:    "test".to_string(),
            preserve_sandbox: true,
            stop_on_terminal: true,
            last_git_sha:     None,
        })
        .await
        .unwrap();
        store_logger.flush().await.unwrap();

        assert_eq!(concluded.conclusion.status, StageOutcome::Succeeded);
    }

    #[tokio::test]
    async fn configured_run_branch_without_remote_is_not_reported_as_pushed() {
        let repo_dir = tempfile::tempdir().unwrap();
        let emitter = Arc::new(Emitter::new(test_run_id()));
        let events = record_events(&emitter);
        let services = test_services(
            RunStoreHandle::local(seeded_run_store().await),
            emitter,
            Arc::new(MockSandbox::linux()),
        );
        let mut run_options = test_run_options(repo_dir.path());
        run_options.git = Some(GitCheckpointOptions {
            base_sha:   None,
            run_branch: Some("fabro/run/test".to_string()),
        });
        let executed = test_executed(
            Graph::new("test"),
            Ok(Outcome::success()),
            run_options,
            5,
            services,
        );
        let options = FinalizeOptions {
            run_dir:          repo_dir.path().to_path_buf(),
            run_id:           test_run_id(),
            workflow_name:    "test".to_string(),
            preserve_sandbox: false,
            stop_on_terminal: true,
            last_git_sha:     Some("final-sha".to_string()),
        };
        let concluded = conclude(executed, &options).await.unwrap();
        let published = crate::pipeline::publish(concluded, &crate::pipeline::PublishOptions {
            pr_config:  None,
            github_app: None,
            origin_url: None,
            model:      "test-model".to_string(),
        })
        .await;

        assert_eq!(published.publish_outcome, PublishOutcome::default());
        assert!(published.publish_error.is_none());
        let finalized = finalize(published, &options).await.unwrap();

        assert!(finalized.outcome.is_ok());
        assert_eq!(finalized.pushed_branch, None);
        let events = events.lock().unwrap();
        let names = events.iter().map(RunEvent::event_name).collect::<Vec<_>>();
        assert_eq!(names, vec!["run.completed"]);
    }

    #[tokio::test]
    async fn final_push_failure_becomes_terminal_publish_failure() {
        let repo_dir = tempfile::tempdir().unwrap();
        let sandbox = Arc::new(MockSandbox::linux());
        let emitter = Arc::new(Emitter::new(test_run_id()));
        let events = record_events(&emitter);
        let services = test_services(
            RunStoreHandle::local(seeded_run_store().await),
            emitter,
            sandbox,
        );
        let mut run_options = test_run_options(repo_dir.path());
        run_options.git = Some(GitCheckpointOptions {
            base_sha:   None,
            run_branch: Some("fabro/run/test".to_string()),
        });
        let executed = test_executed(
            Graph::new("test"),
            Ok(Outcome::success()),
            run_options,
            5,
            services,
        );
        let options = FinalizeOptions {
            run_dir:          repo_dir.path().to_path_buf(),
            run_id:           test_run_id(),
            workflow_name:    "test".to_string(),
            preserve_sandbox: false,
            stop_on_terminal: true,
            last_git_sha:     Some("final-sha".to_string()),
        };
        let concluded = conclude(executed, &options).await.unwrap();
        let published = crate::pipeline::publish(concluded, &crate::pipeline::PublishOptions {
            pr_config:  None,
            github_app: None,
            origin_url: Some("https://github.com/owner/repo.git".to_string()),
            model:      "test-model".to_string(),
        })
        .await;

        assert!(matches!(
            &published.publish_error,
            Some(Error::Stage {
                stage: ErrorStage::Publish,
                ..
            })
        ));
        let finalized = finalize(published, &options).await.unwrap();

        assert!(matches!(
            finalized.outcome,
            Err(Error::Stage {
                stage: ErrorStage::Publish,
                ..
            })
        ));
        assert_eq!(
            finalized
                .conclusion
                .failure
                .as_ref()
                .map(|failure| failure.reason),
            Some(FailureReason::PublishFailed)
        );
        let events = events.lock().unwrap();
        let names = events.iter().map(RunEvent::event_name).collect::<Vec<_>>();
        // Exactly one durable git.push event per high-level push — retries
        // nest inside it as attempts, never as extra events.
        assert_eq!(names, vec!["git.push", "run.failed"]);
        match &events.first().unwrap().body {
            EventBody::GitPush(props) => {
                assert!(!props.success);
                // MockSandbox's default git_push_ref fails before any attempt
                // runs, so the nested history is empty here.
                assert!(props.attempts.is_empty());
            }
            other => panic!("expected git.push, got {other:?}"),
        }
        match &events.last().unwrap().body {
            EventBody::RunFailed(props) => {
                assert_eq!(props.failure.reason, FailureReason::PublishFailed);
            }
            other => panic!("expected run.failed, got {other:?}"),
        }
    }

    /// An empty diff means there is nothing to open a pull request for. The
    /// branch still gets pushed and the run still succeeds.
    #[tokio::test]
    async fn empty_diff_pushes_branch_without_opening_pull_request() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_git_repo(repo_dir.path());
        let emitter = Arc::new(Emitter::new(test_run_id()));
        let events = record_events(&emitter);
        let services = test_services(
            RunStoreHandle::local(seeded_run_store().await),
            emitter,
            Arc::new(fabro_agent::LocalSandbox::new(
                repo_dir.path().to_path_buf(),
            )),
        );
        let mut run_options = test_run_options(repo_dir.path());
        run_options.base_branch = Some("main".to_string());
        run_options.git = Some(GitCheckpointOptions {
            base_sha:   None,
            run_branch: Some("fabro/run/test".to_string()),
        });
        let executed = test_executed(
            Graph::new("test"),
            Ok(Outcome::success()),
            run_options,
            5,
            services,
        );
        let options = FinalizeOptions {
            run_dir:          repo_dir.path().to_path_buf(),
            run_id:           test_run_id(),
            workflow_name:    "test".to_string(),
            preserve_sandbox: false,
            stop_on_terminal: true,
            last_git_sha:     Some("final-sha".to_string()),
        };
        let mut concluded = conclude(executed, &options).await.unwrap();
        concluded.conclusion.diff.patch = None;
        let published = crate::pipeline::publish(concluded, &crate::pipeline::PublishOptions {
            pr_config:  Some(fabro_types::settings::run::PullRequestSettings {
                enabled:        true,
                draft:          true,
                auto_merge:     false,
                merge_strategy: fabro_types::settings::run::MergeStrategy::Squash,
            }),
            github_app: None,
            origin_url: Some("https://github.com/owner/repo.git".to_string()),
            model:      "test-model".to_string(),
        })
        .await;

        assert!(published.publish_error.is_none());
        let finalized = finalize(published, &options).await.unwrap();

        assert!(finalized.outcome.is_ok());
        assert_eq!(finalized.pushed_branch.as_deref(), Some("fabro/run/test"));
        assert_eq!(finalized.pr_url, None);
        let events = events.lock().unwrap();
        let names = events.iter().map(RunEvent::event_name).collect::<Vec<_>>();
        assert_eq!(names, vec!["git.push", "run.completed"]);
    }

    /// `base_sha` is where the run started, not what it produced. Reporting it
    /// as the final commit would both mis-state a durable field and make the
    /// remote-head check reject a branch that was pushed correctly.
    #[tokio::test]
    async fn untracked_final_commit_does_not_fall_back_to_base_sha() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_git_repo(repo_dir.path());
        let emitter = Arc::new(Emitter::new(test_run_id()));
        let services = test_services(
            RunStoreHandle::local(seeded_run_store().await),
            emitter,
            Arc::new(fabro_agent::LocalSandbox::new(
                repo_dir.path().to_path_buf(),
            )),
        );
        let mut run_options = test_run_options(repo_dir.path());
        run_options.base_branch = Some("main".to_string());
        run_options.git = Some(GitCheckpointOptions {
            base_sha:   Some("base-sha".to_string()),
            run_branch: Some("fabro/run/test".to_string()),
        });
        let executed = test_executed(
            Graph::new("test"),
            Ok(Outcome::success()),
            run_options,
            5,
            services,
        );
        let options = FinalizeOptions {
            run_dir:          repo_dir.path().to_path_buf(),
            run_id:           test_run_id(),
            workflow_name:    "test".to_string(),
            preserve_sandbox: false,
            stop_on_terminal: true,
            last_git_sha:     None,
        };
        let mut concluded = conclude(executed, &options).await.unwrap();

        assert_eq!(concluded.conclusion.final_git_commit_sha, None);

        // No pull request wanted, so publish still pushes the branch and the
        // run succeeds without needing a commit SHA at all.
        concluded.conclusion.diff.patch = None;
        let published = crate::pipeline::publish(concluded, &crate::pipeline::PublishOptions {
            pr_config:  None,
            github_app: None,
            origin_url: Some("https://github.com/owner/repo.git".to_string()),
            model:      "test-model".to_string(),
        })
        .await;
        let finalized = finalize(published, &options).await.unwrap();

        assert!(finalized.outcome.is_ok());
        assert_eq!(finalized.pushed_branch.as_deref(), Some("fabro/run/test"));
        assert_eq!(finalized.conclusion.final_git_commit_sha, None);
    }

    #[tokio::test]
    async fn pull_request_failure_precedes_terminal_publish_failure() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_git_repo(repo_dir.path());
        let emitter = Arc::new(Emitter::new(test_run_id()));
        let events = record_events(&emitter);
        let services = test_services(
            RunStoreHandle::local(seeded_run_store().await),
            emitter,
            Arc::new(fabro_agent::LocalSandbox::new(
                repo_dir.path().to_path_buf(),
            )),
        );
        let mut run_options = test_run_options(repo_dir.path());
        run_options.base_branch = Some("main".to_string());
        run_options.git = Some(GitCheckpointOptions {
            base_sha:   None,
            run_branch: Some("fabro/run/test".to_string()),
        });
        let executed = test_executed(
            Graph::new("test"),
            Ok(Outcome::success()),
            run_options,
            5,
            services,
        );
        let options = FinalizeOptions {
            run_dir:          repo_dir.path().to_path_buf(),
            run_id:           test_run_id(),
            workflow_name:    "test".to_string(),
            preserve_sandbox: false,
            stop_on_terminal: true,
            last_git_sha:     Some("final-sha".to_string()),
        };
        let mut concluded = conclude(executed, &options).await.unwrap();
        concluded.conclusion.diff.patch =
            Some("diff --git a/a b/a\n+published change\n".to_string());
        let published = crate::pipeline::publish(concluded, &crate::pipeline::PublishOptions {
            pr_config:  Some(fabro_types::settings::run::PullRequestSettings {
                enabled:        true,
                draft:          true,
                auto_merge:     false,
                merge_strategy: fabro_types::settings::run::MergeStrategy::Squash,
            }),
            github_app: None,
            origin_url: Some("https://github.com/owner/repo.git".to_string()),
            model:      "test-model".to_string(),
        })
        .await;
        let finalized = finalize(published, &options).await.unwrap();

        assert!(matches!(
            finalized.outcome,
            Err(Error::Stage {
                stage: ErrorStage::Publish,
                ..
            })
        ));
        // The push landed before the pull request failed, so the branch is
        // still reported — that is exactly the run where the user needs it.
        assert_eq!(finalized.pushed_branch.as_deref(), Some("fabro/run/test"));
        let events = events.lock().unwrap();
        let names = events.iter().map(RunEvent::event_name).collect::<Vec<_>>();
        assert_eq!(names, vec!["git.push", "pull_request.failed", "run.failed"]);
        match &events.last().unwrap().body {
            EventBody::RunFailed(props) => {
                assert_eq!(props.failure.reason, FailureReason::PublishFailed);
            }
            other => panic!("expected run.failed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn finalize_stops_sandbox_on_terminal_without_deleting() {
        let repo_dir = tempfile::tempdir().unwrap();
        let sandbox = Arc::new(MockSandbox::linux());
        let services = test_services(
            RunStoreHandle::local(seeded_run_store().await),
            Arc::new(Emitter::new(test_run_id())),
            sandbox.clone(),
        );
        let executed = test_executed(
            Graph::new("test"),
            Ok(Outcome::success()),
            test_run_options(repo_dir.path()),
            5,
            services,
        );

        finalize_executed(executed, &FinalizeOptions {
            run_dir:          repo_dir.path().to_path_buf(),
            run_id:           test_run_id(),
            workflow_name:    "test".to_string(),
            preserve_sandbox: false,
            stop_on_terminal: true,
            last_git_sha:     None,
        })
        .await
        .unwrap();

        assert_eq!(sandbox.stop_count(), 1);
        assert_eq!(sandbox.delete_count(), 0);
    }

    #[tokio::test]
    async fn finalize_leaves_sandbox_running_when_stop_on_terminal_is_false() {
        let repo_dir = tempfile::tempdir().unwrap();
        let sandbox = Arc::new(MockSandbox::linux());
        let services = test_services(
            RunStoreHandle::local(seeded_run_store().await),
            Arc::new(Emitter::new(test_run_id())),
            sandbox.clone(),
        );
        let executed = test_executed(
            Graph::new("test"),
            Ok(Outcome::success()),
            test_run_options(repo_dir.path()),
            5,
            services,
        );

        finalize_executed(executed, &FinalizeOptions {
            run_dir:          repo_dir.path().to_path_buf(),
            run_id:           test_run_id(),
            workflow_name:    "test".to_string(),
            preserve_sandbox: false,
            stop_on_terminal: false,
            last_git_sha:     None,
        })
        .await
        .unwrap();

        assert_eq!(sandbox.stop_count(), 0);
        assert_eq!(sandbox.delete_count(), 0);
    }

    #[tokio::test]
    async fn finalize_terminal_event_includes_diff_summary() {
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = repo_dir.path();
        init_git_repo(repo);
        tokio::fs::write(repo.join("notes.txt"), "one\n")
            .await
            .unwrap();
        let base = git_commit_all(repo, "base");
        tokio::fs::write(repo.join("notes.txt"), "one\ntwo\nthree\n")
            .await
            .unwrap();
        let head = git_commit_all(repo, "head");

        let run_store = seeded_run_store().await;
        let emitter = Arc::new(Emitter::new(test_run_id()));
        let events = record_events(&emitter);
        let services = test_services(
            RunStoreHandle::local(run_store),
            Arc::clone(&emitter),
            Arc::new(fabro_agent::LocalSandbox::new(repo.to_path_buf())),
        );
        let mut run_options = test_run_options(repo);
        run_options.git = Some(GitCheckpointOptions {
            base_sha:   Some(base),
            run_branch: None,
        });
        let executed = test_executed(
            Graph::new("test"),
            Ok(Outcome::success()),
            run_options,
            5,
            services,
        );

        finalize_executed(executed, &FinalizeOptions {
            run_dir:          repo.to_path_buf(),
            run_id:           test_run_id(),
            workflow_name:    "test".to_string(),
            preserve_sandbox: true,
            stop_on_terminal: true,
            last_git_sha:     Some(head),
        })
        .await
        .unwrap();

        let events = events.lock().unwrap();
        let run_completed = events
            .iter()
            .find(|event| event.event_name() == "run.completed")
            .expect("run.completed event");
        let properties = run_completed.properties().unwrap();
        assert_eq!(
            properties["diff_summary"],
            serde_json::json!({
                "files_changed": 1,
                "additions": 2,
                "deletions": 0
            })
        );
    }
}
