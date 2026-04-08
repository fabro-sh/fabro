use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result};
use fabro_api::types;
use fabro_config::RunScratch;
use fabro_types::{
    PullRequestRecord, RunBlobId, RunId, parse_blob_ref, parse_legacy_blob_file_ref,
};
use fabro_util::check_report::{CheckDetail, CheckReport, CheckResult, CheckSection, CheckStatus};
use fabro_util::terminal::Styles;
use fabro_util::text::strip_goal_decoration;
use fabro_workflow::outcome::StageStatus;
use fabro_workflow::records::Conclusion;
use indicatif::HumanDuration;

use crate::server_client;
use crate::shared::{
    format_tokens_human, format_usd_micros, print_diagnostics, relative_path, tilde_path,
};

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
    let final_output =
        resolve_final_output_with_client(client, run_id, checkpoint.as_ref()).await?;
    print_final_output(final_output.as_deref(), styles);
    if let Some(run_dir) = local_run_dir {
        print_assets_with_client(client, run_id, run_dir, styles).await?;
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

    if let Some(billing) = conclusion.billing.as_ref() {
        let total_tokens = billing.total_tokens;
        if total_tokens > 0 {
            if let Some(total_usd_micros) = billing.total_usd_micros {
                if total_usd_micros > 0 {
                    eprintln!(
                        "{}",
                        styles.dim.apply_to(format!(
                            "Cost:      {} ({} toks)",
                            format_usd_micros(total_usd_micros),
                            format_tokens_human(total_tokens)
                        ))
                    );
                }
            } else {
                eprintln!(
                    "{}",
                    styles
                        .dim
                        .apply_to(format!("Toks:      {}", format_tokens_human(total_tokens)))
                );
            }
            if billing.cache_read_tokens > 0 || billing.cache_write_tokens > 0 {
                eprintln!(
                    "{}",
                    styles.dim.apply_to(format!(
                        "Cache:     {} read, {} write",
                        format_tokens_human(billing.cache_read_tokens),
                        format_tokens_human(billing.cache_write_tokens),
                    )),
                );
            }
            if billing.reasoning_tokens > 0 {
                eprintln!(
                    "{}",
                    styles.dim.apply_to(format!(
                        "Reasoning: {} tokens",
                        format_tokens_human(billing.reasoning_tokens),
                    )),
                );
            }
        } else if billing.total_usd_micros.is_none() {
            eprintln!(
                "{}",
                styles
                    .dim
                    .apply_to(format!("Toks:      {}", format_tokens_human(total_tokens)))
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

pub(crate) fn print_final_output(output: Option<&str>, styles: &Styles) {
    let Some(output) = output else {
        return;
    };
    let text = output.trim();
    if !text.is_empty() {
        eprintln!("\n{}", styles.bold.apply_to("=== Output ==="));
        eprintln!("{}", styles.render_markdown(text));
    }
}

async fn resolve_final_output_with_client(
    client: &server_client::ServerStoreClient,
    run_id: &RunId,
    checkpoint: Option<&fabro_types::Checkpoint>,
) -> Result<Option<String>> {
    let Some(checkpoint) = checkpoint else {
        return Ok(None);
    };

    for node_id in checkpoint.completed_nodes.iter().rev() {
        let key = format!("response.{node_id}");
        let Some(serde_json::Value::String(response)) = checkpoint.context_values.get(&key) else {
            continue;
        };
        let Some(output) = resolve_response_string(client, run_id, response).await? else {
            continue;
        };
        if !output.trim().is_empty() {
            return Ok(Some(output));
        }
    }

    Ok(None)
}

async fn resolve_response_string(
    client: &server_client::ServerStoreClient,
    run_id: &RunId,
    response: &str,
) -> Result<Option<String>> {
    let Some(blob_id) = blob_id_from_response(response) else {
        return Ok(Some(response.to_string()));
    };

    let Some(bytes) = client.read_run_blob(run_id, &blob_id).await? else {
        return Ok(None);
    };
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).context("blob-backed final output should be valid JSON")?;

    Ok(Some(match value {
        serde_json::Value::String(text) => text,
        other => other.to_string(),
    }))
}

fn blob_id_from_response(response: &str) -> Option<RunBlobId> {
    parse_blob_ref(response).or_else(|| parse_legacy_blob_file_ref(response))
}

async fn resolve_local_artifact_display_paths_with_client(
    client: &server_client::ServerStoreClient,
    run_id: &RunId,
    run_dir: &Path,
) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in client.list_run_artifacts(run_id).await? {
        let retry = u32::try_from(entry.retry)
            .context("server returned invalid negative artifact retry")?;
        let path = RunScratch::new(run_dir)
            .artifact_stage_dir(&entry.node_slug, retry)
            .join(entry.relative_path);
        paths.push(path);
    }
    paths.sort();
    Ok(paths)
}

async fn print_assets_with_client(
    client: &server_client::ServerStoreClient,
    run_id: &RunId,
    run_dir: &Path,
    styles: &Styles,
) -> Result<()> {
    let paths = resolve_local_artifact_display_paths_with_client(client, run_id, run_dir).await?;
    if paths.is_empty() {
        return Ok(());
    }
    let home = dirs::home_dir();
    eprintln!("\n{}", styles.bold.apply_to("=== Artifacts ==="));
    for path in &paths {
        let display = match &home {
            Some(home_dir) => {
                let home_str = home_dir.to_string_lossy();
                if let Some(rest) = path.to_string_lossy().strip_prefix(home_str.as_ref()) {
                    format!("~{rest}")
                } else {
                    path.display().to_string()
                }
            }
            None => path.display().to_string(),
        };
        eprintln!("{display}");
    }
    Ok(())
}
