use fabro_github::{self as github_app, GitHubCredentials, ssh_url_to_https};
use fabro_graphviz::parser;
use fabro_llm::generate::{GenerateParams, generate};
use fabro_retro::retro::Retro;
use fabro_store::RunProjection;
use fabro_types::PullRequestRecord;
use fabro_types::settings::run::MergeStrategy;
use fabro_util::text::strip_goal_decoration;
use tracing::{debug, info};

use super::types::{Concluded, Finalized, PullRequestOptions};
use crate::event::{Emitter, Event, RunNoticeLevel};
use crate::outcome::{StageStatus, format_cost as outcome_format_cost};
use crate::records::{Conclusion, RunRecord};
use crate::runtime_store::RunStoreHandle;

/// Derive a PR title from the workflow goal.
///
/// Uses the first line, truncated to 120 characters for readability.
fn pr_title_from_goal(goal: &str) -> String {
    let stripped = strip_goal_decoration(goal);
    if stripped.chars().count() > 120 {
        let truncated: String = stripped.chars().take(119).collect();
        format!("{truncated}…")
    } else {
        stripped.to_string()
    }
}

/// Truncate a PR body to fit GitHub's 65,536 character limit.
fn truncate_pr_body(body: &str) -> String {
    const MAX_BODY: usize = 65_536;
    const SUFFIX: &str = "\n\n_(truncated)_";
    if body.len() <= MAX_BODY {
        return body.to_string();
    }
    let cutoff = body.floor_char_boundary(MAX_BODY - SUFFIX.len());
    format!("{}{SUFFIX}", &body[..cutoff])
}

/// Format an optional cost as `$X.XX` or an en-dash when absent.
fn format_cost(cost_usd_micros: Option<i64>) -> String {
    cost_usd_micros
        .map(|value| value as f64 / 1_000_000.0)
        .map_or_else(|| "\u{2013}".to_string(), outcome_format_cost)
}

/// Format a duration in milliseconds as a human-readable string.
fn format_duration_ms(ms: u64) -> String {
    let secs = ms / 1000;
    if secs >= 60 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

/// Format the Retro section of the PR body.
///
/// Renders stats, friction points, and open items. Omits sub-sections when
/// empty.
fn format_retro_section(retro: &Retro) -> String {
    let mut parts = Vec::new();
    parts.push("### Retro".to_string());
    parts.push(String::new());

    // Stats summary
    parts.push(format!(
        "*   {} stages completed, {} failed, {} retries",
        retro.stats.stages_completed, retro.stats.stages_failed, retro.stats.total_retries
    ));
    parts.push(format!(
        "*   {} files modified",
        retro.stats.files_touched.len()
    ));

    // Friction points
    if let Some(ref fps) = retro.friction_points {
        if !fps.is_empty() {
            parts.push(String::new());
            parts.push("**Friction points:**".to_string());
            parts.push(String::new());
            for fp in fps {
                parts.push(format!("*   {}", fp.description));
            }
        }
    }

    // Open items
    if let Some(ref items) = retro.open_items {
        if !items.is_empty() {
            parts.push(String::new());
            parts.push("**Open items:**".to_string());
            parts.push(String::new());
            for item in items {
                parts.push(format!("*   {}", item.description));
            }
        }
    }

    parts.join("\n")
}

/// Format the Fabro Details section of the PR body.
///
/// Renders a cost/duration table in a collapsible `<details>` block, and
/// optionally a workflow graph summary in another `<details>` block.
fn format_arc_details_section(
    conclusion: &Conclusion,
    run_record: Option<&RunRecord>,
    dot_source: Option<&str>,
) -> String {
    let mut parts = Vec::new();
    parts.push("### Fabro Details".to_string());
    parts.push(String::new());

    // Cost table
    let total_duration = format_duration_ms(conclusion.duration_ms);
    let total_cost_str = format_cost(conclusion.billing.as_ref().and_then(|b| b.total_usd_micros));
    let stage_count = conclusion.stages.len();
    parts.push(format!(
        "<details>\n<summary>Ran {stage_count} {} in {total_duration} for {total_cost_str}</summary>",
        if stage_count == 1 { "stage" } else { "stages" }
    ));
    parts.push(String::new());

    parts.push("| Stage | Duration | Cost | Retries |".to_string());
    parts.push("|---|---|---|---|".to_string());
    for stage in &conclusion.stages {
        let dur = format_duration_ms(stage.duration_ms);
        let cost = format_cost(stage.billing_usd_micros);
        parts.push(format!(
            "| {} | {} | {} | {} |",
            stage.stage_label, dur, cost, stage.retries
        ));
    }
    // Total row
    let total_retries = conclusion.total_retries;
    parts.push(format!(
        "| **Total** | **{total_duration}** | **{total_cost_str}** | **{total_retries}** |"
    ));

    parts.push(String::new());
    parts.push("</details>".to_string());

    // Workflow graph summary — prefer RunRecord's graph, fall back to DOT parsing
    if let Some(record) = run_record {
        let workflow_name = if record.graph.name.is_empty() {
            "unnamed"
        } else {
            &record.graph.name
        };
        let graph_name = format!("{workflow_name}.fabro");
        let node_count = record.graph.nodes.len();
        let edge_count = record.graph.edges.len();

        parts.push(String::new());
        parts.push(format!(
            "<details>\n<summary>Ran <code>{graph_name}</code> ({node_count} {} and {edge_count} {})</summary>",
            if node_count == 1 { "node" } else { "nodes" },
            if edge_count == 1 { "edge" } else { "edges" }
        ));
        if let Some(dot) = dot_source {
            parts.push(String::new());
            parts.push("```dot".to_string());
            parts.push(dot.to_string());
            parts.push("```".to_string());
        }
        parts.push(String::new());
        parts.push("</details>".to_string());
    } else if let Some(dot) = dot_source {
        parts.push(String::new());

        // Extract graph name and count nodes/edges for the summary
        let (graph_name, node_count, edge_count) = parse_dot_summary(dot);

        parts.push(format!(
            "<details>\n<summary>Ran <code>{graph_name}</code> ({node_count} {} and {edge_count} {})</summary>",
            if node_count == 1 { "node" } else { "nodes" },
            if edge_count == 1 { "edge" } else { "edges" }
        ));
        parts.push(String::new());
        parts.push("```dot".to_string());
        parts.push(dot.to_string());
        parts.push("```".to_string());
        parts.push(String::new());
        parts.push("</details>".to_string());
    }

    parts.join("\n")
}

/// Parse a DOT source string to extract graph name, node count, and edge count.
fn parse_dot_summary(dot: &str) -> (String, usize, usize) {
    match parser::parse(dot) {
        Ok(graph) => (
            format!("{}.fabro", graph.name),
            graph.nodes.len(),
            graph.edges.len(),
        ),
        Err(_) => ("workflow.fabro".to_string(), 0, 0),
    }
}

/// Read plan text from the first `plan*` node response in run state.
///
/// Nodes are sorted alphabetically so `plan` is preferred over `planning`.
/// For repeated visits, earlier visits sort first to match the prior on-disk
/// directory scan behavior.
fn read_plan_text(state: &RunProjection) -> Option<String> {
    let mut plan_nodes = state
        .iter_nodes()
        .filter_map(|(stage_id, node)| {
            stage_id.node_id().starts_with("plan").then_some((
                stage_id.node_id(),
                stage_id.visit(),
                node.response.as_deref(),
            ))
        })
        .collect::<Vec<_>>();
    plan_nodes.sort_by(|left, right| left.0.cmp(right.0).then(left.1.cmp(&right.1)));
    for (node_id, visit, response) in plan_nodes {
        if let Some(response) = response {
            debug!(
                node_id,
                visit, "Found plan node response for PR body from run state"
            );
            return Some(response.to_string());
        }
    }
    None
}

/// Assemble the full PR body from LLM output and programmatic sections.
fn assemble_pr_body(
    llm_output: &str,
    plan_text: Option<&str>,
    retro_section: &str,
    arc_details_section: &str,
) -> String {
    let mut parts = Vec::new();

    parts.push(llm_output.to_string());

    if let Some(plan) = plan_text {
        parts.push(String::new());
        parts.push("<details>".to_string());
        parts.push("<summary>Full plan</summary>".to_string());
        parts.push(String::new());
        parts.push("````md".to_string());
        parts.push(plan.to_string());
        parts.push("````".to_string());
        parts.push(String::new());
        parts.push("</details>".to_string());
    }

    if !retro_section.is_empty() {
        parts.push(String::new());
        parts.push(retro_section.to_string());
    }

    if !arc_details_section.is_empty() {
        parts.push(String::new());
        parts.push(arc_details_section.to_string());
    }

    parts.push(String::new());
    parts.push("\u{2692}\u{fe0f} Generated with [Fabro](https://fabro.sh)".to_string());

    parts.join("\n")
}

fn emit_run_notice(
    emitter: &Emitter,
    level: RunNoticeLevel,
    code: impl Into<String>,
    message: impl Into<String>,
) {
    emitter.emit(&Event::RunNotice {
        level,
        code: code.into(),
        message: message.into(),
    });
}

async fn load_pull_request_diff(run_store: &RunStoreHandle) -> String {
    run_store
        .state()
        .await
        .inspect_err(|err| {
            tracing::warn!(error = %err, "Failed to load final patch from store for PR");
        })
        .ok()
        .and_then(|state| state.final_patch)
        .unwrap_or_default()
}

/// Build a complete PR body by combining LLM-generated narrative with
/// programmatic sections (plan, retro, fabro details).
pub async fn build_pr_body(
    diff: &str,
    goal: &str,
    model: &str,
    run_store: &RunStoreHandle,
    conclusion: Option<&Conclusion>,
) -> Result<String, String> {
    debug!("Building PR body");

    let loaded_conclusion = if conclusion.is_none() {
        run_store
            .state()
            .await
            .inspect_err(|err| {
                tracing::warn!(error = %err, "Failed to load conclusion from store for PR body");
            })
            .ok()
            .and_then(|state| state.conclusion)
    } else {
        None
    };
    let conclusion = conclusion.or(loaded_conclusion.as_ref());
    let run_state = run_store
        .state()
        .await
        .inspect_err(|err| {
            tracing::warn!(error = %err, "Failed to load run state from store for PR body");
        })
        .ok();
    let plan_text = run_state.as_ref().and_then(read_plan_text);
    let retro = run_state.as_ref().and_then(|state| state.retro.clone());
    let run_record = run_state.as_ref().and_then(|state| state.run.clone());
    let dot_source = run_state
        .as_ref()
        .and_then(|state| state.graph_source.clone());

    // Build LLM prompt
    let system = if plan_text.is_some() {
        "Write a PR description with: (1) 2-3 concise paragraphs explaining the change, then (2) a '### Plan Summary' section with bullet points summarizing the plan. Do not include a title. Do not include the full plan.".to_string()
    } else {
        "Write a concise PR description in 2-3 paragraphs explaining the change. Do not include a title.".to_string()
    };

    // Truncate diff to fit context windows (~50k chars)
    let max_diff_len = 50_000;
    let truncated_diff = if diff.len() > max_diff_len {
        &diff[..diff.floor_char_boundary(max_diff_len)]
    } else {
        diff
    };

    let prompt = if let Some(ref plan) = plan_text {
        // Truncate plan for LLM context (~20k chars)
        let max_plan_len = 20_000;
        let truncated_plan = if plan.len() > max_plan_len {
            &plan[..plan.floor_char_boundary(max_plan_len)]
        } else {
            plan.as_str()
        };
        format!(
            "Goal: {goal}\n\nPlan:\n```\n{truncated_plan}\n```\n\nDiff:\n```\n{truncated_diff}\n```"
        )
    } else {
        format!("Goal: {goal}\n\nDiff:\n```\n{truncated_diff}\n```")
    };

    let params = GenerateParams::new(model).system(system).prompt(prompt);

    let result = generate(params)
        .await
        .map_err(|e| format!("LLM generation failed: {e}"))?;

    let llm_output = result.response.text();

    let retro_section = retro.as_ref().map(format_retro_section).unwrap_or_default();
    let arc_details_section = conclusion
        .as_ref()
        .map(|c| format_arc_details_section(c, run_record.as_ref(), dot_source.as_deref()))
        .unwrap_or_default();

    let body = assemble_pr_body(
        &llm_output,
        plan_text.as_deref(),
        &retro_section,
        &arc_details_section,
    );

    info!("PR body generated");

    Ok(body)
}

/// Auto-merge configuration for a pull request.
pub struct AutoMergeOptions {
    pub merge_strategy: MergeStrategy,
}

/// Optionally open a pull request after a successful workflow run.
///
/// Returns `Ok(Some(PullRequestRecord))` if a PR was created, `Ok(None)` if
/// the diff was empty, or `Err` on failure.
#[allow(clippy::too_many_arguments)]
pub async fn maybe_open_pull_request(
    creds: &GitHubCredentials,
    origin_url: &str,
    base_branch: &str,
    head_branch: &str,
    goal: &str,
    diff: &str,
    model: &str,
    draft: bool,
    auto_merge: Option<AutoMergeOptions>,
    run_store: &RunStoreHandle,
    conclusion: Option<&Conclusion>,
) -> Result<Option<PullRequestRecord>, String> {
    if diff.is_empty() {
        debug!("Empty diff, skipping pull request creation");
        return Ok(None);
    }

    let https_url = ssh_url_to_https(origin_url);
    let (owner, repo) = github_app::parse_github_owner_repo(&https_url)?;

    let body = build_pr_body(diff, goal, model, run_store, conclusion).await?;
    let body = truncate_pr_body(&body);

    let title = pr_title_from_goal(goal);

    let created = github_app::create_pull_request(
        creds,
        &owner,
        &repo,
        base_branch,
        head_branch,
        &title,
        &body,
        draft,
        &github_app::github_api_base_url(),
    )
    .await?;

    info!(pr_url = %created.html_url, created.number, "Pull request created");

    if let Some(am_cfg) = auto_merge {
        let merge_method = match am_cfg.merge_strategy {
            MergeStrategy::Squash => github_app::AutoMergeMethod::Squash,
            MergeStrategy::Merge => github_app::AutoMergeMethod::Merge,
            MergeStrategy::Rebase => github_app::AutoMergeMethod::Rebase,
        };
        match github_app::enable_auto_merge(
            creds,
            &owner,
            &repo,
            &created.node_id,
            merge_method,
            &github_app::github_api_base_url(),
        )
        .await
        {
            Ok(()) => {
                info!(pr_number = created.number, "Auto-merge enabled");
            }
            Err(e) => {
                tracing::warn!(
                    pr_number = created.number,
                    error = %e,
                    "Failed to enable auto-merge (repo may not have auto-merge enabled in settings)"
                );
            }
        }
    }

    let record = PullRequestRecord {
        html_url: created.html_url,
        number: created.number,
        owner,
        repo,
        base_branch: base_branch.to_string(),
        head_branch: head_branch.to_string(),
        title,
    };

    Ok(Some(record))
}

/// PULL_REQUEST phase: optionally create a pull request after finalize.
///
/// This stage is infallible: failures are emitted and logged, but the pipeline
/// completes.
pub async fn pull_request(concluded: Concluded, options: &PullRequestOptions) -> Finalized {
    let Concluded {
        run_id,
        outcome,
        conclusion,
        pushed_branch,
        graph,
        run_options,
        emitter,
    } = concluded;

    let mut pr_url = None;
    if let Some(pr_cfg) = &options.pr_config {
        if run_options.dry_run_enabled() {
            tracing::debug!("Skipping PR creation: run is in dry-run mode");
        } else if let Err(ref e) = outcome {
            tracing::debug!(error = %e, "Skipping PR creation: engine returned an error");
        } else if let Ok(ref result) = outcome {
            if matches!(
                result.status,
                StageStatus::Success | StageStatus::PartialSuccess
            ) {
                let diff = load_pull_request_diff(&options.run_store).await;
                if let (Some(base_branch), Some(run_branch), Some(creds), Some(origin)) = (
                    &run_options.base_branch,
                    pushed_branch.as_deref(),
                    &options.github_app,
                    &options.origin_url,
                ) {
                    let auto_merge = if pr_cfg.auto_merge {
                        Some(AutoMergeOptions {
                            merge_strategy: pr_cfg.merge_strategy,
                        })
                    } else {
                        None
                    };

                    match maybe_open_pull_request(
                        creds,
                        origin,
                        base_branch,
                        run_branch,
                        graph.goal(),
                        &diff,
                        &options.model,
                        pr_cfg.draft,
                        auto_merge,
                        &options.run_store,
                        Some(&conclusion),
                    )
                    .await
                    {
                        Ok(Some(record)) => {
                            emitter.emit(&Event::PullRequestCreated {
                                pr_url:      record.html_url.clone(),
                                pr_number:   record.number,
                                owner:       record.owner.clone(),
                                repo:        record.repo.clone(),
                                base_branch: record.base_branch.clone(),
                                head_branch: record.head_branch.clone(),
                                title:       record.title.clone(),
                                draft:       pr_cfg.draft,
                            });
                            pr_url = Some(record.html_url.clone());
                        }
                        Ok(None) => {}
                        Err(e) => {
                            emitter.emit(&Event::PullRequestFailed { error: e.clone() });
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

    Finalized {
        run_id,
        outcome,
        conclusion,
        pushed_branch,
        pr_url,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Once};
    use std::time::Duration;

    use chrono::Utc;
    use fabro_graphviz::graph::Graph;
    use fabro_llm::client::Client;
    use fabro_llm::provider::{ProviderAdapter, StreamEventStream};
    use fabro_llm::types::{FinishReason, Message, Request, Response, StreamEvent, TokenCounts};
    use fabro_llm::{Error as LlmError, set_default_client};
    use fabro_retro::retro::{
        AggregateStats, FrictionKind, FrictionPoint, OpenItem, OpenItemKind, StageRetro,
    };
    use fabro_store::Database;
    use fabro_types::settings::SettingsLayer;
    use fabro_types::{BilledTokenCounts, RunRecord, fixtures};
    use futures::stream;
    use object_store::memory::InMemory;

    use super::*;
    use crate::event::{Event, append_event};
    use crate::records::StageSummary;

    struct MockProvider {
        response_text: String,
    }

    impl MockProvider {
        fn new(text: &str) -> Self {
            Self {
                response_text: text.to_string(),
            }
        }
    }

    #[async_trait::async_trait]
    impl ProviderAdapter for MockProvider {
        fn name(&self) -> &str {
            "mock"
        }

        async fn complete(&self, _request: &Request) -> Result<Response, LlmError> {
            Ok(Response {
                id:            "resp_1".into(),
                model:         "mock-model".into(),
                provider:      "mock".into(),
                message:       Message::assistant(&self.response_text),
                finish_reason: FinishReason::Stop,
                usage:         TokenCounts {
                    input_tokens: 10,
                    output_tokens: 20,
                    ..Default::default()
                },
                raw:           None,
                warnings:      vec![],
                rate_limit:    None,
            })
        }

        async fn stream(&self, _request: &Request) -> Result<StreamEventStream, LlmError> {
            let text = self.response_text.clone();
            let events = vec![
                Ok(StreamEvent::text_delta(&text, Some("t1".into()))),
                Ok(StreamEvent::finish(
                    FinishReason::Stop,
                    TokenCounts {
                        input_tokens: 10,
                        output_tokens: 20,
                        ..Default::default()
                    },
                    Response {
                        id:            "resp_1".into(),
                        model:         "mock-model".into(),
                        provider:      "mock".into(),
                        message:       Message::assistant(&text),
                        finish_reason: FinishReason::Stop,
                        usage:         TokenCounts {
                            input_tokens: 10,
                            output_tokens: 20,
                            ..Default::default()
                        },
                        raw:           None,
                        warnings:      vec![],
                        rate_limit:    None,
                    },
                )),
            ];
            Ok(Box::pin(stream::iter(events)))
        }
    }

    fn test_store() -> Arc<Database> {
        Arc::new(Database::new(
            Arc::new(InMemory::new()),
            "",
            Duration::from_millis(1),
        ))
    }

    fn install_mock_llm() {
        static INIT: Once = Once::new();

        INIT.call_once(|| {
            let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
            providers.insert(
                "mock".to_string(),
                Arc::new(MockProvider::new("Narrative from mock.")),
            );
            set_default_client(Client::new(providers, Some("mock".to_string()), vec![]));
        });
    }

    fn make_test_conclusion() -> Conclusion {
        Conclusion {
            timestamp:            Utc::now(),
            status:               crate::outcome::StageStatus::Success,
            duration_ms:          150_000,
            failure_reason:       None,
            final_git_commit_sha: None,
            stages:               vec![
                StageSummary {
                    stage_id:           "plan".to_string(),
                    stage_label:        "plan".to_string(),
                    duration_ms:        45_000,
                    billing_usd_micros: Some(120_000),
                    retries:            0,
                },
                StageSummary {
                    stage_id:           "implement".to_string(),
                    stage_label:        "implement".to_string(),
                    duration_ms:        90_000,
                    billing_usd_micros: Some(250_000),
                    retries:            0,
                },
                StageSummary {
                    stage_id:           "simplify".to_string(),
                    stage_label:        "simplify".to_string(),
                    duration_ms:        15_000,
                    billing_usd_micros: Some(50_000),
                    retries:            0,
                },
            ],
            billing:              Some(BilledTokenCounts {
                total_usd_micros: Some(420_000),
                ..BilledTokenCounts::default()
            }),
            total_retries:        0,
        }
    }

    fn make_test_retro() -> Retro {
        Retro {
            run_id:          fixtures::RUN_1,
            workflow_name:   "implement".to_string(),
            goal:            "Fix the bug".to_string(),
            timestamp:       Utc::now(),
            smoothness:      None,
            stages:          vec![
                StageRetro {
                    stage_id:           "plan".to_string(),
                    stage_label:        "plan".to_string(),
                    status:             "success".to_string(),
                    duration_ms:        45_000,
                    retries:            0,
                    billing_usd_micros: Some(120_000),
                    notes:              None,
                    failure_reason:     None,
                    files_touched:      vec![],
                },
                StageRetro {
                    stage_id:           "implement".to_string(),
                    stage_label:        "implement".to_string(),
                    status:             "success".to_string(),
                    duration_ms:        90_000,
                    retries:            0,
                    billing_usd_micros: Some(250_000),
                    notes:              None,
                    failure_reason:     None,
                    files_touched:      vec!["src/main.rs".to_string(), "src/lib.rs".to_string()],
                },
                StageRetro {
                    stage_id:           "simplify".to_string(),
                    stage_label:        "simplify".to_string(),
                    status:             "success".to_string(),
                    duration_ms:        15_000,
                    retries:            0,
                    billing_usd_micros: Some(50_000),
                    notes:              None,
                    failure_reason:     None,
                    files_touched:      vec![],
                },
            ],
            stats:           AggregateStats {
                total_duration_ms:        150_000,
                total_billing_usd_micros: Some(420_000),
                total_retries:            0,
                files_touched:            vec!["src/lib.rs".to_string(), "src/main.rs".to_string()],
                stages_completed:         3,
                stages_failed:            0,
            },
            intent:          None,
            outcome:         None,
            learnings:       None,
            friction_points: Some(vec![
                FrictionPoint {
                    kind:        FrictionKind::ToolFailure,
                    description: "Daytona sandbox didn't have cargo on PATH".to_string(),
                    stage_id:    None,
                },
                FrictionPoint {
                    kind:        FrictionKind::Timeout,
                    description: "Proxy timeouts during cold compilations".to_string(),
                    stage_id:    None,
                },
            ]),
            open_items:      Some(vec![OpenItem {
                kind:        OpenItemKind::TechDebt,
                description: "`ToolApprovalFn` type alias still exists".to_string(),
            }]),
        }
    }

    // ── format_retro_section tests ──────────────────────────────────────

    #[test]
    fn format_retro_section_full() {
        let retro = make_test_retro();
        let section = format_retro_section(&retro);

        assert!(section.contains("### Retro"));
        assert!(section.contains("3 stages completed, 0 failed, 0 retries"));
        assert!(section.contains("2 files modified"));
        assert!(section.contains("**Friction points:**"));
        assert!(section.contains("Daytona sandbox didn't have cargo on PATH"));
        assert!(section.contains("Proxy timeouts during cold compilations"));
        assert!(section.contains("**Open items:**"));
        assert!(section.contains("`ToolApprovalFn` type alias still exists"));
    }

    #[test]
    fn format_retro_section_no_friction_no_open() {
        let mut retro = make_test_retro();
        retro.friction_points = None;
        retro.open_items = None;
        let section = format_retro_section(&retro);

        assert!(section.contains("### Retro"));
        assert!(section.contains("3 stages completed"));
        assert!(!section.contains("**Friction points:**"));
        assert!(!section.contains("**Open items:**"));
    }

    #[test]
    fn format_retro_section_empty_stats() {
        let retro = Retro {
            run_id:          fixtures::RUN_2,
            workflow_name:   "test".to_string(),
            goal:            "test".to_string(),
            timestamp:       Utc::now(),
            smoothness:      None,
            stages:          vec![],
            stats:           AggregateStats {
                total_duration_ms:        0,
                total_billing_usd_micros: None,
                total_retries:            0,
                files_touched:            vec![],
                stages_completed:         0,
                stages_failed:            0,
            },
            intent:          None,
            outcome:         None,
            learnings:       None,
            friction_points: None,
            open_items:      None,
        };
        let section = format_retro_section(&retro);

        assert!(section.contains("0 stages completed, 0 failed, 0 retries"));
        assert!(section.contains("0 files modified"));
    }

    // ── format_arc_details_section tests ────────────────────────────────

    #[test]
    fn format_arc_details_cost_table() {
        let conclusion = make_test_conclusion();
        let section = format_arc_details_section(&conclusion, None, None);

        assert!(section.contains("### Fabro Details"));
        assert!(section.contains("Ran 3 stages in 2m 30s for $0.42"));
        assert!(section.contains("| plan | 45s | $0.12 | 0 |"));
        assert!(section.contains("| implement | 1m 30s | $0.25 | 0 |"));
        assert!(section.contains("| simplify | 15s | $0.05 | 0 |"));
        assert!(section.contains("| **Total** | **2m 30s** | **$0.42** | **0** |"));
    }

    #[test]
    fn format_arc_details_no_cost() {
        let mut conclusion = make_test_conclusion();
        for stage in &mut conclusion.stages {
            stage.billing_usd_micros = None;
        }
        conclusion.billing = None;
        let section = format_arc_details_section(&conclusion, None, None);

        // En-dash for missing costs
        assert!(section.contains("| plan | 45s | \u{2013} | 0 |"));
        assert!(section.contains("for \u{2013}"));
    }

    #[test]
    fn format_arc_details_with_dot_graph() {
        let conclusion = make_test_conclusion();
        let dot = "digraph implement {\n  plan [type=\"agent\"]\n  code [type=\"agent\"]\n  plan -> code\n}\n";
        let section = format_arc_details_section(&conclusion, None, Some(dot));

        assert!(section.contains("<code>implement.fabro</code>"));
        assert!(section.contains("2 nodes and 1 edge"));
        assert!(section.contains("```dot"));
        assert!(section.contains("digraph implement"));
    }

    // ── read_plan_text tests ────────────────────────────────────────────

    #[test]
    fn read_plan_text_found() {
        let mut state = RunProjection::default();
        state.set_node(
            fabro_store::StageId::new("plan", 1),
            fabro_store::NodeState {
                response: Some("This is the plan".to_string()),
                ..Default::default()
            },
        );

        let result = read_plan_text(&state);
        assert_eq!(result, Some("This is the plan".to_string()));
    }

    #[test]
    fn read_plan_text_prefix_match() {
        let mut state = RunProjection::default();
        state.set_node(
            fabro_store::StageId::new("planning", 1),
            fabro_store::NodeState {
                response: Some("Planning content".to_string()),
                ..Default::default()
            },
        );

        let result = read_plan_text(&state);
        assert_eq!(result, Some("Planning content".to_string()));
    }

    #[test]
    fn read_plan_text_prefers_alphabetically_first_plan_node() {
        let mut state = RunProjection::default();
        state.set_node(
            fabro_store::StageId::new("planning", 1),
            fabro_store::NodeState {
                response: Some("Planning content".to_string()),
                ..Default::default()
            },
        );
        state.set_node(
            fabro_store::StageId::new("plan", 1),
            fabro_store::NodeState {
                response: Some("Plan content".to_string()),
                ..Default::default()
            },
        );

        let result = read_plan_text(&state);
        assert_eq!(result, Some("Plan content".to_string()));
    }

    #[test]
    fn read_plan_text_not_found() {
        let mut state = RunProjection::default();
        state.set_node(
            fabro_store::StageId::new("implement", 1),
            fabro_store::NodeState::default(),
        );

        let result = read_plan_text(&state);
        assert_eq!(result, None);
    }

    #[test]
    fn read_plan_text_empty_state() {
        let state = RunProjection::default();
        let result = read_plan_text(&state);
        assert_eq!(result, None);
    }

    // ── assemble_pr_body tests ──────────────────────────────────────────

    #[test]
    fn assemble_all_sections() {
        let body = assemble_pr_body(
            "This is the narrative.\n\n### Plan Summary\n\n* Step 1\n* Step 2",
            Some("Full plan text here"),
            "### Retro\n\n* 3 stages completed",
            "### Fabro Details\n\n<details>...</details>",
        );

        assert!(body.contains("This is the narrative."));
        assert!(body.contains("### Plan Summary"));
        assert!(body.contains("<details>\n<summary>Full plan</summary>"));
        assert!(body.contains("````md\nFull plan text here\n````"));
        assert!(body.contains("### Retro"));
        assert!(body.contains("### Fabro Details"));
    }

    #[test]
    fn assemble_no_plan() {
        let body = assemble_pr_body(
            "Narrative only.",
            None,
            "### Retro\n\n* stats",
            "### Fabro Details\n\n<details>...</details>",
        );

        assert!(body.contains("Narrative only."));
        assert!(!body.contains("Full plan"));
        assert!(body.contains("### Retro"));
        assert!(body.contains("### Fabro Details"));
    }

    #[test]
    fn assemble_no_retro() {
        let body = assemble_pr_body("Narrative only.", Some("Plan"), "", "");

        assert!(body.contains("Narrative only."));
        assert!(body.contains("Full plan"));
        // Empty sections should not produce extra headers
        assert!(!body.contains("### Retro"));
        assert!(!body.contains("### Fabro Details"));
    }

    #[test]
    fn assemble_narrative_only() {
        let body = assemble_pr_body("Just the narrative.", None, "", "");

        assert_eq!(
            body,
            "Just the narrative.\n\n\u{2692}\u{fe0f} Generated with [Fabro](https://fabro.sh)"
        );
    }

    #[test]
    fn assemble_conclusion_without_retro() {
        let conclusion = make_test_conclusion();
        let arc_details = format_arc_details_section(&conclusion, None, None);
        let body = assemble_pr_body("Narrative.", None, "", &arc_details);

        assert!(body.contains("### Fabro Details"));
        assert!(body.contains("Ran 3 stages"));
        assert!(!body.contains("### Retro"));
    }

    #[test]
    fn assemble_both_conclusion_and_retro() {
        let conclusion = make_test_conclusion();
        let retro = make_test_retro();
        let retro_section = format_retro_section(&retro);
        let arc_details = format_arc_details_section(&conclusion, None, None);
        let body = assemble_pr_body("Narrative.", None, &retro_section, &arc_details);

        assert!(body.contains("### Retro"));
        assert!(body.contains("### Fabro Details"));
    }

    #[tokio::test]
    async fn build_pr_body_uses_in_memory_conclusion() {
        install_mock_llm();

        let store = test_store();
        let run_store = store.create_run(&fixtures::RUN_1).await.unwrap();
        let conclusion = make_test_conclusion();
        let body = build_pr_body(
            "diff --git a/src/lib.rs b/src/lib.rs\n+fn new_feature() {}\n",
            "Implement feature",
            "mock-model",
            &run_store.clone().into(),
            Some(&conclusion),
        )
        .await
        .unwrap();

        assert!(body.contains("Narrative from mock."));
        assert!(body.contains("### Fabro Details"));
        assert!(body.contains("Ran 3 stages in 2m 30s for $0.42"));
        assert!(body.contains("| **Total** | **2m 30s** | **$0.42** | **0** |"));
    }

    #[tokio::test]
    async fn build_pr_body_uses_store_records_without_legacy_files() {
        install_mock_llm();

        let store = test_store();
        let run_store = store.create_run(&fixtures::RUN_1).await.unwrap();

        let run_record = RunRecord {
            run_id:            fixtures::RUN_1,
            settings:          SettingsLayer::default(),
            graph:             Graph::new("test"),
            workflow_slug:     Some("test".to_string()),
            working_directory: PathBuf::from("/tmp/project"),
            host_repo_path:    Some("/tmp/project".to_string()),
            repo_origin_url:   None,
            base_branch:       Some("main".to_string()),
            labels:            HashMap::new(),
            provenance:        None,
            manifest_blob:     None,
            definition_blob:   None,
        };
        append_event(&run_store, &fixtures::RUN_1, &Event::RunCreated {
            run_id:            fixtures::RUN_1,
            settings:          serde_json::to_value(&run_record.settings).unwrap(),
            graph:             serde_json::to_value(&run_record.graph).unwrap(),
            workflow_source:   Some("digraph test { plan -> code }".to_string()),
            workflow_config:   None,
            labels:            run_record.labels.clone().into_iter().collect(),
            run_dir:           run_record.working_directory.display().to_string(),
            working_directory: run_record.working_directory.display().to_string(),
            host_repo_path:    run_record.host_repo_path.clone(),
            repo_origin_url:   run_record.repo_origin_url.clone(),
            base_branch:       run_record.base_branch.clone(),
            workflow_slug:     run_record.workflow_slug.clone(),
            db_prefix:         None,
            provenance:        run_record.provenance.clone(),
            manifest_blob:     None,
        })
        .await
        .unwrap();
        append_event(&run_store, &fixtures::RUN_1, &Event::RetroCompleted {
            duration_ms: 1,
            response:    Some(String::new()),
            retro:       Some(serde_json::to_value(make_test_retro()).unwrap()),
        })
        .await
        .unwrap();

        let conclusion = make_test_conclusion();
        let body = build_pr_body(
            "diff --git a/src/lib.rs b/src/lib.rs\n+fn new_feature() {}\n",
            "Implement feature",
            "mock-model",
            &run_store.clone().into(),
            Some(&conclusion),
        )
        .await
        .unwrap();

        assert!(body.contains("Narrative from mock."));
        assert!(body.contains("### Retro"));
        assert!(body.contains("### Fabro Details"));
        assert!(body.contains("test.fabro"));
    }

    #[tokio::test]
    async fn build_pr_body_uses_plan_text_from_store_without_response_md() {
        install_mock_llm();

        let store = test_store();
        let run_store = store.create_run(&fixtures::RUN_1).await.unwrap();

        let run_record = RunRecord {
            run_id:            fixtures::RUN_1,
            settings:          SettingsLayer::default(),
            graph:             Graph::new("test"),
            workflow_slug:     Some("test".to_string()),
            working_directory: PathBuf::from("/tmp/project"),
            host_repo_path:    Some("/tmp/project".to_string()),
            repo_origin_url:   None,
            base_branch:       Some("main".to_string()),
            labels:            HashMap::new(),
            provenance:        None,
            manifest_blob:     None,
            definition_blob:   None,
        };
        append_event(&run_store, &fixtures::RUN_1, &Event::RunCreated {
            run_id:            fixtures::RUN_1,
            settings:          serde_json::to_value(&run_record.settings).unwrap(),
            graph:             serde_json::to_value(&run_record.graph).unwrap(),
            workflow_source:   Some("digraph test { plan -> code }".to_string()),
            workflow_config:   None,
            labels:            run_record.labels.clone().into_iter().collect(),
            run_dir:           run_record.working_directory.display().to_string(),
            working_directory: run_record.working_directory.display().to_string(),
            host_repo_path:    run_record.host_repo_path.clone(),
            repo_origin_url:   run_record.repo_origin_url.clone(),
            base_branch:       run_record.base_branch.clone(),
            workflow_slug:     run_record.workflow_slug.clone(),
            db_prefix:         None,
            provenance:        run_record.provenance.clone(),
            manifest_blob:     None,
        })
        .await
        .unwrap();
        append_event(&run_store, &fixtures::RUN_1, &Event::StageCompleted {
            node_id: "plan".to_string(),
            name: "plan".to_string(),
            index: 0,
            duration_ms: 1,
            status: "success".to_string(),
            preferred_label: None,
            suggested_next_ids: vec![],
            billing: None,
            failure: None,
            notes: None,
            files_touched: vec![],
            context_updates: None,
            jump_to_node: None,
            context_values: None,
            node_visits: None,
            loop_failure_signatures: None,
            restart_failure_signatures: None,
            response: Some("Plan from store".to_string()),
            attempt: 1,
            max_attempts: 1,
        })
        .await
        .unwrap();

        let body = build_pr_body(
            "diff --git a/src/lib.rs b/src/lib.rs\n+fn new_feature() {}\n",
            "Implement feature",
            "mock-model",
            &run_store.clone().into(),
            Some(&make_test_conclusion()),
        )
        .await
        .unwrap();

        assert!(body.contains("<summary>Full plan</summary>"));
        assert!(body.contains("Plan from store"));
    }

    // ── parse_dot_summary tests ─────────────────────────────────────────

    #[test]
    fn parse_dot_summary_basic() {
        let dot = r#"digraph my_workflow {
  plan [type="agent"]
  code [type="agent"]
  plan -> code
}"#;
        let (name, nodes, edges) = parse_dot_summary(dot);
        assert_eq!(name, "my_workflow.fabro");
        assert_eq!(nodes, 2);
        assert_eq!(edges, 1);
    }

    #[test]
    fn parse_dot_summary_empty() {
        let (name, nodes, edges) = parse_dot_summary("");
        assert_eq!(name, "workflow.fabro");
        assert_eq!(nodes, 0);
        assert_eq!(edges, 0);
    }

    // ── format_duration_ms tests ────────────────────────────────────────

    #[test]
    fn format_duration_seconds() {
        assert_eq!(format_duration_ms(45_000), "45s");
    }

    #[test]
    fn format_duration_minutes() {
        assert_eq!(format_duration_ms(150_000), "2m 30s");
    }

    #[test]
    fn format_duration_zero() {
        assert_eq!(format_duration_ms(0), "0s");
    }

    // ── Existing tests ─────────────────────────────────────────────────

    #[test]
    fn pr_title_uses_first_line() {
        let goal = "Add Draft PR Mode\n\nMore details here...";
        assert_eq!(pr_title_from_goal(goal), "Add Draft PR Mode");
    }

    #[test]
    fn pr_title_strips_h1_prefix() {
        assert_eq!(
            pr_title_from_goal("# Add Draft PR Mode"),
            "Add Draft PR Mode"
        );
    }

    #[test]
    fn pr_title_strips_h2_prefix() {
        assert_eq!(
            pr_title_from_goal("## Add Draft PR Mode"),
            "Add Draft PR Mode"
        );
    }

    #[test]
    fn pr_title_strips_plan_prefix() {
        assert_eq!(
            pr_title_from_goal("Plan: Add Draft PR Mode"),
            "Add Draft PR Mode"
        );
    }

    #[test]
    fn pr_title_strips_heading_and_plan_prefix() {
        assert_eq!(
            pr_title_from_goal("## Plan: Add Draft PR Mode"),
            "Add Draft PR Mode"
        );
    }

    #[test]
    fn pr_title_strips_h3_prefix() {
        assert_eq!(
            pr_title_from_goal("### Add Draft PR Mode"),
            "Add Draft PR Mode"
        );
    }

    #[test]
    fn pr_title_truncates_long_line() {
        let long = "x".repeat(300);
        let title = pr_title_from_goal(&long);
        assert_eq!(title.chars().count(), 120);
        assert!(title.ends_with('…'));
    }

    #[test]
    fn pr_body_truncates_long_body() {
        let long = "x".repeat(70_000);
        let body = truncate_pr_body(&long);
        assert!(body.len() <= 65_536);
        assert!(body.ends_with("\n\n_(truncated)_"));
    }

    #[test]
    fn pr_body_short_body_unchanged() {
        let short = "Some PR description";
        assert_eq!(truncate_pr_body(short), short);
    }

    #[test]
    fn pr_title_short_goal_unchanged() {
        assert_eq!(pr_title_from_goal("Fix bug"), "Fix bug");
    }

    #[tokio::test]
    async fn empty_diff_returns_none() {
        let store = test_store();
        let run_store = store.create_run(&fixtures::RUN_1).await.unwrap();
        let creds = GitHubCredentials::App(fabro_github::GitHubAppCredentials {
            app_id:          "123".to_string(),
            private_key_pem: "unused".to_string(),
        });
        let result = maybe_open_pull_request(
            &creds,
            "https://github.com/owner/repo.git",
            "main",
            "fabro/run/123",
            "Fix bug",
            "",
            "claude-sonnet-4-20250514",
            false,
            None,
            &run_store.clone().into(),
            None,
        )
        .await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[tokio::test]
    async fn load_pull_request_diff_uses_store_without_disk_patch() {
        let tmp = tempfile::tempdir().unwrap();
        let store = test_store();
        let run_store = store.create_run(&fixtures::RUN_1).await.unwrap();
        let run_record = RunRecord {
            run_id:            fixtures::RUN_1,
            settings:          SettingsLayer::default(),
            graph:             Graph::new("test"),
            workflow_slug:     None,
            working_directory: tmp.path().to_path_buf(),
            host_repo_path:    None,
            repo_origin_url:   None,
            base_branch:       None,
            labels:            std::collections::HashMap::new(),
            provenance:        None,
            manifest_blob:     None,
            definition_blob:   None,
        };
        append_event(&run_store, &fixtures::RUN_1, &Event::RunCreated {
            run_id:            fixtures::RUN_1,
            settings:          serde_json::to_value(&run_record.settings).unwrap(),
            graph:             serde_json::to_value(&run_record.graph).unwrap(),
            workflow_source:   None,
            workflow_config:   None,
            labels:            run_record.labels.clone().into_iter().collect(),
            run_dir:           run_record.working_directory.display().to_string(),
            working_directory: tmp.path().display().to_string(),
            host_repo_path:    None,
            repo_origin_url:   run_record.repo_origin_url.clone(),
            base_branch:       None,
            workflow_slug:     None,
            db_prefix:         None,
            provenance:        run_record.provenance.clone(),
            manifest_blob:     None,
        })
        .await
        .unwrap();
        append_event(&run_store, &fixtures::RUN_1, &Event::WorkflowRunCompleted {
            duration_ms:          1,
            artifact_count:       0,
            status:               "success".to_string(),
            reason:               None,
            total_usd_micros:     None,
            final_git_commit_sha: None,
            final_patch:          Some(
                "diff --git a/src/lib.rs b/src/lib.rs\n+fn from_store() {}\n".to_string(),
            ),
            billing:              None,
        })
        .await
        .unwrap();

        let diff = load_pull_request_diff(&run_store.clone().into()).await;

        assert!(diff.contains("from_store"));
    }
}
