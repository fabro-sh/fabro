use std::path::{Path, PathBuf};

use anyhow::Result;
use fabro_api::types as api_types;
use fabro_config::legacy_env;
use fabro_config::user::{
    active_settings_path, legacy_old_user_config_path, legacy_server_config_path,
    legacy_user_config_path,
};
use fabro_types::settings::CliSettings;
use fabro_types::settings::cli::{CliLayer, OutputFormat};
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
use crate::user_config;

pub(crate) fn check_config(
    settings_path: Option<PathBuf>,
    legacy_paths: &[PathBuf],
) -> CheckResult {
    match (settings_path, legacy_paths.is_empty()) {
        (Some(path), true) => {
            let display = contract_tilde(&path);
            CheckResult {
                name: "Configuration".to_string(),
                status: CheckStatus::Pass,
                summary: display.display().to_string(),
                details: vec![CheckDetail::new(format!(
                    "Loaded from {}",
                    display.display()
                ))],
                remediation: None,
            }
        }
        (Some(path), false) => {
            let display = contract_tilde(&path);
            CheckResult {
                name: "Configuration".to_string(),
                status: CheckStatus::Warning,
                summary: display.display().to_string(),
                details: std::iter::once(CheckDetail::new(format!(
                    "Loaded from {}",
                    display.display()
                )))
                .chain(legacy_paths.iter().map(|legacy| {
                    let legacy_display = contract_tilde(legacy);
                    CheckDetail::new(format!(
                        "Ignoring legacy config file {}",
                        legacy_display.display()
                    ))
                }))
                .collect(),
                remediation: Some("Delete or rename legacy config files".to_string()),
            }
        }
        (None, false) => CheckResult {
            name: "Configuration".to_string(),
            status: CheckStatus::Warning,
            summary: "legacy config files ignored".to_string(),
            details: legacy_paths
                .iter()
                .map(|legacy| {
                    CheckDetail::new(format!("Found legacy config file {}", legacy.display()))
                })
                .chain(std::iter::once(CheckDetail::new(
                    "Rename one to ~/.fabro/settings.toml or create a new settings.toml"
                        .to_string(),
                )))
                .collect(),
            remediation: Some("Create ~/.fabro/settings.toml".to_string()),
        },
        (None, true) => CheckResult {
            name: "Configuration".to_string(),
            status: CheckStatus::Warning,
            summary: "no settings config file found".to_string(),
            details: vec![CheckDetail::new(
                "Create ~/.fabro/settings.toml to configure Fabro".to_string(),
            )],
            remediation: Some("Create ~/.fabro/settings.toml".to_string()),
        },
    }
}

fn check_legacy_env(path: Option<PathBuf>) -> Option<CheckResult> {
    path.map(|path| {
        let display = contract_tilde(&path);
        CheckResult {
            name: "Legacy .env".to_string(),
            status: CheckStatus::Warning,
            summary: "legacy secrets file detected".to_string(),
            details: vec![CheckDetail::new(format!(
                "{} is no longer read by fabro",
                display.display()
            ))],
            remediation: Some("Re-enter credentials with `fabro provider login`.".to_string()),
        }
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StorageDirStatus {
    path: PathBuf,
    exists: bool,
    readable: bool,
    writable: bool,
}

fn probe_storage_dir(path: &Path) -> StorageDirStatus {
    let exists = path.is_dir();
    let readable = exists && std::fs::read_dir(path).is_ok();
    let writable = exists && tempfile::tempfile_in(path).is_ok();

    StorageDirStatus {
        path: path.to_path_buf(),
        exists,
        readable,
        writable,
    }
}

fn check_storage_dir(status: &StorageDirStatus) -> CheckResult {
    let display = contract_tilde(&status.path);
    let display = display.display();
    let details = vec![
        CheckDetail::new(format!(
            "Exists: {}",
            if status.exists { "yes" } else { "no" }
        )),
        CheckDetail::new(format!(
            "Readable: {}",
            if status.readable { "yes" } else { "no" }
        )),
        CheckDetail::new(format!(
            "Writable: {}",
            if status.writable { "yes" } else { "no" }
        )),
    ];
    let is_healthy = status.exists && status.readable && status.writable;

    CheckResult {
        name: "Storage directory".to_string(),
        status: if is_healthy {
            CheckStatus::Pass
        } else {
            CheckStatus::Error
        },
        summary: display.to_string(),
        details,
        remediation: if is_healthy {
            None
        } else if !status.exists {
            Some(format!("Create the directory: mkdir -p {display}"))
        } else {
            Some(format!("Fix permissions on {display}"))
        },
    }
}

fn check_version_parity(server_version: &str) -> CheckResult {
    let cli_version = FABRO_VERSION;
    if server_version == cli_version {
        CheckResult {
            name: "Version parity".to_string(),
            status: CheckStatus::Pass,
            summary: cli_version.to_string(),
            details: vec![CheckDetail::new(format!(
                "CLI and server are both {cli_version}"
            ))],
            remediation: None,
        }
    } else {
        CheckResult {
            name: "Version parity".to_string(),
            status: CheckStatus::Warning,
            summary: format!("CLI {cli_version}, server {server_version}"),
            details: vec![CheckDetail::new(format!(
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
        name: "Version parity".to_string(),
        status: CheckStatus::Warning,
        summary: "skipped".to_string(),
        details: vec![CheckDetail::new(format!(
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
            title: section.title,
            checks: section
                .checks
                .into_iter()
                .map(|check| CheckResult {
                    name: check.name,
                    status: convert_diagnostics_status(check.status),
                    summary: check.summary,
                    details: check
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
    verbose: bool,
    cli: &CliSettings,
    cli_layer: &CliLayer,
    printer: Printer,
) -> Result<i32, anyhow::Error> {
    let styles = Styles::detect_stdout();
    let json = cli.output.format == OutputFormat::Json;
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
    let legacy_config_paths = [
        legacy_user_config_path(),
        legacy_old_user_config_path(),
        legacy_server_config_path(),
    ]
    .into_iter()
    .flatten()
    .filter(|path| path.exists())
    .collect::<Vec<_>>();
    let legacy_env_path = {
        let p = legacy_env::legacy_env_file_path();
        p.exists().then_some(p)
    };

    let settings = user_config::load_settings().unwrap_or_default();
    let storage_dir_path = user_config::storage_dir(&settings)
        .unwrap_or_else(|_| fabro_util::Home::from_env().storage_dir());
    let storage_dir = probe_storage_dir(&storage_dir_path);

    let mut local_checks = vec![
        check_config(
            settings_config_path
                .exists()
                .then_some(settings_config_path),
            &legacy_config_paths,
        ),
        check_storage_dir(&storage_dir),
    ];
    if let Some(legacy_env_check) = check_legacy_env(legacy_env_path) {
        local_checks.push(legacy_env_check);
    }

    let mut report = CheckReport {
        title: "Fabro Doctor".to_string(),
        sections: vec![CheckSection {
            title: "Local".to_string(),
            checks: local_checks,
        }],
    };

    let ctx = match CommandContext::for_target(&args.target, printer, cli.clone(), cli_layer) {
        Ok(ctx) => ctx,
        Err(err) => {
            report.sections.push(CheckSection {
                title: "Server".to_string(),
                checks: vec![CheckResult {
                    name: "Fabro server".to_string(),
                    status: CheckStatus::Error,
                    summary: "settings resolution failed".to_string(),
                    details: vec![CheckDetail::new(err.to_string())],
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
                title: "Server".to_string(),
                checks: vec![CheckResult {
                    name: "Fabro server".to_string(),
                    status: CheckStatus::Error,
                    summary: "unreachable".to_string(),
                    details: vec![CheckDetail::new(err.to_string())],
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

    if let Err(err) = server.api().get_health().send().await {
        report.sections.push(CheckSection {
            title: "Server".to_string(),
            checks: vec![CheckResult {
                name: "Fabro server".to_string(),
                status: CheckStatus::Error,
                summary: "health check failed".to_string(),
                details: vec![CheckDetail::new(err.to_string())],
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
        title: "Server".to_string(),
        checks: vec![CheckResult {
            name: "Location".to_string(),
            status: CheckStatus::Pass,
            summary: server.base_url().to_string(),
            details: vec![],
            remediation: None,
        }],
    });

    match server.api().run_diagnostics().send().await {
        Ok(response) => {
            let diagnostics = response.into_inner();
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
                title: "Server".to_string(),
                checks: vec![CheckResult {
                    name: "Diagnostics".to_string(),
                    status: CheckStatus::Error,
                    summary: "probe failed".to_string(),
                    details: vec![CheckDetail::new(err.to_string())],
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
        let result = check_config(Some(PathBuf::from("/home/user/.fabro/settings.toml")), &[]);
        assert_eq!(result.status, CheckStatus::Pass);
        assert!(result.summary.contains(".fabro/settings.toml"));
    }

    #[test]
    fn check_config_warning_without_path() {
        let result = check_config(None, &[]);
        assert_eq!(result.status, CheckStatus::Warning);
        assert!(result.remediation.is_some());
    }

    #[test]
    fn check_config_warning_for_legacy_only_path() {
        let result = check_config(None, &[PathBuf::from("/home/user/.fabro/cli.toml")]);
        assert_eq!(result.status, CheckStatus::Warning);
        assert!(result.summary.contains("legacy"));
    }

    #[test]
    fn check_legacy_env_warning_when_present() {
        let result = check_legacy_env(Some(PathBuf::from("/home/user/.fabro/.env")));
        assert_eq!(
            result.as_ref().map(|check| check.status),
            Some(CheckStatus::Warning)
        );
        assert!(
            result
                .as_ref()
                .is_some_and(|check| check.summary.contains("legacy secrets file"))
        );
    }

    #[test]
    fn check_legacy_env_is_omitted_when_absent() {
        let result = check_legacy_env(None);
        assert!(result.is_none());
    }

    // -- check_storage_dir --

    #[test]
    fn probe_storage_dir_existing_dir_is_readable_and_writable() {
        let dir = tempfile::tempdir().unwrap();
        let status = probe_storage_dir(dir.path());

        assert_eq!(
            status,
            StorageDirStatus {
                path: dir.path().to_path_buf(),
                exists: true,
                readable: true,
                writable: true,
            }
        );
    }

    #[test]
    fn probe_storage_dir_missing_dir_is_not_usable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing");
        let status = probe_storage_dir(&path);

        assert_eq!(
            status,
            StorageDirStatus {
                path,
                exists: false,
                readable: false,
                writable: false,
            }
        );
    }

    #[test]
    fn check_storage_dir_pass() {
        let result = check_storage_dir(&StorageDirStatus {
            path: PathBuf::from("/home/user/.fabro"),
            exists: true,
            readable: true,
            writable: true,
        });

        assert_eq!(result.status, CheckStatus::Pass);
        assert_eq!(result.summary, "/home/user/.fabro");
        assert!(result.remediation.is_none());
        assert_eq!(result.details.len(), 3);
    }

    #[test]
    fn check_storage_dir_not_exists() {
        let result = check_storage_dir(&StorageDirStatus {
            path: PathBuf::from("/tmp/nonexistent-fabro-doctor-test-xyz"),
            exists: false,
            readable: false,
            writable: false,
        });

        assert_eq!(result.status, CheckStatus::Error);
        assert!(result.summary.contains("nonexistent-fabro-doctor-test-xyz"));
        assert!(result.remediation.as_deref().unwrap().contains("mkdir -p"));
    }

    #[test]
    fn check_storage_dir_not_writable() {
        let result = check_storage_dir(&StorageDirStatus {
            path: PathBuf::from("/home/user/.fabro"),
            exists: true,
            readable: true,
            writable: false,
        });

        assert_eq!(result.status, CheckStatus::Error);
        assert_eq!(result.summary, "/home/user/.fabro");
        assert_eq!(
            result.remediation.as_deref(),
            Some("Fix permissions on /home/user/.fabro")
        );
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
            title: "Fabro Doctor".to_string(),
            sections: vec![CheckSection {
                title: "Local".to_string(),
                checks: vec![CheckResult {
                    name: "Configuration".to_string(),
                    status: CheckStatus::Pass,
                    summary: "loaded".to_string(),
                    details: vec![CheckDetail::new(
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
