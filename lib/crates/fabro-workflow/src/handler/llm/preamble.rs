use std::collections::{HashMap, HashSet};
use std::fmt::Write;

use fabro_graphviz::graph::{Graph, Node, is_llm_handler_type};

use crate::artifact::{artifact_path, format_artifact_reference};
use crate::context::{Context, WorkflowContext, keys};
use crate::outcome::{Outcome, OutcomeExt};

const COMPACT_OUTPUT_MAX_LINES: usize = 25;
const SUMMARY_HIGH_OUTPUT_MAX_LINES: usize = 50;

/// Build a fidelity-appropriate preamble string for non-full context modes.
///
/// The preamble provides prior conversation context to the next LLM session,
/// tailored by the fidelity mode:
/// - `Truncate`: Only graph goal and run ID
/// - `Compact`: Nested-bullet summary with handler-specific sub-items
/// - `SummaryLow`: Brief textual summary (~600 token target)
/// - `SummaryMedium`: Moderate detail (~1500 token target)
/// - `SummaryHigh`: Detailed per-stage Markdown report
/// - `Full`: Returns empty string (full-fidelity nodes share a thread)
#[must_use]
pub fn build_preamble(
    fidelity: keys::Fidelity,
    context: &Context,
    graph: &Graph,
    completed_nodes: &[String],
    node_outcomes: &HashMap<String, Outcome>,
) -> String {
    use keys::Fidelity;

    let goal = graph.goal();
    let run_id = context.run_id();

    let preamble = match fidelity {
        Fidelity::Full => String::new(),
        Fidelity::Truncate => {
            format!("Goal: {goal}\nRun ID: {run_id}\n")
        }
        Fidelity::Compact => {
            build_compact_preamble(goal, completed_nodes, node_outcomes, context, graph)
        }
        Fidelity::SummaryLow => build_summary_preamble(
            goal,
            &run_id,
            completed_nodes,
            node_outcomes,
            context,
            graph,
            SummaryDetail::Low,
        ),
        Fidelity::SummaryMedium => build_summary_preamble(
            goal,
            &run_id,
            completed_nodes,
            node_outcomes,
            context,
            graph,
            SummaryDetail::Medium,
        ),
        Fidelity::SummaryHigh => build_summary_preamble(
            goal,
            &run_id,
            completed_nodes,
            node_outcomes,
            context,
            graph,
            SummaryDetail::High,
        ),
    };

    let parent_preamble = context.get_string(keys::INTERNAL_PARENT_PREAMBLE, "");
    if !parent_preamble.is_empty() && !preamble.is_empty() {
        format!(
            "## Parent workflow context\n{parent_preamble}\n\n## Current sub-workflow\n{preamble}"
        )
    } else {
        preamble
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn is_meta_handler(graph: &Graph, node_id: &str) -> bool {
    graph
        .nodes
        .get(node_id)
        .and_then(|n| n.handler_type())
        .is_some_and(|h| h == "start" || h == "exit")
}

fn is_blank_value(val: Option<&serde_json::Value>) -> bool {
    val.and_then(|v| v.as_str()).is_some_and(str::is_empty)
}

fn is_context_key_excluded(key: &str) -> bool {
    key.starts_with(keys::INTERNAL_PREFIX)
        || key.starts_with(keys::CURRENT_PREFIX)
        || key.starts_with(keys::GRAPH_PREFIX)
        || key.starts_with(keys::THREAD_PREFIX)
        || key.starts_with(keys::RESPONSE_PREFIX)
        || key == keys::OUTCOME
        || key == keys::LAST_STAGE
        || key == keys::LAST_RESPONSE
        || key == keys::PREFERRED_LABEL
}

fn format_value(val: &serde_json::Value) -> String {
    match val.as_str() {
        Some(s) => s.to_string(),
        None => val.to_string(),
    }
}

fn format_token_count(tokens: i64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}m", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1000 {
        format!("{:.1}k", tokens as f64 / 1000.0)
    } else {
        tokens.to_string()
    }
}

fn tail_lines(text: &str, max_lines: usize, indent: &str) -> String {
    use std::fmt::Write;

    let total = text.lines().count();
    let omitted = total.saturating_sub(max_lines);

    let mut out = String::new();
    if omitted > 0 {
        let _ = write!(out, "{indent}({omitted} lines omitted)");
    }
    for line in text.lines().skip(omitted) {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(indent);
        out.push_str(line);
    }
    out
}

/// Returns the set of context keys that are rendered inline under a stage's
/// handler-specific details, so they can be skipped in the trailing context
/// section.
fn stage_rendered_keys(node_id: &str, outcome: &Outcome) -> HashSet<String> {
    let candidates = [
        keys::COMMAND_OUTPUT.to_string(),
        keys::COMMAND_STDERR.to_string(),
        keys::LAST_STAGE.to_string(),
        keys::LAST_RESPONSE.to_string(),
        keys::response_key(node_id),
    ];
    candidates
        .into_iter()
        .filter(|k| outcome.context_updates.contains_key(k))
        .collect()
}

/// Render handler-specific nested bullets for compact mode.
fn render_compact_stage_details(
    _node_id: &str,
    node: Option<&Node>,
    outcome: &Outcome,
) -> Vec<String> {
    let handler = node.and_then(|n| n.handler_type());
    match handler {
        Some("command") => {
            let mut lines = Vec::new();
            if let Some(n) = node {
                if let Some(cmd) = n
                    .attrs
                    .get("script")
                    .or_else(|| n.attrs.get("tool_command"))
                    .and_then(|v| v.as_str())
                {
                    lines.push(format!("  - Script: `{cmd}`"));
                }
            }
            if let Some(stdout_val) = outcome.context_updates.get(keys::COMMAND_OUTPUT) {
                let stdout = format_value(stdout_val);
                if stdout.trim().is_empty() {
                    lines.push("  - Stdout: (empty)".to_string());
                } else {
                    lines.push("  - Stdout:".to_string());
                    lines.push("    ```".to_string());
                    lines.push(tail_lines(stdout.trim(), COMPACT_OUTPUT_MAX_LINES, "    "));
                    lines.push("    ```".to_string());
                }
            }
            if let Some(stderr_val) = outcome.context_updates.get(keys::COMMAND_STDERR) {
                let stderr = format_value(stderr_val);
                if stderr.trim().is_empty() {
                    lines.push("  - Stderr: (empty)".to_string());
                } else {
                    lines.push("  - Stderr:".to_string());
                    lines.push("    ```".to_string());
                    lines.push(tail_lines(stderr.trim(), COMPACT_OUTPUT_MAX_LINES, "    "));
                    lines.push("    ```".to_string());
                }
            }
            lines
        }
        h if is_llm_handler_type(h) => {
            let mut lines = Vec::new();
            if let Some(usage) = &outcome.usage {
                let input = format_token_count(usage.tokens().input_tokens);
                let output = format_token_count(usage.tokens().billable_output_tokens());
                lines.push(format!(
                    "  - Model: {}, {} tokens in / {} out",
                    usage.model_id(),
                    input,
                    output
                ));
            }
            if !outcome.files_touched.is_empty() {
                lines.push(format!("  - Files: {}", outcome.files_touched.join(", ")));
            }
            lines
        }
        _ => Vec::new(),
    }
}

/// Render a full `## Stage: {node_id}` section for summary:high mode.
fn render_summary_high_stage_section(
    node_id: &str,
    node: Option<&Node>,
    outcome: &Outcome,
) -> Vec<String> {
    let handler = node.and_then(|n| n.handler_type());
    let mut lines = Vec::new();
    lines.push(format!("\n## Stage: {node_id}"));
    lines.push(format!("- Status: {}", outcome.status));

    if let Some(h) = handler {
        lines.push(format!("- Handler: {h}"));
    }

    match handler {
        Some("command") => {
            if let Some(n) = node {
                if let Some(cmd) = n
                    .attrs
                    .get("script")
                    .or_else(|| n.attrs.get("tool_command"))
                    .and_then(|v| v.as_str())
                {
                    lines.push(format!("- Script: `{cmd}`"));
                }
            }
            if let Some(stdout_val) = outcome.context_updates.get(keys::COMMAND_OUTPUT) {
                if let Some(path) = artifact_path(stdout_val) {
                    lines.push(format!("- Stdout: {}", format_artifact_reference(path)));
                } else {
                    let stdout = format_value(stdout_val);
                    if stdout.trim().is_empty() {
                        lines.push("- Stdout: (empty)".to_string());
                    } else {
                        lines.push("- Stdout:".to_string());
                        lines.push("  ```".to_string());
                        lines.push(tail_lines(
                            stdout.trim(),
                            SUMMARY_HIGH_OUTPUT_MAX_LINES,
                            "  ",
                        ));
                        lines.push("  ```".to_string());
                    }
                }
            }
            if let Some(stderr_val) = outcome.context_updates.get(keys::COMMAND_STDERR) {
                if let Some(path) = artifact_path(stderr_val) {
                    lines.push(format!("- Stderr: {}", format_artifact_reference(path)));
                } else {
                    let stderr = format_value(stderr_val);
                    if stderr.trim().is_empty() {
                        lines.push("- Stderr: (empty)".to_string());
                    } else {
                        lines.push("- Stderr:".to_string());
                        lines.push("  ```".to_string());
                        lines.push(tail_lines(
                            stderr.trim(),
                            SUMMARY_HIGH_OUTPUT_MAX_LINES,
                            "  ",
                        ));
                        lines.push("  ```".to_string());
                    }
                }
            }
        }
        h if is_llm_handler_type(h) => {
            if let Some(usage) = &outcome.usage {
                lines.push(format!("- Model: {}", usage.model_id()));
                lines.push(format!(
                    "- Tokens: {} in / {} out",
                    format_token_count(usage.tokens().input_tokens),
                    format_token_count(usage.tokens().billable_output_tokens())
                ));
            }
            if !outcome.files_touched.is_empty() {
                lines.push(format!(
                    "- Files touched: {}",
                    outcome.files_touched.join(", ")
                ));
            }
            // Include full response from context_updates (or artifact pointer)
            if let Some(resp_val) = outcome.context_updates.get(&keys::response_key(node_id)) {
                if let Some(path) = artifact_path(resp_val) {
                    lines.push(format!("- Response: {}", format_artifact_reference(path)));
                } else {
                    let resp = format_value(resp_val);
                    if !resp.is_empty() {
                        lines.push("- Response:".to_string());
                        // Blockquote each line
                        for line in resp.lines() {
                            lines.push(format!("  > {line}"));
                        }
                    }
                }
            }
        }
        _ => {
            if let Some(notes) = outcome.notes.as_deref() {
                lines.push(format!("- Notes: {notes}"));
            }
            if let Some(reason) = outcome.failure_reason() {
                lines.push(format!("- Failure reason: {reason}"));
            }
        }
    }

    lines
}

/// Append filtered context as a `## Context` bullet list.
fn append_filtered_context(
    parts: &mut Vec<String>,
    context: &Context,
    rendered_keys: &HashSet<String>,
) {
    let snapshot = context.snapshot();
    let mut context_keys: Vec<&String> = snapshot
        .keys()
        .filter(|k| {
            !is_context_key_excluded(k)
                && !rendered_keys.contains(*k)
                && !is_blank_value(snapshot.get(*k))
        })
        .collect();
    if !context_keys.is_empty() {
        context_keys.sort();
        parts.push(String::from("\n## Context"));
        for key in context_keys {
            if let Some(val) = snapshot.get(key) {
                parts.push(format!("- {key}: {}", format_value(val)));
            }
        }
    }
}

/// Append filtered context as a `## Current context` Markdown table.
fn append_filtered_context_table(
    parts: &mut Vec<String>,
    context: &Context,
    rendered_keys: &HashSet<String>,
) {
    let snapshot = context.snapshot();
    let mut context_keys: Vec<&String> = snapshot
        .keys()
        .filter(|k| {
            !is_context_key_excluded(k)
                && !rendered_keys.contains(*k)
                && !is_blank_value(snapshot.get(*k))
        })
        .collect();
    if !context_keys.is_empty() {
        context_keys.sort();
        parts.push(String::from("\n## Current context"));
        parts.push("| Key | Value |".to_string());
        parts.push("|-----|-------|".to_string());
        for key in context_keys {
            if let Some(val) = snapshot.get(key) {
                parts.push(format!("| {key} | {} |", format_value(val)));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Compact preamble
// ---------------------------------------------------------------------------

fn build_compact_preamble(
    goal: &str,
    completed_nodes: &[String],
    node_outcomes: &HashMap<String, Outcome>,
    context: &Context,
    graph: &Graph,
) -> String {
    let mut parts = Vec::new();
    parts.push(format!("Goal: {goal}"));

    let mut all_rendered_keys = HashSet::new();

    {
        let mut header_emitted = false;
        for node_id in completed_nodes {
            if is_meta_handler(graph, node_id) {
                continue;
            }
            if !header_emitted {
                parts.push(String::from("\n## Completed stages"));
                header_emitted = true;
            }
            let node = graph.nodes.get(node_id);
            if let Some(outcome) = node_outcomes.get(node_id) {
                let status = &outcome.status;
                parts.push(format!("- **{node_id}**: {status}"));

                let details = render_compact_stage_details(node_id, node, outcome);
                parts.extend(details);

                all_rendered_keys.extend(stage_rendered_keys(node_id, outcome));
            } else {
                parts.push(format!("- **{node_id}**: completed"));
            }
        }
    }

    append_filtered_context(&mut parts, context, &all_rendered_keys);

    parts.push(String::new());
    parts.join("\n")
}

// ---------------------------------------------------------------------------
// Summary preamble
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum SummaryDetail {
    Low,
    Medium,
    High,
}

fn build_summary_preamble(
    goal: &str,
    run_id: &str,
    completed_nodes: &[String],
    node_outcomes: &HashMap<String, Outcome>,
    context: &Context,
    graph: &Graph,
    detail: SummaryDetail,
) -> String {
    let mut parts = Vec::new();
    parts.push(format!("Goal: {goal}"));
    parts.push(format!("Run ID: {run_id}"));

    let mut all_rendered_keys = HashSet::new();

    match detail {
        SummaryDetail::High => {
            let total_nodes = graph
                .nodes
                .keys()
                .filter(|id| !is_meta_handler(graph, id))
                .count();
            let completed_count = completed_nodes
                .iter()
                .filter(|id| !is_meta_handler(graph, id))
                .count();
            parts.push(format!(
                "Pipeline progress: {completed_count} of {total_nodes} stages completed"
            ));

            for node_id in completed_nodes {
                if is_meta_handler(graph, node_id) {
                    continue;
                }
                let node = graph.nodes.get(node_id);
                if let Some(outcome) = node_outcomes.get(node_id) {
                    let section = render_summary_high_stage_section(node_id, node, outcome);
                    parts.extend(section);
                    all_rendered_keys.extend(stage_rendered_keys(node_id, outcome));
                } else {
                    parts.push(format!("\n## Stage: {node_id}"));
                    parts.push("- Status: completed".to_string());
                }
            }

            append_filtered_context_table(&mut parts, context, &all_rendered_keys);
        }
        SummaryDetail::Medium => {
            let stage_count = completed_nodes.len();
            parts.push(format!("Completed {stage_count} stage(s) so far."));

            let recent_count = 5;
            let stages_to_show: Vec<&String> = if stage_count > recent_count {
                let skipped = stage_count - recent_count;
                parts.push(format!("\n({skipped} earlier stage(s) omitted)"));
                completed_nodes.iter().skip(skipped).collect()
            } else {
                completed_nodes.iter().collect()
            };

            {
                let mut header_emitted = false;
                for node_id in &stages_to_show {
                    if is_meta_handler(graph, node_id) {
                        continue;
                    }
                    if !header_emitted {
                        parts.push(String::from("\nRecent stages:"));
                        header_emitted = true;
                    }
                    if let Some(outcome) = node_outcomes.get(*node_id) {
                        let status = outcome.status.to_string();
                        let mut line = format!("- {node_id}: {status}");
                        if let Some(notes) = outcome.notes.as_deref() {
                            let _ = write!(line, " ({notes})");
                        }
                        if let Some(reason) = outcome.failure_reason() {
                            let _ = write!(line, " [reason: {reason}]");
                        }
                        parts.push(line);

                        let node = graph.nodes.get(*node_id);
                        let details = render_compact_stage_details(node_id, node, outcome);
                        parts.extend(details);

                        all_rendered_keys.extend(stage_rendered_keys(node_id, outcome));
                    } else {
                        parts.push(format!("- {node_id}: completed"));
                    }
                }
            }

            append_filtered_context(&mut parts, context, &all_rendered_keys);
        }
        SummaryDetail::Low => {
            let stage_count = completed_nodes.len();
            parts.push(format!("Completed {stage_count} stage(s) so far."));

            let recent_count = 2;
            let stages_to_show: Vec<&String> = if stage_count > recent_count {
                let skipped = stage_count - recent_count;
                parts.push(format!("\n({skipped} earlier stage(s) omitted)"));
                completed_nodes.iter().skip(skipped).collect()
            } else {
                completed_nodes.iter().collect()
            };

            {
                let mut header_emitted = false;
                for node_id in &stages_to_show {
                    if is_meta_handler(graph, node_id) {
                        continue;
                    }
                    if !header_emitted {
                        parts.push(String::from("\nRecent stages:"));
                        header_emitted = true;
                    }
                    if let Some(outcome) = node_outcomes.get(*node_id) {
                        let status = outcome.status.to_string();
                        let mut line = format!("- {node_id}: {status}");
                        if let Some(notes) = outcome.notes.as_deref() {
                            let _ = write!(line, " ({notes})");
                        }
                        if let Some(reason) = outcome.failure_reason() {
                            let _ = write!(line, " [reason: {reason}]");
                        }
                        parts.push(line);

                        let node = graph.nodes.get(*node_id);
                        let handler = node.and_then(|n| n.handler_type());
                        if let Some(h) = handler {
                            parts.push(format!("  - Handler: {h}"));
                        }
                        match handler {
                            Some("command") => {
                                if let Some(n) = node {
                                    if let Some(cmd) = n
                                        .attrs
                                        .get("script")
                                        .or_else(|| n.attrs.get("tool_command"))
                                        .and_then(|v| v.as_str())
                                    {
                                        parts.push(format!("  - Script: `{cmd}`"));
                                    }
                                }
                            }
                            h if is_llm_handler_type(h) => {
                                if let Some(usage) = &outcome.usage {
                                    parts.push(format!("  - Model: {}", usage.model_id()));
                                }
                            }
                            _ => {}
                        }
                    } else {
                        parts.push(format!("- {node_id}: completed"));
                    }
                }
            }
        }
    }

    parts.push(String::new());
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use fabro_graphviz::graph::AttrValue;
    use fabro_llm::types::TokenCounts;
    use fabro_model::Provider;

    use super::*;
    use crate::outcome::{BilledModelUsage, billed_model_usage_from_llm};

    fn stage_usage(model: &str, input_tokens: i64, output_tokens: i64) -> BilledModelUsage {
        billed_model_usage_from_llm(model, Provider::Anthropic, None, &TokenCounts {
            input_tokens,
            output_tokens,
            ..TokenCounts::default()
        })
    }

    // --- truncate mode ---

    #[test]
    fn build_preamble_truncate_includes_goal_and_run_id() {
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("Fix the login bug".to_string()),
        );
        let context = Context::new();
        context.set(keys::INTERNAL_RUN_ID, serde_json::json!("abc-123"));
        let completed_nodes: Vec<String> = Vec::new();
        let node_outcomes: HashMap<String, Outcome> = HashMap::new();

        let preamble = build_preamble(
            keys::Fidelity::Truncate,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(
            preamble.contains("Fix the login bug"),
            "should contain the goal"
        );
        assert!(preamble.contains("Run ID:"), "should contain run ID label");
        assert!(
            preamble.contains("abc-123"),
            "should contain the run ID value"
        );
    }

    #[test]
    fn build_preamble_truncate_excludes_completed_stages() {
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("Deploy app".to_string()),
        );
        let context = Context::new();
        let completed_nodes = vec!["plan".to_string(), "code".to_string()];
        let mut node_outcomes: HashMap<String, Outcome> = HashMap::new();
        node_outcomes.insert("plan".to_string(), Outcome::success());
        node_outcomes.insert("code".to_string(), Outcome::success());

        let preamble = build_preamble(
            keys::Fidelity::Truncate,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(
            !preamble.contains("plan"),
            "truncate should not list completed stages"
        );
        assert!(
            !preamble.contains("code"),
            "truncate should not list completed stages"
        );
    }

    // --- compact mode ---

    #[test]
    fn build_preamble_compact_lists_completed_stages() {
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("Deploy app".to_string()),
        );
        let context = Context::new();
        context.set(keys::INTERNAL_RUN_ID, serde_json::json!("run-456"));
        let completed_nodes = vec!["plan".to_string(), "code".to_string()];
        let mut node_outcomes: HashMap<String, Outcome> = HashMap::new();
        node_outcomes.insert("plan".to_string(), Outcome::success());
        node_outcomes.insert(
            "code".to_string(),
            Outcome::fail_classify("compilation error"),
        );

        let preamble = build_preamble(
            keys::Fidelity::Compact,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(preamble.contains("Deploy app"), "should contain the goal");
        assert!(
            preamble.contains("## Completed stages"),
            "should have Completed stages heading"
        );
        assert!(
            preamble.contains("**plan**"),
            "should list completed stage 'plan' in bold"
        );
        assert!(
            preamble.contains("success"),
            "should show plan's success status"
        );
        assert!(
            preamble.contains("**code**"),
            "should list completed stage 'code' in bold"
        );
        assert!(preamble.contains("fail"), "should show code's fail status");
    }

    #[test]
    fn build_preamble_compact_includes_context_values() {
        let graph = Graph::new("test");
        let context = Context::new();
        context.set(keys::GRAPH_GOAL, serde_json::json!("Build it"));
        context.set("user.name", serde_json::json!("alice"));
        let completed_nodes: Vec<String> = Vec::new();
        let node_outcomes: HashMap<String, Outcome> = HashMap::new();

        let preamble = build_preamble(
            keys::Fidelity::Compact,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(
            !preamble.contains("graph.goal"),
            "should exclude graph.* context keys"
        );
        assert!(
            preamble.contains("user.name"),
            "should include user.name context key"
        );
        assert!(preamble.contains("alice"), "should include context value");
    }

    #[test]
    fn build_preamble_compact_excludes_internal_keys() {
        let graph = Graph::new("test");
        let context = Context::new();
        context.set(keys::INTERNAL_FIDELITY, serde_json::json!("compact"));
        context.set(keys::retry_count_key("plan"), serde_json::json!(1));
        context.set(keys::CURRENT_NODE, serde_json::json!("work"));
        context.set(
            keys::graph_attr_key("default_fidelity"),
            serde_json::json!("compact"),
        );
        context.set("thread.main.current_node", serde_json::json!("work"));
        context.set(
            keys::response_key("plan"),
            serde_json::json!("some response"),
        );
        context.set(keys::LAST_STAGE, serde_json::json!("plan"));
        context.set(keys::LAST_RESPONSE, serde_json::json!("resp"));
        context.set(keys::PREFERRED_LABEL, serde_json::json!("success"));
        context.set("user.name", serde_json::json!("bob"));
        let completed_nodes: Vec<String> = Vec::new();
        let node_outcomes: HashMap<String, Outcome> = HashMap::new();

        let preamble = build_preamble(
            keys::Fidelity::Compact,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(
            !preamble.contains("internal.fidelity"),
            "should exclude internal keys"
        );
        assert!(
            !preamble.contains("internal.retry_count"),
            "should exclude internal keys"
        );
        assert!(
            !preamble.contains("current_node"),
            "should exclude current keys"
        );
        assert!(
            !preamble.contains("graph.default_fidelity"),
            "should exclude graph.* keys"
        );
        assert!(
            !preamble.contains("thread.main"),
            "should exclude thread.* keys"
        );
        assert!(
            !preamble.contains("response.plan"),
            "should exclude response.* keys"
        );
        assert!(
            !preamble.contains("- last_stage:"),
            "should exclude last_stage"
        );
        assert!(
            !preamble.contains("- last_response:"),
            "should exclude last_response"
        );
        assert!(
            !preamble.contains("- preferred_label:"),
            "should exclude preferred_label"
        );
        assert!(
            preamble.contains("user.name"),
            "should include non-internal keys"
        );
    }

    #[test]
    fn build_preamble_compact_shows_notes_on_stages() {
        // Compact no longer shows notes inline (handler-specific details replace them),
        // but notes are still available in the outcome for non-handler stages.
        let graph = Graph::new("test");
        let context = Context::new();
        let completed_nodes = vec!["work".to_string()];
        let mut node_outcomes: HashMap<String, Outcome> = HashMap::new();
        let mut outcome = Outcome::success();
        outcome.notes = Some("auto-status: completed".to_string());
        node_outcomes.insert("work".to_string(), outcome);

        let preamble = build_preamble(
            keys::Fidelity::Compact,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        // Compact uses bold node IDs and handler-specific details now
        assert!(
            preamble.contains("**work**"),
            "should include node ID in bold"
        );
        assert!(preamble.contains("success"), "should show success status");
    }

    // --- compact handler-specific details ---

    #[test]
    fn compact_command_stage_shows_command_stdout_stderr() {
        let mut graph = Graph::new("test");
        let mut run_tests = Node::new("run_tests");
        run_tests.attrs.insert(
            "shape".to_string(),
            AttrValue::String("parallelogram".to_string()),
        );
        run_tests.attrs.insert(
            "script".to_string(),
            AttrValue::String("echo '10 passed'".to_string()),
        );
        graph.nodes.insert("run_tests".to_string(), run_tests);

        let context = Context::new();
        let completed_nodes = vec!["run_tests".to_string()];
        let mut node_outcomes: HashMap<String, Outcome> = HashMap::new();
        let mut outcome = Outcome::success();
        outcome.context_updates.insert(
            keys::COMMAND_OUTPUT.to_string(),
            serde_json::json!("10 passed\n"),
        );
        outcome
            .context_updates
            .insert(keys::COMMAND_STDERR.to_string(), serde_json::json!(""));
        node_outcomes.insert("run_tests".to_string(), outcome);

        let preamble = build_preamble(
            keys::Fidelity::Compact,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(
            preamble.contains("Script: `echo '10 passed'`"),
            "should show script command"
        );
        assert!(preamble.contains("Stdout:"), "should show stdout label");
        assert!(preamble.contains("10 passed"), "should show stdout content");
        assert!(
            preamble.contains("Stderr: (empty)"),
            "should show empty stderr"
        );
    }

    #[test]
    fn compact_agent_loop_stage_shows_model_and_files() {
        let mut graph = Graph::new("test");
        let mut report = Node::new("report");
        report
            .attrs
            .insert("shape".to_string(), AttrValue::String("box".to_string()));
        graph.nodes.insert("report".to_string(), report);

        let context = Context::new();
        let completed_nodes = vec!["report".to_string()];
        let mut node_outcomes: HashMap<String, Outcome> = HashMap::new();
        let mut outcome = Outcome::success();
        outcome.usage = Some(stage_usage("claude-sonnet-4-20250514", 1234, 567));
        outcome.files_touched = vec!["src/lib.rs".to_string(), "src/main.rs".to_string()];
        node_outcomes.insert("report".to_string(), outcome);

        let preamble = build_preamble(
            keys::Fidelity::Compact,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(
            preamble.contains("claude-sonnet-4-20250514"),
            "should show model name"
        );
        assert!(
            preamble.contains("1.2k tokens in"),
            "should show token count"
        );
        assert!(
            preamble.contains("src/lib.rs, src/main.rs"),
            "should show files touched"
        );
    }

    #[test]
    fn compact_context_excludes_engine_keys() {
        let graph = Graph::new("test");
        let context = Context::new();
        context.set(
            keys::graph_attr_key("default_fidelity"),
            serde_json::json!("compact"),
        );
        context.set("thread.main.current_node", serde_json::json!("work"));
        context.set(
            keys::response_key("plan"),
            serde_json::json!("some LLM response"),
        );
        context.set(keys::LAST_STAGE, serde_json::json!("plan"));
        context.set("user.preference", serde_json::json!("dark"));
        let completed_nodes: Vec<String> = Vec::new();
        let node_outcomes: HashMap<String, Outcome> = HashMap::new();

        let preamble = build_preamble(
            keys::Fidelity::Compact,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(
            !preamble.contains("graph.default_fidelity"),
            "should exclude graph.* keys"
        );
        assert!(
            !preamble.contains("thread.main"),
            "should exclude thread.* keys"
        );
        assert!(
            !preamble.contains("response.plan"),
            "should exclude response.* keys"
        );
        assert!(
            !preamble.contains("- last_stage:"),
            "should exclude last_stage"
        );
        assert!(
            preamble.contains("user.preference"),
            "should include user keys"
        );
    }

    #[test]
    fn compact_context_deduplicates_stage_rendered_keys() {
        let mut graph = Graph::new("test");
        let mut step = Node::new("step");
        step.attrs.insert(
            "shape".to_string(),
            AttrValue::String("parallelogram".to_string()),
        );
        step.attrs.insert(
            "script".to_string(),
            AttrValue::String("echo hi".to_string()),
        );
        graph.nodes.insert("step".to_string(), step);

        let context = Context::new();
        // command.output is set in context (the engine copies context_updates to
        // context)
        context.set(keys::COMMAND_OUTPUT, serde_json::json!("hi\n"));
        context.set(keys::COMMAND_STDERR, serde_json::json!(""));
        let completed_nodes = vec!["step".to_string()];
        let mut node_outcomes: HashMap<String, Outcome> = HashMap::new();
        let mut outcome = Outcome::success();
        outcome
            .context_updates
            .insert(keys::COMMAND_OUTPUT.to_string(), serde_json::json!("hi\n"));
        outcome
            .context_updates
            .insert(keys::COMMAND_STDERR.to_string(), serde_json::json!(""));
        node_outcomes.insert("step".to_string(), outcome);

        let preamble = build_preamble(
            keys::Fidelity::Compact,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        // command.output should NOT appear in the Context section
        // because it's already rendered inline under the stage
        let context_section = preamble.split("## Context").nth(1).unwrap_or("");
        assert!(
            !context_section.contains("command.output"),
            "command.output should be deduplicated from context section"
        );
    }

    // --- summary:low mode ---

    #[test]
    fn build_preamble_summary_low_includes_stage_count() {
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("Run tests".to_string()),
        );
        let context = Context::new();
        let completed_nodes = vec!["plan".to_string(), "code".to_string(), "test".to_string()];
        let mut node_outcomes: HashMap<String, Outcome> = HashMap::new();
        node_outcomes.insert("plan".to_string(), Outcome::success());
        node_outcomes.insert("code".to_string(), Outcome::success());
        node_outcomes.insert("test".to_string(), Outcome::fail_classify("test failure"));

        let preamble = build_preamble(
            keys::Fidelity::SummaryLow,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(preamble.contains("Run tests"), "should contain the goal");
        assert!(
            preamble.contains("3 stage(s)"),
            "should mention total stage count"
        );
    }

    #[test]
    fn build_preamble_summary_low_shows_only_recent_stages() {
        let mut graph = Graph::new("test");
        let mut step3 = Node::new("step3");
        step3.attrs.insert(
            "shape".to_string(),
            AttrValue::String("parallelogram".to_string()),
        );
        graph.nodes.insert("step3".to_string(), step3);

        let context = Context::new();
        let completed_nodes = vec![
            "step1".to_string(),
            "step2".to_string(),
            "step3".to_string(),
            "step4".to_string(),
        ];
        let mut node_outcomes: HashMap<String, Outcome> = HashMap::new();
        node_outcomes.insert("step1".to_string(), Outcome::success());
        node_outcomes.insert("step2".to_string(), Outcome::success());
        node_outcomes.insert("step3".to_string(), Outcome::success());
        node_outcomes.insert("step4".to_string(), Outcome::fail_classify("error"));

        let preamble = build_preamble(
            keys::Fidelity::SummaryLow,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        // summary:low shows only 2 recent stages
        assert!(!preamble.contains("step1"), "should omit older stages");
        assert!(!preamble.contains("step2"), "should omit older stages");
        assert!(preamble.contains("step3"), "should show recent stage");
        assert!(preamble.contains("step4"), "should show most recent stage");
        assert!(
            preamble.contains("omitted"),
            "should indicate omitted stages"
        );
        // Handler type should appear for nodes with known handlers
        assert!(
            preamble.contains("Handler: command"),
            "should show handler type for step3"
        );
    }

    #[test]
    fn summary_low_command_stage_shows_handler_and_command() {
        let mut graph = Graph::new("test");
        let mut run_tests = Node::new("run_tests");
        run_tests.attrs.insert(
            "shape".to_string(),
            AttrValue::String("parallelogram".to_string()),
        );
        run_tests.attrs.insert(
            "script".to_string(),
            AttrValue::String("cargo test".to_string()),
        );
        graph.nodes.insert("run_tests".to_string(), run_tests);

        let context = Context::new();
        let completed_nodes = vec!["run_tests".to_string()];
        let mut node_outcomes: HashMap<String, Outcome> = HashMap::new();
        let mut outcome = Outcome::fail_classify("exit code 1");
        outcome.context_updates.insert(
            keys::COMMAND_OUTPUT.to_string(),
            serde_json::json!("test failed"),
        );
        node_outcomes.insert("run_tests".to_string(), outcome);

        let preamble = build_preamble(
            keys::Fidelity::SummaryLow,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(
            preamble.contains("Handler: command"),
            "should show handler type"
        );
        assert!(
            preamble.contains("Script: `cargo test`"),
            "should show script command"
        );
        // Low mode should NOT include stdout/stderr
        assert!(
            !preamble.contains("Stdout:"),
            "should not show stdout in low mode"
        );
    }

    #[test]
    fn summary_low_agent_loop_stage_shows_handler_and_model() {
        let mut graph = Graph::new("test");
        let mut report = Node::new("report");
        report
            .attrs
            .insert("shape".to_string(), AttrValue::String("box".to_string()));
        graph.nodes.insert("report".to_string(), report);

        let context = Context::new();
        let completed_nodes = vec!["report".to_string()];
        let mut node_outcomes: HashMap<String, Outcome> = HashMap::new();
        let mut outcome = Outcome::success();
        outcome.usage = Some(stage_usage("claude-sonnet-4-20250514", 1000, 200));
        node_outcomes.insert("report".to_string(), outcome);

        let preamble = build_preamble(
            keys::Fidelity::SummaryLow,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(
            preamble.contains("Handler: agent"),
            "should show handler type"
        );
        assert!(
            preamble.contains("Model: claude-sonnet-4-20250514"),
            "should show model name"
        );
    }

    #[test]
    fn build_preamble_summary_low_excludes_context_values() {
        let graph = Graph::new("test");
        let context = Context::new();
        context.set("user.name", serde_json::json!("alice"));
        let completed_nodes: Vec<String> = Vec::new();
        let node_outcomes: HashMap<String, Outcome> = HashMap::new();

        let preamble = build_preamble(
            keys::Fidelity::SummaryLow,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(
            !preamble.contains("user.name"),
            "summary:low should not include context values"
        );
    }

    // --- summary:medium mode ---

    #[test]
    fn build_preamble_summary_medium_shows_more_stages_than_low() {
        let graph = Graph::new("test");
        let context = Context::new();
        let completed_nodes = vec![
            "s1".to_string(),
            "s2".to_string(),
            "s3".to_string(),
            "s4".to_string(),
            "s5".to_string(),
            "s6".to_string(),
            "s7".to_string(),
        ];
        let mut node_outcomes: HashMap<String, Outcome> = HashMap::new();
        node_outcomes.insert("s1".to_string(), Outcome::success());
        node_outcomes.insert("s2".to_string(), Outcome::success());
        node_outcomes.insert("s3".to_string(), Outcome::success());
        node_outcomes.insert("s4".to_string(), Outcome::success());
        node_outcomes.insert("s5".to_string(), Outcome::success());
        node_outcomes.insert("s6".to_string(), Outcome::success());
        node_outcomes.insert("s7".to_string(), Outcome::success());

        let preamble = build_preamble(
            keys::Fidelity::SummaryMedium,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        // summary:medium shows 5 recent stages
        assert!(!preamble.contains("- s1:"), "should omit oldest stages");
        assert!(!preamble.contains("- s2:"), "should omit oldest stages");
        assert!(preamble.contains("s3"), "should show recent stage s3");
        assert!(preamble.contains("s7"), "should show most recent stage s7");
        assert!(
            preamble.contains("omitted"),
            "should indicate omitted stages"
        );
    }

    #[test]
    fn build_preamble_summary_medium_includes_context_values() {
        let graph = Graph::new("test");
        let context = Context::new();
        context.set("user.name", serde_json::json!("alice"));
        let completed_nodes: Vec<String> = Vec::new();
        let node_outcomes: HashMap<String, Outcome> = HashMap::new();

        let preamble = build_preamble(
            keys::Fidelity::SummaryMedium,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(
            preamble.contains("user.name"),
            "summary:medium should include context values"
        );
        assert!(preamble.contains("alice"), "should include context value");
    }

    #[test]
    fn build_preamble_summary_medium_uses_compact_handler_details() {
        let mut graph = Graph::new("test");
        let mut run_tests = Node::new("run_tests");
        run_tests.attrs.insert(
            "shape".to_string(),
            AttrValue::String("parallelogram".to_string()),
        );
        run_tests.attrs.insert(
            "script".to_string(),
            AttrValue::String("make test".to_string()),
        );
        graph.nodes.insert("run_tests".to_string(), run_tests);

        let context = Context::new();
        let completed_nodes = vec!["run_tests".to_string()];
        let mut node_outcomes: HashMap<String, Outcome> = HashMap::new();
        let mut outcome = Outcome::success();
        outcome.context_updates.insert(
            keys::COMMAND_OUTPUT.to_string(),
            serde_json::json!("All tests passed\n"),
        );
        outcome
            .context_updates
            .insert(keys::COMMAND_STDERR.to_string(), serde_json::json!(""));
        node_outcomes.insert("run_tests".to_string(), outcome);

        let preamble = build_preamble(
            keys::Fidelity::SummaryMedium,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(
            preamble.contains("Script: `make test`"),
            "should show script command via compact renderer"
        );
        assert!(
            preamble.contains("All tests passed"),
            "should show stdout via compact renderer"
        );
        assert!(
            !preamble.contains("set command.output"),
            "should not dump raw context updates"
        );
    }

    #[test]
    fn summary_medium_agent_loop_stage_shows_compact_details() {
        let mut graph = Graph::new("test");
        let mut report = Node::new("report");
        report
            .attrs
            .insert("shape".to_string(), AttrValue::String("box".to_string()));
        graph.nodes.insert("report".to_string(), report);

        let context = Context::new();
        let completed_nodes = vec!["report".to_string()];
        let mut node_outcomes: HashMap<String, Outcome> = HashMap::new();
        let mut outcome = Outcome::success();
        outcome.usage = Some(stage_usage("claude-sonnet-4-20250514", 1500, 300));
        outcome.files_touched = vec!["src/lib.rs".to_string()];
        node_outcomes.insert("report".to_string(), outcome);

        let preamble = build_preamble(
            keys::Fidelity::SummaryMedium,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(
            preamble.contains("claude-sonnet-4-20250514"),
            "should show model name"
        );
        assert!(preamble.contains("src/lib.rs"), "should show files touched");
    }

    // --- summary:high mode ---

    #[test]
    fn build_preamble_summary_high_shows_all_stages() {
        let graph = Graph::new("test");
        let context = Context::new();
        let completed_nodes = vec![
            "s1".to_string(),
            "s2".to_string(),
            "s3".to_string(),
            "s4".to_string(),
            "s5".to_string(),
            "s6".to_string(),
        ];
        let mut node_outcomes: HashMap<String, Outcome> = HashMap::new();
        node_outcomes.insert("s1".to_string(), Outcome::success());
        node_outcomes.insert("s2".to_string(), Outcome::success());
        node_outcomes.insert("s3".to_string(), Outcome::success());
        node_outcomes.insert("s4".to_string(), Outcome::success());
        node_outcomes.insert("s5".to_string(), Outcome::success());
        node_outcomes.insert("s6".to_string(), Outcome::success());

        let preamble = build_preamble(
            keys::Fidelity::SummaryHigh,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        // summary:high shows ALL stages as ## Stage: headings
        assert!(
            preamble.contains("## Stage: s1"),
            "should show all stages including s1"
        );
        assert!(
            preamble.contains("## Stage: s6"),
            "should show all stages including s6"
        );
        assert!(!preamble.contains("omitted"), "should not omit any stages");
    }

    #[test]
    fn build_preamble_summary_high_includes_failure_reasons() {
        let graph = Graph::new("test");
        let context = Context::new();
        let completed_nodes = vec!["work".to_string()];
        let mut node_outcomes: HashMap<String, Outcome> = HashMap::new();
        node_outcomes.insert(
            "work".to_string(),
            Outcome::fail_classify("connection timeout"),
        );

        let preamble = build_preamble(
            keys::Fidelity::SummaryHigh,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(
            preamble.contains("connection timeout"),
            "should include failure reason"
        );
    }

    #[test]
    fn build_preamble_summary_high_includes_context_values() {
        let graph = Graph::new("test");
        let context = Context::new();
        context.set(keys::GRAPH_GOAL, serde_json::json!("Build"));
        context.set("user.name", serde_json::json!("alice"));
        let completed_nodes: Vec<String> = Vec::new();
        let node_outcomes: HashMap<String, Outcome> = HashMap::new();

        let preamble = build_preamble(
            keys::Fidelity::SummaryHigh,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(
            !preamble.contains("graph.goal"),
            "should exclude graph.* from context"
        );
        // Table format for summary:high
        assert!(
            preamble.contains("| user.name |"),
            "should include context values as table"
        );
    }

    // --- summary:high handler-specific ---

    #[test]
    fn summary_high_produces_stage_sections() {
        let graph = Graph::new("test");
        let context = Context::new();
        let completed_nodes = vec!["start".to_string(), "work".to_string()];
        let mut node_outcomes: HashMap<String, Outcome> = HashMap::new();
        node_outcomes.insert("start".to_string(), Outcome::success());
        node_outcomes.insert("work".to_string(), Outcome::success());

        let preamble = build_preamble(
            keys::Fidelity::SummaryHigh,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(
            preamble.contains("## Stage: start"),
            "should have stage heading for start"
        );
        assert!(
            preamble.contains("## Stage: work"),
            "should have stage heading for work"
        );
    }

    #[test]
    fn summary_high_command_stage_full_detail() {
        let mut graph = Graph::new("test");
        let mut run_tests = Node::new("run_tests");
        run_tests.attrs.insert(
            "shape".to_string(),
            AttrValue::String("parallelogram".to_string()),
        );
        run_tests.attrs.insert(
            "script".to_string(),
            AttrValue::String("make test".to_string()),
        );
        graph.nodes.insert("run_tests".to_string(), run_tests);

        let context = Context::new();
        let completed_nodes = vec!["run_tests".to_string()];
        let mut node_outcomes: HashMap<String, Outcome> = HashMap::new();
        let mut outcome = Outcome::success();
        outcome.context_updates.insert(
            keys::COMMAND_OUTPUT.to_string(),
            serde_json::json!("All tests passed\n"),
        );
        outcome.context_updates.insert(
            keys::COMMAND_STDERR.to_string(),
            serde_json::json!("warning: unused var\n"),
        );
        node_outcomes.insert("run_tests".to_string(), outcome);

        let preamble = build_preamble(
            keys::Fidelity::SummaryHigh,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(
            preamble.contains("## Stage: run_tests"),
            "should have stage heading"
        );
        assert!(preamble.contains("Handler: command"), "should show handler");
        assert!(
            preamble.contains("Script: `make test`"),
            "should show script command"
        );
        assert!(
            preamble.contains("All tests passed"),
            "should include stdout"
        );
        assert!(
            preamble.contains("warning: unused var"),
            "should include stderr"
        );
    }

    #[test]
    fn summary_high_agent_loop_stage_with_response_preview() {
        let mut graph = Graph::new("test");
        let mut report = Node::new("report");
        report
            .attrs
            .insert("shape".to_string(), AttrValue::String("box".to_string()));
        graph.nodes.insert("report".to_string(), report);

        let context = Context::new();
        let completed_nodes = vec!["report".to_string()];
        let mut node_outcomes: HashMap<String, Outcome> = HashMap::new();
        let mut outcome = Outcome::success();
        outcome.usage = Some(stage_usage("claude-sonnet-4-20250514", 1500, 300));
        outcome.files_touched = vec!["src/lib.rs".to_string()];
        outcome.context_updates.insert(
            keys::response_key("report"),
            serde_json::json!("The tests all pass successfully."),
        );
        node_outcomes.insert("report".to_string(), outcome);

        let preamble = build_preamble(
            keys::Fidelity::SummaryHigh,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(
            preamble.contains("## Stage: report"),
            "should have stage heading"
        );
        assert!(preamble.contains("Handler: agent"), "should show handler");
        assert!(
            preamble.contains("Model: claude-sonnet-4-20250514"),
            "should show model"
        );
        assert!(preamble.contains("1.5k in"), "should show formatted tokens");
        assert!(
            preamble.contains("Files touched: src/lib.rs"),
            "should show files"
        );
        assert!(
            preamble.contains("The tests all pass"),
            "should include response"
        );
    }

    #[test]
    fn summary_high_context_as_table() {
        let graph = Graph::new("test");
        let context = Context::new();
        context.set("user.name", serde_json::json!("alice"));
        context.set("custom.key", serde_json::json!("value"));
        let completed_nodes: Vec<String> = Vec::new();
        let node_outcomes: HashMap<String, Outcome> = HashMap::new();

        let preamble = build_preamble(
            keys::Fidelity::SummaryHigh,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(
            preamble.contains("## Current context"),
            "should have context table heading"
        );
        assert!(
            preamble.contains("| Key | Value |"),
            "should have table header"
        );
        assert!(
            preamble.contains("| user.name | alice |"),
            "should have context row"
        );
    }

    #[test]
    fn summary_high_pipeline_progress_count() {
        let mut graph = Graph::new("test");
        // Create 4 nodes total (including start/exit)
        let start = Node::new("start");
        graph.nodes.insert("start".to_string(), start);
        let work = Node::new("work");
        graph.nodes.insert("work".to_string(), work);
        let test = Node::new("test");
        graph.nodes.insert("test".to_string(), test);
        let exit = Node::new("exit");
        graph.nodes.insert("exit".to_string(), exit);

        let context = Context::new();
        let completed_nodes = vec!["start".to_string(), "work".to_string()];
        let mut node_outcomes: HashMap<String, Outcome> = HashMap::new();
        node_outcomes.insert("start".to_string(), Outcome::success());
        node_outcomes.insert("work".to_string(), Outcome::success());

        let preamble = build_preamble(
            keys::Fidelity::SummaryHigh,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(
            preamble.contains("2 of 4 stages completed"),
            "should show pipeline progress with total node count, got:\n{preamble}"
        );
    }

    // --- format_token_count ---

    #[test]
    fn format_token_count_formatting() {
        assert_eq!(format_token_count(500), "500");
        assert_eq!(format_token_count(999), "999");
        assert_eq!(format_token_count(1000), "1.0k");
        assert_eq!(format_token_count(1234), "1.2k");
        assert_eq!(format_token_count(1500), "1.5k");
        assert_eq!(format_token_count(10000), "10.0k");
        assert_eq!(format_token_count(1_000_000), "1.0m");
        assert_eq!(format_token_count(3_456_789), "3.5m");
    }

    // --- is_context_key_excluded ---

    #[test]
    fn is_context_key_excluded_checks() {
        assert!(is_context_key_excluded(keys::INTERNAL_FIDELITY));
        assert!(is_context_key_excluded(&keys::retry_count_key("plan")));
        assert!(is_context_key_excluded(keys::CURRENT_NODE));
        assert!(is_context_key_excluded(keys::CURRENT_PREAMBLE));
        assert!(is_context_key_excluded(&keys::graph_attr_key(
            "default_fidelity"
        )));
        assert!(is_context_key_excluded(keys::GRAPH_GOAL));
        assert!(is_context_key_excluded(&keys::thread_current_node_key(
            "main"
        )));
        assert!(is_context_key_excluded(&keys::response_key("plan")));
        assert!(is_context_key_excluded(keys::OUTCOME));
        assert!(is_context_key_excluded(keys::LAST_STAGE));
        assert!(is_context_key_excluded(keys::LAST_RESPONSE));
        assert!(is_context_key_excluded(keys::PREFERRED_LABEL));
        assert!(!is_context_key_excluded("user.name"));
        assert!(!is_context_key_excluded("custom.key"));
        assert!(!is_context_key_excluded(keys::COMMAND_OUTPUT));
    }

    // --- meta node filtering ---

    #[test]
    fn compact_preamble_excludes_start_node() {
        let mut graph = Graph::new("test");
        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        graph.nodes.insert("start".to_string(), start);
        let plan = Node::new("plan");
        graph.nodes.insert("plan".to_string(), plan);

        let context = Context::new();
        let completed_nodes = vec!["start".to_string(), "plan".to_string()];
        let mut node_outcomes: HashMap<String, Outcome> = HashMap::new();
        node_outcomes.insert("start".to_string(), Outcome::success());
        node_outcomes.insert("plan".to_string(), Outcome::success());

        let preamble = build_preamble(
            keys::Fidelity::Compact,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(
            !preamble.contains("**start**"),
            "should not show start node, got:\n{preamble}"
        );
        assert!(preamble.contains("**plan**"), "should show non-meta nodes");
    }

    #[test]
    fn summary_high_excludes_start_node() {
        let mut graph = Graph::new("test");
        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        graph.nodes.insert("start".to_string(), start);
        let work = Node::new("work");
        graph.nodes.insert("work".to_string(), work);

        let context = Context::new();
        let completed_nodes = vec!["start".to_string(), "work".to_string()];
        let mut node_outcomes: HashMap<String, Outcome> = HashMap::new();
        node_outcomes.insert("start".to_string(), Outcome::success());
        node_outcomes.insert("work".to_string(), Outcome::success());

        let preamble = build_preamble(
            keys::Fidelity::SummaryHigh,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(
            !preamble.contains("## Stage: start"),
            "should not show start stage, got:\n{preamble}"
        );
        assert!(
            preamble.contains("## Stage: work"),
            "should show non-meta stages"
        );
    }

    #[test]
    fn summary_high_progress_excludes_meta_nodes() {
        let mut graph = Graph::new("test");
        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        graph.nodes.insert("start".to_string(), start);
        let work = Node::new("work");
        graph.nodes.insert("work".to_string(), work);
        let test_node = Node::new("test");
        graph.nodes.insert("test".to_string(), test_node);
        let mut exit = Node::new("exit");
        exit.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Msquare".to_string()),
        );
        graph.nodes.insert("exit".to_string(), exit);

        let context = Context::new();
        let completed_nodes = vec!["start".to_string(), "work".to_string()];
        let mut node_outcomes: HashMap<String, Outcome> = HashMap::new();
        node_outcomes.insert("start".to_string(), Outcome::success());
        node_outcomes.insert("work".to_string(), Outcome::success());

        let preamble = build_preamble(
            keys::Fidelity::SummaryHigh,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(
            preamble.contains("1 of 2 stages completed"),
            "should exclude meta nodes from progress count, got:\n{preamble}"
        );
    }

    #[test]
    fn summary_medium_excludes_start_node() {
        let mut graph = Graph::new("test");
        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        graph.nodes.insert("start".to_string(), start);
        let work = Node::new("work");
        graph.nodes.insert("work".to_string(), work);

        let context = Context::new();
        let completed_nodes = vec!["start".to_string(), "work".to_string()];
        let mut node_outcomes: HashMap<String, Outcome> = HashMap::new();
        node_outcomes.insert("start".to_string(), Outcome::success());
        node_outcomes.insert("work".to_string(), Outcome::success());

        let preamble = build_preamble(
            keys::Fidelity::SummaryMedium,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(
            !preamble.contains("- start:"),
            "should not show start stage, got:\n{preamble}"
        );
        assert!(preamble.contains("- work:"), "should show non-meta stages");
    }

    #[test]
    fn summary_low_excludes_start_node() {
        let mut graph = Graph::new("test");
        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        graph.nodes.insert("start".to_string(), start);
        let work = Node::new("work");
        graph.nodes.insert("work".to_string(), work);

        let context = Context::new();
        let completed_nodes = vec!["start".to_string(), "work".to_string()];
        let mut node_outcomes: HashMap<String, Outcome> = HashMap::new();
        node_outcomes.insert("start".to_string(), Outcome::success());
        node_outcomes.insert("work".to_string(), Outcome::success());

        let preamble = build_preamble(
            keys::Fidelity::SummaryLow,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(
            !preamble.contains("- start:"),
            "should not show start stage, got:\n{preamble}"
        );
        assert!(preamble.contains("- work:"), "should show non-meta stages");
    }

    #[test]
    fn summary_medium_no_recent_stages_when_only_start() {
        let mut graph = Graph::new("test");
        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        graph.nodes.insert("start".to_string(), start);

        let context = Context::new();
        let completed_nodes = vec!["start".to_string()];
        let mut node_outcomes: HashMap<String, Outcome> = HashMap::new();
        node_outcomes.insert("start".to_string(), Outcome::success());

        let preamble = build_preamble(
            keys::Fidelity::SummaryMedium,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(
            !preamble.contains("Recent stages:"),
            "should not show Recent stages header when only meta nodes, got:\n{preamble}"
        );
    }

    #[test]
    fn summary_low_no_recent_stages_when_only_start() {
        let mut graph = Graph::new("test");
        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        graph.nodes.insert("start".to_string(), start);

        let context = Context::new();
        let completed_nodes = vec!["start".to_string()];
        let mut node_outcomes: HashMap<String, Outcome> = HashMap::new();
        node_outcomes.insert("start".to_string(), Outcome::success());

        let preamble = build_preamble(
            keys::Fidelity::SummaryLow,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(
            !preamble.contains("Recent stages:"),
            "should not show Recent stages header when only meta nodes, got:\n{preamble}"
        );
    }

    #[test]
    fn compact_preamble_no_completed_stages_when_only_start() {
        let mut graph = Graph::new("test");
        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        graph.nodes.insert("start".to_string(), start);

        let context = Context::new();
        let completed_nodes = vec!["start".to_string()];
        let mut node_outcomes: HashMap<String, Outcome> = HashMap::new();
        node_outcomes.insert("start".to_string(), Outcome::success());

        let preamble = build_preamble(
            keys::Fidelity::Compact,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(
            !preamble.contains("## Completed stages"),
            "should not show Completed stages header when only meta nodes, got:\n{preamble}"
        );
    }

    // --- blank context values ---

    #[test]
    fn blank_context_values_excluded() {
        let graph = Graph::new("test");
        let context = Context::new();
        context.set("failure_class", serde_json::json!(""));
        context.set("failure_signature", serde_json::json!(""));
        context.set("user.name", serde_json::json!("alice"));
        let completed_nodes: Vec<String> = Vec::new();
        let node_outcomes: HashMap<String, Outcome> = HashMap::new();

        let preamble = build_preamble(
            keys::Fidelity::Compact,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(
            !preamble.contains("failure_class"),
            "should exclude blank failure_class"
        );
        assert!(
            !preamble.contains("failure_signature"),
            "should exclude blank failure_signature"
        );
        assert!(
            preamble.contains("user.name"),
            "should include non-blank context"
        );
    }

    #[test]
    fn blank_context_values_excluded_from_summary_high_table() {
        let graph = Graph::new("test");
        let context = Context::new();
        context.set("failure_class", serde_json::json!(""));
        context.set("user.name", serde_json::json!("alice"));
        let completed_nodes: Vec<String> = Vec::new();
        let node_outcomes: HashMap<String, Outcome> = HashMap::new();

        let preamble = build_preamble(
            keys::Fidelity::SummaryHigh,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(
            !preamble.contains("failure_class"),
            "should exclude blank failure_class from table"
        );
        assert!(
            preamble.contains("| user.name | alice |"),
            "should include non-blank context in table"
        );
    }

    // --- empty state ---

    #[test]
    fn build_preamble_compact_with_no_stages() {
        let graph = Graph::new("test");
        let context = Context::new();
        let completed_nodes: Vec<String> = Vec::new();
        let node_outcomes: HashMap<String, Outcome> = HashMap::new();

        let preamble = build_preamble(
            keys::Fidelity::Compact,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(
            !preamble.contains("Completed stages"),
            "should not show stages header when empty"
        );
    }

    #[test]
    fn build_preamble_prepends_parent_preamble_when_present() {
        let graph = Graph::new("test");
        let context = Context::new();
        context.set(
            keys::INTERNAL_PARENT_PREAMBLE,
            serde_json::json!("Parent completed plan and review"),
        );
        let completed_nodes: Vec<String> = Vec::new();
        let node_outcomes: HashMap<String, Outcome> = HashMap::new();

        let preamble = build_preamble(
            keys::Fidelity::Compact,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(
            preamble.contains("## Parent workflow context"),
            "should contain parent section header"
        );
        assert!(
            preamble.contains("Parent completed plan and review"),
            "should contain parent preamble text"
        );
        assert!(
            preamble.contains("## Current sub-workflow"),
            "should contain current sub-workflow section header"
        );
    }

    // --- tail_lines ---

    #[test]
    fn tail_lines_returns_full_text_when_under_limit() {
        let text = "line1\nline2\nline3";
        let result = tail_lines(text, 5, "");
        assert_eq!(result, text);
    }

    #[test]
    fn tail_lines_returns_full_text_at_exact_limit() {
        let text = "line1\nline2\nline3";
        let result = tail_lines(text, 3, "");
        assert_eq!(result, text);
    }

    #[test]
    fn tail_lines_truncates_and_shows_omission() {
        let text = "line1\nline2\nline3\nline4\nline5";
        let result = tail_lines(text, 2, "");
        assert_eq!(result, "(3 lines omitted)\nline4\nline5");
        assert!(!result.contains("line1"));
        assert!(!result.contains("line2"));
        assert!(!result.contains("line3"));
    }

    #[test]
    fn tail_lines_applies_indent_to_each_line() {
        let result = tail_lines("a\nb\nc", 5, "  ");
        assert_eq!(result, "  a\n  b\n  c");
    }

    #[test]
    fn tail_lines_truncates_with_indent() {
        let result = tail_lines("a\nb\nc\nd\ne", 2, ">> ");
        assert_eq!(result, ">> (3 lines omitted)\n>> d\n>> e");
    }

    #[test]
    fn compact_command_stage_truncates_long_stdout() {
        let mut graph = Graph::new("test");
        let mut build = Node::new("build");
        build.attrs.insert(
            "shape".to_string(),
            AttrValue::String("parallelogram".to_string()),
        );
        build.attrs.insert(
            "script".to_string(),
            AttrValue::String("cargo check".to_string()),
        );
        graph.nodes.insert("build".to_string(), build);

        let context = Context::new();
        let completed_nodes = vec!["build".to_string()];
        let mut node_outcomes: HashMap<String, Outcome> = HashMap::new();
        let mut outcome = Outcome::success();
        // Generate >25 lines of stdout
        let long_stdout: String = (1..=30)
            .map(|i| format!("output line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        outcome.context_updates.insert(
            keys::COMMAND_OUTPUT.to_string(),
            serde_json::json!(long_stdout),
        );
        node_outcomes.insert("build".to_string(), outcome);

        let preamble = build_preamble(
            keys::Fidelity::Compact,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(
            preamble.contains("(5 lines omitted)"),
            "should show omission indicator for long stdout, got:\n{preamble}"
        );
        assert!(
            preamble.contains("output line 30"),
            "should keep last lines"
        );
        assert!(
            !preamble.contains("output line 1\n"),
            "should drop early lines"
        );
    }

    #[test]
    fn summary_high_command_stage_truncates_long_stdout() {
        let mut graph = Graph::new("test");
        let mut build = Node::new("build");
        build.attrs.insert(
            "shape".to_string(),
            AttrValue::String("parallelogram".to_string()),
        );
        build.attrs.insert(
            "script".to_string(),
            AttrValue::String("cargo check".to_string()),
        );
        graph.nodes.insert("build".to_string(), build);

        let context = Context::new();
        let completed_nodes = vec!["build".to_string()];
        let mut node_outcomes: HashMap<String, Outcome> = HashMap::new();
        let mut outcome = Outcome::success();
        // Generate >50 lines of stdout
        let long_stdout: String = (1..=60)
            .map(|i| format!("output line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        outcome.context_updates.insert(
            keys::COMMAND_OUTPUT.to_string(),
            serde_json::json!(long_stdout),
        );
        node_outcomes.insert("build".to_string(), outcome);

        let preamble = build_preamble(
            keys::Fidelity::SummaryHigh,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(
            preamble.contains("(10 lines omitted)"),
            "should show omission indicator for long stdout, got:\n{preamble}"
        );
        assert!(
            preamble.contains("output line 60"),
            "should keep last lines"
        );
        assert!(
            !preamble.contains("output line 1\n"),
            "should drop early lines"
        );
    }

    #[test]
    fn summary_high_artifact_stdout_not_truncated() {
        let mut graph = Graph::new("test");
        let mut build = Node::new("build");
        build.attrs.insert(
            "shape".to_string(),
            AttrValue::String("parallelogram".to_string()),
        );
        graph.nodes.insert("build".to_string(), build);

        let context = Context::new();
        let completed_nodes = vec!["build".to_string()];
        let mut node_outcomes: HashMap<String, Outcome> = HashMap::new();
        let mut outcome = Outcome::success();
        // Artifact pointer — should NOT be truncated
        outcome.context_updates.insert(
            keys::COMMAND_OUTPUT.to_string(),
            serde_json::json!("file:///tmp/artifacts/stdout.txt"),
        );
        node_outcomes.insert("build".to_string(), outcome);

        let preamble = build_preamble(
            keys::Fidelity::SummaryHigh,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(
            !preamble.contains("lines omitted"),
            "artifact pointers should not be truncated, got:\n{preamble}"
        );
        assert!(
            preamble.contains("/tmp/artifacts/stdout.txt"),
            "should show artifact path"
        );
    }

    #[test]
    fn build_preamble_no_parent_preamble_when_absent() {
        let graph = Graph::new("test");
        let context = Context::new();
        let completed_nodes: Vec<String> = Vec::new();
        let node_outcomes: HashMap<String, Outcome> = HashMap::new();

        let preamble = build_preamble(
            keys::Fidelity::Compact,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(
            !preamble.contains("Parent workflow context"),
            "should not contain parent section when no parent preamble"
        );
    }
}
