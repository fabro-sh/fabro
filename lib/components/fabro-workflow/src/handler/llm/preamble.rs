use std::collections::{HashMap, HashSet};
use std::fmt::Write;

use fabro_graphviz::graph::{Graph, Node, is_llm_handler_type};

use crate::artifact::{self, PromptLargeValue, PromptValueSelection};
use crate::context::{Context, WorkflowContext, keys};
use crate::outcome::{Outcome, OutcomeExt};

const COMPACT_OUTPUT_MAX_LINES: usize = 25;
const SUMMARY_HIGH_OUTPUT_MAX_LINES: usize = 50;
const SUMMARY_MEDIUM_RECENT_STAGES: usize = 5;
const SUMMARY_LOW_RECENT_STAGES: usize = 2;

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

/// The context and outcome values [`build_preamble`] will render at
/// `fidelity`, keyed the way the prompt demotion pass looks them up.
///
/// This mirrors the builders in this module and must stay in lockstep with
/// them: demotion materializes exactly the values named here, so a value the
/// builders render but this selection omits would reach the prompt inline at
/// full size, and a value named here but never rendered would create an
/// unreferenced materialization.
#[must_use]
pub(crate) fn rendered_value_selection(
    fidelity: keys::Fidelity,
    context_values: &HashMap<String, serde_json::Value>,
    graph: &Graph,
    completed_nodes: &[String],
    node_outcomes: &HashMap<String, Outcome>,
) -> PromptValueSelection {
    use keys::Fidelity;

    let mut selection = PromptValueSelection::default();
    // Full renders no preamble, Truncate renders only goal and run ID, and
    // SummaryLow renders stage statuses without any context or output values.
    let stage_window: &[String] = match fidelity {
        Fidelity::Full | Fidelity::Truncate | Fidelity::SummaryLow => return selection,
        Fidelity::Compact | Fidelity::SummaryHigh => completed_nodes,
        Fidelity::SummaryMedium => {
            recent_stage_window(completed_nodes, SUMMARY_MEDIUM_RECENT_STAGES)
        }
    };

    let mut all_rendered_keys = HashSet::new();
    for node_id in stage_window {
        if is_meta_handler(graph, node_id) {
            continue;
        }
        let Some(outcome) = node_outcomes.get(node_id) else {
            continue;
        };
        let node = graph.nodes.get(node_id);
        let handler = node.and_then(|n| n.handler_type());
        match handler {
            Some("command") if outcome.context_updates.contains_key(keys::COMMAND_OUTPUT) => {
                selection.select_outcome_value(node_id, keys::COMMAND_OUTPUT);
            }
            // Only summary:high stage sections include the model response.
            h if is_llm_handler_type(h) && matches!(fidelity, Fidelity::SummaryHigh) => {
                let response_key = keys::response_key(node_id);
                if outcome.context_updates.contains_key(&response_key) {
                    selection.select_outcome_value(node_id, &response_key);
                }
            }
            _ => {}
        }
        all_rendered_keys.extend(stage_rendered_keys(node_id, outcome));
    }

    for (key, value) in context_values {
        if keys::is_preamble_hidden_key(key)
            || all_rendered_keys.contains(key)
            || is_blank_value(Some(value))
        {
            continue;
        }
        selection.select_context_value(key);
    }

    selection
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// The trailing slice of completed nodes a recency-limited summary shows.
/// Meta stages are filtered later, so they still consume window slots.
fn recent_stage_window(completed_nodes: &[String], recent_count: usize) -> &[String] {
    let skipped = completed_nodes.len().saturating_sub(recent_count);
    &completed_nodes[skipped..]
}

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

fn format_value(val: &serde_json::Value) -> String {
    if let Some(large) = artifact::prompt_large_value(val) {
        return format!(
            "{}; Preview: {}",
            large.location_summary(),
            format_preview(large.preview, "")
        );
    }
    match val.as_str() {
        Some(s) => s.to_string(),
        None => val.to_string(),
    }
}

fn format_preview(preview: &str, continuation_indent: &str) -> String {
    let separator = format!("\n{continuation_indent}");
    let mut rendered = preview.lines().collect::<Vec<_>>().join(&separator);
    rendered.push('…');
    rendered
}

fn append_large_value(
    parts: &mut Vec<String>,
    label: &str,
    preview_indent: &str,
    large: PromptLargeValue<'_>,
) {
    parts.push(format!("{label} ({})", large.location_summary()));
    parts.push(format!(
        "{preview_indent}Preview: {}",
        format_preview(large.preview, preview_indent)
    ));
}

fn format_large_value_table_cell(large: PromptLargeValue<'_>) -> String {
    let summary = large.location_summary().replace('|', "\\|");
    let preview = large
        .preview
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('|', "\\|");
    format!("{summary}; Preview: {preview}…")
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
            if let Some(cmd) = node.and_then(Node::script) {
                lines.push(format!("  - Script: `{cmd}`"));
            }
            if let Some(output_val) = outcome.context_updates.get(keys::COMMAND_OUTPUT) {
                if let Some(large) = artifact::prompt_large_value(output_val) {
                    append_large_value(&mut lines, "  - Output", "    ", large);
                } else {
                    let output = format_value(output_val);
                    if output.trim().is_empty() {
                        lines.push("  - Output: (empty)".to_string());
                    } else {
                        lines.push("  - Output:".to_string());
                        lines.push("    ```".to_string());
                        lines.push(tail_lines(output.trim(), COMPACT_OUTPUT_MAX_LINES, "    "));
                        lines.push("    ```".to_string());
                    }
                }
            }
            lines
        }
        h if is_llm_handler_type(h) => {
            let mut lines = Vec::new();
            if let Some(usage) = &outcome.usage {
                lines.push(format!("  - Model: {}", usage.model_id()));
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
            if let Some(cmd) = node.and_then(Node::script) {
                lines.push(format!("- Script: `{cmd}`"));
            }
            if let Some(output_val) = outcome.context_updates.get(keys::COMMAND_OUTPUT) {
                if let Some(large) = artifact::prompt_large_value(output_val) {
                    append_large_value(&mut lines, "- Output", "  ", large);
                } else if let Some(path) = artifact::artifact_path(output_val) {
                    lines.push(format!(
                        "- Output: {}",
                        artifact::format_artifact_reference(path)
                    ));
                } else {
                    let output = format_value(output_val);
                    if output.trim().is_empty() {
                        lines.push("- Output: (empty)".to_string());
                    } else {
                        lines.push("- Output:".to_string());
                        lines.push("  ```".to_string());
                        lines.push(tail_lines(
                            output.trim(),
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
            }
            if !outcome.files_touched.is_empty() {
                lines.push(format!(
                    "- Files touched: {}",
                    outcome.files_touched.join(", ")
                ));
            }
            // Include full response from context_updates (or artifact pointer)
            if let Some(resp_val) = outcome.context_updates.get(&keys::response_key(node_id)) {
                if let Some(large) = artifact::prompt_large_value(resp_val) {
                    append_large_value(&mut lines, "- Response", "  ", large);
                } else if let Some(path) = artifact::artifact_path(resp_val) {
                    lines.push(format!(
                        "- Response: {}",
                        artifact::format_artifact_reference(path)
                    ));
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
            !keys::is_preamble_hidden_key(k)
                && !rendered_keys.contains(*k)
                && !is_blank_value(snapshot.get(*k))
        })
        .collect();
    if !context_keys.is_empty() {
        context_keys.sort();
        parts.push(String::from("\n## Context"));
        for key in context_keys {
            if let Some(val) = snapshot.get(key) {
                if let Some(large) = artifact::prompt_large_value(val) {
                    append_large_value(parts, &format!("- {key}"), "  ", large);
                } else {
                    parts.push(format!("- {key}: {}", format_value(val)));
                }
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
            !keys::is_preamble_hidden_key(k)
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
                let rendered = artifact::prompt_large_value(val)
                    .map_or_else(|| format_value(val), format_large_value_table_cell);
                parts.push(format!("| {key} | {rendered} |"));
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

            let stages_to_show = recent_stage_window(completed_nodes, SUMMARY_MEDIUM_RECENT_STAGES);
            let skipped = stage_count - stages_to_show.len();
            if skipped > 0 {
                parts.push(format!("\n({skipped} earlier stage(s) omitted)"));
            }

            {
                let mut header_emitted = false;
                for node_id in stages_to_show {
                    if is_meta_handler(graph, node_id) {
                        continue;
                    }
                    if !header_emitted {
                        parts.push(String::from("\nRecent stages:"));
                        header_emitted = true;
                    }
                    if let Some(outcome) = node_outcomes.get(node_id) {
                        let status = outcome.status.to_string();
                        let mut line = format!("- {node_id}: {status}");
                        if let Some(notes) = outcome.notes.as_deref() {
                            let _ = write!(line, " ({notes})");
                        }
                        if let Some(reason) = outcome.failure_reason() {
                            let _ = write!(line, " [reason: {reason}]");
                        }
                        parts.push(line);

                        let node = graph.nodes.get(node_id);
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

            let stages_to_show = recent_stage_window(completed_nodes, SUMMARY_LOW_RECENT_STAGES);
            let skipped = stage_count - stages_to_show.len();
            if skipped > 0 {
                parts.push(format!("\n({skipped} earlier stage(s) omitted)"));
            }

            {
                let mut header_emitted = false;
                for node_id in stages_to_show {
                    if is_meta_handler(graph, node_id) {
                        continue;
                    }
                    if !header_emitted {
                        parts.push(String::from("\nRecent stages:"));
                        header_emitted = true;
                    }
                    if let Some(outcome) = node_outcomes.get(node_id) {
                        let status = outcome.status.to_string();
                        let mut line = format!("- {node_id}: {status}");
                        if let Some(notes) = outcome.notes.as_deref() {
                            let _ = write!(line, " ({notes})");
                        }
                        if let Some(reason) = outcome.failure_reason() {
                            let _ = write!(line, " [reason: {reason}]");
                        }
                        parts.push(line);

                        let node = graph.nodes.get(node_id);
                        let handler = node.and_then(|n| n.handler_type());
                        if let Some(h) = handler {
                            parts.push(format!("  - Handler: {h}"));
                        }
                        match handler {
                            Some("command") => {
                                if let Some(cmd) = node.and_then(Node::script) {
                                    parts.push(format!("  - Script: `{cmd}`"));
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
    use fabro_model::{Catalog, ModelRef, ProviderId};

    use super::*;
    use crate::outcome::{BilledModelUsage, billed_model_usage_from_llm};

    fn stage_usage(model: &str, input_tokens: i64, output_tokens: i64) -> BilledModelUsage {
        billed_model_usage_from_llm(
            Catalog::builtin(),
            &ModelRef {
                provider: ProviderId::anthropic(),
                model_id: model.into(),
                speed:    None,
            },
            &TokenCounts {
                input_tokens,
                output_tokens,
                ..TokenCounts::default()
            },
        )
        .unwrap()
    }

    fn large_prompt_value(bytes: u64, path: &str, preview: &str) -> serde_json::Value {
        serde_json::json!({
            "fabroLargeValue": {
                "bytes": bytes,
                "path": path,
                "hint": "too large to inline; read this file for the full value",
                "preview": preview,
            }
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
            preamble.contains("succeeded"),
            "should show plan's success status"
        );
        assert!(
            preamble.contains("**code**"),
            "should list completed stage 'code' in bold"
        );
        assert!(
            preamble.contains("failed"),
            "should show code's fail status"
        );
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
    fn compact_preamble_renders_large_values_without_marker_chrome() {
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("Review security findings".to_string()),
        );
        let mut scan = Node::new("scan");
        scan.attrs.insert(
            "shape".to_string(),
            AttrValue::String("parallelogram".to_string()),
        );
        scan.attrs.insert(
            "script".to_string(),
            AttrValue::String("scan --json".to_string()),
        );
        graph.nodes.insert("scan".to_string(), scan);

        let context = Context::new();
        context.set(
            "security_findings",
            large_prompt_value(
                1_843_279,
                "/workspace/.fabro/blobs/findings.json",
                "{\"findings\":[\n{\"severity\":\"high\"}",
            ),
        );
        let completed_nodes = vec!["scan".to_string()];
        let mut outcome = Outcome::success();
        outcome.context_updates.insert(
            keys::COMMAND_OUTPUT.to_string(),
            large_prompt_value(
                12 * 1024,
                "/workspace/.fabro/blobs/output.json",
                "first result\nsecond result",
            ),
        );
        let node_outcomes = HashMap::from([("scan".to_string(), outcome)]);

        let preamble = build_preamble(
            keys::Fidelity::Compact,
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert_eq!(
            preamble,
            concat!(
                "Goal: Review security findings\n",
                "\n## Completed stages\n",
                "- **scan**: succeeded\n",
                "  - Script: `scan --json`\n",
                "  - Output (12.0 KB; full value: `/workspace/.fabro/blobs/output.json`)\n",
                "    Preview: first result\n",
                "    second result…\n",
                "\n## Context\n",
                "- security_findings (1.8 MB; full value: ",
                "`/workspace/.fabro/blobs/findings.json`)\n",
                "  Preview: {\"findings\":[\n",
                "  {\"severity\":\"high\"}…\n",
            )
        );
        assert!(!preamble.contains("fabroLargeValue"));
        assert!(!preamble.contains("too large to inline"));
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
        assert!(preamble.contains("succeeded"), "should show success status");
    }

    // --- compact handler-specific details ---

    #[test]
    fn compact_command_stage_shows_command_output() {
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
        assert!(preamble.contains("Output:"), "should show output label");
        assert!(preamble.contains("10 passed"), "should show output content");
        assert!(
            !preamble.contains("Stderr:"),
            "should not show stderr label"
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
            !preamble.contains("tokens"),
            "token accounting must stay out of the agent-facing preamble; agents \
             read it as a budget signal, got:\n{preamble}"
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
        let completed_nodes = vec!["step".to_string()];
        let mut node_outcomes: HashMap<String, Outcome> = HashMap::new();
        let mut outcome = Outcome::success();
        outcome
            .context_updates
            .insert(keys::COMMAND_OUTPUT.to_string(), serde_json::json!("hi\n"));
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
        // Low mode should NOT include output
        assert!(
            !preamble.contains("Output:"),
            "should not show output in low mode"
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
            "should show output via compact renderer"
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
            serde_json::json!("All tests passed\nwarning: unused var\n"),
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
            "should include output"
        );
        assert!(
            preamble.contains("warning: unused var"),
            "should include merged stderr"
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
        assert!(
            !preamble.contains("tokens"),
            "token accounting must stay out of the agent-facing preamble; agents \
             read it as a budget signal, got:\n{preamble}"
        );
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
    fn summary_high_table_compacts_large_value_preview() {
        let graph = Graph::new("test");
        let context = Context::new();
        context.set(
            "security_findings",
            large_prompt_value(
                1_843_279,
                "/workspace/.fabro/blobs/findings.json",
                "{\"findings\": [\n{\"message\": \"a | b\"}]}",
            ),
        );

        let preamble = build_preamble(
            keys::Fidelity::SummaryHigh,
            &context,
            &graph,
            &[],
            &HashMap::new(),
        );

        assert!(preamble.contains(concat!(
            "| security_findings | 1.8 MB; full value: ",
            "`/workspace/.fabro/blobs/findings.json`; Preview: ",
            "{\"findings\": [ {\"message\": \"a \\| b\"}]}… |",
        )));
        assert!(!preamble.contains("fabroLargeValue"));
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

    // --- is_preamble_hidden_key ---

    #[test]
    fn is_preamble_hidden_key_checks() {
        assert!(keys::is_preamble_hidden_key(keys::INTERNAL_FIDELITY));
        assert!(keys::is_preamble_hidden_key(&keys::retry_count_key("plan")));
        assert!(keys::is_preamble_hidden_key(keys::CURRENT_NODE));
        assert!(keys::is_preamble_hidden_key(keys::CURRENT_PREAMBLE));
        assert!(keys::is_preamble_hidden_key(&keys::graph_attr_key(
            "default_fidelity"
        )));
        assert!(keys::is_preamble_hidden_key(keys::GRAPH_GOAL));
        assert!(keys::is_preamble_hidden_key(
            &keys::thread_current_node_key("main")
        ));
        assert!(keys::is_preamble_hidden_key(&keys::response_key("plan")));
        assert!(keys::is_preamble_hidden_key(keys::OUTCOME));
        assert!(keys::is_preamble_hidden_key(keys::LAST_STAGE));
        assert!(keys::is_preamble_hidden_key(keys::LAST_RESPONSE));
        assert!(keys::is_preamble_hidden_key(keys::PREFERRED_LABEL));
        assert!(!keys::is_preamble_hidden_key("user.name"));
        assert!(!keys::is_preamble_hidden_key("custom.key"));
        assert!(!keys::is_preamble_hidden_key(keys::COMMAND_OUTPUT));
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
    fn compact_command_stage_truncates_long_output() {
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
        // Generate >25 lines of output
        let long_output: String = (1..=30)
            .map(|i| format!("output line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        outcome.context_updates.insert(
            keys::COMMAND_OUTPUT.to_string(),
            serde_json::json!(long_output),
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
            "should show omission indicator for long output, got:\n{preamble}"
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
    fn summary_high_command_stage_truncates_long_output() {
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
        // Generate >50 lines of output
        let long_output: String = (1..=60)
            .map(|i| format!("output line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        outcome.context_updates.insert(
            keys::COMMAND_OUTPUT.to_string(),
            serde_json::json!(long_output),
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
            "should show omission indicator for long output, got:\n{preamble}"
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
    fn summary_high_artifact_output_not_truncated() {
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
        // Artifact pointer should not be truncated.
        outcome.context_updates.insert(
            keys::COMMAND_OUTPUT.to_string(),
            serde_json::json!("file:///tmp/artifacts/output.txt"),
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
            preamble.contains("/tmp/artifacts/output.txt"),
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

    // --- rendered_value_selection ---

    /// Distinctive fill patterns so a leaked raw value is detectable in a
    /// rendered preamble.
    const RAW_RESPONSE_FILL: &str = "RRRR";
    const STRUCTURED_OUTPUT_FILL: &str = "OOOO";
    const COMMAND_OUTPUT_FILL: &str = "DDDD";
    const EXTRA_CONTEXT_FILL: &str = "NNNN";

    /// A completed agent stage (`plan`) with a raw response and a structured
    /// output, followed by a completed command stage (`run_cmd`), with all
    /// values present both in the outcomes and in the context snapshot the
    /// way `ExecutionState::record` applies them.
    fn selection_fixture() -> (
        Graph,
        Vec<String>,
        HashMap<String, Outcome>,
        HashMap<String, serde_json::Value>,
    ) {
        let mut graph = Graph::new("selection");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("Ship the fix".to_string()),
        );
        let mut plan = Node::new("plan");
        plan.attrs
            .insert("shape".to_string(), AttrValue::String("box".to_string()));
        let mut run_cmd = Node::new("run_cmd");
        run_cmd.attrs.insert(
            "shape".to_string(),
            AttrValue::String("parallelogram".to_string()),
        );
        run_cmd.attrs.insert(
            "script".to_string(),
            AttrValue::String("make test".to_string()),
        );
        graph.nodes.insert("plan".to_string(), plan);
        graph.nodes.insert("run_cmd".to_string(), run_cmd);

        let raw_response = RAW_RESPONSE_FILL.repeat(200);
        let structured_output = serde_json::json!({"rows": STRUCTURED_OUTPUT_FILL.repeat(200)});
        let command_output = COMMAND_OUTPUT_FILL.repeat(200);

        let mut plan_outcome = Outcome::success();
        plan_outcome.context_updates.insert(
            "response.plan".to_string(),
            serde_json::json!(raw_response.clone()),
        );
        plan_outcome
            .context_updates
            .insert("output.plan".to_string(), structured_output.clone());
        let mut cmd_outcome = Outcome::success();
        cmd_outcome.context_updates.insert(
            keys::COMMAND_OUTPUT.to_string(),
            serde_json::json!(command_output.clone()),
        );

        let completed = vec!["plan".to_string(), "run_cmd".to_string()];
        let outcomes = HashMap::from([
            ("plan".to_string(), plan_outcome),
            ("run_cmd".to_string(), cmd_outcome),
        ]);
        let context_values = HashMap::from([
            ("response.plan".to_string(), serde_json::json!(raw_response)),
            ("output.plan".to_string(), structured_output),
            (
                keys::COMMAND_OUTPUT.to_string(),
                serde_json::json!(command_output),
            ),
            (
                "notes.data".to_string(),
                serde_json::json!(EXTRA_CONTEXT_FILL.repeat(200)),
            ),
            (
                keys::INTERNAL_RUN_ID.to_string(),
                serde_json::json!("run-1"),
            ),
        ]);
        (graph, completed, outcomes, context_values)
    }

    #[test]
    fn rendered_selection_is_empty_for_full_truncate_and_summary_low() {
        let (graph, completed, outcomes, context_values) = selection_fixture();

        for fidelity in [
            keys::Fidelity::Full,
            keys::Fidelity::Truncate,
            keys::Fidelity::SummaryLow,
        ] {
            let selection =
                rendered_value_selection(fidelity, &context_values, &graph, &completed, &outcomes);
            assert!(
                selection.is_empty(),
                "{fidelity} renders no values, so nothing may be selected"
            );
        }
    }

    #[test]
    fn rendered_selection_compact_omits_llm_response() {
        let (graph, completed, outcomes, context_values) = selection_fixture();

        let selection = rendered_value_selection(
            keys::Fidelity::Compact,
            &context_values,
            &graph,
            &completed,
            &outcomes,
        );

        let mut expected = PromptValueSelection::default();
        expected.select_outcome_value("run_cmd", keys::COMMAND_OUTPUT);
        expected.select_context_value("output.plan");
        expected.select_context_value("notes.data");
        assert_eq!(selection, expected);
    }

    #[test]
    fn rendered_selection_summary_high_includes_llm_response() {
        let (graph, completed, outcomes, context_values) = selection_fixture();

        let selection = rendered_value_selection(
            keys::Fidelity::SummaryHigh,
            &context_values,
            &graph,
            &completed,
            &outcomes,
        );

        let mut expected = PromptValueSelection::default();
        expected.select_outcome_value("plan", &keys::response_key("plan"));
        expected.select_outcome_value("run_cmd", keys::COMMAND_OUTPUT);
        expected.select_context_value("output.plan");
        expected.select_context_value("notes.data");
        assert_eq!(selection, expected);
    }

    #[test]
    fn rendered_selection_medium_ignores_stages_outside_recent_window() {
        let (graph, _, outcomes, context_values) = selection_fixture();
        // run_cmd falls outside the medium window of recent stages, so its
        // outcome output is not rendered — but the context copy of
        // command.output then is, because no shown stage supersedes it.
        let mut completed = vec!["run_cmd".to_string()];
        completed.extend((0..SUMMARY_MEDIUM_RECENT_STAGES).map(|i| format!("later_{i}")));

        let selection = rendered_value_selection(
            keys::Fidelity::SummaryMedium,
            &context_values,
            &graph,
            &completed,
            &outcomes,
        );

        assert!(!selection.renders_outcome_value("run_cmd", keys::COMMAND_OUTPUT));
        assert!(selection.renders_context_value(keys::COMMAND_OUTPUT));
        assert!(selection.renders_context_value("output.plan"));
    }

    /// Drift guard between [`rendered_value_selection`] and the builders: at
    /// every fidelity, demoting exactly the selected values yields a preamble
    /// that references every demoted path and inlines no unselected fill.
    #[test]
    fn rendered_selection_matches_builders_at_every_fidelity() {
        let (graph, completed, outcomes, context_values) = selection_fixture();

        for &fidelity in keys::Fidelity::variants() {
            let selection =
                rendered_value_selection(fidelity, &context_values, &graph, &completed, &outcomes);

            let mut demoted_values = context_values.clone();
            let mut demoted_outcomes = outcomes.clone();
            let mut expected_paths = Vec::new();
            for (key, value) in &mut demoted_values {
                if selection.renders_context_value(key) {
                    let path = format!("/blobs/context-{key}.json");
                    *value = large_prompt_value(9_000, &path, "preview");
                    expected_paths.push(path);
                }
            }
            for (node_id, outcome) in &mut demoted_outcomes {
                for (key, value) in &mut outcome.context_updates {
                    if selection.renders_outcome_value(node_id, key) {
                        let path = format!("/blobs/{node_id}-{key}.json");
                        *value = large_prompt_value(9_000, &path, "preview");
                        expected_paths.push(path);
                    }
                }
            }

            let context = Context::new();
            for (key, value) in demoted_values {
                context.set(key, value);
            }
            let preamble =
                build_preamble(fidelity, &context, &graph, &completed, &demoted_outcomes);

            for path in &expected_paths {
                assert!(
                    preamble.contains(path),
                    "{fidelity}: selected value was not referenced: {path}\n{preamble}"
                );
            }
            for fill in [
                RAW_RESPONSE_FILL,
                STRUCTURED_OUTPUT_FILL,
                COMMAND_OUTPUT_FILL,
                EXTRA_CONTEXT_FILL,
            ] {
                assert!(
                    !preamble.contains(fill),
                    "{fidelity}: unselected large value leaked inline ({fill})\n{preamble}"
                );
            }
        }
    }
}
