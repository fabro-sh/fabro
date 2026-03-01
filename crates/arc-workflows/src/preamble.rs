use std::collections::{HashMap, HashSet};

use crate::artifact::{artifact_path, format_artifact_reference};
use crate::context::Context;
use crate::graph::{Graph, Node};
use crate::outcome::Outcome;

/// Build a fidelity-appropriate preamble string for non-full context modes.
///
/// The preamble provides prior conversation context to the next LLM session,
/// tailored by the fidelity mode:
/// - `truncate`: Only graph goal and run ID
/// - `compact`: Nested-bullet summary with handler-specific sub-items
/// - `summary:low`: Brief textual summary (~600 token target)
/// - `summary:medium`: Moderate detail (~1500 token target)
/// - `summary:high`: Detailed per-stage Markdown report
#[must_use]
pub fn build_preamble(
    fidelity: &str,
    context: &Context,
    graph: &Graph,
    completed_nodes: &[String],
    node_outcomes: &HashMap<String, Outcome>,
) -> String {
    let goal = graph.goal();
    let run_id = context.get_string("internal.run_id", "unknown");

    match fidelity {
        "truncate" => {
            format!("Goal: {goal}\nRun ID: {run_id}\n")
        }
        "compact" => build_compact_preamble(goal, completed_nodes, node_outcomes, context, graph),
        "summary:low" => build_summary_preamble(
            goal,
            &run_id,
            completed_nodes,
            node_outcomes,
            context,
            graph,
            SummaryDetail::Low,
        ),
        "summary:medium" => build_summary_preamble(
            goal,
            &run_id,
            completed_nodes,
            node_outcomes,
            context,
            graph,
            SummaryDetail::Medium,
        ),
        "summary:high" => build_summary_preamble(
            goal,
            &run_id,
            completed_nodes,
            node_outcomes,
            context,
            graph,
            SummaryDetail::High,
        ),
        _ => {
            // Unknown fidelity mode: fall back to compact
            build_compact_preamble(goal, completed_nodes, node_outcomes, context, graph)
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn is_context_key_excluded(key: &str) -> bool {
    key.starts_with("internal.")
        || key.starts_with("current")
        || key.starts_with("graph.")
        || key.starts_with("thread.")
        || key.starts_with("response.")
        || key == "outcome"
        || key == "last_stage"
        || key == "last_response"
        || key == "preferred_label"
}

fn format_value(val: &serde_json::Value) -> String {
    match val.as_str() {
        Some(s) => s.to_string(),
        None => val.to_string(),
    }
}

fn format_token_count(tokens: i64) -> String {
    if tokens >= 1000 {
        let k = tokens as f64 / 1000.0;
        // One decimal place, strip trailing zero after decimal
        let formatted = format!("{k:.1}");
        format!("{formatted}k")
    } else {
        tokens.to_string()
    }
}

/// Returns the set of context keys that are rendered inline under a stage's
/// handler-specific details, so they can be skipped in the trailing context section.
fn stage_rendered_keys(node_id: &str, outcome: &Outcome) -> HashSet<String> {
    let candidates = [
        "script.output".to_string(),
        "script.stderr".to_string(),
        "last_stage".to_string(),
        "last_response".to_string(),
        format!("response.{node_id}"),
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
        Some("script") => {
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
            if let Some(stdout_val) = outcome.context_updates.get("script.output") {
                let stdout = format_value(stdout_val);
                if stdout.trim().is_empty() {
                    lines.push("  - Stdout: (empty)".to_string());
                } else {
                    lines.push("  - Stdout:".to_string());
                    lines.push("    ```".to_string());
                    lines.push(format!("    {}", stdout.trim()));
                    lines.push("    ```".to_string());
                }
            }
            if let Some(stderr_val) = outcome.context_updates.get("script.stderr") {
                let stderr = format_value(stderr_val);
                if stderr.trim().is_empty() {
                    lines.push("  - Stderr: (empty)".to_string());
                } else {
                    lines.push("  - Stderr:".to_string());
                    lines.push("    ```".to_string());
                    lines.push(format!("    {}", stderr.trim()));
                    lines.push("    ```".to_string());
                }
            }
            lines
        }
        Some("codergen") => {
            let mut lines = Vec::new();
            if let Some(usage) = &outcome.usage {
                let input = format_token_count(usage.input_tokens);
                let output = format_token_count(usage.output_tokens);
                lines.push(format!(
                    "  - Model: {}, {} tokens in / {} out",
                    usage.model, input, output
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
        Some("script") => {
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
            if let Some(stdout_val) = outcome.context_updates.get("script.output") {
                if let Some(path) = artifact_path(stdout_val) {
                    lines.push(format!("- Stdout: {}", format_artifact_reference(path)));
                } else {
                    let stdout = format_value(stdout_val);
                    if stdout.trim().is_empty() {
                        lines.push("- Stdout: (empty)".to_string());
                    } else {
                        lines.push("- Stdout:".to_string());
                        lines.push("  ```".to_string());
                        lines.push(format!("  {}", stdout.trim()));
                        lines.push("  ```".to_string());
                    }
                }
            }
            if let Some(stderr_val) = outcome.context_updates.get("script.stderr") {
                if let Some(path) = artifact_path(stderr_val) {
                    lines.push(format!("- Stderr: {}", format_artifact_reference(path)));
                } else {
                    let stderr = format_value(stderr_val);
                    if stderr.trim().is_empty() {
                        lines.push("- Stderr: (empty)".to_string());
                    } else {
                        lines.push("- Stderr:".to_string());
                        lines.push("  ```".to_string());
                        lines.push(format!("  {}", stderr.trim()));
                        lines.push("  ```".to_string());
                    }
                }
            }
        }
        Some("codergen") => {
            if let Some(usage) = &outcome.usage {
                lines.push(format!("- Model: {}", usage.model));
                lines.push(format!(
                    "- Tokens: {} in / {} out",
                    format_token_count(usage.input_tokens),
                    format_token_count(usage.output_tokens)
                ));
            }
            if !outcome.files_touched.is_empty() {
                lines.push(format!(
                    "- Files touched: {}",
                    outcome.files_touched.join(", ")
                ));
            }
            // Include full response from context_updates (or artifact pointer)
            if let Some(resp_val) = outcome.context_updates.get(&format!("response.{node_id}")) {
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
            if let Some(reason) = outcome.failure_reason.as_deref() {
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
        .filter(|k| !is_context_key_excluded(k) && !rendered_keys.contains(*k))
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
        .filter(|k| !is_context_key_excluded(k) && !rendered_keys.contains(*k))
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

    if !completed_nodes.is_empty() {
        parts.push(String::from("\n## Completed stages"));
        for node_id in completed_nodes {
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
            // Pipeline progress: count all nodes (including start/exit)
            let total_nodes = graph.nodes.len();
            let completed_count = completed_nodes.len();
            parts.push(format!(
                "Pipeline progress: {completed_count} of {total_nodes} stages completed"
            ));

            for node_id in completed_nodes {
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

            if !stages_to_show.is_empty() {
                parts.push(String::from("\nRecent stages:"));
                for node_id in &stages_to_show {
                    if let Some(outcome) = node_outcomes.get(*node_id) {
                        let status = outcome.status.to_string();
                        let mut line = format!("- {node_id}: {status}");
                        if let Some(notes) = outcome.notes.as_deref() {
                            line.push_str(&format!(" ({notes})"));
                        }
                        if let Some(reason) = outcome.failure_reason.as_deref() {
                            line.push_str(&format!(" [reason: {reason}]"));
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

            if !stages_to_show.is_empty() {
                parts.push(String::from("\nRecent stages:"));
                for node_id in &stages_to_show {
                    if let Some(outcome) = node_outcomes.get(*node_id) {
                        let status = outcome.status.to_string();
                        let mut line = format!("- {node_id}: {status}");
                        if let Some(notes) = outcome.notes.as_deref() {
                            line.push_str(&format!(" ({notes})"));
                        }
                        if let Some(reason) = outcome.failure_reason.as_deref() {
                            line.push_str(&format!(" [reason: {reason}]"));
                        }
                        parts.push(line);

                        let node = graph.nodes.get(*node_id);
                        let handler = node.and_then(|n| n.handler_type());
                        if let Some(h) = handler {
                            parts.push(format!("  - Handler: {h}"));
                        }
                        match handler {
                            Some("script") => {
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
                            Some("codergen") => {
                                if let Some(usage) = &outcome.usage {
                                    parts.push(format!("  - Model: {}", usage.model));
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
    use super::*;
    use crate::graph::AttrValue;
    use crate::outcome::StageUsage;

    // --- truncate mode ---

    #[test]
    fn build_preamble_truncate_includes_goal_and_run_id() {
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("Fix the login bug".to_string()),
        );
        let context = Context::new();
        context.set("internal.run_id", serde_json::json!("abc-123"));
        let completed_nodes: Vec<String> = Vec::new();
        let node_outcomes: HashMap<String, Outcome> = HashMap::new();

        let preamble = build_preamble(
            "truncate",
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
            "truncate",
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
        context.set("internal.run_id", serde_json::json!("run-456"));
        let completed_nodes = vec!["plan".to_string(), "code".to_string()];
        let mut node_outcomes: HashMap<String, Outcome> = HashMap::new();
        node_outcomes.insert("plan".to_string(), Outcome::success());
        node_outcomes.insert("code".to_string(), Outcome::fail("compilation error"));

        let preamble = build_preamble(
            "compact",
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
        context.set("graph.goal", serde_json::json!("Build it"));
        context.set("user.name", serde_json::json!("alice"));
        let completed_nodes: Vec<String> = Vec::new();
        let node_outcomes: HashMap<String, Outcome> = HashMap::new();

        let preamble = build_preamble(
            "compact",
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
        context.set("internal.fidelity", serde_json::json!("compact"));
        context.set("internal.retry_count.plan", serde_json::json!(1));
        context.set("current_node", serde_json::json!("work"));
        context.set("graph.default_fidelity", serde_json::json!("compact"));
        context.set("thread.main.current_node", serde_json::json!("work"));
        context.set("response.plan", serde_json::json!("some response"));
        context.set("last_stage", serde_json::json!("plan"));
        context.set("last_response", serde_json::json!("resp"));
        context.set("preferred_label", serde_json::json!("success"));
        context.set("user.name", serde_json::json!("bob"));
        let completed_nodes: Vec<String> = Vec::new();
        let node_outcomes: HashMap<String, Outcome> = HashMap::new();

        let preamble = build_preamble(
            "compact",
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
            "compact",
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
    fn compact_script_stage_shows_command_stdout_stderr() {
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
            "script.output".to_string(),
            serde_json::json!("10 passed\n"),
        );
        outcome
            .context_updates
            .insert("script.stderr".to_string(), serde_json::json!(""));
        node_outcomes.insert("run_tests".to_string(), outcome);

        let preamble = build_preamble(
            "compact",
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
    fn compact_codergen_stage_shows_model_and_files() {
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
        outcome.usage = Some(StageUsage {
            model: "claude-sonnet-4-20250514".to_string(),
            input_tokens: 1234,
            output_tokens: 567,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            cost: None,
        });
        outcome.files_touched = vec!["src/lib.rs".to_string(), "src/main.rs".to_string()];
        node_outcomes.insert("report".to_string(), outcome);

        let preamble = build_preamble(
            "compact",
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
        context.set("graph.default_fidelity", serde_json::json!("compact"));
        context.set("thread.main.current_node", serde_json::json!("work"));
        context.set("response.plan", serde_json::json!("some LLM response"));
        context.set("last_stage", serde_json::json!("plan"));
        context.set("user.preference", serde_json::json!("dark"));
        let completed_nodes: Vec<String> = Vec::new();
        let node_outcomes: HashMap<String, Outcome> = HashMap::new();

        let preamble = build_preamble(
            "compact",
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
        // script.output is set in context (the engine copies context_updates to context)
        context.set("script.output", serde_json::json!("hi\n"));
        context.set("script.stderr", serde_json::json!(""));
        let completed_nodes = vec!["step".to_string()];
        let mut node_outcomes: HashMap<String, Outcome> = HashMap::new();
        let mut outcome = Outcome::success();
        outcome
            .context_updates
            .insert("script.output".to_string(), serde_json::json!("hi\n"));
        outcome
            .context_updates
            .insert("script.stderr".to_string(), serde_json::json!(""));
        node_outcomes.insert("step".to_string(), outcome);

        let preamble = build_preamble(
            "compact",
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        // script.output should NOT appear in the Context section
        // because it's already rendered inline under the stage
        let context_section = preamble.split("## Context").nth(1).unwrap_or("");
        assert!(
            !context_section.contains("script.output"),
            "script.output should be deduplicated from context section"
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
        node_outcomes.insert("test".to_string(), Outcome::fail("test failure"));

        let preamble = build_preamble(
            "summary:low",
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
        node_outcomes.insert("step4".to_string(), Outcome::fail("error"));

        let preamble = build_preamble(
            "summary:low",
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
            preamble.contains("Handler: script"),
            "should show handler type for step3"
        );
    }

    #[test]
    fn summary_low_script_stage_shows_handler_and_command() {
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
        let mut outcome = Outcome::fail("exit code 1");
        outcome.context_updates.insert(
            "script.output".to_string(),
            serde_json::json!("test failed"),
        );
        node_outcomes.insert("run_tests".to_string(), outcome);

        let preamble = build_preamble(
            "summary:low",
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(
            preamble.contains("Handler: script"),
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
    fn summary_low_codergen_stage_shows_handler_and_model() {
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
        outcome.usage = Some(StageUsage {
            model: "claude-sonnet-4-20250514".to_string(),
            input_tokens: 1000,
            output_tokens: 200,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            cost: None,
        });
        node_outcomes.insert("report".to_string(), outcome);

        let preamble = build_preamble(
            "summary:low",
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(
            preamble.contains("Handler: codergen"),
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
            "summary:low",
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
            "summary:medium",
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
            "summary:medium",
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
            "script.output".to_string(),
            serde_json::json!("All tests passed\n"),
        );
        outcome
            .context_updates
            .insert("script.stderr".to_string(), serde_json::json!(""));
        node_outcomes.insert("run_tests".to_string(), outcome);

        let preamble = build_preamble(
            "summary:medium",
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
            !preamble.contains("set script.output"),
            "should not dump raw context updates"
        );
    }

    #[test]
    fn summary_medium_codergen_stage_shows_compact_details() {
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
        outcome.usage = Some(StageUsage {
            model: "claude-sonnet-4-20250514".to_string(),
            input_tokens: 1500,
            output_tokens: 300,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            cost: None,
        });
        outcome.files_touched = vec!["src/lib.rs".to_string()];
        node_outcomes.insert("report".to_string(), outcome);

        let preamble = build_preamble(
            "summary:medium",
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
            "summary:high",
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
        node_outcomes.insert("work".to_string(), Outcome::fail("connection timeout"));

        let preamble = build_preamble(
            "summary:high",
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
        context.set("graph.goal", serde_json::json!("Build"));
        context.set("user.name", serde_json::json!("alice"));
        let completed_nodes: Vec<String> = Vec::new();
        let node_outcomes: HashMap<String, Outcome> = HashMap::new();

        let preamble = build_preamble(
            "summary:high",
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
            "summary:high",
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
    fn summary_high_script_stage_full_detail() {
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
            "script.output".to_string(),
            serde_json::json!("All tests passed\n"),
        );
        outcome.context_updates.insert(
            "script.stderr".to_string(),
            serde_json::json!("warning: unused var\n"),
        );
        node_outcomes.insert("run_tests".to_string(), outcome);

        let preamble = build_preamble(
            "summary:high",
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(
            preamble.contains("## Stage: run_tests"),
            "should have stage heading"
        );
        assert!(preamble.contains("Handler: script"), "should show handler");
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
    fn summary_high_codergen_stage_with_response_preview() {
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
        outcome.usage = Some(StageUsage {
            model: "claude-sonnet-4-20250514".to_string(),
            input_tokens: 1500,
            output_tokens: 300,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            cost: None,
        });
        outcome.files_touched = vec!["src/lib.rs".to_string()];
        outcome.context_updates.insert(
            "response.report".to_string(),
            serde_json::json!("The tests all pass successfully."),
        );
        node_outcomes.insert("report".to_string(), outcome);

        let preamble = build_preamble(
            "summary:high",
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        assert!(
            preamble.contains("## Stage: report"),
            "should have stage heading"
        );
        assert!(
            preamble.contains("Handler: codergen"),
            "should show handler"
        );
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
            "summary:high",
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
            "summary:high",
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
    }

    // --- is_context_key_excluded ---

    #[test]
    fn is_context_key_excluded_checks() {
        assert!(is_context_key_excluded("internal.fidelity"));
        assert!(is_context_key_excluded("internal.retry_count.plan"));
        assert!(is_context_key_excluded("current_node"));
        assert!(is_context_key_excluded("current.preamble"));
        assert!(is_context_key_excluded("graph.default_fidelity"));
        assert!(is_context_key_excluded("graph.goal"));
        assert!(is_context_key_excluded("thread.main.current_node"));
        assert!(is_context_key_excluded("response.plan"));
        assert!(is_context_key_excluded("outcome"));
        assert!(is_context_key_excluded("last_stage"));
        assert!(is_context_key_excluded("last_response"));
        assert!(is_context_key_excluded("preferred_label"));
        assert!(!is_context_key_excluded("user.name"));
        assert!(!is_context_key_excluded("custom.key"));
        assert!(!is_context_key_excluded("script.output"));
    }

    // --- unknown fidelity mode ---

    #[test]
    fn build_preamble_unknown_mode_falls_back_to_compact() {
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("Test fallback".to_string()),
        );
        let context = Context::new();
        let completed_nodes = vec!["step1".to_string()];
        let mut node_outcomes: HashMap<String, Outcome> = HashMap::new();
        node_outcomes.insert("step1".to_string(), Outcome::success());

        let preamble = build_preamble(
            "unknown_mode",
            &context,
            &graph,
            &completed_nodes,
            &node_outcomes,
        );

        // Should behave like compact: include goal and stages
        assert!(
            preamble.contains("Test fallback"),
            "should contain the goal"
        );
        assert!(
            preamble.contains("step1"),
            "should list completed stages like compact"
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
            "compact",
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
}
