use std::io::Write;

use anyhow::{bail, Result};
use fabro_util::terminal::Styles;
use fabro_workflows::run_status::{RunStatus, RunStatusRecord};
use tracing::info;

use crate::args::WaitArgs;
use crate::shared::format_duration_ms;

pub fn run(args: WaitArgs, styles: &Styles) -> Result<()> {
    let cli_config = crate::cli_config::load_cli_config(None)?;
    let base = fabro_workflows::run_lookup::runs_base(&cli_config.storage_dir());
    let run_info = fabro_workflows::run_lookup::resolve_run(&base, &args.run)?;

    info!(run_id = %run_info.run_id, "Waiting for run to complete");

    let status_path = run_info.path.join("status.json");
    let deadline = args
        .timeout
        .map(|secs| std::time::Instant::now() + std::time::Duration::from_secs(secs));
    let interval = std::time::Duration::from_millis(args.interval);

    let final_status = loop {
        let status = match RunStatusRecord::load(&status_path) {
            Ok(record) => record.status,
            Err(_) => RunStatus::Dead,
        };

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

    let conclusion_path = run_info.path.join("conclusion.json");
    let conclusion = fabro_workflows::records::Conclusion::load(&conclusion_path).ok();

    if args.json {
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
    run_id: &str,
    conclusion: Option<&fabro_workflows::records::Conclusion>,
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
    run_id: &str,
    conclusion: Option<&fabro_workflows::records::Conclusion>,
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
    use fabro_workflows::outcome::StageStatus;
    use fabro_workflows::records::Conclusion;

    fn no_color_styles() -> Styles {
        Styles::new(false)
    }

    #[test]
    fn json_output_succeeded_with_conclusion() {
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
        let json = build_json_output(RunStatus::Succeeded, "ABC123", Some(&conclusion));
        assert_eq!(json["run_id"], "ABC123");
        assert_eq!(json["status"], "succeeded");
        assert_eq!(json["duration_ms"], 12345);
        assert!((json["total_cost"].as_f64().unwrap() - 0.42).abs() < f64::EPSILON);
    }

    #[test]
    fn json_output_failed_without_conclusion() {
        let json = build_json_output(RunStatus::Failed, "DEF456", None);
        assert_eq!(json["run_id"], "DEF456");
        assert_eq!(json["status"], "failed");
        assert!(json.get("duration_ms").is_none());
        assert!(json.get("total_cost").is_none());
    }

    #[test]
    fn json_output_dead_status() {
        let json = build_json_output(RunStatus::Dead, "GHI789", None);
        assert_eq!(json["status"], "dead");
    }

    #[test]
    fn json_output_no_cost_when_none() {
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
        let json = build_json_output(RunStatus::Failed, "JKL012", Some(&conclusion));
        assert!(json.get("total_cost").is_none());
        assert_eq!(json["duration_ms"], 500);
    }

    #[test]
    fn human_output_succeeded() {
        let styles = no_color_styles();
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
        print_human_output(RunStatus::Succeeded, "ABC123", Some(&conclusion), &styles);
    }

    #[test]
    fn human_output_failed_no_conclusion() {
        let styles = no_color_styles();
        print_human_output(RunStatus::Failed, "DEF456", None, &styles);
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
