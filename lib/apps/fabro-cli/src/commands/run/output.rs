use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, Result};
use cli_table::format::{Border, Justify, Separator};
use cli_table::{Cell, CellStruct, Style, Table};
use fabro_api::types;
use fabro_types::diagnostic::{Diagnostic, RelatedDiagnostic, Severity};
use fabro_types::{BlobRefEncoding, PullRequestLink, RunId, StageId, parse_blob_ref_encoded};
use fabro_util::check_report::{CheckDetail, CheckReport, CheckResult, CheckSection, CheckStatus};
use fabro_util::error::render_with_causes;
use fabro_util::printer::Printer;
use fabro_util::terminal::Styles;
use fabro_util::text::strip_goal_decoration;
use fabro_workflow::outcome::StageOutcome;
use fabro_workflow::records::Conclusion;
use indicatif::HumanDuration;

use crate::server_client;
use crate::shared::{format_tokens_human, format_usd_micros, print_diagnostics, relative_path};

pub(crate) fn print_workflow_summary(
    workflow: &types::PreflightWorkflowSummary,
    graph_path_override: Option<&Path>,
    styles: &Styles,
    printer: Printer,
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

    fabro_util::printerr!(
        printer,
        "{} {} {}",
        styles.bold.apply_to("Workflow:"),
        workflow.name,
        styles.dim.apply_to(format!(
            "({} nodes, {} edges)",
            workflow.nodes, workflow.edges
        )),
    );
    fabro_util::printerr!(
        printer,
        "{} {}",
        styles.dim.apply_to("Graph:"),
        styles.dim.apply_to(graph_path),
    );

    if !workflow.goal.is_empty() {
        let stripped = strip_goal_decoration(&workflow.goal);
        fabro_util::printerr!(printer, "{} {stripped}\n", styles.bold.apply_to("Goal:"));
    }

    print_diagnostics(&diagnostics, styles, printer);
}

fn api_diagnostic_to_local(diagnostic: &types::WorkflowDiagnostic) -> Diagnostic {
    Diagnostic {
        rule:        diagnostic.rule.clone(),
        severity:    match diagnostic.severity {
            types::WorkflowDiagnosticSeverity::Error => Severity::Error,
            types::WorkflowDiagnosticSeverity::Warning => Severity::Warning,
            types::WorkflowDiagnosticSeverity::Info => Severity::Info,
        },
        message:     diagnostic.message.clone(),
        node_id:     diagnostic.node_id.clone(),
        edge:        diagnostic
            .edge
            .as_ref()
            .map(|edge| (edge[0].clone(), edge[1].clone())),
        fix:         diagnostic.fix.clone(),
        source_path: diagnostic.source_path.clone(),
        line:        diagnostic.line.and_then(|value| u32::try_from(value).ok()),
        column:      diagnostic
            .column
            .and_then(|value| u32::try_from(value).ok()),
        span_start:  diagnostic
            .span_start
            .and_then(|value| usize::try_from(value).ok()),
        span_len:    diagnostic
            .span_len
            .and_then(|value| usize::try_from(value).ok()),
        related:     diagnostic
            .related
            .iter()
            .map(|related| RelatedDiagnostic {
                message:     related.message.clone(),
                source_path: related.source_path.clone(),
                line:        related.line.and_then(|value| u32::try_from(value).ok()),
                column:      related.column.and_then(|value| u32::try_from(value).ok()),
            })
            .collect(),
    }
}

pub(crate) fn api_diagnostics_to_local(
    diagnostics: &[types::WorkflowDiagnostic],
) -> Vec<Diagnostic> {
    diagnostics.iter().map(api_diagnostic_to_local).collect()
}

pub(crate) fn api_check_report_to_local(report: &types::PreflightCheckReport) -> CheckReport {
    CheckReport {
        title:    report.title.clone(),
        sections: report
            .sections
            .iter()
            .map(|section| CheckSection {
                title:  section.title.clone(),
                checks: section
                    .checks
                    .iter()
                    .map(|check| CheckResult {
                        name:        check.name.clone(),
                        status:      match check.status {
                            types::PreflightCheckResultStatus::Pass => CheckStatus::Pass,
                            types::PreflightCheckResultStatus::Warning => CheckStatus::Warning,
                            types::PreflightCheckResultStatus::Error => CheckStatus::Error,
                        },
                        summary:     check.summary.clone(),
                        details:     check
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
    client: &server_client::Client,
    run_id: &fabro_types::RunId,
    styles: &Styles,
    printer: Printer,
) -> Result<()> {
    let run_state = client.get_run_state(run_id).await?;
    let conclusion = run_state.conclusion.clone();
    let pr_url = run_state
        .pull_request
        .as_ref()
        .map(PullRequestLink::html_url);
    let Some(conclusion) = conclusion else {
        return Ok(());
    };

    print_run_conclusion(
        &conclusion,
        run_id,
        None,
        pr_url.as_deref(),
        styles,
        printer,
    );
    let final_output = resolve_final_output_with_client(client, run_id, &run_state).await?;
    print_final_output(final_output.as_deref(), styles, printer);
    print_assets_with_client(client, run_id, styles, printer).await?;
    Ok(())
}

pub(crate) fn print_run_conclusion(
    conclusion: &Conclusion,
    run_id: impl std::fmt::Display,
    pushed_branch: Option<&str>,
    pr_url: Option<&str>,
    styles: &Styles,
    printer: Printer,
) {
    let run_id = run_id.to_string();
    fabro_util::printerr!(printer, "\n{}", styles.bold.apply_to("=== Run Result ==="));
    fabro_util::printerr!(
        printer,
        "{}",
        styles.dim.apply_to(format!("Run:       {run_id}"))
    );

    let status_str = conclusion.status.to_string().to_uppercase();
    let status_color = match conclusion.status {
        StageOutcome::Succeeded | StageOutcome::PartiallySucceeded => &styles.bold_green,
        _ => &styles.bold_red,
    };
    fabro_util::printerr!(printer, "Status:    {}", status_color.apply_to(&status_str));
    fabro_util::printerr!(
        printer,
        "Duration:  {}",
        HumanDuration(Duration::from_millis(conclusion.timing.wall_time_ms))
    );

    if let Some(usage) = conclusion.usage {
        let total_tokens = usage.total_tokens();
        let cost_usd_micros = usage.cost.map(|cost| cost.usd_micros);
        if total_tokens > 0 {
            if let Some(total_usd_micros) = cost_usd_micros {
                if total_usd_micros > 0 {
                    fabro_util::printerr!(
                        printer,
                        "{}",
                        styles.dim.apply_to(format!(
                            "Cost:      {} ({} toks)",
                            format_usd_micros(total_usd_micros),
                            format_tokens_human(total_tokens)
                        ))
                    );
                }
            } else {
                fabro_util::printerr!(
                    printer,
                    "{}",
                    styles
                        .dim
                        .apply_to(format!("Toks:      {}", format_tokens_human(total_tokens)))
                );
            }
            if usage.tokens.cache_read > 0 || usage.tokens.cache_write > 0 {
                fabro_util::printerr!(
                    printer,
                    "{}",
                    styles.dim.apply_to(format!(
                        "Cache:     {} read, {} write",
                        format_tokens_human(usage.tokens.cache_read),
                        format_tokens_human(usage.tokens.cache_write),
                    )),
                );
            }
            if usage.tokens.reasoning > 0 {
                fabro_util::printerr!(
                    printer,
                    "{}",
                    styles.dim.apply_to(format!(
                        "Reasoning: {} tokens",
                        format_tokens_human(usage.tokens.reasoning),
                    )),
                );
            }
        } else if cost_usd_micros.is_none() {
            fabro_util::printerr!(
                printer,
                "{}",
                styles
                    .dim
                    .apply_to(format!("Toks:      {}", format_tokens_human(total_tokens)))
            );
        }
    }

    if let Some(ref failure) = conclusion.failure {
        let rendered = render_with_causes(&failure.detail.message, &failure.detail.causes);
        fabro_util::printerr!(printer, "Failure:   {}", styles.red.apply_to(rendered));
    }

    if pushed_branch.is_some() || pr_url.is_some() {
        fabro_util::printerr!(printer, "");
        if let Some(branch) = pushed_branch {
            fabro_util::printerr!(
                printer,
                "{} {branch}",
                styles.bold.apply_to("Pushed branch:")
            );
        }
        if let Some(url) = pr_url {
            fabro_util::printerr!(printer, "{} {url}", styles.bold.apply_to("Pull request:"));
        }
    }
}

pub(crate) fn print_final_output(output: Option<&str>, styles: &Styles, printer: Printer) {
    let Some(output) = output else {
        return;
    };
    let text = output.trim();
    if !text.is_empty() {
        fabro_util::printerr!(printer, "\n{}", styles.bold.apply_to("=== Output ==="));
        fabro_util::printerr!(printer, "{}", styles.render_markdown(text));
    }
}

/// The run's final output: the response of the last stage that produced
/// one, resolved from the blob table when the projection holds a blob
/// reference in its place.
async fn resolve_final_output_with_client(
    client: &server_client::Client,
    run_id: &RunId,
    run_state: &server_client::RunProjection,
) -> Result<Option<String>> {
    let responses = run_state
        .iter_stages()
        .filter_map(|(_, stage)| stage.response.clone())
        .collect::<Vec<_>>();
    for response in responses.iter().rev() {
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
    client: &server_client::Client,
    run_id: &RunId,
    response: &str,
) -> Result<Option<String>> {
    let Some((blob_hash, encoding)) = parse_blob_ref_encoded(response) else {
        return Ok(Some(response.to_string()));
    };

    let Some(bytes) = client.read_run_blob(run_id, &blob_hash).await? else {
        return Ok(None);
    };
    match encoding {
        BlobRefEncoding::Text => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
        BlobRefEncoding::Json => {
            let value: serde_json::Value = serde_json::from_slice(&bytes)
                .context("blob-backed final output should be valid JSON")?;
            Ok(Some(match value {
                serde_json::Value::String(text) => text,
                other => other.to_string(),
            }))
        }
    }
}

async fn list_artifact_display_entries_with_client(
    client: &server_client::Client,
    run_id: &RunId,
) -> Result<Vec<(StageId, u32, String)>> {
    let mut entries = Vec::new();
    for entry in client.list_run_artifacts(run_id).await? {
        let retry = u32::try_from(entry.retry)
            .context("server returned invalid negative artifact retry")?;
        let stage_id = entry
            .stage_id
            .parse()
            .context("server returned invalid artifact stage ID")?;
        entries.push((stage_id, retry, entry.relative_path));
    }
    entries.sort();
    Ok(entries)
}

async fn print_assets_with_client(
    client: &server_client::Client,
    run_id: &RunId,
    styles: &Styles,
    printer: Printer,
) -> Result<()> {
    let entries = list_artifact_display_entries_with_client(client, run_id).await?;
    if entries.is_empty() {
        return Ok(());
    }

    let use_color = styles.use_color;

    let title: Vec<CellStruct> = vec![
        "STAGE".cell().bold(use_color),
        "RETRY".cell().bold(use_color).justify(Justify::Right),
        "PATH".cell().bold(use_color),
    ];

    let rows: Vec<Vec<CellStruct>> = entries
        .iter()
        .map(|(stage_id, retry, relative_path)| {
            vec![
                stage_id.to_string().cell().bold(use_color),
                retry.cell().justify(Justify::Right),
                relative_path.clone().cell(),
            ]
        })
        .collect();

    let color_choice = if use_color {
        cli_table::ColorChoice::Auto
    } else {
        cli_table::ColorChoice::Never
    };
    let table = rows
        .table()
        .title(title)
        .color_choice(color_choice)
        .border(Border::builder().build())
        .separator(Separator::builder().build());

    fabro_util::printerr!(printer, "\n{}", styles.bold.apply_to("=== Artifacts ==="));
    fabro_util::printerr!(
        printer,
        "{}",
        table
            .display()
            .expect("rendering the artifacts table should succeed")
    );
    fabro_util::printerr!(
        printer,
        "{}",
        styles.dim.apply_to(format!(
            "Copy with: fabro artifact cp {run_id}:<path> <dest> --stage <node@visit> --retry <retry>"
        ))
    );
    Ok(())
}
