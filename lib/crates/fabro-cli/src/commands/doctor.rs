use std::path::PathBuf;

use anyhow::Result;
use fabro_api::types as api_types;
use fabro_config::user::active_settings_path;
use fabro_types::settings::cli::OutputVerbosity;
pub(crate) use fabro_util::check_report::{
    CheckDetail, CheckReport, CheckResult, CheckSection, CheckStatus,
};
use fabro_util::path::contract_tilde;
use fabro_util::printer::Printer;
use fabro_util::terminal::Styles;
use fabro_util::version::FABRO_VERSION;

use crate::args::DoctorArgs;
use crate::command_context::CommandContext;
use crate::shared::print_json_pretty;

pub(crate) fn check_config(settings_path: Option<PathBuf>) -> CheckResult {
    match settings_path {
        Some(path) => {
            let display = contract_tilde(&path);
            CheckResult {
                name:        "Configuration".to_string(),
                status:      CheckStatus::Pass,
                summary:     display.display().to_string(),
                details:     vec![CheckDetail::new(format!(
                    "Loaded from {}",
                    display.display()
                ))],
                remediation: None,
            }
        }
        None => CheckResult {
            name:        "Configuration".to_string(),
            status:      CheckStatus::Warning,
            summary:     "no settings config file found".to_string(),
            details:     vec![CheckDetail::new(
                "Create ~/.fabro/settings.toml to configure Fabro".to_string(),
            )],
            remediation: Some("Create ~/.fabro/settings.toml".to_string()),
        },
    }
}

fn check_version_parity(server_version: &str) -> CheckResult {
    let cli_version = FABRO_VERSION;
    if server_version == cli_version {
        CheckResult {
            name:        "Version parity".to_string(),
            status:      CheckStatus::Pass,
            summary:     cli_version.to_string(),
            details:     vec![CheckDetail::new(format!(
                "CLI and server are both {cli_version}"
            ))],
            remediation: None,
        }
    } else {
        CheckResult {
            name:        "Version parity".to_string(),
            status:      CheckStatus::Warning,
            summary:     format!("CLI {cli_version}, server {server_version}"),
            details:     vec![CheckDetail::new(format!(
                "CLI version {cli_version} does not match server version {server_version}"
            ))],
            remediation: Some(
                "Upgrade or restart components so the CLI and server run the same version."
                    .to_string(),
            ),
        }
    }
}

fn skipped_version_parity(reason: &str) -> CheckResult {
    CheckResult {
        name:        "Version parity".to_string(),
        status:      CheckStatus::Warning,
        summary:     "skipped".to_string(),
        details:     vec![CheckDetail::new(format!(
            "Could not retrieve server version: {reason}"
        ))],
        remediation: None,
    }
}

fn convert_diagnostics_status(status: api_types::DiagnosticsCheckStatus) -> CheckStatus {
    match status {
        api_types::DiagnosticsCheckStatus::Pass => CheckStatus::Pass,
        api_types::DiagnosticsCheckStatus::Warning => CheckStatus::Warning,
        api_types::DiagnosticsCheckStatus::Error => CheckStatus::Error,
    }
}

fn convert_diagnostics_sections(sections: Vec<api_types::DiagnosticsSection>) -> Vec<CheckSection> {
    sections
        .into_iter()
        .map(|section| CheckSection {
            title:  section.title,
            checks: section
                .checks
                .into_iter()
                .map(|check| CheckResult {
                    name:        check.name,
                    status:      convert_diagnostics_status(check.status),
                    summary:     check.summary,
                    details:     check
                        .details
                        .into_iter()
                        .map(|detail| CheckDetail {
                            text: detail.text,
                            warn: detail.warn,
                        })
                        .collect(),
                    remediation: check.remediation,
                })
                .collect(),
        })
        .collect()
}

fn render_report_text(
    report: &CheckReport,
    styles: &Styles,
    verbose: bool,
    max_width: Option<u16>,
) -> String {
    report.render(styles, verbose, None, max_width)
}

fn render_report(report: &CheckReport, styles: &Styles, verbose: bool, printer: Printer) {
    let term_width = console::Term::stderr().size().1;
    {
        use std::fmt::Write as _;
        let _ = write!(
            printer.stdout(),
            "{}",
            render_report_text(report, styles, verbose, Some(term_width))
        );
    }
}

pub(crate) async fn run_doctor(
    args: &DoctorArgs,
    base_ctx: &CommandContext,
) -> Result<i32, anyhow::Error> {
    let verbose =
        args.verbose || base_ctx.user_settings().cli.output.verbosity == OutputVerbosity::Verbose;
    let printer = base_ctx.printer();
    let styles = Styles::detect_stdout();
    let json = base_ctx.json_output();
    let spinner = if json {
        None
    } else {
        let spinner = indicatif::ProgressBar::new_spinner();
        spinner.set_style(
            indicatif::ProgressStyle::with_template("{spinner:.cyan} {msg}")
                .expect("valid template")
                .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", ""]),
        );
        spinner.set_message("Running checks...");
        spinner.enable_steady_tick(std::time::Duration::from_millis(80));
        Some(spinner)
    };

    let settings_config_path = active_settings_path(None);

    let local_checks = vec![check_config(
        settings_config_path
            .exists()
            .then_some(settings_config_path),
    )];

    let mut report = CheckReport {
        title:    "Fabro Doctor".to_string(),
        sections: vec![CheckSection {
            title:  "Local".to_string(),
            checks: local_checks,
        }],
    };

    let ctx = match base_ctx.with_target(&args.target) {
        Ok(ctx) => ctx,
        Err(err) => {
            report.sections.push(CheckSection {
                title:  "Server".to_string(),
                checks: vec![CheckResult {
                    name:        "Fabro server".to_string(),
                    status:      CheckStatus::Error,
                    summary:     "settings resolution failed".to_string(),
                    details:     vec![CheckDetail::new(err.to_string())],
                    remediation: Some(
                        "Fix the local CLI settings or provide `--server`, then run doctor again."
                            .to_string(),
                    ),
                }],
            });

            if let Some(spinner) = spinner {
                spinner.finish_and_clear();
            }

            if json {
                print_json_pretty(&report)?;
            } else {
                render_report(&report, &styles, verbose, printer);
            }
            return Ok(1);
        }
    };

    let server = match ctx.server().await {
        Ok(server) => server,
        Err(err) => {
            report.sections.push(CheckSection {
                title:  "Server".to_string(),
                checks: vec![CheckResult {
                    name:        "Fabro server".to_string(),
                    status:      CheckStatus::Error,
                    summary:     "unreachable".to_string(),
                    details:     vec![CheckDetail::new(err.to_string())],
                    remediation: Some(
                        "Start or connect to the server with `--server` and run doctor again."
                            .to_string(),
                    ),
                }],
            });

            if let Some(spinner) = spinner {
                spinner.finish_and_clear();
            }

            if json {
                print_json_pretty(&report)?;
            } else {
                render_report(&report, &styles, verbose, printer);
            }
            return Ok(1);
        }
    };

    if let Err(err) = server.get_health().await {
        report.sections.push(CheckSection {
            title:  "Server".to_string(),
            checks: vec![CheckResult {
                name:        "Fabro server".to_string(),
                status:      CheckStatus::Error,
                summary:     "health check failed".to_string(),
                details:     vec![CheckDetail::new(err.to_string())],
                remediation: Some(
                    "Check that the server is reachable and responding to /health.".to_string(),
                ),
            }],
        });

        if let Some(spinner) = spinner {
            spinner.finish_and_clear();
        }

        if json {
            print_json_pretty(&report)?;
        } else {
            render_report(&report, &styles, verbose, printer);
        }
        return Ok(1);
    }

    report.sections.push(CheckSection {
        title:  "Server".to_string(),
        checks: vec![CheckResult {
            name:        "Location".to_string(),
            status:      CheckStatus::Pass,
            summary:     server.base_url().clone(),
            details:     vec![],
            remediation: None,
        }],
    });

    match server.run_diagnostics().await {
        Ok(diagnostics) => {
            report.sections[0]
                .checks
                .push(check_version_parity(&diagnostics.version));
            report
                .sections
                .extend(convert_diagnostics_sections(diagnostics.sections));
        }
        Err(err) => {
            report.sections[0]
                .checks
                .push(skipped_version_parity(&err.to_string()));
            report.sections.push(CheckSection {
                title:  "Server".to_string(),
                checks: vec![CheckResult {
                    name:        "Diagnostics".to_string(),
                    status:      CheckStatus::Error,
                    summary:     "probe failed".to_string(),
                    details:     vec![CheckDetail::new(err.to_string())],
                    remediation: Some(
                        "Fix the server diagnostics failure and run `fabro doctor` again."
                            .to_string(),
                    ),
                }],
            });
        }
    }

    if let Some(spinner) = spinner {
        spinner.finish_and_clear();
    }

    if json {
        print_json_pretty(&report)?;
    } else {
        render_report(&report, &styles, verbose, printer);
    }

    Ok(i32::from(report.has_errors()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_config_pass_with_path() {
        let result = check_config(Some(PathBuf::from("/home/user/.fabro/settings.toml")));
        assert_eq!(result.status, CheckStatus::Pass);
        assert!(result.summary.contains(".fabro/settings.toml"));
    }

    #[test]
    fn check_config_warning_without_path() {
        let result = check_config(None);
        assert_eq!(result.status, CheckStatus::Warning);
        assert!(result.remediation.is_some());
    }

    #[test]
    fn check_version_parity_warns_on_mismatch() {
        let result = check_version_parity("0.0.0-test");
        assert_eq!(result.status, CheckStatus::Warning);
    }

    #[test]
    fn version_parity_skipped_when_diagnostics_unavailable() {
        let result = skipped_version_parity("boom");
        assert_eq!(result.name, "Version parity");
        assert_eq!(result.status, CheckStatus::Warning);
        assert_eq!(result.summary, "skipped");
        assert_eq!(
            result.details[0].text,
            "Could not retrieve server version: boom"
        );
    }

    #[test]
    fn render_report_text_without_color_has_no_ansi() {
        let report = CheckReport {
            title:    "Fabro Doctor".to_string(),
            sections: vec![CheckSection {
                title:  "Local".to_string(),
                checks: vec![CheckResult {
                    name:        "Configuration".to_string(),
                    status:      CheckStatus::Pass,
                    summary:     "loaded".to_string(),
                    details:     vec![CheckDetail::new(
                        "Loaded from ~/.fabro/settings.toml".into(),
                    )],
                    remediation: None,
                }],
            }],
        };

        let rendered = render_report_text(&report, &Styles::new(false), false, Some(80));
        assert!(
            !rendered.contains("\x1b["),
            "rendered output should be plain text"
        );
        assert!(rendered.contains("Fabro Doctor"));
        assert!(rendered.contains("[✓] Configuration (loaded)"));
    }
}
