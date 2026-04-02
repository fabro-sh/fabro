use std::io::Write;

use anyhow::{Result, bail};
use fabro_types::RunId;
use fabro_util::terminal::Styles;
use fabro_workflow::records::Conclusion;
use fabro_workflow::run_lookup::{resolve_run_combined, runs_base};
use fabro_workflow::run_status::RunStatus;
use tracing::info;

use crate::args::{GlobalArgs, WaitArgs};
use crate::shared::format_duration_ms;
use crate::store;
use crate::user_config::load_user_settings_with_globals;

#[cfg(test)]
const WAIT_STARTUP_GRACE: std::time::Duration = std::time::Duration::from_millis(500);
#[cfg(not(test))]
const WAIT_STARTUP_GRACE: std::time::Duration = std::time::Duration::from_secs(3);

pub(crate) async fn run(args: &WaitArgs, styles: &Styles, globals: &GlobalArgs) -> Result<()> {
    let cli_settings = load_user_settings_with_globals(globals)?;
    let base = runs_base(&cli_settings.storage_dir());
    let store = store::build_store(&cli_settings.storage_dir())?;
    let run_info = resolve_run_combined(store.as_ref(), &base, &args.run).await?;

    info!(run_id = %run_info.run_id, "Waiting for run to complete");

    let deadline = args
        .timeout
        .map(|secs| std::time::Instant::now() + std::time::Duration::from_secs(secs));
    let interval = std::time::Duration::from_millis(args.interval);
    let started_waiting_at = std::time::Instant::now();

    let final_status = loop {
        let run_store = store::open_run_reader(&cli_settings.storage_dir(), &run_info.run_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Run {} not found in store", run_info.run_id))?;
        let status = run_store.get_status().await?.map(|record| record.status);
        let status = status.unwrap_or_else(|| {
            if started_waiting_at.elapsed() < WAIT_STARTUP_GRACE {
                RunStatus::Submitted
            } else {
                RunStatus::Dead
            }
        });

        if status.is_terminal() {
            break status;
        }

        if let Some(dl) = deadline {
            let now = std::time::Instant::now();
            if now >= dl {
                bail!(
                    "Timed out after {}s waiting for run '{}'",
                    args.timeout.unwrap(),
                    run_info.run_id
                );
            }
            std::thread::sleep(interval.min(dl - now));
        } else {
            std::thread::sleep(interval);
        }
    };

    let run_store = store::open_run_reader(&cli_settings.storage_dir(), &run_info.run_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Run {} not found in store", run_info.run_id))?;
    let conclusion = run_store.get_conclusion().await?;

    if globals.json {
        let json_value = build_json_output(final_status, &run_info.run_id, conclusion.as_ref());
        let mut out = std::io::stdout().lock();
        serde_json::to_writer_pretty(&mut out, &json_value)?;
        writeln!(out)?;
    } else {
        print_human_output(final_status, &run_info.run_id, conclusion.as_ref(), styles);
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
        if let Some(cost) = c.total_cost {
            value["total_cost"] = cost.into();
        }
    }
    value
}

fn print_human_output(
    status: RunStatus,
    run_id: &RunId,
    conclusion: Option<&Conclusion>,
    styles: &Styles,
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
                .total_cost
                .map(|v| format!("  ${v:.2}"))
                .unwrap_or_default();
            format!("  {duration}{cost}")
        }
        None => String::new(),
    };

    eprintln!(
        "{} {}{details}",
        status_display,
        styles.dim.apply_to(run_id),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use fabro_types::fixtures;
    use fabro_workflow::outcome::StageStatus;
    use fabro_workflow::records::Conclusion;
    use fabro_workflow::run_status::{RunStatusRecord, RunStatusRecordExt};

    fn no_color_styles() -> Styles {
        Styles::new(false)
    }

    #[test]
    fn json_output_succeeded_with_conclusion() {
        let run_id = fixtures::RUN_1;
        let conclusion = Conclusion {
            timestamp: chrono::Utc::now(),
            status: StageStatus::Success,
            duration_ms: 12345,
            failure_reason: None,
            final_git_commit_sha: None,
            stages: vec![],
            total_cost: Some(0.42),
            total_retries: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_read_tokens: 0,
            total_cache_write_tokens: 0,
            total_reasoning_tokens: 0,
            has_pricing: false,
        };
        let json = build_json_output(RunStatus::Succeeded, &run_id, Some(&conclusion));
        assert_eq!(json["run_id"], run_id.to_string());
        assert_eq!(json["status"], "succeeded");
        assert_eq!(json["duration_ms"], 12345);
        assert!((json["total_cost"].as_f64().unwrap() - 0.42).abs() < f64::EPSILON);
    }

    #[test]
    fn json_output_failed_without_conclusion() {
        let run_id = fixtures::RUN_2;
        let json = build_json_output(RunStatus::Failed, &run_id, None);
        assert_eq!(json["run_id"], run_id.to_string());
        assert_eq!(json["status"], "failed");
        assert!(json.get("duration_ms").is_none());
        assert!(json.get("total_cost").is_none());
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
            timestamp: chrono::Utc::now(),
            status: StageStatus::Fail,
            duration_ms: 500,
            failure_reason: Some("error".into()),
            final_git_commit_sha: None,
            stages: vec![],
            total_cost: None,
            total_retries: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_read_tokens: 0,
            total_cache_write_tokens: 0,
            total_reasoning_tokens: 0,
            has_pricing: false,
        };
        let json = build_json_output(RunStatus::Failed, &run_id, Some(&conclusion));
        assert!(json.get("total_cost").is_none());
        assert_eq!(json["duration_ms"], 500);
    }

    #[test]
    fn human_output_succeeded() {
        let styles = no_color_styles();
        let run_id = fixtures::RUN_5;
        let conclusion = Conclusion {
            timestamp: chrono::Utc::now(),
            status: StageStatus::Success,
            duration_ms: 8000,
            failure_reason: None,
            final_git_commit_sha: None,
            stages: vec![],
            total_cost: Some(0.15),
            total_retries: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_read_tokens: 0,
            total_cache_write_tokens: 0,
            total_reasoning_tokens: 0,
            has_pricing: false,
        };
        // Just verify no panic; actual stderr output is hard to capture
        print_human_output(RunStatus::Succeeded, &run_id, Some(&conclusion), &styles);
    }

    #[test]
    fn human_output_failed_no_conclusion() {
        let styles = no_color_styles();
        print_human_output(RunStatus::Failed, &fixtures::RUN_6, None, &styles);
    }

    #[test]
    fn poll_terminal_immediately() {
        let dir = tempfile::tempdir().unwrap();
        let status_path = dir.path().join("status.json");
        let record = RunStatusRecord::new(RunStatus::Succeeded, None);
        record.save(&status_path).unwrap();

        // Simulate what the poll loop does
        let status = RunStatusRecord::load(&status_path).unwrap().status;
        assert!(status.is_terminal());
        assert_eq!(status, RunStatus::Succeeded);
    }

    #[test]
    fn missing_status_treated_as_dead() {
        let status = match RunStatusRecord::load(std::path::Path::new("/nonexistent/status.json")) {
            Ok(record) => record.status,
            Err(_) => RunStatus::Dead,
        };
        assert_eq!(status, RunStatus::Dead);
    }
}
