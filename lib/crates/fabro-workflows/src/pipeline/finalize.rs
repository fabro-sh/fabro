use std::path::Path;
use std::sync::Arc;

use crate::checkpoint::Checkpoint;
use crate::conclusion::Conclusion;
use crate::error::FabroError;
use crate::event::{EventEmitter, RunNoticeLevel, WorkflowRunEvent};
use crate::outcome::{Outcome, OutcomeExt, StageStatus};
use crate::run_settings::RunSettings;
use crate::run_status::{RunStatus, StatusReason};
use fabro_hooks::{HookContext, HookEvent, HookRunner};

use super::types::{FinalizeOptions, Finalized, Retroed};

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

pub fn build_conclusion(
    run_dir: &Path,
    status: StageStatus,
    failure_reason: Option<String>,
    run_duration_ms: u64,
    final_git_commit_sha: Option<String>,
) -> Conclusion {
    let checkpoint = Checkpoint::load(&run_dir.join("checkpoint.json")).ok();
    let stage_durations = fabro_retro::retro::extract_stage_durations(run_dir);

    let mut total_input_tokens: i64 = 0;
    let mut total_output_tokens: i64 = 0;
    let mut total_cache_read_tokens: i64 = 0;
    let mut total_cache_write_tokens: i64 = 0;
    let mut total_reasoning_tokens: i64 = 0;
    let mut has_pricing = false;

    let (stages, total_cost, total_retries) = if let Some(ref cp) = checkpoint {
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

            stages.push(crate::conclusion::StageSummary {
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

pub fn persist_terminal_outcome(
    run_dir: &Path,
    conclusion: &Conclusion,
    run_status: RunStatus,
    status_reason: Option<StatusReason>,
) {
    let _ = conclusion.save(&run_dir.join("conclusion.json"));
    crate::run_status::write_run_status(run_dir, run_status, status_reason);
}

/// Write a finalize commit to the shadow branch with retro.json and final node files.
///
/// This captures the last diff.patch (written after the final checkpoint) and retro.json.
/// Best-effort: errors are logged as warnings.
pub async fn write_finalize_commit(config: &RunSettings, run_dir: &Path) {
    let (Some(meta_branch), Some(repo_path)) = (
        config.git.as_ref().and_then(|g| g.meta_branch.as_ref()),
        config.host_repo_path.as_ref(),
    ) else {
        return;
    };

    let store = crate::git::MetadataStore::new(repo_path, &config.git_author);
    let mut entries = crate::git::scan_node_files(run_dir);
    if let Ok(retro_bytes) = std::fs::read(run_dir.join("retro.json")) {
        entries.push(("retro.json".to_string(), retro_bytes));
    }
    let refs: Vec<(&str, &[u8])> = entries
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_slice()))
        .collect();
    if let Err(e) = store.write_files(&config.run_id, &refs, "finalize run") {
        tracing::warn!(error = %e, "Failed to write finalize commit to metadata branch");
        return;
    }

    let refspec = format!("refs/heads/{meta_branch}");
    crate::sandbox_git::git_push_host(repo_path, &refspec, &config.github_app, "finalize metadata")
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
    run_id: &str,
    workflow_name: &str,
    preserve: bool,
) -> std::result::Result<(), String> {
    let hook_ctx = HookContext::new(
        HookEvent::SandboxCleanup,
        run_id.to_string(),
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
) -> Result<Finalized, FabroError> {
    let Retroed {
        graph,
        outcome,
        settings,
        hook_runner,
        emitter,
        sandbox,
        duration_ms,
        retro: _,
    } = retroed;

    let (final_status, failure_reason, run_status, status_reason) =
        classify_engine_result(&outcome);
    let conclusion = build_conclusion(
        &options.run_dir,
        final_status,
        failure_reason,
        duration_ms,
        options.last_git_sha.clone(),
    );

    write_finalize_commit(&settings, &options.run_dir).await;

    let mut pr_url = None;
    if let Some(pr_cfg) = &options.pr_config {
        if let Err(ref e) = outcome {
            tracing::debug!(error = %e, "Skipping PR creation: engine returned an error");
        } else if let Ok(ref result) = outcome {
            if matches!(
                result.status,
                StageStatus::Success | StageStatus::PartialSuccess
            ) {
                let diff = tokio::fs::read_to_string(options.run_dir.join("final.patch"))
                    .await
                    .unwrap_or_default();
                if let (
                    Some(ref base_branch),
                    Some(ref run_branch),
                    Some(ref creds),
                    Some(ref origin),
                ) = (
                    &settings.base_branch,
                    settings.git.as_ref().and_then(|g| g.run_branch.as_ref()),
                    &options.github_app,
                    &options.origin_url,
                ) {
                    let auto_merge = if pr_cfg.auto_merge {
                        Some(crate::pull_request::AutoMergeConfig {
                            merge_strategy: pr_cfg.merge_strategy,
                        })
                    } else {
                        None
                    };

                    match crate::pull_request::maybe_open_pull_request(
                        creds,
                        origin,
                        base_branch,
                        run_branch,
                        graph.goal(),
                        &diff,
                        &options.model,
                        pr_cfg.draft,
                        auto_merge,
                        &options.run_dir,
                    )
                    .await
                    {
                        Ok(Some(record)) => {
                            emitter.emit(&WorkflowRunEvent::PullRequestCreated {
                                pr_url: record.html_url.clone(),
                                pr_number: record.number,
                                draft: pr_cfg.draft,
                            });
                            pr_url = Some(record.html_url.clone());
                            if let Err(e) = record.save(&options.run_dir.join("pull_request.json"))
                            {
                                tracing::warn!(error = %e, "Failed to save pull_request.json");
                            }
                        }
                        Ok(None) => {}
                        Err(e) => {
                            emitter.emit(&WorkflowRunEvent::PullRequestFailed {
                                error: e.to_string(),
                            });
                            emit_run_notice(
                                &emitter,
                                RunNoticeLevel::Warn,
                                "pull_request_failed",
                                format!("PR creation failed: {e}"),
                            );
                        }
                    }
                }
            }
        }
    }

    if options.preserve_sandbox {
        let info = sandbox.sandbox_info();
        if !info.is_empty() {
            emit_run_notice(
                &emitter,
                RunNoticeLevel::Info,
                "sandbox_preserved",
                format!("sandbox preserved: {info}"),
            );
        } else {
            emit_run_notice(
                &emitter,
                RunNoticeLevel::Info,
                "sandbox_preserved",
                "sandbox preserved",
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

    persist_terminal_outcome(&options.run_dir, &conclusion, run_status, status_reason);

    Ok(Finalized {
        run_id: settings.run_id,
        outcome,
        conclusion,
        pr_url,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use fabro_config::config::FabroConfig;
    use fabro_graphviz::graph::Graph;

    use super::*;
    use crate::pipeline::types::Retroed;
    use crate::run_settings::RunSettings;

    fn test_settings(run_dir: &std::path::Path) -> RunSettings {
        RunSettings {
            config: FabroConfig::default(),
            run_dir: run_dir.to_path_buf(),
            cancel_token: None,
            dry_run: true,
            run_id: "run-test".to_string(),
            labels: HashMap::new(),
            git_author: crate::git::GitAuthor::default(),
            workflow_slug: None,
            github_app: None,
            host_repo_path: None,
            base_branch: None,
            git: None,
        }
    }

    #[tokio::test]
    async fn finalize_writes_conclusion_json() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let retroed = Retroed {
            graph: Graph::new("test"),
            outcome: Ok(Outcome::success()),
            settings: test_settings(&run_dir),
            hook_runner: None,
            emitter: Arc::new(EventEmitter::new()),
            sandbox: Arc::new(fabro_agent::LocalSandbox::new(
                std::env::current_dir().unwrap(),
            )),
            duration_ms: 5,
            retro: None,
        };

        let finalized = finalize(
            retroed,
            &FinalizeOptions {
                run_dir: run_dir.clone(),
                run_id: "run-test".to_string(),
                workflow_name: "test".to_string(),
                hook_runner: None,
                preserve_sandbox: true,
                pr_config: None,
                github_app: None,
                origin_url: None,
                model: "test-model".to_string(),
                last_git_sha: None,
            },
        )
        .await
        .unwrap();

        assert!(run_dir.join("conclusion.json").exists());
        assert_eq!(finalized.conclusion.status, StageStatus::Success);
    }
}
