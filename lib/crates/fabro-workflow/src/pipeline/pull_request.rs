use std::sync::{Arc, LazyLock};

use fabro_auth::CredentialSource;
use fabro_github::{self as github_app, ssh_url_to_https};
use fabro_graphviz::parser;
use fabro_llm::client::Client;
use fabro_llm::generate::{GenerateParams, generate_object};
use fabro_model::Catalog;
use fabro_retro::retro::Retro;
use fabro_store::RunProjection;
use fabro_types::PullRequestRecord;
use fabro_types::settings::run::MergeStrategy;
use fabro_util::text::strip_goal_decoration;
use tracing::{debug, info, warn};

use super::types::{Concluded, Finalized, PullRequestOptions};
use crate::event::{Event, RunNoticeLevel};
use crate::outcome::{StageOutcome, format_cost as outcome_format_cost};
use crate::records::{Conclusion, RunSpec};
use crate::runtime_store::RunStoreHandle;

/// Maximum length of a PR title (Unicode scalar values).
const PR_TITLE_MAX_CHARS: usize = 72;

/// Structured output schema for the LLM-generated PR title and body.
static PR_CONTENT_SCHEMA: LazyLock<serde_json::Value> = LazyLock::new(|| {
    serde_json::json!({
        "type": "object",
        "properties": {
            "title": { "type": "string" },
            "body":  { "type": "string" }
        },
        "required": ["title", "body"],
        "additionalProperties": false
    })
});

/// Complete pull request content generated for a workflow run.
#[derive(Debug, serde::Deserialize)]
pub struct PrContent {
    pub title: String,
    pub body:  String,
}

/// System prompt that instructs the LLM how to write a Fabro PR title and
/// body. The trailing programmatic sections (Plan `<details>`, Retro,
/// Fabro Details, footer) are appended after the LLM body — the prompt
/// explicitly forbids the LLM from duplicating them.
const PR_BODY_SYSTEM_PROMPT: &str = include_str!("prompts/pr_body.md");

const DEFAULT_PR_TITLE: &str = "Update workflow output";
const EMPTY_BODY_NOTICE: &str = "> _The LLM did not produce a description for this change. The diff and the appended details are the source of truth for review._";

/// Truncation budget for the LLM prompt's plan / diff sections.
#[derive(Debug, PartialEq, Eq)]
struct TruncationCaps {
    plan: usize,
    diff: usize,
}

const DIFF_HARD_CAP: usize = 500_000;
const PLAN_HARD_CAP: usize = 100_000;
const DIFF_FRACTION_NUM: usize = 4;
const PLAN_FRACTION_NUM: usize = 1;
const FRACTION_DEN: usize = 10;
const UNKNOWN_MODEL_CTX: usize = 200_000;

/// Resolve truncation caps based on the model's context window. Unknown
/// models use the baseline 200k context-window assumption.
fn truncation_caps(model: &str) -> TruncationCaps {
    let ctx = Catalog::builtin()
        .get(model)
        .and_then(|m| usize::try_from(m.context_window()).ok())
        .unwrap_or(UNKNOWN_MODEL_CTX);

    truncation_caps_for_context_window(ctx)
}

fn truncation_caps_for_context_window(ctx: usize) -> TruncationCaps {
    TruncationCaps {
        diff: ctx
            .saturating_mul(DIFF_FRACTION_NUM)
            .checked_div(FRACTION_DEN)
            .unwrap_or(DIFF_HARD_CAP)
            .min(DIFF_HARD_CAP),
        plan: ctx
            .saturating_mul(PLAN_FRACTION_NUM)
            .checked_div(FRACTION_DEN)
            .unwrap_or(PLAN_HARD_CAP)
            .min(PLAN_HARD_CAP),
    }
}

/// Truncate `s` to at most `max` Unicode scalar values without splitting a
/// UTF-8 sequence.
fn truncate_chars(s: &str, max: usize) -> &str {
    s.char_indices()
        .nth(max)
        .map_or(s, |(boundary, _)| &s[..boundary])
}

/// Truncate `s` to at most `max` Unicode scalar values, replacing the
/// trailing char with `…` when truncation occurs.
fn truncate_with_ellipsis(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let truncated: String = s.chars().take(max - 1).collect();
        format!("{truncated}\u{2026}")
    } else {
        s.to_string()
    }
}

/// Cap a PR title at [`PR_TITLE_MAX_CHARS`].
fn enforce_title_cap(title: &str) -> String {
    truncate_with_ellipsis(title, PR_TITLE_MAX_CHARS)
}

/// Derive a PR title from the workflow goal.
///
/// Uses the first line, truncated to the same cap as LLM-generated titles.
fn pr_title_from_goal(goal: &str) -> String {
    truncate_with_ellipsis(strip_goal_decoration(goal), PR_TITLE_MAX_CHARS)
}

fn fallback_pr_title(goal: &str) -> String {
    let title = pr_title_from_goal(goal);
    if title.trim().is_empty() {
        DEFAULT_PR_TITLE.to_string()
    } else {
        title
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
    run_spec: Option<&RunSpec>,
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

    // Workflow graph summary — prefer RunSpec's graph, fall back to DOT parsing
    if let Some(record) = run_spec {
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
        .iter_stages()
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

/// Build complete PR content by combining LLM-generated narrative with
/// deterministic fallbacks and programmatic sections.
pub async fn build_pr_content(
    diff: &str,
    goal: &str,
    model: &str,
    run_store: &RunStoreHandle,
    llm_source: &dyn CredentialSource,
    conclusion: Option<&Conclusion>,
    run_state: Option<&RunProjection>,
) -> Result<PrContent, String> {
    let client = Client::from_source(llm_source)
        .await
        .map_err(|e| format!("Failed to create LLM client: {e}"))?;

    build_pr_content_with_client(
        diff,
        goal,
        model,
        run_store,
        conclusion,
        run_state,
        Arc::new(client),
    )
    .await
}

async fn build_pr_content_with_client(
    diff: &str,
    goal: &str,
    model: &str,
    run_store: &RunStoreHandle,
    conclusion: Option<&Conclusion>,
    run_state: Option<&RunProjection>,
    client: Arc<Client>,
) -> Result<PrContent, String> {
    info!("Building PR content");

    let loaded_run_state = if run_state.is_none() {
        run_store
            .state()
            .await
            .inspect_err(|err| {
                tracing::warn!(error = %err, "Failed to load run state from store for PR body");
            })
            .ok()
    } else {
        None
    };
    let run_state = run_state.or(loaded_run_state.as_ref());
    let conclusion = conclusion.or_else(|| run_state.and_then(|state| state.conclusion.as_ref()));
    let plan_text = run_state.and_then(read_plan_text);
    let retro = run_state.and_then(|state| state.retro.clone());
    let run_spec = run_state.and_then(|state| state.spec.clone());
    let dot_source = run_state.and_then(|state| state.graph_source.clone());

    let caps = truncation_caps(model);
    let truncated_diff = truncate_chars(diff, caps.diff);

    let prompt = if let Some(ref plan) = plan_text {
        let truncated_plan = truncate_chars(plan, caps.plan);
        format!(
            "Goal: {goal}\n\nPlan:\n```\n{truncated_plan}\n```\n\nDiff:\n```\n{truncated_diff}\n```"
        )
    } else {
        format!("Goal: {goal}\n\nDiff:\n```\n{truncated_diff}\n```")
    };

    let params = GenerateParams::new(model, client)
        .system(PR_BODY_SYSTEM_PROMPT)
        .prompt(prompt);

    let result = generate_object(params, PR_CONTENT_SCHEMA.clone())
        .await
        .map_err(|e| format!("LLM generation failed: {e}"))?;

    let output = result
        .output
        .ok_or_else(|| "LLM generation returned no structured output".to_string())?;
    let generated: PrContent = serde_json::from_value(output)
        .map_err(|e| format!("Failed to deserialize PR content: {e}"))?;

    let title = if generated.title.trim().is_empty() {
        fallback_pr_title(goal)
    } else {
        generated.title.trim().to_string()
    };
    let title = enforce_title_cap(&title);

    let llm_body = if generated.body.trim().is_empty() {
        warn!(model = %model, "LLM generated empty PR body; using skeleton PR body");
        EMPTY_BODY_NOTICE.to_string()
    } else {
        generated.body
    };

    let retro_section = retro.as_ref().map(format_retro_section).unwrap_or_default();
    let arc_details_section = conclusion
        .as_ref()
        .map(|c| format_arc_details_section(c, run_spec.as_ref(), dot_source.as_deref()))
        .unwrap_or_default();

    let body = assemble_pr_body(
        &llm_body,
        plan_text.as_deref(),
        &retro_section,
        &arc_details_section,
    );

    info!("PR content generated");

    Ok(PrContent { title, body })
}

/// Auto-merge configuration for a pull request.
pub struct AutoMergeOptions {
    pub merge_strategy: MergeStrategy,
}

/// Inputs for [`maybe_open_pull_request`].
pub struct OpenPullRequestRequest<'a> {
    pub github:      github_app::GitHubContext<'a>,
    pub origin_url:  &'a str,
    pub base_branch: &'a str,
    pub head_branch: &'a str,
    pub goal:        &'a str,
    pub diff:        &'a str,
    pub model:       &'a str,
    pub draft:       bool,
    pub auto_merge:  Option<AutoMergeOptions>,
    pub run_store:   &'a RunStoreHandle,
    pub llm_source:  &'a dyn CredentialSource,
    pub conclusion:  Option<&'a Conclusion>,
    pub run_state:   Option<&'a RunProjection>,
}

/// Optionally open a pull request after a successful workflow run.
///
/// Returns `Ok(Some(PullRequestRecord))` if a PR was created, `Ok(None)` if
/// the diff was empty, or `Err` on failure.
pub async fn maybe_open_pull_request(
    req: OpenPullRequestRequest<'_>,
) -> Result<Option<PullRequestRecord>, String> {
    if req.diff.is_empty() {
        debug!("Empty diff, skipping pull request creation");
        return Ok(None);
    }

    let https_url = ssh_url_to_https(req.origin_url);
    let (owner, repo) =
        github_app::parse_github_owner_repo(&https_url).map_err(|err| format!("{err:#}"))?;

    let content = build_pr_content(
        req.diff,
        req.goal,
        req.model,
        req.run_store,
        req.llm_source,
        req.conclusion,
        req.run_state,
    )
    .await
    .map_err(|err| format!("{err:#}"))?;
    let body = truncate_pr_body(&content.body);
    let title = content.title;

    let created = github_app::create_pull_request(
        &req.github,
        &owner,
        &repo,
        req.base_branch,
        req.head_branch,
        &title,
        &body,
        req.draft,
    )
    .await
    .map_err(|err| format!("{err:#}"))?;

    info!(pr_url = %created.html_url, created.number, "Pull request created");

    if let Some(am_cfg) = req.auto_merge {
        match github_app::enable_auto_merge(
            &req.github,
            &owner,
            &repo,
            &created.node_id,
            am_cfg.merge_strategy,
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
        base_branch: req.base_branch.to_string(),
        head_branch: req.head_branch.to_string(),
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
        outcome,
        conclusion,
        graph,
        run_options,
        services,
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
                StageOutcome::Succeeded | StageOutcome::PartiallySucceeded
            ) {
                let diff = load_pull_request_diff(&services.run_store).await;
                if let (Some(base_branch), Some(run_branch), Some(creds), Some(origin)) = (
                    &run_options.base_branch,
                    run_options.run_branch(),
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

                    match maybe_open_pull_request(OpenPullRequestRequest {
                        github: github_app::GitHubContext::new(
                            creds,
                            &github_app::github_api_base_url(),
                        ),
                        origin_url: origin,
                        base_branch,
                        head_branch: run_branch,
                        goal: graph.goal(),
                        diff: &diff,
                        model: &options.model,
                        draft: pr_cfg.draft,
                        auto_merge,
                        run_store: &services.run_store,
                        llm_source: services.llm_source.as_ref(),
                        conclusion: Some(&conclusion),
                        run_state: None,
                    })
                    .await
                    {
                        Ok(Some(record)) => {
                            services
                                .emitter
                                .emit(&Event::pull_request_created(&record, pr_cfg.draft));
                            pr_url = Some(record.html_url.clone());
                        }
                        Ok(None) => {}
                        Err(e) => {
                            services
                                .emitter
                                .emit(&Event::PullRequestFailed { error: e.clone() });
                            services.emitter.notice(
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
        run_id: run_options.run_id,
        outcome,
        conclusion,
        pushed_branch: run_options.run_branch().map(str::to_string),
        pr_url,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    use chrono::Utc;
    use fabro_auth::{
        AuthCredential, AuthDetails, CredentialSource, EnvCredentialSource, VaultCredentialSource,
    };
    use fabro_graphviz::graph::Graph;
    use fabro_llm::Error as LlmError;
    use fabro_llm::client::Client;
    use fabro_llm::provider::{ProviderAdapter, StreamEventStream};
    use fabro_llm::types::{FinishReason, Message, Request, Response, StreamEvent, TokenCounts};
    use fabro_retro::retro::{
        AggregateStats, FrictionKind, FrictionPoint, OpenItem, OpenItemKind, StageRetro,
    };
    use fabro_store::Database;
    use fabro_types::{BilledTokenCounts, RunSpec, SuccessReason, first_event_seq, fixtures};
    use fabro_vault::{SecretType, Vault};
    use futures::stream;
    use httpmock::Method::POST;
    use httpmock::MockServer;
    use object_store::memory::InMemory;
    use tokio::sync::RwLock as AsyncRwLock;

    use super::*;
    use crate::event::{Event, append_event};
    use crate::records::StageSummary;

    struct MockProvider {
        name:          String,
        response_text: String,
    }

    impl MockProvider {
        fn new(name: &str, text: &str) -> Self {
            Self {
                name:          name.to_string(),
                response_text: text.to_string(),
            }
        }
    }

    #[async_trait::async_trait]
    impl ProviderAdapter for MockProvider {
        fn name(&self) -> &str {
            &self.name
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
            None,
        ))
    }

    fn explicit_client(provider_name: &str, text: &str) -> Arc<Client> {
        let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
        providers.insert(
            provider_name.to_string(),
            Arc::new(MockProvider::new(provider_name, text)),
        );
        Arc::new(Client::new(
            providers,
            Some(provider_name.to_string()),
            vec![],
        ))
    }

    fn test_llm_source() -> Arc<dyn CredentialSource> {
        Arc::new(EnvCredentialSource::new())
    }

    fn openai_api_key_credential(key: &str) -> AuthCredential {
        AuthCredential {
            provider: fabro_model::Provider::OpenAi,
            details:  AuthDetails::ApiKey {
                key: key.to_string(),
            },
        }
    }

    fn openai_responses_payload(text: &str) -> serde_json::Value {
        serde_json::json!({
            "id": "resp_1",
            "model": "gpt-5.4",
            "output": [
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [
                        {
                            "type": "output_text",
                            "text": text
                        }
                    ]
                }
            ],
            "status": "completed",
            "usage": {
                "input_tokens": 10,
                "output_tokens": 20
            }
        })
    }

    /// JSON string the MockProvider/openai mock returns to simulate the
    /// structured-output response for `(title, body)`.
    fn pr_content_json(title: &str, body: &str) -> String {
        serde_json::to_string(&serde_json::json!({
            "title": title,
            "body": body,
        }))
        .unwrap()
    }

    fn make_test_conclusion() -> Conclusion {
        Conclusion {
            timestamp:            Utc::now(),
            status:               crate::outcome::StageOutcome::Succeeded,
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
                    status:             "succeeded".to_string(),
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
                    status:             "succeeded".to_string(),
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
                    status:             "succeeded".to_string(),
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
        state.stage_entry("plan", 1, first_event_seq(1)).response =
            Some("This is the plan".to_string());

        let result = read_plan_text(&state);
        assert_eq!(result, Some("This is the plan".to_string()));
    }

    #[test]
    fn read_plan_text_prefix_match() {
        let mut state = RunProjection::default();
        state
            .stage_entry("planning", 1, first_event_seq(1))
            .response = Some("Planning content".to_string());

        let result = read_plan_text(&state);
        assert_eq!(result, Some("Planning content".to_string()));
    }

    #[test]
    fn read_plan_text_prefers_alphabetically_first_plan_node() {
        let mut state = RunProjection::default();
        state
            .stage_entry("planning", 1, first_event_seq(1))
            .response = Some("Planning content".to_string());
        state.stage_entry("plan", 1, first_event_seq(2)).response =
            Some("Plan content".to_string());

        let result = read_plan_text(&state);
        assert_eq!(result, Some("Plan content".to_string()));
    }

    #[test]
    fn read_plan_text_not_found() {
        let mut state = RunProjection::default();
        state.stage_entry("implement", 1, first_event_seq(1));

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
    async fn build_pr_content_uses_in_memory_conclusion() {
        let store = test_store();
        let run_store = store.create_run(&fixtures::RUN_1).await.unwrap();
        let PrContent { title, body } = build_pr_content_with_client(
            "diff --git a/src/lib.rs b/src/lib.rs\n+fn new_feature() {}\n",
            "Implement feature",
            "mock-model",
            &run_store.clone().into(),
            Some(&make_test_conclusion()),
            None,
            explicit_client(
                "mock",
                &pr_content_json("Mock title", "Narrative from mock."),
            ),
        )
        .await
        .unwrap();

        assert_eq!(title, "Mock title");
        assert!(body.contains("Narrative from mock."));
        assert!(body.contains("### Fabro Details"));
        assert!(body.contains("Ran 3 stages in 2m 30s for $0.42"));
        assert!(body.contains("| **Total** | **2m 30s** | **$0.42** | **0** |"));
    }

    #[tokio::test]
    async fn build_pr_content_uses_store_records_without_legacy_files() {
        let store = test_store();
        let run_store = store.create_run(&fixtures::RUN_1).await.unwrap();

        let run_spec = RunSpec {
            run_id:           fixtures::RUN_1,
            settings:         fabro_types::WorkflowSettings::default(),
            graph:            Graph::new("test"),
            workflow_slug:    Some("test".to_string()),
            source_directory: Some("/tmp/project".to_string()),
            git:              Some(fabro_types::GitContext {
                origin_url:   String::new(),
                branch:       "main".to_string(),
                sha:          None,
                dirty:        fabro_types::DirtyStatus::Clean,
                push_outcome: fabro_types::PreRunPushOutcome::NotAttempted,
            }),
            labels:           HashMap::new(),
            provenance:       None,
            manifest_blob:    None,
            definition_blob:  None,
            fork_source_ref:  None,
            in_place:         false,
        };
        append_event(&run_store, &fixtures::RUN_1, &Event::RunCreated {
            run_id:           fixtures::RUN_1,
            settings:         serde_json::to_value(&run_spec.settings).unwrap(),
            graph:            serde_json::to_value(&run_spec.graph).unwrap(),
            workflow_source:  Some("digraph test { plan -> code }".to_string()),
            workflow_config:  None,
            labels:           run_spec.labels.clone().into_iter().collect(),
            run_dir:          "/tmp/project".to_string(),
            source_directory: run_spec.source_directory.clone(),
            workflow_slug:    run_spec.workflow_slug.clone(),
            db_prefix:        None,
            provenance:       run_spec.provenance.clone(),
            manifest_blob:    None,
            git:              run_spec.git.clone(),
            fork_source_ref:  None,
            in_place:         false,
            web_url:          None,
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

        let body = build_pr_content_with_client(
            "diff --git a/src/lib.rs b/src/lib.rs\n+fn new_feature() {}\n",
            "Implement feature",
            "mock-model",
            &run_store.clone().into(),
            Some(&make_test_conclusion()),
            None,
            explicit_client(
                "mock",
                &pr_content_json("Mock title", "Narrative from mock."),
            ),
        )
        .await
        .unwrap()
        .body;

        assert!(body.contains("Narrative from mock."));
        assert!(body.contains("### Retro"));
        assert!(body.contains("### Fabro Details"));
        assert!(body.contains("test.fabro"));
    }

    #[tokio::test]
    async fn build_pr_content_uses_plan_text_from_store_without_response_md() {
        let store = test_store();
        let run_store = store.create_run(&fixtures::RUN_1).await.unwrap();

        let run_spec = RunSpec {
            run_id:           fixtures::RUN_1,
            settings:         fabro_types::WorkflowSettings::default(),
            graph:            Graph::new("test"),
            workflow_slug:    Some("test".to_string()),
            source_directory: Some("/tmp/project".to_string()),
            git:              Some(fabro_types::GitContext {
                origin_url:   String::new(),
                branch:       "main".to_string(),
                sha:          None,
                dirty:        fabro_types::DirtyStatus::Clean,
                push_outcome: fabro_types::PreRunPushOutcome::NotAttempted,
            }),
            labels:           HashMap::new(),
            provenance:       None,
            manifest_blob:    None,
            definition_blob:  None,
            fork_source_ref:  None,
            in_place:         false,
        };
        append_event(&run_store, &fixtures::RUN_1, &Event::RunCreated {
            run_id:           fixtures::RUN_1,
            settings:         serde_json::to_value(&run_spec.settings).unwrap(),
            graph:            serde_json::to_value(&run_spec.graph).unwrap(),
            workflow_source:  Some("digraph test { plan -> code }".to_string()),
            workflow_config:  None,
            labels:           run_spec.labels.clone().into_iter().collect(),
            run_dir:          "/tmp/project".to_string(),
            source_directory: run_spec.source_directory.clone(),
            workflow_slug:    run_spec.workflow_slug.clone(),
            db_prefix:        None,
            provenance:       run_spec.provenance.clone(),
            manifest_blob:    None,
            git:              run_spec.git.clone(),
            fork_source_ref:  None,
            in_place:         false,
            web_url:          None,
        })
        .await
        .unwrap();
        append_event(&run_store, &fixtures::RUN_1, &Event::StageCompleted {
            node_id: "plan".to_string(),
            name: "plan".to_string(),
            index: 0,
            duration_ms: 1,
            status: "succeeded".to_string(),
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

        let body = build_pr_content_with_client(
            "diff --git a/src/lib.rs b/src/lib.rs\n+fn new_feature() {}\n",
            "Implement feature",
            "mock-model",
            &run_store.clone().into(),
            Some(&make_test_conclusion()),
            None,
            explicit_client(
                "mock",
                &pr_content_json("Mock title", "Narrative from mock."),
            ),
        )
        .await
        .unwrap()
        .body;

        assert!(body.contains("<summary>Full plan</summary>"));
        assert!(body.contains("Plan from store"));
    }

    #[tokio::test]
    async fn build_pr_content_uses_explicit_llm_client() {
        let store = test_store();
        let run_store = store.create_run(&fixtures::RUN_1).await.unwrap();
        let body = build_pr_content_with_client(
            "diff --git a/src/lib.rs b/src/lib.rs\n+fn new_feature() {}\n",
            "Implement feature",
            "gpt-5.4",
            &run_store.clone().into(),
            Some(&make_test_conclusion()),
            None,
            explicit_client(
                "openai",
                &pr_content_json("Explicit title", "Narrative from explicit client."),
            ),
        )
        .await
        .unwrap()
        .body;

        assert!(body.contains("Narrative from explicit client."));
        assert!(!body.contains("Narrative from mock."));
    }

    #[tokio::test]
    async fn build_pr_content_uses_vault_only_openai_codex_source() {
        let server = MockServer::start_async().await;
        let response_mock = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/v1/responses")
                    .header("authorization", "Bearer vault-openai-key");
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(openai_responses_payload(&pr_content_json(
                        "Vault title",
                        "Narrative from vault source.",
                    )));
            })
            .await;

        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::load(dir.path().join("secrets.json")).unwrap();
        vault
            .set(
                "openai_codex",
                &serde_json::to_string(&openai_api_key_credential("vault-openai-key")).unwrap(),
                SecretType::Credential,
                None,
            )
            .unwrap();
        let base_url = server.url("/v1");
        let llm_source: Arc<dyn CredentialSource> =
            Arc::new(VaultCredentialSource::with_env_lookup(
                Arc::new(AsyncRwLock::new(vault)),
                move |name| match name {
                    "OPENAI_BASE_URL" => Some(base_url.clone()),
                    _ => None,
                },
            ));

        let store = test_store();
        let run_store = store.create_run(&fixtures::RUN_1).await.unwrap();
        let run_store_handle: RunStoreHandle = run_store.into();

        let PrContent { title, body } = build_pr_content(
            "diff --git a/src/lib.rs b/src/lib.rs\n+fn new_feature() {}\n",
            "Implement feature",
            "gpt-5.4",
            &run_store_handle,
            llm_source.as_ref(),
            Some(&make_test_conclusion()),
            None,
        )
        .await
        .unwrap();

        assert_eq!(title, "Vault title");
        assert!(body.contains("Narrative from vault source."));
        response_mock.assert_async().await;
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
        assert_eq!(title.chars().count(), 72);
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

    #[test]
    fn truncation_caps_scale_with_context_window_and_clamp() {
        assert_eq!(
            truncation_caps_for_context_window(100_000),
            TruncationCaps {
                diff: 40_000,
                plan: 10_000,
            }
        );
        assert_eq!(
            truncation_caps_for_context_window(200_000),
            TruncationCaps {
                diff: 80_000,
                plan: 20_000,
            }
        );
        assert_eq!(
            truncation_caps_for_context_window(1_000_000),
            TruncationCaps {
                diff: 400_000,
                plan: 100_000,
            }
        );
        assert_eq!(
            truncation_caps_for_context_window(10_000_000),
            TruncationCaps {
                diff: 500_000,
                plan: 100_000,
            }
        );
        assert_eq!(truncation_caps("unknown-model"), TruncationCaps {
            diff: 80_000,
            plan: 20_000,
        });
    }

    #[tokio::test]
    async fn empty_diff_returns_none() {
        let store = test_store();
        let run_store = store.create_run(&fixtures::RUN_1).await.unwrap();
        let run_store_handle: RunStoreHandle = run_store.into();
        let llm_source = test_llm_source();
        let creds = fabro_github::GitHubCredentials::App(fabro_github::GitHubAppCredentials {
            app_id:          "123".to_string(),
            private_key_pem: "unused".to_string(),
            slug:            None,
        });
        let base_url = github_app::github_api_base_url();
        let result = maybe_open_pull_request(OpenPullRequestRequest {
            github:      github_app::GitHubContext::new(&creds, &base_url),
            origin_url:  "https://github.com/owner/repo.git",
            base_branch: "main",
            head_branch: "fabro/run/123",
            goal:        "Fix bug",
            diff:        "",
            model:       "claude-sonnet-4-20250514",
            draft:       false,
            auto_merge:  None,
            run_store:   &run_store_handle,
            llm_source:  llm_source.as_ref(),
            conclusion:  None,
            run_state:   None,
        })
        .await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[tokio::test]
    async fn load_pull_request_diff_uses_store_without_disk_patch() {
        let tmp = tempfile::tempdir().unwrap();
        let store = test_store();
        let run_store = store.create_run(&fixtures::RUN_1).await.unwrap();
        let run_spec = RunSpec {
            run_id:           fixtures::RUN_1,
            settings:         fabro_types::WorkflowSettings::default(),
            graph:            Graph::new("test"),
            workflow_slug:    None,
            source_directory: Some(tmp.path().display().to_string()),
            git:              None,
            labels:           std::collections::HashMap::new(),
            provenance:       None,
            manifest_blob:    None,
            definition_blob:  None,
            fork_source_ref:  None,
            in_place:         false,
        };
        append_event(&run_store, &fixtures::RUN_1, &Event::RunCreated {
            run_id:           fixtures::RUN_1,
            settings:         serde_json::to_value(&run_spec.settings).unwrap(),
            graph:            serde_json::to_value(&run_spec.graph).unwrap(),
            workflow_source:  None,
            workflow_config:  None,
            labels:           run_spec.labels.clone().into_iter().collect(),
            run_dir:          tmp.path().display().to_string(),
            source_directory: run_spec.source_directory.clone(),
            workflow_slug:    None,
            db_prefix:        None,
            provenance:       run_spec.provenance.clone(),
            manifest_blob:    None,
            git:              None,
            fork_source_ref:  None,
            in_place:         false,
            web_url:          None,
        })
        .await
        .unwrap();
        append_event(&run_store, &fixtures::RUN_1, &Event::WorkflowRunCompleted {
            duration_ms:          1,
            artifact_count:       0,
            status:               "succeeded".to_string(),
            reason:               SuccessReason::Completed,
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

    // ── Structured-output PR content tests ──────────────────────────────

    /// MockProvider returns an over-long title; builder must cap it at 72
    /// chars and end with `…`. Exercises [`enforce_title_cap`] inside
    /// [`build_pr_content_with_client`].
    #[tokio::test]
    async fn build_pr_content_truncates_long_title() {
        let store = test_store();
        let run_store = store.create_run(&fixtures::RUN_1).await.unwrap();
        let long_title = "x".repeat(200);
        let payload = pr_content_json(&long_title, "Body content.");
        let title = build_pr_content_with_client(
            "diff --git a/src/lib.rs b/src/lib.rs\n+fn x() {}\n",
            "Implement feature",
            "mock-model",
            &run_store.clone().into(),
            Some(&make_test_conclusion()),
            None,
            explicit_client("mock", &payload),
        )
        .await
        .unwrap()
        .title;

        assert_eq!(title.chars().count(), 72);
        assert!(title.ends_with('\u{2026}'));
    }

    #[tokio::test]
    async fn build_pr_content_uses_default_title_when_generated_and_goal_titles_empty() {
        let store = test_store();
        let run_store = store.create_run(&fixtures::RUN_1).await.unwrap();
        let payload = pr_content_json("", "Body content.");
        let title = build_pr_content_with_client(
            "diff --git a/src/lib.rs b/src/lib.rs\n+fn x() {}\n",
            "## Plan:",
            "mock-model",
            &run_store.clone().into(),
            Some(&make_test_conclusion()),
            None,
            explicit_client("mock", &payload),
        )
        .await
        .unwrap()
        .title;

        assert_eq!(title, DEFAULT_PR_TITLE);
    }

    /// Empty or whitespace-only bodies use the skeleton fallback instead of
    /// aborting PR creation.
    #[tokio::test]
    async fn build_pr_content_uses_skeleton_when_body_empty() {
        let store = test_store();
        let run_store = store.create_run(&fixtures::RUN_1).await.unwrap();

        let run_spec = RunSpec {
            run_id:           fixtures::RUN_1,
            settings:         fabro_types::WorkflowSettings::default(),
            graph:            Graph::new("test"),
            workflow_slug:    Some("test".to_string()),
            source_directory: Some("/tmp/project".to_string()),
            git:              None,
            labels:           HashMap::new(),
            provenance:       None,
            manifest_blob:    None,
            definition_blob:  None,
            fork_source_ref:  None,
            in_place:         false,
        };
        append_event(&run_store, &fixtures::RUN_1, &Event::RunCreated {
            run_id:           fixtures::RUN_1,
            settings:         serde_json::to_value(&run_spec.settings).unwrap(),
            graph:            serde_json::to_value(&run_spec.graph).unwrap(),
            workflow_source:  Some("digraph test { plan -> code }".to_string()),
            workflow_config:  None,
            labels:           run_spec.labels.clone().into_iter().collect(),
            run_dir:          "/tmp/project".to_string(),
            source_directory: run_spec.source_directory.clone(),
            workflow_slug:    run_spec.workflow_slug.clone(),
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
        append_event(&run_store, &fixtures::RUN_1, &Event::StageCompleted {
            node_id: "plan".to_string(),
            name: "plan".to_string(),
            index: 0,
            duration_ms: 1,
            status: "succeeded".to_string(),
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
        append_event(&run_store, &fixtures::RUN_1, &Event::RetroCompleted {
            duration_ms: 1,
            response:    Some(String::new()),
            retro:       Some(serde_json::to_value(make_test_retro()).unwrap()),
        })
        .await
        .unwrap();

        let payload = pr_content_json("Mock", "   \n");
        let body = build_pr_content_with_client(
            "diff --git a/src/lib.rs b/src/lib.rs\n+fn x() {}\n",
            "Implement feature",
            "mock-model",
            &run_store.clone().into(),
            Some(&make_test_conclusion()),
            None,
            explicit_client("mock", &payload),
        )
        .await
        .unwrap()
        .body;

        assert!(body.contains("The LLM did not produce a description"));
        assert!(body.contains("<summary>Full plan</summary>"));
        assert!(body.contains("Plan from store"));
        assert!(body.contains("### Retro"));
        assert!(body.contains("### Fabro Details"));
        assert!(body.contains("Generated with [Fabro](https://fabro.sh)"));
    }

    // ── maybe_open_pull_request fallback tests ──────────────────────────

    /// Set of mock servers and credentials for the `maybe_open_pull_request`
    /// fallback path. The builder's `Client::from_source` rebuilds the LLM
    /// client from the credential source, so the in-process MockProvider
    /// cannot intercept — we mock the OpenAI HTTP endpoint instead.
    struct FallbackHarness {
        _vault_dir:     tempfile::TempDir,
        // Held to keep the mock listener alive for the duration of the test;
        // the test interacts with it via `Client::from_source` (which goes
        // out via HTTP to the mock URL stored in `llm_source`).
        openai_server:  MockServer,
        github_server:  MockServer,
        openai_mock_id: usize,
        github_mock_id: usize,
        llm_source:     Arc<dyn CredentialSource>,
        creds:          fabro_github::GitHubCredentials,
        run_store:      RunStoreHandle,
    }

    impl FallbackHarness {
        async fn assert_mocks_called_once(&self) {
            httpmock::Mock::new(self.openai_mock_id, &self.openai_server)
                .assert_async()
                .await;
            httpmock::Mock::new(self.github_mock_id, &self.github_server)
                .assert_async()
                .await;
        }
    }

    /// Stand up an OpenAI mock that returns the given structured-output
    /// payload, a GitHub mock that accepts a PR creation, a vault-backed
    /// credential source, and a run store seeded with a non-empty
    /// `final_patch`.
    async fn setup_fallback_test_harness(openai_payload_text: &str) -> FallbackHarness {
        let openai_server = MockServer::start_async().await;
        let openai_mock = openai_server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/v1/responses")
                    .header("authorization", "Bearer vault-openai-key");
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(openai_responses_payload(openai_payload_text));
            })
            .await;

        let github_server = MockServer::start_async().await;
        let github_mock = github_server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/repos/owner/repo/pulls")
                    .header("authorization", "Bearer test-token");
                then.status(201)
                    .header("content-type", "application/json")
                    .json_body(serde_json::json!({
                        "number": 1,
                        "html_url": "https://example.test/owner/repo/pull/1",
                        "node_id": "PR_kwTest1",
                    }));
            })
            .await;

        let vault_dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::load(vault_dir.path().join("secrets.json")).unwrap();
        vault
            .set(
                "openai_codex",
                &serde_json::to_string(&openai_api_key_credential("vault-openai-key")).unwrap(),
                SecretType::Credential,
                None,
            )
            .unwrap();
        let base_url = openai_server.url("/v1");
        let llm_source: Arc<dyn CredentialSource> =
            Arc::new(VaultCredentialSource::with_env_lookup(
                Arc::new(AsyncRwLock::new(vault)),
                move |name| match name {
                    "OPENAI_BASE_URL" => Some(base_url.clone()),
                    _ => None,
                },
            ));

        let creds = fabro_github::GitHubCredentials::Token("test-token".to_string());

        let store = test_store();
        let run_store = store.create_run(&fixtures::RUN_1).await.unwrap();
        // Seed a non-empty `final_patch` so `load_pull_request_diff` returns
        // diff content and the early-return for empty diffs does not fire.
        let run_spec = RunSpec {
            run_id:           fixtures::RUN_1,
            settings:         fabro_types::WorkflowSettings::default(),
            graph:            Graph::new("test"),
            workflow_slug:    None,
            source_directory: None,
            git:              None,
            labels:           HashMap::new(),
            provenance:       None,
            manifest_blob:    None,
            definition_blob:  None,
            fork_source_ref:  None,
            in_place:         false,
        };
        append_event(&run_store, &fixtures::RUN_1, &Event::RunCreated {
            run_id:           fixtures::RUN_1,
            settings:         serde_json::to_value(&run_spec.settings).unwrap(),
            graph:            serde_json::to_value(&run_spec.graph).unwrap(),
            workflow_source:  None,
            workflow_config:  None,
            labels:           run_spec.labels.clone().into_iter().collect(),
            run_dir:          "/tmp/x".to_string(),
            source_directory: None,
            workflow_slug:    None,
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
        append_event(&run_store, &fixtures::RUN_1, &Event::WorkflowRunCompleted {
            duration_ms:          1,
            artifact_count:       0,
            status:               "succeeded".to_string(),
            reason:               SuccessReason::Completed,
            total_usd_micros:     None,
            final_git_commit_sha: None,
            final_patch:          Some(
                "diff --git a/src/lib.rs b/src/lib.rs\n+fn from_store() {}\n".to_string(),
            ),
            billing:              None,
        })
        .await
        .unwrap();

        let openai_mock_id = openai_mock.id;
        let github_mock_id = github_mock.id;

        FallbackHarness {
            _vault_dir: vault_dir,
            openai_server,
            github_server,
            openai_mock_id,
            github_mock_id,
            llm_source,
            creds,
            run_store: run_store.into(),
        }
    }

    /// LLM returns a usable body but an empty title; the content builder
    /// falls back to `pr_title_from_goal` (first line, decoration stripped)
    /// and PR creation succeeds with that title.
    #[tokio::test]
    async fn maybe_open_pull_request_falls_back_to_goal_title_when_llm_returns_empty_title() {
        let payload = pr_content_json("", "Narrative.");
        let harness = setup_fallback_test_harness(&payload).await;

        let github_base_url = harness.github_server.url("");
        let github = github_app::GitHubContext::new(&harness.creds, &github_base_url);

        let result = maybe_open_pull_request(OpenPullRequestRequest {
            github,
            origin_url: "https://github.com/owner/repo.git",
            base_branch: "main",
            head_branch: "fabro/run/123",
            goal: "Fix telemetry leak\n\ndetails...",
            diff: "diff --git a/src/lib.rs b/src/lib.rs\n+fn x() {}\n",
            model: "gpt-5.4",
            draft: false,
            auto_merge: None,
            run_store: &harness.run_store,
            llm_source: harness.llm_source.as_ref(),
            conclusion: None,
            run_state: None,
        })
        .await
        .expect("PR creation should succeed");

        let record = result.expect("PR record should be Some");
        assert_eq!(record.title, "Fix telemetry leak");
        harness.assert_mocks_called_once().await;
    }

    /// LLM returns an empty title; the content builder fallback still caps
    /// the deterministic goal title at 72 chars ending with `…`.
    #[tokio::test]
    async fn maybe_open_pull_request_caps_fallback_title_at_72_chars() {
        let payload = pr_content_json("", "Narrative.");
        let harness = setup_fallback_test_harness(&payload).await;

        let github_base_url = harness.github_server.url("");
        let github = github_app::GitHubContext::new(&harness.creds, &github_base_url);

        // Single ~200-char line, no `Plan:` / heading prefix, no newlines.
        let goal = "x".repeat(200);

        let result = maybe_open_pull_request(OpenPullRequestRequest {
            github,
            origin_url: "https://github.com/owner/repo.git",
            base_branch: "main",
            head_branch: "fabro/run/123",
            goal: &goal,
            diff: "diff --git a/src/lib.rs b/src/lib.rs\n+fn x() {}\n",
            model: "gpt-5.4",
            draft: false,
            auto_merge: None,
            run_store: &harness.run_store,
            llm_source: harness.llm_source.as_ref(),
            conclusion: None,
            run_state: None,
        })
        .await
        .expect("PR creation should succeed");

        let record = result.expect("PR record should be Some");
        assert_eq!(record.title.chars().count(), 72);
        assert!(record.title.ends_with('\u{2026}'));
        harness.assert_mocks_called_once().await;
    }
}
