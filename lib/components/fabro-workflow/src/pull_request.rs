use std::collections::HashSet;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use fabro_github::{self as github_app, ssh_url_to_https};
use fabro_graphviz::parser;
use fabro_llm::credentials::CredentialProvider;
use fabro_llm::lithos_catalog::Catalog;
use fabro_llm::{Client, ClientOptions, Request, selection};
use fabro_store::RunProjection;
use fabro_types::PullRequestLink;
use fabro_types::settings::run::MergeStrategy;
use fabro_util::text::strip_goal_decoration;
use lithos_llm::catalog::ProviderId;
use lithos_llm::types::{Cost, Message, Role};
use tokio::time::sleep;
use tracing::{debug, info, warn};

use crate::outcome::format_cost as outcome_format_cost;
use crate::records::{Conclusion, RunSpec};

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
/// body. The trailing programmatic sections (Plan `<details>`, Fabro Details,
/// footer) are appended after the LLM body — the prompt
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
fn truncation_caps(
    model: &str,
    eligible: &HashSet<ProviderId>,
    catalog: &Catalog,
) -> TruncationCaps {
    let ctx = selection::select(catalog, model, None, eligible)
        .ok()
        .and_then(|entry| entry.model.limits())
        .and_then(|limits| usize::try_from(limits.context_tokens).ok())
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
fn format_cost(cost: Option<Cost>) -> String {
    cost.map(|cost| cost.usd_micros as f64 / 1_000_000.0)
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
    let total_duration = format_duration_ms(conclusion.timing.wall_time_ms);
    let total_cost_str = format_cost(conclusion.usage.and_then(|usage| usage.cost));
    let stage_count = conclusion.stages.len();
    parts.push(format!(
        "<details>\n<summary>Ran {stage_count} {} in {total_duration} for {total_cost_str}</summary>",
        if stage_count == 1 { "stage" } else { "stages" }
    ));
    parts.push(String::new());

    parts.push("| Stage | Duration | Cost | Retries |".to_string());
    parts.push("|---|---|---|---|".to_string());
    for stage in &conclusion.stages {
        let dur = format_duration_ms(stage.timing.wall_time_ms);
        let cost = format_cost(stage.usage.cost);
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

    if !arc_details_section.is_empty() {
        parts.push(String::new());
        parts.push(arc_details_section.to_string());
    }

    parts.push(String::new());
    parts.push("\u{2692}\u{fe0f} Generated with [Fabro](https://fabro.sh)".to_string());

    parts.join("\n")
}

/// Build complete PR content by combining LLM-generated narrative with
/// deterministic fallbacks and programmatic sections.
pub async fn build_pr_content(
    diff: &str,
    goal: &str,
    model: &str,
    llm_source: Arc<dyn CredentialProvider>,
    catalog: Arc<Catalog>,
    conclusion: Option<&Conclusion>,
    run_state: Option<&RunProjection>,
) -> Result<PrContent, String> {
    let client = fabro_llm::build_client(
        Catalog::clone(&catalog),
        llm_source,
        ClientOptions::standard(),
    )
    .await
    .map_err(|e| format!("Failed to create LLM client: {e}"))?
    .client;

    build_pr_content_with_client(
        diff,
        goal,
        model,
        catalog.as_ref(),
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
    catalog: &Catalog,
    conclusion: Option<&Conclusion>,
    run_state: Option<&RunProjection>,
    client: Arc<Client>,
) -> Result<PrContent, String> {
    info!("Building PR content");

    let conclusion = conclusion.or_else(|| run_state.and_then(|state| state.conclusion.as_ref()));
    let plan_text = run_state.and_then(read_plan_text);
    let run_spec = run_state.map(|state| state.spec.clone());
    let dot_source = run_state.and_then(|state| state.spec.graph_source.clone());

    let eligible = client.available_providers().iter().cloned().collect();
    let caps = truncation_caps(model, &eligible, catalog);
    let truncated_diff = truncate_chars(diff, caps.diff);

    let prompt = if let Some(ref plan) = plan_text {
        let truncated_plan = truncate_chars(plan, caps.plan);
        format!(
            "Goal: {goal}\n\nPlan:\n```\n{truncated_plan}\n```\n\nDiff:\n```\n{truncated_diff}\n```"
        )
    } else {
        format!("Goal: {goal}\n\nDiff:\n```\n{truncated_diff}\n```")
    };

    let request = Request::builder()
        .model(model)
        .system(PR_BODY_SYSTEM_PROMPT)
        .message(Message::text(Role::User, prompt))
        .build()
        .map_err(|e| format!("invalid PR content request: {e}"))?;
    let completion = client
        .complete_object(request, "pr_content", PR_CONTENT_SCHEMA.clone())
        .await
        .map_err(|e| format!("LLM generation failed: {e}"))?;

    let generated: PrContent = serde_json::from_value(completion.object)
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

    let arc_details_section = conclusion
        .as_ref()
        .map(|c| format_arc_details_section(c, run_spec.as_ref(), dot_source.as_deref()))
        .unwrap_or_default();

    let body = assemble_pr_body(&llm_body, plan_text.as_deref(), &arc_details_section);

    info!("PR content generated");

    Ok(PrContent { title, body })
}

/// Auto-merge configuration for a pull request.
pub struct AutoMergeOptions {
    pub merge_strategy: MergeStrategy,
}

/// Inputs for [`open_pull_request`].
pub struct OpenPullRequestRequest<'a> {
    pub github:            github_app::GitHubContext<'a>,
    pub origin_url:        &'a str,
    pub base_branch:       &'a str,
    pub head_branch:       &'a str,
    /// Commit that must be visible at the remote branch before the PR is
    /// opened.
    pub expected_head_sha: &'a str,
    pub goal:              &'a str,
    pub diff:              &'a str,
    pub model:             &'a str,
    pub draft:             bool,
    pub auto_merge:        Option<AutoMergeOptions>,
    pub llm_source:        Arc<dyn CredentialProvider>,
    pub catalog:           Arc<Catalog>,
    pub conclusion:        Option<&'a Conclusion>,
    pub run_state:         Option<&'a RunProjection>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedPullRequest {
    pub link:        PullRequestLink,
    pub title:       String,
    pub base_branch: String,
    pub head_branch: String,
}

/// Adopt an open pull request that already exists for the head branch at the
/// expected commit, e.g. when GitHub created the pull request but the caller
/// stopped before persisting the result.
async fn reconcile_existing_pull_request(
    req: &OpenPullRequestRequest<'_>,
    owner: &str,
    repo: &str,
    context: &'static str,
) -> anyhow::Result<Option<CreatedPullRequest>> {
    let Some(existing) = github_app::find_open_pull_request(
        &req.github,
        owner,
        repo,
        req.base_branch,
        req.head_branch,
        req.expected_head_sha,
    )
    .await?
    else {
        return Ok(None);
    };
    info!(pr_url = %existing.html_url, pr_number = existing.number, context, "Existing pull request reconciled");
    enable_auto_merge_if_requested(
        &req.github,
        owner,
        repo,
        &existing.node_id,
        existing.number,
        req.auto_merge.as_ref(),
    )
    .await;
    Ok(Some(CreatedPullRequest {
        link:        PullRequestLink {
            owner:  owner.to_string(),
            repo:   repo.to_string(),
            number: existing.number,
        },
        title:       existing.title,
        base_branch: req.base_branch.to_string(),
        head_branch: req.head_branch.to_string(),
    }))
}

async fn enable_auto_merge_if_requested(
    github: &github_app::GitHubContext<'_>,
    owner: &str,
    repo: &str,
    node_id: &str,
    number: u64,
    options: Option<&AutoMergeOptions>,
) {
    let Some(options) = options else {
        return;
    };
    match github_app::enable_auto_merge(github, owner, repo, node_id, options.merge_strategy).await
    {
        Ok(()) => info!(pr_number = number, "Auto-merge enabled"),
        Err(err) => warn!(
            pr_number = number,
            error = %err,
            "Failed to enable auto-merge (repo may not have auto-merge enabled in settings)"
        ),
    }
}

/// How many times to read the remote branch head before giving up.
///
/// `GET /repos/{owner}/{repo}/branches/{branch}` is replica-served, so shortly
/// after the push that publish just made it can still report the previous
/// commit — or 404 for a branch that is new on the remote.
const BRANCH_HEAD_ATTEMPTS: u32 = 3;
const BRANCH_HEAD_RETRY_DELAY: Duration = Duration::from_millis(500);

/// Confirm the remote branch points at the run's final commit.
///
/// Publish failures are terminal, so a replica that has not caught up yet must
/// not be mistaken for a genuinely stale branch.
async fn verify_remote_head(
    req: &OpenPullRequestRequest<'_>,
    owner: &str,
    repo: &str,
) -> Result<(), String> {
    let mut last_seen = Ok(None);
    for attempt in 1..=BRANCH_HEAD_ATTEMPTS {
        last_seen = github_app::branch_head_sha(&req.github, owner, repo, req.head_branch).await;
        match &last_seen {
            Ok(Some(head)) if head == req.expected_head_sha => return Ok(()),
            Ok(head) => debug!(
                attempt,
                head = ?head,
                expected = req.expected_head_sha,
                "Remote branch head does not match the final commit yet"
            ),
            Err(err) => debug!(attempt, error = %err, "Failed to read remote branch head"),
        }
        if attempt < BRANCH_HEAD_ATTEMPTS {
            sleep(BRANCH_HEAD_RETRY_DELAY).await;
        }
    }

    Err(match last_seen {
        Ok(Some(head)) => format!(
            "remote branch '{}' points to commit {head}, expected final commit {}",
            req.head_branch, req.expected_head_sha
        ),
        Ok(None) => format!(
            "remote branch '{}' does not exist; expected final commit {}",
            req.head_branch, req.expected_head_sha
        ),
        Err(err) => format!("failed to verify remote branch head: {err:#}"),
    })
}

/// Open a pull request for a completed run.
///
/// Callers are responsible for skipping runs with an empty diff; reaching here
/// means a pull request is expected, so every failure is an error.
pub async fn open_pull_request(
    req: OpenPullRequestRequest<'_>,
) -> Result<CreatedPullRequest, String> {
    let https_url = ssh_url_to_https(req.origin_url);
    let (owner, repo) =
        github_app::parse_github_owner_repo(&https_url).map_err(|err| format!("{err:#}"))?;

    // Verify before generating content: this is the cheap check, and a stale
    // branch would otherwise cost a full LLM call before failing.
    verify_remote_head(&req, &owner, &repo).await?;

    if let Some(existing) = reconcile_existing_pull_request(&req, &owner, &repo, "before creation")
        .await
        .map_err(|err| format!("failed to reconcile an existing pull request: {err:#}"))?
    {
        return Ok(existing);
    }

    let content = build_pr_content(
        req.diff,
        req.goal,
        req.model,
        Arc::clone(&req.llm_source),
        Arc::clone(&req.catalog),
        req.conclusion,
        req.run_state,
    )
    .await
    .map_err(|err| format!("{err:#}"))?;
    let body = truncate_pr_body(&content.body);
    let title = content.title;

    let created = match github_app::create_pull_request(
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
    {
        Ok(created) => created,
        Err(create_err) => {
            match reconcile_existing_pull_request(&req, &owner, &repo, "after a failed create")
                .await
            {
                Ok(Some(existing)) => return Ok(existing),
                Ok(None) => return Err(format!("{create_err:#}")),
                Err(reconcile_err) => {
                    return Err(format!(
                        "{create_err:#}; failed to reconcile the pull request after creation: {reconcile_err:#}"
                    ));
                }
            }
        }
    };

    info!(pr_url = %created.html_url, created.number, "Pull request created");
    enable_auto_merge_if_requested(
        &req.github,
        &owner,
        &repo,
        &created.node_id,
        created.number,
        req.auto_merge.as_ref(),
    )
    .await;

    let link = PullRequestLink {
        owner,
        repo,
        number: created.number,
    };

    Ok(CreatedPullRequest {
        link,
        title,
        base_branch: req.base_branch.to_string(),
        head_branch: req.head_branch.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use chrono::Utc;
    use fabro_auth::VaultCredentialSource;
    use fabro_graphviz::graph::Graph;
    use fabro_llm::adapter::{ProviderAdapter, ResolvedCall};
    use fabro_llm::credentials::CredentialProvider;
    use fabro_llm::lithos_catalog::AdapterId;
    use fabro_llm::{Response, ResponseStream};
    use fabro_types::{
        PetriAdmission, RunProjection, RunSpec, WorkflowSettings, first_event_seq, fixtures,
        test_support,
    };
    use fabro_vault::{SecretType, Vault};
    use httpmock::Method::{GET, POST};
    use httpmock::MockServer;
    use lithos_llm::types::{ContentPart, CostSource, TokenCounts, Usage};
    use tokio::sync::RwLock as AsyncRwLock;

    use super::*;
    use crate::records::StageSummary;

    /// Answers every completion with one fixed text, attributed to the route
    /// that was asked.
    struct MockProvider {
        id:            AdapterId,
        response_text: String,
    }

    impl MockProvider {
        fn new(text: &str) -> Self {
            Self {
                id:            AdapterId::new("mock"),
                response_text: text.to_string(),
            }
        }

        fn response(&self, call: &ResolvedCall) -> Response {
            let handle = call.route().handle();
            let mut response =
                Response::new(handle.provider().clone(), handle.model().clone(), vec![
                    ContentPart::Text {
                        text: self.response_text.clone(),
                    },
                ]);
            response.id = Some("resp_1".to_string());
            response.usage = TokenCounts {
                input: 10,
                output: 20,
                ..TokenCounts::default()
            };
            response
        }
    }

    #[async_trait::async_trait]
    impl ProviderAdapter for MockProvider {
        fn id(&self) -> &AdapterId {
            &self.id
        }

        async fn complete(&self, call: &ResolvedCall) -> Result<Response, fabro_llm::Error> {
            Ok(self.response(call))
        }

        async fn stream(&self, call: &ResolvedCall) -> Result<ResponseStream, fabro_llm::Error> {
            Ok(fabro_llm::test_support::response_to_stream(
                self.response(call),
            ))
        }
    }

    fn test_catalog_with_provider_base_url(provider: &str, base_url: &str) -> Arc<Catalog> {
        Arc::new(fabro_llm::test_support::test_catalog_with_provider_base_url(provider, base_url))
    }

    /// The catalog every mock-backed test resolves against: the built-ins plus
    /// a `mock` provider that passes any model name through.
    fn mock_catalog() -> Catalog {
        fabro_llm::test_support::test_catalog_with_overlay(
            r#"
[providers.mock]
display_name = "Mock"
base_url = "http://mock.invalid/v1"
auth = { type = "bearer" }
allow_passthrough = true

[providers.mock.metadata.agent]
profile = "openai"

[providers.mock.models.mock-model]
display_name = "Mock Model"
api_model = "mock-model"
limits = { context_tokens = 8192, max_output_tokens = 1024 }
capabilities = { text = true, tools = true, response_format = { json_object = true, json_schema = true } }
"#,
        )
    }

    /// A client over [`mock_catalog`] whose `provider_name` answers with
    /// `text`.
    fn explicit_client(provider_name: &str, text: &str) -> Arc<Client> {
        let adapter: Arc<dyn ProviderAdapter> = Arc::new(MockProvider::new(text));
        let mut options = fabro_llm::ClientOptions::default();
        options
            .adapters
            .push((ProviderId::new(provider_name), adapter));
        Arc::new(
            fabro_llm::build_offline_client(mock_catalog(), options)
                .expect("mock client should build")
                .client,
        )
    }

    fn test_projection() -> RunProjection {
        RunProjection::new(
            "Test run".to_string(),
            RunSpec {
                run_id:              fixtures::RUN_1,
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
                definition_blob:     None,
                spec_blob:           None,
                git:                 None,
                fork_source_ref:     None,
                admission:           PetriAdmission::default(),
            },
            Utc::now(),
        )
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

    /// A usage with only a catalog cost, for the cost table.
    fn priced(usd_micros: u64) -> Usage {
        Usage {
            tokens: TokenCounts::default(),
            cost:   Some(Cost {
                usd_micros,
                source: CostSource::Catalog,
            }),
        }
    }

    fn make_test_conclusion() -> Conclusion {
        Conclusion {
            timestamp:            Utc::now(),
            status:               crate::outcome::StageOutcome::Succeeded,
            timing:               fabro_types::RunTiming::wall_only(150_000),
            failure:              None,
            final_git_commit_sha: None,
            stages:               vec![
                StageSummary {
                    stage_id:    "plan".to_string(),
                    stage_label: "plan".to_string(),
                    timing:      fabro_types::StageTiming::wall_only(45_000),
                    usage:       priced(120_000),
                    retries:     0,
                },
                StageSummary {
                    stage_id:    "implement".to_string(),
                    stage_label: "implement".to_string(),
                    timing:      fabro_types::StageTiming::wall_only(90_000),
                    usage:       priced(250_000),
                    retries:     0,
                },
                StageSummary {
                    stage_id:    "simplify".to_string(),
                    stage_label: "simplify".to_string(),
                    timing:      fabro_types::StageTiming::wall_only(15_000),
                    usage:       priced(50_000),
                    retries:     0,
                },
            ],
            usage:                Some(priced(420_000)),
            total_retries:        0,
            diff:                 fabro_types::RunDiff::default(),
        }
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
            stage.usage.cost = None;
        }
        conclusion.usage = None;
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
        let mut state = test_projection();
        state.stage_entry("plan", 1, first_event_seq(1)).response =
            Some("This is the plan".to_string());

        let result = read_plan_text(&state);
        assert_eq!(result, Some("This is the plan".to_string()));
    }

    #[test]
    fn read_plan_text_prefix_match() {
        let mut state = test_projection();
        state
            .stage_entry("planning", 1, first_event_seq(1))
            .response = Some("Planning content".to_string());

        let result = read_plan_text(&state);
        assert_eq!(result, Some("Planning content".to_string()));
    }

    #[test]
    fn read_plan_text_prefers_alphabetically_first_plan_node() {
        let mut state = test_projection();
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
        let mut state = test_projection();
        state.stage_entry("implement", 1, first_event_seq(1));

        let result = read_plan_text(&state);
        assert_eq!(result, None);
    }

    #[test]
    fn read_plan_text_empty_state() {
        let state = test_projection();
        let result = read_plan_text(&state);
        assert_eq!(result, None);
    }

    // ── assemble_pr_body tests ──────────────────────────────────────────

    #[test]
    fn assemble_all_sections() {
        let body = assemble_pr_body(
            "This is the narrative.\n\n### Plan Summary\n\n* Step 1\n* Step 2",
            Some("Full plan text here"),
            "### Fabro Details\n\n<details>...</details>",
        );

        assert!(body.contains("This is the narrative."));
        assert!(body.contains("### Plan Summary"));
        assert!(body.contains("<details>\n<summary>Full plan</summary>"));
        assert!(body.contains("````md\nFull plan text here\n````"));
        assert!(body.contains("### Fabro Details"));
    }

    #[test]
    fn assemble_no_plan() {
        let body = assemble_pr_body(
            "Narrative only.",
            None,
            "### Fabro Details\n\n<details>...</details>",
        );

        assert!(body.contains("Narrative only."));
        assert!(!body.contains("Full plan"));
        assert!(body.contains("### Fabro Details"));
    }

    #[test]
    fn assemble_no_details() {
        let body = assemble_pr_body("Narrative only.", Some("Plan"), "");

        assert!(body.contains("Narrative only."));
        assert!(body.contains("Full plan"));
        assert!(!body.contains("### Fabro Details"));
    }

    #[test]
    fn assemble_narrative_only() {
        let body = assemble_pr_body("Just the narrative.", None, "");

        assert_eq!(
            body,
            "Just the narrative.\n\n\u{2692}\u{fe0f} Generated with [Fabro](https://fabro.sh)"
        );
    }

    #[test]
    fn assemble_conclusion() {
        let conclusion = make_test_conclusion();
        let arc_details = format_arc_details_section(&conclusion, None, None);
        let body = assemble_pr_body("Narrative.", None, &arc_details);

        assert!(body.contains("### Fabro Details"));
        assert!(body.contains("Ran 3 stages"));
    }

    #[tokio::test]
    async fn build_pr_content_uses_in_memory_conclusion() {
        let PrContent { title, body } = build_pr_content_with_client(
            "diff --git a/src/lib.rs b/src/lib.rs\n+fn new_feature() {}\n",
            "Implement feature",
            "mock-model",
            &mock_catalog(),
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
    async fn build_pr_content_uses_explicit_llm_client() {
        let body = build_pr_content_with_client(
            "diff --git a/src/lib.rs b/src/lib.rs\n+fn new_feature() {}\n",
            "Implement feature",
            "gpt-5.4",
            &mock_catalog(),
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
                "OPENAI_API_KEY",
                "vault-openai-key",
                SecretType::Token,
                None,
            )
            .unwrap();
        let llm_source: Arc<dyn CredentialProvider> = Arc::new(VaultCredentialSource::new(
            Arc::new(AsyncRwLock::new(vault)),
        ));
        // Use catalog settings to override base_url instead of env var
        let catalog = test_catalog_with_provider_base_url("openai", &server.url("/v1"));

        let PrContent { title, body } = build_pr_content(
            "diff --git a/src/lib.rs b/src/lib.rs\n+fn new_feature() {}\n",
            "Implement feature",
            "gpt-5.4",
            llm_source,
            catalog,
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
        assert_eq!(
            truncation_caps(
                "unknown-model",
                &mock_catalog().enabled_provider_ids().into_iter().collect(),
                &mock_catalog(),
            ),
            TruncationCaps {
                diff: 80_000,
                plan: 20_000,
            }
        );
    }

    #[tokio::test]
    async fn stale_remote_branch_is_rejected_before_pull_request_creation() {
        let payload = pr_content_json("Fix bug", "Narrative.");
        let harness = setup_fallback_test_harness_with_branch_sha(&payload, "stale-sha").await;
        let github_base_url = harness.github_server.url("");
        let error = open_pull_request(OpenPullRequestRequest {
            github:            fabro_github::GitHubContext::new(&harness.creds, &github_base_url),
            origin_url:        "https://github.com/owner/repo.git",
            base_branch:       "main",
            head_branch:       "fabro/run/123",
            expected_head_sha: "final-sha",
            goal:              "Fix bug",
            diff:              "diff --git a/src/lib.rs b/src/lib.rs\n+fn x() {}\n",
            model:             "claude-sonnet-4-20250514",
            draft:             false,
            auto_merge:        None,
            llm_source:        Arc::clone(&harness.llm_source),
            catalog:           harness.catalog.clone(),
            conclusion:        None,
            run_state:         None,
        })
        .await
        .expect_err("stale remote branch must prevent PR creation");

        assert!(error.contains("stale-sha"));
        assert!(error.contains("final-sha"));
        // The branch is re-read to ride out replica lag...
        httpmock::Mock::new(harness.branch_mock_id, &harness.github_server)
            .assert_calls_async(BRANCH_HEAD_ATTEMPTS as usize)
            .await;
        // ...but the check runs first, so no LLM call and no PR creation.
        httpmock::Mock::new(harness.openai_mock_id, &harness.openai_server)
            .assert_calls_async(0)
            .await;
        httpmock::Mock::new(harness.github_mock_id, &harness.github_server)
            .assert_calls_async(0)
            .await;
    }

    // ── Structured-output PR content tests ──────────────────────────────

    /// MockProvider returns an over-long title; builder must cap it at 72
    /// chars and end with `…`. Exercises [`enforce_title_cap`] inside
    /// [`build_pr_content_with_client`].
    #[tokio::test]
    async fn build_pr_content_truncates_long_title() {
        let long_title = "x".repeat(200);
        let payload = pr_content_json(&long_title, "Body content.");
        let title = build_pr_content_with_client(
            "diff --git a/src/lib.rs b/src/lib.rs\n+fn x() {}\n",
            "Implement feature",
            "mock-model",
            &mock_catalog(),
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
        let payload = pr_content_json("", "Body content.");
        let title = build_pr_content_with_client(
            "diff --git a/src/lib.rs b/src/lib.rs\n+fn x() {}\n",
            "## Plan:",
            "mock-model",
            &mock_catalog(),
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
        // The plan node's response is what the body quotes as the plan.
        let mut state = test_projection();
        state.stage_entry("plan", 1, first_event_seq(1)).response =
            Some("Plan from store".to_string());
        let payload = pr_content_json("Mock", "   \n");
        let body = build_pr_content_with_client(
            "diff --git a/src/lib.rs b/src/lib.rs\n+fn x() {}\n",
            "Implement feature",
            "mock-model",
            &mock_catalog(),
            Some(&make_test_conclusion()),
            Some(&state),
            explicit_client("mock", &payload),
        )
        .await
        .unwrap()
        .body;

        assert!(body.contains("The LLM did not produce a description"));
        assert!(body.contains("<summary>Full plan</summary>"));
        assert!(body.contains("Plan from store"));
        assert!(body.contains("### Fabro Details"));
        assert!(body.contains("Generated with [Fabro](https://fabro.sh)"));
    }

    // ── open_pull_request fallback tests ──────────────────────────

    /// Set of mock servers and credentials for the `open_pull_request`
    /// fallback path. The builder's `Client::from_source` rebuilds the LLM
    /// client from the credential source, so the in-process MockProvider
    /// cannot intercept — we mock the OpenAI HTTP endpoint instead.
    struct FallbackHarness {
        _vault_dir:        tempfile::TempDir,
        // Held to keep the mock listener alive for the duration of the test;
        // the test interacts with it via `Client::from_source` (which goes
        // out via HTTP to the mock URL stored in `llm_source`).
        openai_server:     MockServer,
        github_server:     MockServer,
        openai_mock_id:    usize,
        branch_mock_id:    usize,
        reconcile_mock_id: usize,
        github_mock_id:    usize,
        llm_source:        Arc<dyn CredentialProvider>,
        catalog:           Arc<Catalog>,
        creds:             fabro_github::GitHubCredentials,
    }

    impl FallbackHarness {
        async fn assert_mocks_called_once(&self) {
            httpmock::Mock::new(self.openai_mock_id, &self.openai_server)
                .assert_async()
                .await;
            httpmock::Mock::new(self.branch_mock_id, &self.github_server)
                .assert_async()
                .await;
            httpmock::Mock::new(self.reconcile_mock_id, &self.github_server)
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
        setup_fallback_test_harness_with_branch_sha(openai_payload_text, "final-sha").await
    }

    async fn setup_fallback_test_harness_with_branch_sha(
        openai_payload_text: &str,
        branch_sha: &str,
    ) -> FallbackHarness {
        setup_fallback_test_harness_with(openai_payload_text, branch_sha, serde_json::json!([]))
            .await
    }

    async fn setup_fallback_test_harness_with(
        openai_payload_text: &str,
        branch_sha: &str,
        reconcile_response: serde_json::Value,
    ) -> FallbackHarness {
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
        let branch_sha = branch_sha.to_string();
        let branch_mock = github_server
            .mock_async(move |when, then| {
                when.method(GET)
                    .path("/repos/owner/repo/branches/fabro/run/123")
                    .header("authorization", "Bearer test-token");
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(serde_json::json!({
                        "commit": { "sha": branch_sha }
                    }));
            })
            .await;
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
        let reconcile_mock = github_server
            .mock_async(move |when, then| {
                when.method(GET)
                    .path("/repos/owner/repo/pulls")
                    .query_param("state", "open")
                    .query_param("base", "main")
                    .query_param("head", "owner:fabro/run/123")
                    .header("authorization", "Bearer test-token");
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(reconcile_response);
            })
            .await;

        let vault_dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::load(vault_dir.path().join("secrets.json")).unwrap();
        vault
            .set(
                "OPENAI_API_KEY",
                "vault-openai-key",
                SecretType::Token,
                None,
            )
            .unwrap();
        let llm_source: Arc<dyn CredentialProvider> = Arc::new(VaultCredentialSource::new(
            Arc::new(AsyncRwLock::new(vault)),
        ));
        // Use catalog settings to override base_url instead of env var
        let catalog = test_catalog_with_provider_base_url("openai", &openai_server.url("/v1"));

        let creds = fabro_github::GitHubCredentials::Pat("test-token".to_string());

        let openai_mock_id = openai_mock.id;
        let branch_mock_id = branch_mock.id;
        let reconcile_mock_id = reconcile_mock.id;
        let github_mock_id = github_mock.id;

        FallbackHarness {
            _vault_dir: vault_dir,
            openai_server,
            github_server,
            openai_mock_id,
            branch_mock_id,
            reconcile_mock_id,
            github_mock_id,
            llm_source,
            catalog,
            creds,
        }
    }

    /// An open pull request already exists for the head branch at the
    /// expected commit — for example after a crash between GitHub creating
    /// the pull request and the caller persisting it. `open_pull_request`
    /// adopts it without an LLM call and without a create request.
    #[tokio::test]
    async fn open_pull_request_adopts_an_existing_pull_request_without_creating() {
        let payload = pr_content_json("Unused", "Unused.");
        let harness = setup_fallback_test_harness_with(
            &payload,
            "final-sha",
            serde_json::json!([{
                "html_url": "https://github.com/owner/repo/pull/7",
                "number": 7,
                "node_id": "PR_existing",
                "title": "Reconciled title",
                "head": {"sha": "final-sha"}
            }]),
        )
        .await;

        let github_base_url = harness.github_server.url("");
        let github = github_app::GitHubContext::new(&harness.creds, &github_base_url);

        let result = open_pull_request(OpenPullRequestRequest {
            github,
            origin_url: "https://github.com/owner/repo.git",
            base_branch: "main",
            head_branch: "fabro/run/123",
            expected_head_sha: "final-sha",
            goal: "Fix telemetry leak",
            diff: "diff --git a/src/lib.rs b/src/lib.rs\n+fn x() {}\n",
            model: "gpt-5.4",
            draft: false,
            auto_merge: None,
            llm_source: Arc::clone(&harness.llm_source),
            catalog: harness.catalog.clone(),
            conclusion: None,
            run_state: None,
        })
        .await
        .expect("reconciliation should adopt the existing pull request");

        assert_eq!(result.link.number, 7);
        assert_eq!(result.title, "Reconciled title");
        // Adoption must not cost an LLM call or a create request.
        assert_eq!(
            httpmock::Mock::new(harness.openai_mock_id, &harness.openai_server)
                .calls_async()
                .await,
            0
        );
        assert_eq!(
            httpmock::Mock::new(harness.github_mock_id, &harness.github_server)
                .calls_async()
                .await,
            0
        );
    }

    /// LLM returns a usable body but an empty title; the content builder
    /// falls back to `pr_title_from_goal` (first line, decoration stripped)
    /// and PR creation succeeds with that title.
    #[tokio::test]
    async fn open_pull_request_falls_back_to_goal_title_when_llm_returns_empty_title() {
        let payload = pr_content_json("", "Narrative.");
        let harness = setup_fallback_test_harness(&payload).await;

        let github_base_url = harness.github_server.url("");
        let github = github_app::GitHubContext::new(&harness.creds, &github_base_url);

        let result = open_pull_request(OpenPullRequestRequest {
            github,
            origin_url: "https://github.com/owner/repo.git",
            base_branch: "main",
            head_branch: "fabro/run/123",
            expected_head_sha: "final-sha",
            goal: "Fix telemetry leak\n\ndetails...",
            diff: "diff --git a/src/lib.rs b/src/lib.rs\n+fn x() {}\n",
            model: "gpt-5.4",
            draft: false,
            auto_merge: None,
            llm_source: Arc::clone(&harness.llm_source),
            catalog: harness.catalog.clone(),
            conclusion: None,
            run_state: None,
        })
        .await
        .expect("PR creation should succeed");

        assert_eq!(result.title, "Fix telemetry leak");
        harness.assert_mocks_called_once().await;
    }

    /// LLM returns an empty title; the content builder fallback still caps
    /// the deterministic goal title at 72 chars ending with `…`.
    #[tokio::test]
    async fn open_pull_request_caps_fallback_title_at_72_chars() {
        let payload = pr_content_json("", "Narrative.");
        let harness = setup_fallback_test_harness(&payload).await;

        let github_base_url = harness.github_server.url("");
        let github = github_app::GitHubContext::new(&harness.creds, &github_base_url);

        // Single ~200-char line, no `Plan:` / heading prefix, no newlines.
        let goal = "x".repeat(200);

        let result = open_pull_request(OpenPullRequestRequest {
            github,
            origin_url: "https://github.com/owner/repo.git",
            base_branch: "main",
            head_branch: "fabro/run/123",
            expected_head_sha: "final-sha",
            goal: &goal,
            diff: "diff --git a/src/lib.rs b/src/lib.rs\n+fn x() {}\n",
            model: "gpt-5.4",
            draft: false,
            auto_merge: None,
            llm_source: Arc::clone(&harness.llm_source),
            catalog: harness.catalog.clone(),
            conclusion: None,
            run_state: None,
        })
        .await
        .expect("PR creation should succeed");

        let title = result.title;
        assert_eq!(title.chars().count(), 72);
        assert!(title.ends_with('\u{2026}'));
        harness.assert_mocks_called_once().await;
    }
}
