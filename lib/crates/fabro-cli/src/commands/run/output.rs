use std::path::Path;
use std::time::Duration;

use fabro_graphviz::graph::Graph;
use fabro_store::RuntimeState;
use fabro_util::terminal::Styles;
use fabro_util::text::strip_goal_decoration;
use fabro_workflows::asset_snapshot::collect_asset_paths;
use fabro_workflows::outcome::{StageStatus, format_cost};
use fabro_workflows::pipeline::{Persisted, Validated};
use fabro_workflows::pull_request::PullRequestRecord;
use fabro_workflows::records::{Checkpoint, CheckpointExt, Conclusion, ConclusionExt};
use indicatif::HumanDuration;

use crate::shared::{format_tokens_human, print_diagnostics, relative_path, tilde_path};

fn print_workflow_header(
    graph: &Graph,
    diagnostics: &[fabro_validate::Diagnostic],
    dot_path: Option<&Path>,
    styles: &Styles,
) {
    eprintln!(
        "{} {} {}",
        styles.bold.apply_to("Workflow:"),
        graph.name,
        styles.dim.apply_to(format!(
            "({} nodes, {} edges)",
            graph.nodes.len(),
            graph.edges.len()
        )),
    );
    let graph_path = dot_path.map_or_else(|| "<inline>".to_string(), relative_path);
    eprintln!(
        "{} {}",
        styles.dim.apply_to("Graph:"),
        styles.dim.apply_to(graph_path),
    );

    let goal = graph.goal();
    if !goal.is_empty() {
        let stripped = strip_goal_decoration(goal);
        eprintln!("{} {stripped}\n", styles.bold.apply_to("Goal:"));
    }

    print_diagnostics(diagnostics, styles);
}

pub(crate) fn print_workflow_report(
    validated: &Validated,
    dot_path: Option<&Path>,
    styles: &Styles,
) {
    print_workflow_header(validated.graph(), validated.diagnostics(), dot_path, styles);
}

pub(crate) fn print_workflow_report_from_persisted(
    persisted: &Persisted,
    dot_path: Option<&Path>,
    styles: &Styles,
) {
    print_workflow_header(persisted.graph(), persisted.diagnostics(), dot_path, styles);
}

pub(crate) fn print_diagnostics_from_error(
    diagnostics: &[fabro_validate::Diagnostic],
    styles: &Styles,
) {
    print_diagnostics(diagnostics, styles);
}

pub(crate) fn print_run_summary(run_dir: &Path, run_id: impl std::fmt::Display, styles: &Styles) {
    let run_id = run_id.to_string();
    let conclusion_path = run_dir.join("conclusion.json");
    let Ok(conclusion) = Conclusion::load(&conclusion_path) else {
        return;
    };

    let pr_url = std::fs::read_to_string(run_dir.join("pull_request.json"))
        .ok()
        .and_then(|content| {
            serde_json::from_str::<PullRequestRecord>(&content)
                .ok()
                .map(|record| record.html_url)
        });

    print_run_conclusion(
        &conclusion,
        &run_id,
        run_dir,
        None,
        pr_url.as_deref(),
        styles,
    );
    print_final_output(run_dir, styles);
    print_assets(run_dir, styles);
}

pub(crate) fn print_run_conclusion(
    conclusion: &Conclusion,
    run_id: impl std::fmt::Display,
    run_dir: &Path,
    pushed_branch: Option<&str>,
    pr_url: Option<&str>,
    styles: &Styles,
) {
    let run_id = run_id.to_string();
    eprintln!("\n{}", styles.bold.apply_to("=== Run Result ==="));
    eprintln!("{}", styles.dim.apply_to(format!("Run:       {run_id}")));

    let status_str = conclusion.status.to_string().to_uppercase();
    let status_color = match conclusion.status {
        StageStatus::Success | StageStatus::PartialSuccess => &styles.bold_green,
        _ => &styles.bold_red,
    };
    eprintln!("Status:    {}", status_color.apply_to(&status_str));
    eprintln!(
        "Duration:  {}",
        HumanDuration(Duration::from_millis(conclusion.duration_ms))
    );

    let total_tokens = conclusion.total_input_tokens + conclusion.total_output_tokens;
    if total_tokens > 0 {
        if conclusion.has_pricing {
            if let Some(cost) = conclusion.total_cost {
                if cost > 0.0 {
                    eprintln!(
                        "{}",
                        styles.dim.apply_to(format!(
                            "Cost:      {} ({} toks)",
                            format_cost(cost),
                            format_tokens_human(total_tokens)
                        ))
                    );
                }
            }
        } else {
            eprintln!(
                "{}",
                styles
                    .dim
                    .apply_to(format!("Toks:      {}", format_tokens_human(total_tokens)))
            );
        }
        if conclusion.total_cache_read_tokens > 0 {
            eprintln!(
                "{}",
                styles.dim.apply_to(format!(
                    "Cache:     {} read, {} write",
                    format_tokens_human(conclusion.total_cache_read_tokens),
                    format_tokens_human(conclusion.total_cache_write_tokens),
                )),
            );
        }
        if conclusion.total_reasoning_tokens > 0 {
            eprintln!(
                "{}",
                styles.dim.apply_to(format!(
                    "Reasoning: {} tokens",
                    format_tokens_human(conclusion.total_reasoning_tokens),
                )),
            );
        }
    }

    eprintln!(
        "{}",
        styles
            .dim
            .apply_to(format!("Run:       {}", tilde_path(run_dir)))
    );

    if let Some(ref failure) = conclusion.failure_reason {
        eprintln!("Failure:   {}", styles.red.apply_to(failure));
    }

    if pushed_branch.is_some() || pr_url.is_some() {
        eprintln!();
        if let Some(branch) = pushed_branch {
            eprintln!("{} {branch}", styles.bold.apply_to("Pushed branch:"));
        }
        if let Some(url) = pr_url {
            eprintln!("{} {url}", styles.bold.apply_to("Pull request:"));
        }
    }
}

pub(crate) fn print_final_output(run_dir: &Path, styles: &Styles) {
    let Ok(checkpoint) = Checkpoint::load(&run_dir.join("checkpoint.json")) else {
        return;
    };

    for node_id in checkpoint.completed_nodes.iter().rev() {
        let key = format!("response.{node_id}");
        if let Some(serde_json::Value::String(response)) = checkpoint.context_values.get(&key) {
            let text = response.trim();
            if !text.is_empty() {
                eprintln!("\n{}", styles.bold.apply_to("=== Output ==="));
                eprintln!("{}", styles.render_markdown(text));
            }
            return;
        }
    }
}

pub(crate) fn print_assets(run_dir: &Path, styles: &Styles) {
    let runtime_state = RuntimeState::new(run_dir);
    let paths = collect_asset_paths(&runtime_state.assets_dir());
    if paths.is_empty() {
        return;
    }
    let home = dirs::home_dir();
    eprintln!("\n{}", styles.bold.apply_to("=== Assets ==="));
    for path in &paths {
        let display = match &home {
            Some(home_dir) => {
                let home_str = home_dir.to_string_lossy();
                if let Some(rest) = path.strip_prefix(home_str.as_ref()) {
                    format!("~{rest}")
                } else {
                    path.clone()
                }
            }
            None => path.clone(),
        };
        eprintln!("{display}");
    }
}
