use std::path::Path;

use fabro_config::run::MergeStrategy;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use fabro_github::{self as github_app, ssh_url_to_https, GitHubAppCredentials};

use crate::conclusion::Conclusion;
use fabro_retro::retro::Retro;

/// Record of a pull request created for a workflow run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequestRecord {
    pub html_url: String,
    pub number: u64,
    pub owner: String,
    pub repo: String,
    pub base_branch: String,
    pub head_branch: String,
    pub title: String,
}

impl PullRequestRecord {
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize pull_request.json: {e}"))?;
        std::fs::write(path, json).map_err(|e| format!("Failed to write pull_request.json: {e}"))
    }
}

/// Derive a PR title from the workflow goal.
///
/// Uses the first line, truncated to 120 characters for readability.
fn pr_title_from_goal(goal: &str) -> String {
    let stripped = fabro_util::text::strip_goal_decoration(goal);
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
fn format_cost(cost: Option<f64>) -> String {
    cost.map(crate::cost::format_cost)
        .unwrap_or_else(|| "\u{2013}".to_string())
}

/// Format a duration in milliseconds as a human-readable string.
fn format_duration_ms(ms: u64) -> String {
    let secs = ms / 1000;
    if secs >= 60 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}s", secs)
    }
}

/// Format the Retro section of the PR body.
///
/// Renders stats, friction points, and open items. Omits sub-sections when empty.
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
/// optionally a DOT graph in another `<details>` block.
fn format_arc_details_section(conclusion: &Conclusion, dot_source: Option<&str>) -> String {
    let mut parts = Vec::new();
    parts.push("### Fabro Details".to_string());
    parts.push(String::new());

    // Cost table
    let total_duration = format_duration_ms(conclusion.duration_ms);
    let total_cost_str = format_cost(conclusion.total_cost);
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
        let cost = format_cost(stage.cost);
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

    // DOT graph
    if let Some(dot) = dot_source {
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
    match fabro_graphviz::parser::parse(dot) {
        Ok(graph) => (
            format!("{}.fabro", graph.name),
            graph.nodes.len(),
            graph.edges.len(),
        ),
        Err(_) => ("workflow.fabro".to_string(), 0, 0),
    }
}

/// Read the workflow graph source from `run_dir/graph.fabro` (or `graph.dot` fallback).
fn read_dot_source(run_dir: &Path) -> Option<String> {
    let fabro_path = run_dir.join("graph.fabro");
    if let Ok(content) = std::fs::read_to_string(&fabro_path) {
        debug!(path = %fabro_path.display(), "Read workflow graph for PR body");
        return Some(content);
    }
    let dot_path = run_dir.join("graph.dot");
    match std::fs::read_to_string(&dot_path) {
        Ok(content) => {
            debug!(path = %dot_path.display(), "Read workflow graph for PR body (dot fallback)");
            Some(content)
        }
        Err(_) => None,
    }
}

/// Read plan text from the first `nodes/plan*/response.md` found in run_dir.
///
/// Entries are sorted alphabetically so `plan` is preferred over `planning`.
fn read_plan_text(run_dir: &Path) -> Option<String> {
    let nodes_dir = run_dir.join("nodes");
    let mut entries: Vec<_> = std::fs::read_dir(&nodes_dir).ok()?.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let dir_name = entry.file_name();
        let dir_name_str = dir_name.to_string_lossy();
        if dir_name_str.starts_with("plan") && entry.file_type().is_ok_and(|ft| ft.is_dir()) {
            let response_path = entry.path().join("response.md");
            if let Ok(content) = std::fs::read_to_string(&response_path) {
                debug!(node_dir = %dir_name_str, "Found plan node response for PR body");
                return Some(content);
            }
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

/// Build a complete PR body by combining LLM-generated narrative with
/// programmatic sections (plan, retro, fabro details).
pub async fn build_pr_body(
    diff: &str,
    goal: &str,
    model: &str,
    run_dir: &Path,
) -> Result<String, String> {
    debug!("Building PR body");

    let plan_text = read_plan_text(run_dir);
    let conclusion = Conclusion::load(&run_dir.join("conclusion.json")).ok();
    let retro = Retro::load(run_dir).ok();
    let dot_source = read_dot_source(run_dir);

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
        format!("Goal: {goal}\n\nPlan:\n```\n{truncated_plan}\n```\n\nDiff:\n```\n{truncated_diff}\n```")
    } else {
        format!("Goal: {goal}\n\nDiff:\n```\n{truncated_diff}\n```")
    };

    let params = fabro_llm::generate::GenerateParams::new(model)
        .system(system)
        .prompt(prompt);

    let result = fabro_llm::generate::generate(params)
        .await
        .map_err(|e| format!("LLM generation failed: {e}"))?;

    let llm_output = result.response.text();

    let retro_section = retro.as_ref().map(format_retro_section).unwrap_or_default();
    let arc_details_section = conclusion
        .as_ref()
        .map(|c| format_arc_details_section(c, dot_source.as_deref()))
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
pub struct AutoMergeConfig {
    pub merge_strategy: MergeStrategy,
}

/// Optionally open a pull request after a successful workflow run.
///
/// Returns `Ok(Some(PullRequestRecord))` if a PR was created, `Ok(None)` if
/// the diff was empty, or `Err` on failure.
#[allow(clippy::too_many_arguments)]
pub async fn maybe_open_pull_request(
    creds: &GitHubAppCredentials,
    origin_url: &str,
    base_branch: &str,
    head_branch: &str,
    goal: &str,
    diff: &str,
    model: &str,
    draft: bool,
    auto_merge: Option<AutoMergeConfig>,
    run_dir: &Path,
) -> Result<Option<PullRequestRecord>, String> {
    if diff.is_empty() {
        debug!("Empty diff, skipping pull request creation");
        return Ok(None);
    }

    let https_url = ssh_url_to_https(origin_url);
    let (owner, repo) = github_app::parse_github_owner_repo(&https_url)?;

    let body = build_pr_body(diff, goal, model, run_dir).await?;
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
    )
    .await?;

    info!(pr_url = %created.html_url, created.number, "Pull request created");

    if let Some(am_cfg) = auto_merge {
        let merge_method = match am_cfg.merge_strategy {
            MergeStrategy::Squash => github_app::AutoMergeMethod::Squash,
            MergeStrategy::Merge => github_app::AutoMergeMethod::Merge,
            MergeStrategy::Rebase => github_app::AutoMergeMethod::Rebase,
        };
        match github_app::enable_auto_merge(creds, &owner, &repo, &created.node_id, merge_method)
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

    Ok(Some(PullRequestRecord {
        html_url: created.html_url,
        number: created.number,
        owner,
        repo,
        base_branch: base_branch.to_string(),
        head_branch: head_branch.to_string(),
        title,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conclusion::StageSummary;
    use chrono::Utc;
    use fabro_retro::retro::{
        AggregateStats, FrictionKind, FrictionPoint, OpenItem, OpenItemKind, StageRetro,
    };

    fn make_test_conclusion() -> Conclusion {
        Conclusion {
            timestamp: Utc::now(),
            status: crate::outcome::StageStatus::Success,
            duration_ms: 150_000,
            failure_reason: None,
            final_git_commit_sha: None,
            stages: vec![
                StageSummary {
                    stage_id: "plan".to_string(),
                    stage_label: "plan".to_string(),
                    duration_ms: 45_000,
                    cost: Some(0.12),
                    retries: 0,
                },
                StageSummary {
                    stage_id: "implement".to_string(),
                    stage_label: "implement".to_string(),
                    duration_ms: 90_000,
                    cost: Some(0.25),
                    retries: 0,
                },
                StageSummary {
                    stage_id: "simplify".to_string(),
                    stage_label: "simplify".to_string(),
                    duration_ms: 15_000,
                    cost: Some(0.05),
                    retries: 0,
                },
            ],
            total_cost: Some(0.42),
            total_retries: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_read_tokens: 0,
            total_cache_write_tokens: 0,
            total_reasoning_tokens: 0,
            has_pricing: true,
        }
    }

    fn make_test_retro() -> Retro {
        Retro {
            run_id: "test-run".to_string(),
            workflow_name: "implement".to_string(),
            goal: "Fix the bug".to_string(),
            timestamp: Utc::now(),
            smoothness: None,
            stages: vec![
                StageRetro {
                    stage_id: "plan".to_string(),
                    stage_label: "plan".to_string(),
                    status: "success".to_string(),
                    duration_ms: 45_000,
                    retries: 0,
                    cost: Some(0.12),
                    notes: None,
                    failure_reason: None,
                    files_touched: vec![],
                },
                StageRetro {
                    stage_id: "implement".to_string(),
                    stage_label: "implement".to_string(),
                    status: "success".to_string(),
                    duration_ms: 90_000,
                    retries: 0,
                    cost: Some(0.25),
                    notes: None,
                    failure_reason: None,
                    files_touched: vec!["src/main.rs".to_string(), "src/lib.rs".to_string()],
                },
                StageRetro {
                    stage_id: "simplify".to_string(),
                    stage_label: "simplify".to_string(),
                    status: "success".to_string(),
                    duration_ms: 15_000,
                    retries: 0,
                    cost: Some(0.05),
                    notes: None,
                    failure_reason: None,
                    files_touched: vec![],
                },
            ],
            stats: AggregateStats {
                total_duration_ms: 150_000,
                total_cost: Some(0.42),
                total_retries: 0,
                files_touched: vec!["src/lib.rs".to_string(), "src/main.rs".to_string()],
                stages_completed: 3,
                stages_failed: 0,
            },
            intent: None,
            outcome: None,
            learnings: None,
            friction_points: Some(vec![
                FrictionPoint {
                    kind: FrictionKind::ToolFailure,
                    description: "Daytona sandbox didn't have cargo on PATH".to_string(),
                    stage_id: None,
                },
                FrictionPoint {
                    kind: FrictionKind::Timeout,
                    description: "Proxy timeouts during cold compilations".to_string(),
                    stage_id: None,
                },
            ]),
            open_items: Some(vec![OpenItem {
                kind: OpenItemKind::TechDebt,
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
            run_id: "test".to_string(),
            workflow_name: "test".to_string(),
            goal: "test".to_string(),
            timestamp: Utc::now(),
            smoothness: None,
            stages: vec![],
            stats: AggregateStats {
                total_duration_ms: 0,
                total_cost: None,
                total_retries: 0,
                files_touched: vec![],
                stages_completed: 0,
                stages_failed: 0,
            },
            intent: None,
            outcome: None,
            learnings: None,
            friction_points: None,
            open_items: None,
        };
        let section = format_retro_section(&retro);

        assert!(section.contains("0 stages completed, 0 failed, 0 retries"));
        assert!(section.contains("0 files modified"));
    }

    // ── format_arc_details_section tests ────────────────────────────────

    #[test]
    fn format_arc_details_cost_table() {
        let conclusion = make_test_conclusion();
        let section = format_arc_details_section(&conclusion, None);

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
            stage.cost = None;
        }
        conclusion.total_cost = None;
        let section = format_arc_details_section(&conclusion, None);

        // En-dash for missing costs
        assert!(section.contains("| plan | 45s | \u{2013} | 0 |"));
        assert!(section.contains("for \u{2013}"));
    }

    #[test]
    fn format_arc_details_with_dot_graph() {
        let conclusion = make_test_conclusion();
        let dot = "digraph implement {\n  plan [type=\"agent\"]\n  code [type=\"agent\"]\n  plan -> code\n}\n";
        let section = format_arc_details_section(&conclusion, Some(dot));

        assert!(section.contains("<code>implement.fabro</code>"));
        assert!(section.contains("2 nodes and 1 edge"));
        assert!(section.contains("```dot"));
        assert!(section.contains("digraph implement"));
    }

    // ── read_plan_text tests ────────────────────────────────────────────

    #[test]
    fn read_plan_text_found() {
        let tmp = tempfile::tempdir().unwrap();
        let plan_dir = tmp.path().join("nodes").join("plan");
        std::fs::create_dir_all(&plan_dir).unwrap();
        std::fs::write(plan_dir.join("response.md"), "This is the plan").unwrap();

        let result = read_plan_text(tmp.path());
        assert_eq!(result, Some("This is the plan".to_string()));
    }

    #[test]
    fn read_plan_text_prefix_match() {
        let tmp = tempfile::tempdir().unwrap();
        let plan_dir = tmp.path().join("nodes").join("planning");
        std::fs::create_dir_all(&plan_dir).unwrap();
        std::fs::write(plan_dir.join("response.md"), "Planning content").unwrap();

        let result = read_plan_text(tmp.path());
        assert_eq!(result, Some("Planning content".to_string()));
    }

    #[test]
    fn read_plan_text_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let nodes_dir = tmp.path().join("nodes").join("implement");
        std::fs::create_dir_all(nodes_dir).unwrap();

        let result = read_plan_text(tmp.path());
        assert_eq!(result, None);
    }

    #[test]
    fn read_plan_text_no_nodes_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let result = read_plan_text(tmp.path());
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
        let arc_details = format_arc_details_section(&conclusion, None);
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
        let arc_details = format_arc_details_section(&conclusion, None);
        let body = assemble_pr_body("Narrative.", None, &retro_section, &arc_details);

        assert!(body.contains("### Retro"));
        assert!(body.contains("### Fabro Details"));
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

    // ── read_dot_source tests ───────────────────────────────────────────

    #[test]
    fn read_dot_source_found() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("graph.fabro"), "digraph test {}").unwrap();
        let result = read_dot_source(tmp.path());
        assert_eq!(result, Some("digraph test {}".to_string()));
    }

    #[test]
    fn read_dot_source_dot_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("graph.dot"), "digraph old {}").unwrap();
        let result = read_dot_source(tmp.path());
        assert_eq!(result, Some("digraph old {}".to_string()));
    }

    #[test]
    fn read_dot_source_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let result = read_dot_source(tmp.path());
        assert_eq!(result, None);
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

    #[test]
    fn pull_request_record_save_writes_json() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("pull_request.json");
        let record = PullRequestRecord {
            html_url: "https://github.com/owner/repo/pull/42".to_string(),
            number: 42,
            owner: "owner".to_string(),
            repo: "repo".to_string(),
            base_branch: "main".to_string(),
            head_branch: "fabro/run/abc".to_string(),
            title: "Fix the thing".to_string(),
        };
        record.save(&path).unwrap();

        let content: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(content["html_url"], "https://github.com/owner/repo/pull/42");
        assert_eq!(content["number"], 42);
        assert_eq!(content["owner"], "owner");
        assert_eq!(content["repo"], "repo");
        assert_eq!(content["base_branch"], "main");
        assert_eq!(content["head_branch"], "fabro/run/abc");
        assert_eq!(content["title"], "Fix the thing");
    }

    #[tokio::test]
    async fn empty_diff_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let creds = GitHubAppCredentials {
            app_id: "123".to_string(),
            private_key_pem: "unused".to_string(),
        };
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
            tmp.path(),
        )
        .await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }
}
