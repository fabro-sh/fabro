#![expect(
    clippy::disallowed_types,
    reason = "sync CLI `run wait` command: blocking std::io::Write is the intended output mechanism"
)]
#![expect(
    clippy::disallowed_methods,
    reason = "sync CLI `run wait` command: prints status to std::io::stdout directly"
)]

use std::io::Write;

use anyhow::{Result, bail};
use fabro_types::RunId;
use fabro_types::settings::CliSettings;
use fabro_types::settings::cli::{CliLayer, OutputFormat};
use fabro_util::printer::Printer;
use fabro_util::terminal::Styles;
use fabro_workflow::records::Conclusion;
use fabro_workflow::run_status::RunStatus;
use tokio::time;
use tracing::info;

use crate::args::WaitArgs;
use crate::command_context::CommandContext;
use crate::server_runs::ServerSummaryLookup;
use crate::shared::{format_duration_ms, format_usd_micros};

pub(crate) async fn run(
    args: &WaitArgs,
    styles: &Styles,
    cli: &CliSettings,
    cli_layer: &CliLayer,
    printer: Printer,
) -> Result<()> {
    let ctx = CommandContext::for_target(&args.server, printer, cli.clone(), cli_layer)?;
    let lookup = ServerSummaryLookup::from_client(ctx.server().await?).await?;
    let run_info = lookup.resolve(&args.run)?;
    let client = lookup.client();

    let run_id = run_info.run_id();
    info!(run_id = %run_id, "Waiting for run to complete");

    let deadline = args
        .timeout
        .map(|secs| std::time::Instant::now() + std::time::Duration::from_secs(secs));
    let interval = std::time::Duration::from_millis(args.interval);
    let final_status = loop {
        let status = client.retrieve_run(&run_id).await?.status;

        if status.is_terminal() {
            break status;
        }

        if let Some(dl) = deadline {
            let now = std::time::Instant::now();
            if now >= dl {
                bail!(
                    "Timed out after {}s waiting for run '{}'",
                    args.timeout.unwrap(),
                    run_id
                );
            }
            time::sleep(interval.min(dl - now)).await;
        } else {
            time::sleep(interval).await;
        }
    };

    let conclusion = client.get_run_state(&run_id).await?.conclusion;

    if cli.output.format == OutputFormat::Json {
        let json_value = build_json_output(final_status, &run_id, conclusion.as_ref());
        let mut out = std::io::stdout().lock();
        serde_json::to_writer_pretty(&mut out, &json_value)?;
        writeln!(out)?;
    } else {
        print_human_output(final_status, &run_id, conclusion.as_ref(), styles, printer);
    }

    if final_status == RunStatus::Succeeded {
        Ok(())
    } else {
        std::process::exit(1);
    }
}

fn build_json_output(
    status: RunStatus,
    run_id: &RunId,
    conclusion: Option<&Conclusion>,
) -> serde_json::Value {
    let mut value = serde_json::json!({
        "run_id": run_id,
        "status": status.to_string(),
    });
    if let Some(c) = conclusion {
        value["duration_ms"] = c.duration_ms.into();
        if let Some(total_usd_micros) = c
            .billing
            .as_ref()
            .and_then(|billing| billing.total_usd_micros)
        {
            value["total_usd_micros"] = total_usd_micros.into();
        }
    }
    value
}

fn print_human_output(
    status: RunStatus,
    run_id: &RunId,
    conclusion: Option<&Conclusion>,
    styles: &Styles,
    printer: Printer,
) {
    let (style, label) = match status {
        RunStatus::Succeeded => (&styles.bold_green, "Succeeded"),
        RunStatus::Failed => (&styles.bold_red, "Failed"),
        RunStatus::Dead => (&styles.bold_red, "Dead"),
        // Poll loop only breaks on is_terminal() which is Succeeded | Failed | Dead
        _ => unreachable!(),
    };
    let status_display = style.apply_to(label);

    let details = match conclusion {
        Some(c) => {
            let duration = format_duration_ms(c.duration_ms);
            let cost = c
                .billing
                .as_ref()
                .and_then(|billing| billing.total_usd_micros)
                .map(|value| format!("  {}", format_usd_micros(value)))
                .unwrap_or_default();
            format!("  {duration}{cost}")
        }
        None => String::new(),
    };

    fabro_util::printerr!(
        printer,
        "{} {}{details}",
        status_display,
        styles.dim.apply_to(run_id),
    );
}

#[cfg(test)]
mod tests {
    use fabro_types::{BilledTokenCounts, fixtures};
    use fabro_workflow::outcome::StageStatus;
    use fabro_workflow::records::Conclusion;
    use fabro_workflow::run_status::RunStatusRecord;

    use super::*;

    fn no_color_styles() -> Styles {
        Styles::new(false)
    }

    #[test]
    fn json_output_succeeded_with_conclusion() {
        let run_id = fixtures::RUN_1;
        let conclusion = Conclusion {
            timestamp:            chrono::Utc::now(),
            status:               StageStatus::Success,
            duration_ms:          12345,
            failure_reason:       None,
            final_git_commit_sha: None,
            stages:               vec![],
            billing:              Some(BilledTokenCounts {
                input_tokens:       0,
                output_tokens:      0,
                total_tokens:       0,
                reasoning_tokens:   0,
                cache_read_tokens:  0,
                cache_write_tokens: 0,
                total_usd_micros:   Some(420_000),
            }),
            total_retries:        0,
        };
        let json = build_json_output(RunStatus::Succeeded, &run_id, Some(&conclusion));
        assert_eq!(json["run_id"], run_id.to_string());
        assert_eq!(json["status"], "succeeded");
        assert_eq!(json["duration_ms"], 12345);
        assert_eq!(json["total_usd_micros"], 420_000);
    }

    #[test]
    fn json_output_failed_without_conclusion() {
        let run_id = fixtures::RUN_2;
        let json = build_json_output(RunStatus::Failed, &run_id, None);
        assert_eq!(json["run_id"], run_id.to_string());
        assert_eq!(json["status"], "failed");
        assert!(json.get("duration_ms").is_none());
        assert!(json.get("total_usd_micros").is_none());
    }

    #[test]
    fn json_output_dead_status() {
        let json = build_json_output(RunStatus::Dead, &fixtures::RUN_3, None);
        assert_eq!(json["status"], "dead");
    }

    #[test]
    fn json_output_no_cost_when_none() {
        let run_id = fixtures::RUN_4;
        let conclusion = Conclusion {
            timestamp:            chrono::Utc::now(),
            status:               StageStatus::Fail,
            duration_ms:          500,
            failure_reason:       Some("error".into()),
            final_git_commit_sha: None,
            stages:               vec![],
            billing:              None,
            total_retries:        0,
        };
        let json = build_json_output(RunStatus::Failed, &run_id, Some(&conclusion));
        assert!(json.get("total_usd_micros").is_none());
        assert_eq!(json["duration_ms"], 500);
    }

    #[test]
    fn human_output_succeeded() {
        let styles = no_color_styles();
        let run_id = fixtures::RUN_5;
        let conclusion = Conclusion {
            timestamp:            chrono::Utc::now(),
            status:               StageStatus::Success,
            duration_ms:          8000,
            failure_reason:       None,
            final_git_commit_sha: None,
            stages:               vec![],
            billing:              Some(BilledTokenCounts {
                input_tokens:       0,
                output_tokens:      0,
                total_tokens:       0,
                reasoning_tokens:   0,
                cache_read_tokens:  0,
                cache_write_tokens: 0,
                total_usd_micros:   Some(150_000),
            }),
            total_retries:        0,
        };
        // Just verify no panic; actual stderr output is hard to capture
        print_human_output(
            RunStatus::Succeeded,
            &run_id,
            Some(&conclusion),
            &styles,
            Printer::Default,
        );
    }

    #[test]
    fn human_output_failed_no_conclusion() {
        let styles = no_color_styles();
        print_human_output(
            RunStatus::Failed,
            &fixtures::RUN_6,
            None,
            &styles,
            Printer::Default,
        );
    }

    #[test]
    fn poll_terminal_immediately() {
        let dir = tempfile::tempdir().unwrap();
        let status_path = dir.path().join("status.json");
        let record = RunStatusRecord::new(RunStatus::Succeeded, None);
        std::fs::write(&status_path, serde_json::to_string_pretty(&record).unwrap()).unwrap();

        // Simulate what the poll loop does
        let status = serde_json::from_str::<RunStatusRecord>(
            &std::fs::read_to_string(&status_path).unwrap(),
        )
        .unwrap()
        .status;
        assert!(status.is_terminal());
        assert_eq!(status, RunStatus::Succeeded);
    }
}
