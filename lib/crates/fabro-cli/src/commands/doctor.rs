use std::path::PathBuf;
use std::process::Command;
use std::sync::LazyLock;

use anyhow::Result;
use fabro_api::types as api_types;
use fabro_config::dotenv;
use fabro_config::user::{default_user_config_path, legacy_user_config_path};
pub(crate) use fabro_util::check_report::{
    CheckDetail, CheckReport, CheckResult, CheckSection, CheckStatus,
};
use fabro_util::terminal::Styles;
use fabro_util::version::FABRO_VERSION;
use regex::Regex;
use semver::Version;

use crate::args::{DoctorArgs, GlobalArgs};
use crate::server_client;
use crate::shared::print_json_pretty;

pub(crate) struct DepSpec {
    pub name: &'static str,
    command: &'static [&'static str],
    pub required: bool,
    pub min_version: Version,
    pattern: &'static LazyLock<Regex>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ProbeOutcome {
    NotFound,
    Failed,
    Ok { version: Option<Version> },
}

static OPENSSL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:OpenSSL|LibreSSL)\s+(\d+)\.(\d+)\.(\d+)").unwrap());
static DOT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"graphviz version (\d+)\.(\d+)\.(\d+)").unwrap());

pub(crate) const DEP_SPECS: &[DepSpec] = &[
    DepSpec {
        name: "openssl",
        command: &["openssl", "version"],
        required: true,
        min_version: Version::new(3, 0, 0),
        pattern: &OPENSSL_RE,
    },
    DepSpec {
        name: "dot",
        command: &["dot", "-V"],
        required: false,
        min_version: Version::new(2, 0, 0),
        pattern: &DOT_RE,
    },
];

fn parse_version(re: &Regex, output: &str) -> Option<Version> {
    let caps = re.captures(output)?;
    Some(Version::new(
        caps[1].parse().ok()?,
        caps[2].parse().ok()?,
        caps[3].parse().ok()?,
    ))
}

pub(crate) fn probe_system_deps() -> Vec<ProbeOutcome> {
    DEP_SPECS
        .iter()
        .map(|spec| {
            let result = Command::new(spec.command[0])
                .args(&spec.command[1..])
                .output()
                .ok();

            match result {
                None => ProbeOutcome::NotFound,
                Some(output) if !output.status.success() => ProbeOutcome::Failed,
                Some(output) => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let version = parse_version(spec.pattern, &stdout)
                        .or_else(|| parse_version(spec.pattern, &stderr));
                    ProbeOutcome::Ok { version }
                }
            }
        })
        .collect()
}

fn dep_issue(name: &str, issue: &str, required: bool) -> (CheckStatus, String) {
    let severity = if required { "required" } else { "optional" };
    let status = if required {
        CheckStatus::Error
    } else {
        CheckStatus::Warning
    };
    (status, format!("{name}: {issue} ({severity})"))
}

pub(crate) fn check_system_deps(specs: &[DepSpec], outcomes: &[ProbeOutcome]) -> CheckResult {
    let mut details = Vec::new();
    let mut worst_status = CheckStatus::Pass;

    for (spec, outcome) in specs.iter().zip(outcomes) {
        let (status, text) = match outcome {
            ProbeOutcome::NotFound => dep_issue(spec.name, "not found", spec.required),
            ProbeOutcome::Failed => dep_issue(spec.name, "command failed", spec.required),
            ProbeOutcome::Ok { version: None } => {
                (CheckStatus::Pass, format!("{}: version unknown", spec.name))
            }
            ProbeOutcome::Ok {
                version: Some(version),
            } => {
                if version < &spec.min_version {
                    (
                        CheckStatus::Warning,
                        format!("{}: {version} (minimum {})", spec.name, spec.min_version),
                    )
                } else {
                    (CheckStatus::Pass, format!("{}: {version}", spec.name))
                }
            }
        };

        worst_status = worst_status.max(status);
        details.push(CheckDetail::new(text));
    }

    let summary = match worst_status {
        CheckStatus::Pass => "all found".to_string(),
        CheckStatus::Warning => "some issues".to_string(),
        CheckStatus::Error => "missing required tools".to_string(),
    };

    let remediation = match worst_status {
        CheckStatus::Pass => None,
        _ => Some("Install missing system dependencies".to_string()),
    };

    CheckResult {
        name: "System dependencies".to_string(),
        status: worst_status,
        summary,
        details,
        remediation,
    }
}

pub(crate) fn check_config(
    user_path: Option<PathBuf>,
    legacy_path: Option<PathBuf>,
) -> CheckResult {
    match (user_path, legacy_path) {
        (Some(path), None) => CheckResult {
            name: "Configuration".to_string(),
            status: CheckStatus::Pass,
            summary: path.display().to_string(),
            details: vec![CheckDetail::new(format!("Loaded from {}", path.display()))],
            remediation: None,
        },
        (Some(path), Some(legacy)) => CheckResult {
            name: "Configuration".to_string(),
            status: CheckStatus::Warning,
            summary: path.display().to_string(),
            details: vec![
                CheckDetail::new(format!("Loaded from {}", path.display())),
                CheckDetail::new(format!("Ignoring legacy config file {}", legacy.display())),
            ],
            remediation: Some(format!("Delete or rename {}", legacy.display())),
        },
        (None, Some(legacy)) => CheckResult {
            name: "Configuration".to_string(),
            status: CheckStatus::Warning,
            summary: "legacy config file ignored".to_string(),
            details: vec![
                CheckDetail::new(format!("Found legacy config file {}", legacy.display())),
                CheckDetail::new("Rename it to ~/.fabro/user.toml".to_string()),
            ],
            remediation: Some(format!("Rename {} to ~/.fabro/user.toml", legacy.display())),
        },
        (None, None) => CheckResult {
            name: "Configuration".to_string(),
            status: CheckStatus::Warning,
            summary: "no user config file found".to_string(),
            details: vec![CheckDetail::new(
                "Create ~/.fabro/user.toml to configure Fabro".to_string(),
            )],
            remediation: Some("Create ~/.fabro/user.toml".to_string()),
        },
    }
}

fn check_legacy_env(path: Option<PathBuf>) -> CheckResult {
    match path {
        Some(path) => CheckResult {
            name: "Legacy .env".to_string(),
            status: CheckStatus::Warning,
            summary: "legacy secrets file detected".to_string(),
            details: vec![CheckDetail::new(format!(
                "{} is no longer read by fabro",
                path.display()
            ))],
            remediation: Some(
                "Re-enter credentials with `fabro provider login` or `fabro secret set`."
                    .to_string(),
            ),
        },
        None => CheckResult {
            name: "Legacy .env".to_string(),
            status: CheckStatus::Pass,
            summary: "not present".to_string(),
            details: Vec::new(),
            remediation: None,
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

fn render_report(report: &CheckReport, styles: &Styles, verbose: bool) {
    let term_width = console::Term::stderr().size().1;
    print!("{}", report.render(styles, verbose, None, Some(term_width)));
}

pub(crate) async fn run_doctor(
    args: &DoctorArgs,
    verbose: bool,
    globals: &GlobalArgs,
) -> Result<i32, anyhow::Error> {
    let styles = Styles::detect_stdout();
    let spinner = if globals.json {
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

    let user_config_path = default_user_config_path();
    let legacy_config_path = legacy_user_config_path();
    let legacy_env_path = dotenv::env_file_path().ok().filter(|path| path.exists());

    let mut report = CheckReport {
        title: "Fabro Doctor".to_string(),
        sections: vec![CheckSection {
            title: "Local".to_string(),
            checks: vec![
                check_config(
                    user_config_path.filter(|path| path.exists()),
                    legacy_config_path.filter(|path| path.exists()),
                ),
                check_legacy_env(legacy_env_path),
            ],
        }],
    };

    let client = match server_client::connect_server_backed_api_client(&args.target).await {
        Ok(client) => client,
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

            if globals.json {
                print_json_pretty(&report)?;
            } else {
                render_report(&report, &styles, verbose);
            }
            return Ok(1);
        }
    };

    let health = match client.get_health().send().await {
        Ok(response) => response.into_inner(),
        Err(err) => {
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

            if globals.json {
                print_json_pretty(&report)?;
            } else {
                render_report(&report, &styles, verbose);
            }
            return Ok(1);
        }
    };

    report.sections[0]
        .checks
        .push(check_version_parity(&health.version));

    match client.run_diagnostics().send().await {
        Ok(response) => {
            let diagnostics = response.into_inner();
            report
                .sections
                .extend(convert_diagnostics_sections(diagnostics.sections));
        }
        Err(err) => {
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

    if globals.json {
        print_json_pretty(&report)?;
    } else {
        render_report(&report, &styles, verbose);
    }

    Ok(i32::from(report.has_errors()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_config_pass_with_path() {
        let result = check_config(Some(PathBuf::from("/home/user/.fabro/user.toml")), None);
        assert_eq!(result.status, CheckStatus::Pass);
        assert!(result.summary.contains(".fabro/user.toml"));
    }

    #[test]
    fn check_config_warning_without_path() {
        let result = check_config(None, None);
        assert_eq!(result.status, CheckStatus::Warning);
        assert!(result.remediation.is_some());
    }

    #[test]
    fn check_config_warning_for_legacy_only_path() {
        let result = check_config(None, Some(PathBuf::from("/home/user/.fabro/cli.toml")));
        assert_eq!(result.status, CheckStatus::Warning);
        assert!(result.summary.contains("legacy"));
    }

    #[test]
    fn check_legacy_env_warning_when_present() {
        let result = check_legacy_env(Some(PathBuf::from("/home/user/.fabro/.env")));
        assert_eq!(result.status, CheckStatus::Warning);
        assert!(result.summary.contains("legacy secrets file"));
    }

    #[test]
    fn check_version_parity_warns_on_mismatch() {
        let result = check_version_parity("0.0.0-test");
        assert_eq!(result.status, CheckStatus::Warning);
    }

    #[test]
    fn parse_version_openssl() {
        assert_eq!(
            parse_version(&OPENSSL_RE, "OpenSSL 3.4.1 11 Feb 2025"),
            Some(Version::new(3, 4, 1))
        );
    }

    #[test]
    fn parse_version_dot() {
        assert_eq!(
            parse_version(&DOT_RE, "dot - graphviz version 12.2.1 (20241206.2024)"),
            Some(Version::new(12, 2, 1))
        );
    }

    #[test]
    fn parse_version_garbage_returns_none() {
        assert_eq!(parse_version(&OPENSSL_RE, "not a version"), None);
        assert_eq!(parse_version(&DOT_RE, "no version here"), None);
    }

    fn spec(name: &'static str, required: bool, min_version: Version) -> DepSpec {
        DepSpec {
            name,
            command: &["echo", "unused"],
            required,
            min_version,
            pattern: &DOT_RE,
        }
    }

    #[test]
    fn check_system_deps_all_present() {
        let specs = [
            spec("openssl", true, Version::new(3, 0, 0)),
            spec("dot", false, Version::new(2, 0, 0)),
        ];
        let outcomes = [
            ProbeOutcome::Ok {
                version: Some(Version::new(3, 4, 1)),
            },
            ProbeOutcome::Ok {
                version: Some(Version::new(12, 2, 1)),
            },
        ];
        let result = check_system_deps(&specs, &outcomes);
        assert_eq!(result.status, CheckStatus::Pass);
        assert_eq!(result.summary, "all found");
    }

    #[test]
    fn check_system_deps_required_missing_is_error() {
        let specs = [spec("openssl", true, Version::new(3, 0, 0))];
        let outcomes = [ProbeOutcome::NotFound];
        let result = check_system_deps(&specs, &outcomes);
        assert_eq!(result.status, CheckStatus::Error);
    }

    #[test]
    fn check_system_deps_optional_missing_is_warning() {
        let specs = [spec("dot", false, Version::new(2, 0, 0))];
        let outcomes = [ProbeOutcome::NotFound];
        let result = check_system_deps(&specs, &outcomes);
        assert_eq!(result.status, CheckStatus::Warning);
    }

    #[test]
    fn check_system_deps_outdated_is_warning() {
        let specs = [spec("openssl", true, Version::new(3, 0, 0))];
        let outcomes = [ProbeOutcome::Ok {
            version: Some(Version::new(1, 1, 1)),
        }];
        let result = check_system_deps(&specs, &outcomes);
        assert_eq!(result.status, CheckStatus::Warning);
    }

    #[test]
    fn check_system_deps_unparseable_success_is_pass() {
        let specs = [spec("openssl", true, Version::new(3, 0, 0))];
        let outcomes = [ProbeOutcome::Ok { version: None }];
        let result = check_system_deps(&specs, &outcomes);
        assert_eq!(result.status, CheckStatus::Pass);
        assert!(result.details[0].text.contains("version unknown"));
    }

    #[test]
    fn check_system_deps_required_command_failed_is_error() {
        let specs = [spec("openssl", true, Version::new(3, 0, 0))];
        let outcomes = [ProbeOutcome::Failed];
        let result = check_system_deps(&specs, &outcomes);
        assert_eq!(result.status, CheckStatus::Error);
    }

    #[test]
    fn check_system_deps_optional_command_failed_is_warning() {
        let specs = [spec("dot", false, Version::new(2, 0, 0))];
        let outcomes = [ProbeOutcome::Failed];
        let result = check_system_deps(&specs, &outcomes);
        assert_eq!(result.status, CheckStatus::Warning);
    }

    #[test]
    fn check_system_deps_error_beats_warning() {
        let specs = [
            spec("openssl", true, Version::new(3, 0, 0)),
            spec("dot", false, Version::new(2, 0, 0)),
        ];
        let outcomes = [ProbeOutcome::NotFound, ProbeOutcome::NotFound];
        let result = check_system_deps(&specs, &outcomes);
        assert_eq!(result.status, CheckStatus::Error);
    }
}
