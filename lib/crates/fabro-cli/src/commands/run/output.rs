use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use fabro_api::types;
use fabro_store::RuntimeState;
use fabro_types::PullRequestRecord;
use fabro_util::check_report::{CheckDetail, CheckReport, CheckResult, CheckSection, CheckStatus};
use fabro_util::terminal::Styles;
use fabro_util::text::strip_goal_decoration;
use fabro_workflow::artifact_snapshot::collect_artifact_paths;
use fabro_workflow::outcome::{StageStatus, format_cost};
use fabro_workflow::records::Conclusion;
use indicatif::HumanDuration;

use crate::server_client;
use crate::shared::{format_tokens_human, print_diagnostics, relative_path, tilde_path};

pub(crate) fn print_preflight_workflow_summary(
    workflow: &types::PreflightWorkflowSummary,
    graph_path_override: Option<&Path>,
    styles: &Styles,
) {
    let graph_path = graph_path_override
        .map(relative_path)
        .or_else(|| {
            workflow.graph_path.as_deref().map(|path| {
                let path = Path::new(path);
                if path.is_absolute() {
                    relative_path(path)
                } else {
                    path.display().to_string()
                }
            })
        })
        .unwrap_or_else(|| "<inline>".to_string());
    let diagnostics = workflow
        .diagnostics
        .iter()
        .map(api_diagnostic_to_local)
        .collect::<Vec<_>>();

    eprintln!(
        "{} {} {}",
        styles.bold.apply_to("Workflow:"),
        workflow.name,
        styles.dim.apply_to(format!(
            "({} nodes, {} edges)",
            workflow.nodes, workflow.edges
        )),
    );
    eprintln!(
        "{} {}",
        styles.dim.apply_to("Graph:"),
        styles.dim.apply_to(graph_path),
    );

    if !workflow.goal.is_empty() {
        let stripped = strip_goal_decoration(&workflow.goal);
        eprintln!("{} {stripped}\n", styles.bold.apply_to("Goal:"));
    }

    print_diagnostics(&diagnostics, styles);
}

fn api_diagnostic_to_local(diagnostic: &types::WorkflowDiagnostic) -> fabro_validate::Diagnostic {
    fabro_validate::Diagnostic {
        rule: diagnostic.rule.clone(),
        severity: match diagnostic.severity {
            types::WorkflowDiagnosticSeverity::Error => fabro_validate::Severity::Error,
            types::WorkflowDiagnosticSeverity::Warning => fabro_validate::Severity::Warning,
            types::WorkflowDiagnosticSeverity::Info => fabro_validate::Severity::Info,
        },
        message: diagnostic.message.clone(),
        node_id: diagnostic.node_id.clone(),
        edge: diagnostic
            .edge
            .as_ref()
            .map(|edge| (edge[0].clone(), edge[1].clone())),
        fix: diagnostic.fix.clone(),
    }
}

pub(crate) fn api_diagnostics_to_local(
    diagnostics: &[types::WorkflowDiagnostic],
) -> Vec<fabro_validate::Diagnostic> {
    diagnostics.iter().map(api_diagnostic_to_local).collect()
}

pub(crate) fn api_check_report_to_local(report: &types::PreflightCheckReport) -> CheckReport {
    CheckReport {
        title: report.title.clone(),
        sections: report
            .sections
            .iter()
            .map(|section| CheckSection {
                title: section.title.clone(),
                checks: section
                    .checks
                    .iter()
                    .map(|check| CheckResult {
                        name: check.name.clone(),
                        status: match check.status {
                            types::PreflightCheckResultStatus::Pass => CheckStatus::Pass,
                            types::PreflightCheckResultStatus::Warning => CheckStatus::Warning,
                            types::PreflightCheckResultStatus::Error => CheckStatus::Error,
                        },
                        summary: check.summary.clone(),
                        details: check
                            .details
                            .iter()
                            .map(|detail| CheckDetail {
                                text: detail.text.clone(),
                                warn: detail.warn,
                            })
                            .collect(),
                        remediation: check.remediation.clone(),
                    })
                    .collect(),
            })
            .collect(),
    }
}

pub(crate) async fn print_run_summary_with_client(
    client: &server_client::ServerStoreClient,
    run_id: &fabro_types::RunId,
    local_run_dir: Option<&Path>,
    styles: &Styles,
) -> Result<()> {
    let run_state = client.get_run_state(run_id).await?;
    let checkpoint = run_state.checkpoint.clone();
    let conclusion = run_state.conclusion.clone();
    let pr_url = run_state
        .pull_request
        .as_ref()
        .map(|record: &PullRequestRecord| record.html_url.clone());
    let Some(conclusion) = conclusion else {
        return Ok(());
    };

    print_run_conclusion(
        &conclusion,
        run_id,
        local_run_dir,
        None,
        pr_url.as_deref(),
        styles,
    );
    print_final_output(checkpoint.as_ref(), styles);
    if let Some(run_dir) = local_run_dir {
        print_assets(run_dir, styles);
    }
    Ok(())
}

pub(crate) fn print_run_conclusion(
    conclusion: &Conclusion,
    run_id: impl std::fmt::Display,
    run_dir: Option<&Path>,
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

    if let Some(run_dir) = run_dir {
        eprintln!(
            "{}",
            styles
                .dim
                .apply_to(format!("Run:       {}", tilde_path(run_dir)))
        );
    }

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

pub(crate) fn print_final_output(checkpoint: Option<&fabro_types::Checkpoint>, styles: &Styles) {
    let Some(checkpoint) = checkpoint else {
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
    let paths = collect_artifact_paths(&runtime_state.artifacts_dir());
    if paths.is_empty() {
        return;
    }
    let home = dirs::home_dir();
    eprintln!("\n{}", styles.bold.apply_to("=== Artifacts ==="));
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
