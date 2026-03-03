use std::fmt::Write;
use std::path::PathBuf;
use std::process::Command;

use arc_api::server_config::{ApiAuthStrategy, AuthProvider};
use arc_llm::provider::Provider;
use arc_util::terminal::Styles;
use regex::Regex;

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    Pass,
    Warning,
    Error,
}

#[derive(Debug, Clone)]
pub struct CheckDetail {
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct CheckResult {
    pub name: String,
    pub status: CheckStatus,
    pub summary: String,
    pub details: Vec<CheckDetail>,
    pub remediation: Option<String>,
}

pub struct DoctorReport {
    pub checks: Vec<CheckResult>,
}

impl DoctorReport {
    pub fn has_errors(&self) -> bool {
        self.checks
            .iter()
            .any(|c| c.status == CheckStatus::Error)
    }

    pub fn issue_count(&self) -> usize {
        self.checks
            .iter()
            .filter(|c| matches!(c.status, CheckStatus::Warning | CheckStatus::Error))
            .count()
    }

    pub fn render(&self, s: &Styles, verbose: bool, live: bool) -> String {
        let mut out = String::new();

        writeln!(out, "{}", s.bold.apply_to("Arc Doctor")).unwrap();
        writeln!(out).unwrap();

        for check in &self.checks {
            let (icon, color) = match check.status {
                CheckStatus::Pass => ("[✓]", &s.green),
                CheckStatus::Warning => ("[!]", &s.yellow),
                CheckStatus::Error => ("[✗]", &s.red),
            };

            writeln!(
                out,
                "  {} {} ({})",
                color.apply_to(icon),
                s.bold.apply_to(&check.name),
                check.summary,
            )
            .unwrap();

            if verbose {
                for detail in &check.details {
                    writeln!(out, "      • {}", detail.text).unwrap();
                }
            }
        }

        let issues = self.issue_count();
        writeln!(out).unwrap();

        if issues == 0 {
            writeln!(out, "All checks passed.").unwrap();
        } else {
            writeln!(
                out,
                "Doctor found issues in {issues} {}.",
                if issues == 1 { "category" } else { "categories" }
            )
            .unwrap();

            let errors: Vec<_> = self
                .checks
                .iter()
                .filter(|c| c.status == CheckStatus::Error)
                .collect();
            if !errors.is_empty() {
                writeln!(out).unwrap();
                writeln!(out, "{}", s.bold.apply_to("Errors:")).unwrap();
                for check in &errors {
                    write!(out, "  • {}", check.name).unwrap();
                    if let Some(ref rem) = check.remediation {
                        write!(out, " — {rem}").unwrap();
                    }
                    writeln!(out).unwrap();
                }
            }

            let warnings: Vec<_> = self
                .checks
                .iter()
                .filter(|c| c.status == CheckStatus::Warning)
                .collect();
            if !warnings.is_empty() {
                writeln!(out).unwrap();
                writeln!(out, "{}", s.bold.apply_to("Warnings:")).unwrap();
                for check in &warnings {
                    write!(out, "  • {}", check.name).unwrap();
                    if let Some(ref rem) = check.remediation {
                        write!(out, " — {rem}").unwrap();
                    }
                    writeln!(out).unwrap();
                }
            }
        }

        if !live {
            writeln!(out).unwrap();
            writeln!(out, "Run with --live to probe service connectivity.").unwrap();
        }

        out
    }
}

// ---------------------------------------------------------------------------
// System dependency types and parsers
// ---------------------------------------------------------------------------

pub struct DepProbeResult {
    pub name: &'static str,
    pub required: bool,
    pub raw_output: Option<String>,
    pub success: bool,
    pub version: Option<(u32, u32, u32)>,
    pub min_version: (u32, u32, u32),
}

pub fn parse_openssl_version(output: &str) -> Option<(u32, u32, u32)> {
    let re = Regex::new(r"(?:OpenSSL|LibreSSL)\s+(\d+)\.(\d+)\.(\d+)").ok()?;
    let caps = re.captures(output)?;
    Some((caps[1].parse().ok()?, caps[2].parse().ok()?, caps[3].parse().ok()?))
}

pub fn parse_node_version(output: &str) -> Option<(u32, u32, u32)> {
    let re = Regex::new(r"v(\d+)\.(\d+)\.(\d+)").ok()?;
    let caps = re.captures(output)?;
    Some((caps[1].parse().ok()?, caps[2].parse().ok()?, caps[3].parse().ok()?))
}

pub fn parse_gh_version(output: &str) -> Option<(u32, u32, u32)> {
    let re = Regex::new(r"gh version (\d+)\.(\d+)\.(\d+)").ok()?;
    let caps = re.captures(output)?;
    Some((caps[1].parse().ok()?, caps[2].parse().ok()?, caps[3].parse().ok()?))
}

pub fn parse_dot_version(output: &str) -> Option<(u32, u32, u32)> {
    let re = Regex::new(r"graphviz version (\d+)\.(\d+)\.(\d+)").ok()?;
    let caps = re.captures(output)?;
    Some((caps[1].parse().ok()?, caps[2].parse().ok()?, caps[3].parse().ok()?))
}

fn format_version(v: (u32, u32, u32)) -> String {
    format!("{}.{}.{}", v.0, v.1, v.2)
}

pub fn probe_system_deps() -> Vec<DepProbeResult> {
    let specs: Vec<(&str, &[&str], bool, (u32, u32, u32), fn(&str) -> Option<(u32, u32, u32)>)> = vec![
        ("openssl", &["openssl", "version"], true, (3, 0, 0), parse_openssl_version),
        ("node", &["node", "--version"], true, (20, 0, 0), parse_node_version),
        ("gh", &["gh", "--version"], false, (2, 0, 0), parse_gh_version),
        ("dot", &["dot", "-V"], false, (2, 0, 0), parse_dot_version),
    ];

    specs
        .into_iter()
        .map(|(name, args, required, min_version, parser)| {
            let result = Command::new(args[0])
                .args(&args[1..])
                .output()
                .ok();

            let success = result.as_ref().is_some_and(|o| o.status.success());

            let output = result.map(|o| {
                let stdout = String::from_utf8_lossy(&o.stdout);
                let stderr = String::from_utf8_lossy(&o.stderr);
                format!("{stdout}{stderr}")
            });

            let version = output.as_deref().and_then(parser);

            DepProbeResult {
                name,
                required,
                raw_output: output,
                success,
                version,
                min_version,
            }
        })
        .collect()
}

pub fn check_system_deps(deps: &[DepProbeResult]) -> CheckResult {
    let mut details = Vec::new();
    let mut worst_status = CheckStatus::Pass;

    for dep in deps {
        let (status, text) = match (&dep.raw_output, dep.success, dep.version) {
            (None, _, _) => {
                if dep.required {
                    (CheckStatus::Error, format!("{}: not found (required)", dep.name))
                } else {
                    (CheckStatus::Warning, format!("{}: not found (optional)", dep.name))
                }
            }
            (Some(_), false, _) => {
                if dep.required {
                    (CheckStatus::Error, format!("{}: command failed (required)", dep.name))
                } else {
                    (CheckStatus::Warning, format!("{}: command failed (optional)", dep.name))
                }
            }
            (Some(_), true, None) => {
                (CheckStatus::Pass, format!("{}: version unknown", dep.name))
            }
            (Some(_), true, Some(v)) => {
                if v < dep.min_version {
                    (
                        CheckStatus::Warning,
                        format!(
                            "{}: {} (minimum {})",
                            dep.name,
                            format_version(v),
                            format_version(dep.min_version),
                        ),
                    )
                } else {
                    (CheckStatus::Pass, format!("{}: {}", dep.name, format_version(v)))
                }
            }
        };

        if status == CheckStatus::Error
            || (status == CheckStatus::Warning && worst_status != CheckStatus::Error)
        {
            worst_status = status;
        }

        details.push(CheckDetail { text });
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

// ---------------------------------------------------------------------------
// Check functions (pure, testable)
// ---------------------------------------------------------------------------

fn apply_live_result(
    live_result: Option<&Result<(), String>>,
    details: &mut Vec<CheckDetail>,
    remediation_msg: &str,
) -> (CheckStatus, Option<String>) {
    match live_result {
        Some(Ok(())) => {
            details.push(CheckDetail {
                text: "Connectivity: OK".to_string(),
            });
            (CheckStatus::Pass, None)
        }
        Some(Err(e)) => {
            details.push(CheckDetail {
                text: format!("Connectivity: {e}"),
            });
            (CheckStatus::Warning, Some(remediation_msg.to_string()))
        }
        None => (CheckStatus::Pass, None),
    }
}

pub fn check_config(path: Option<PathBuf>) -> CheckResult {
    match path {
        Some(p) => CheckResult {
            name: "Configuration".to_string(),
            status: CheckStatus::Pass,
            summary: p.display().to_string(),
            details: vec![CheckDetail {
                text: format!("Loaded from {}", p.display()),
            }],
            remediation: None,
        },
        None => CheckResult {
            name: "Configuration".to_string(),
            status: CheckStatus::Warning,
            summary: "no config file found".to_string(),
            details: vec![CheckDetail {
                text: "Create ~/.arc/arc.toml to configure Arc".to_string(),
            }],
            remediation: Some("Create ~/.arc/arc.toml".to_string()),
        },
    }
}

pub fn check_llm_providers(
    statuses: &[(Provider, bool)],
    live_results: Option<&[(Provider, Result<(), String>)]>,
) -> CheckResult {
    let configured: Vec<_> = statuses.iter().filter(|(_, set)| *set).collect();
    let total = statuses.len();
    let count = configured.len();

    let mut details: Vec<CheckDetail> = statuses
        .iter()
        .map(|(provider, set)| {
            let env_vars = provider.api_key_env_vars().join(" or ");
            let status_text = if *set { "set" } else { "not set" };
            CheckDetail {
                text: format!("{provider} ({env_vars}): {status_text}"),
            }
        })
        .collect();

    let mut has_live_error = false;
    if let Some(results) = live_results {
        for (provider, result) in results {
            match result {
                Ok(()) => details.push(CheckDetail {
                    text: format!("{provider} connectivity: OK"),
                }),
                Err(e) => {
                    has_live_error = true;
                    details.push(CheckDetail {
                        text: format!("{provider} connectivity: {e}"),
                    });
                }
            }
        }
    }

    if count == 0 {
        CheckResult {
            name: "LLM providers".to_string(),
            status: CheckStatus::Error,
            summary: format!("{count} of {total} configured"),
            details,
            remediation: Some("Set at least one provider API key".to_string()),
        }
    } else if has_live_error {
        CheckResult {
            name: "LLM providers".to_string(),
            status: CheckStatus::Warning,
            summary: format!("{count} of {total} configured (connectivity issues)"),
            details,
            remediation: Some("Check provider API keys and network connectivity".to_string()),
        }
    } else {
        CheckResult {
            name: "LLM providers".to_string(),
            status: CheckStatus::Pass,
            summary: format!("{count} of {total} configured"),
            details,
            remediation: None,
        }
    }
}

pub fn check_brave_search(
    api_key_set: bool,
    live_result: Option<&Result<(), String>>,
) -> CheckResult {
    let mut details = vec![CheckDetail {
        text: format!(
            "BRAVE_SEARCH_API_KEY is {}",
            if api_key_set { "set" } else { "not set" }
        ),
    }];

    let (mut status, mut remediation) = if api_key_set {
        (CheckStatus::Pass, None)
    } else {
        (
            CheckStatus::Warning,
            Some("Set BRAVE_SEARCH_API_KEY to enable web search".to_string()),
        )
    };

    if api_key_set {
        let (live_status, live_remediation) = apply_live_result(
            live_result,
            &mut details,
            "Check BRAVE_SEARCH_API_KEY and network connectivity",
        );
        if live_status == CheckStatus::Warning {
            status = live_status;
            remediation = live_remediation;
        }
    }

    let summary = match (api_key_set, live_result) {
        (true, Some(Ok(()))) => "API key set, connected".to_string(),
        (true, Some(Err(_))) => "API key set, connectivity error".to_string(),
        (true, None) => "API key set".to_string(),
        (false, _) => "not configured".to_string(),
    };

    CheckResult {
        name: "Brave Search".to_string(),
        status,
        summary,
        details,
        remediation,
    }
}

pub struct SandboxStatus {
    pub daytona_configured: bool,
    pub daytona_probe: Option<Result<(), String>>,
    pub docker_probe: Option<Result<(), String>>,
}

pub fn check_sandbox(status: &SandboxStatus) -> CheckResult {
    let mut configured = Vec::new();
    let mut available = Vec::new();
    let mut details = Vec::new();
    let mut errors = Vec::new();

    if status.daytona_configured {
        configured.push("Daytona");
    }

    match &status.daytona_probe {
        Some(Ok(())) => {
            available.push("Daytona");
            details.push(CheckDetail {
                text: "Daytona (DAYTONA_API_KEY): available".to_string(),
            });
        }
        Some(Err(e)) => {
            errors.push(format!("Daytona: {e}"));
            details.push(CheckDetail {
                text: format!("Daytona (DAYTONA_API_KEY): error — {e}"),
            });
        }
        None if status.daytona_configured => {
            details.push(CheckDetail {
                text: "Daytona (DAYTONA_API_KEY): configured".to_string(),
            });
        }
        None => {
            details.push(CheckDetail {
                text: "Daytona (DAYTONA_API_KEY): not configured".to_string(),
            });
        }
    }

    match &status.docker_probe {
        Some(Ok(())) => {
            available.push("Docker");
            details.push(CheckDetail {
                text: "Docker: available".to_string(),
            });
        }
        Some(Err(e)) => {
            errors.push(format!("Docker: {e}"));
            details.push(CheckDetail {
                text: format!("Docker: error — {e}"),
            });
        }
        None => {
            details.push(CheckDetail {
                text: "Docker: not probed".to_string(),
            });
        }
    }

    if !errors.is_empty() {
        CheckResult {
            name: "Sandbox".to_string(),
            status: CheckStatus::Error,
            summary: errors.join("; "),
            details,
            remediation: Some("Fix sandbox configuration errors".to_string()),
        }
    } else if configured.is_empty() && available.is_empty() {
        CheckResult {
            name: "Sandbox".to_string(),
            status: CheckStatus::Warning,
            summary: "no sandbox configured".to_string(),
            details,
            remediation: Some(
                "Install Docker or set DAYTONA_API_KEY to enable sandboxed execution".to_string(),
            ),
        }
    } else {
        let summary = if available.is_empty() {
            format!("{} configured", configured.join(" + "))
        } else {
            format!("{} available", available.join(" + "))
        };
        CheckResult {
            name: "Sandbox".to_string(),
            status: CheckStatus::Pass,
            summary,
            details,
            remediation: None,
        }
    }
}

pub struct GithubAppStatus {
    pub app_id: bool,
    pub client_id: bool,
    pub client_secret: bool,
    pub webhook_secret: bool,
    pub private_key: bool,
}

impl GithubAppStatus {
    fn all_set(&self) -> bool {
        self.app_id
            && self.client_id
            && self.client_secret
            && self.webhook_secret
            && self.private_key
    }

    fn none_set(&self) -> bool {
        !self.app_id
            && !self.client_id
            && !self.client_secret
            && !self.webhook_secret
            && !self.private_key
    }
}

pub fn check_github_app(status: &GithubAppStatus) -> CheckResult {
    let fields = [
        ("git.app_id", status.app_id),
        ("git.client_id", status.client_id),
        ("GITHUB_APP_CLIENT_SECRET", status.client_secret),
        ("GITHUB_APP_WEBHOOK_SECRET", status.webhook_secret),
        ("GITHUB_APP_PRIVATE_KEY", status.private_key),
    ];

    let details: Vec<CheckDetail> = fields
        .iter()
        .map(|(name, set)| CheckDetail {
            text: format!("{name}: {}", if *set { "set" } else { "not set" }),
        })
        .collect();

    if status.all_set() {
        CheckResult {
            name: "GitHub App".to_string(),
            status: CheckStatus::Pass,
            summary: "fully configured".to_string(),
            details,
            remediation: None,
        }
    } else if status.none_set() {
        CheckResult {
            name: "GitHub App".to_string(),
            status: CheckStatus::Warning,
            summary: "not configured".to_string(),
            details,
            remediation: Some(
                "Configure GitHub App in arc.toml and set env vars to enable GitHub integration"
                    .to_string(),
            ),
        }
    } else {
        let missing: Vec<_> = fields
            .iter()
            .filter(|(_, set)| !set)
            .map(|(name, _)| *name)
            .collect();
        CheckResult {
            name: "GitHub App".to_string(),
            status: CheckStatus::Error,
            summary: "partially configured".to_string(),
            details,
            remediation: Some(format!("Missing: {}", missing.join(", "))),
        }
    }
}

pub struct ApiStatus {
    pub base_url: String,
    pub authentication_strategies: String,
}

pub fn check_api(
    status: &ApiStatus,
    live_result: Option<&Result<(), String>>,
) -> CheckResult {
    let mut details = vec![
        CheckDetail {
            text: format!("Base URL: {}", status.base_url),
        },
        CheckDetail {
            text: format!("Authentication: {}", status.authentication_strategies),
        },
    ];

    let (check_status, remediation) = apply_live_result(
        live_result,
        &mut details,
        "Check that the API server is running and reachable",
    );

    CheckResult {
        name: "Arc API".to_string(),
        status: check_status,
        summary: status.base_url.clone(),
        details,
        remediation,
    }
}

pub struct WebStatus {
    pub url: String,
    pub auth_provider: String,
    pub allowed_usernames_count: usize,
}

pub fn check_web(
    status: &WebStatus,
    live_result: Option<&Result<(), String>>,
) -> CheckResult {
    let mut details = vec![
        CheckDetail {
            text: format!("URL: {}", status.url),
        },
        CheckDetail {
            text: format!("Auth provider: {}", status.auth_provider),
        },
        CheckDetail {
            text: format!("Allowed usernames: {}", status.allowed_usernames_count),
        },
    ];

    let (check_status, remediation) = apply_live_result(
        live_result,
        &mut details,
        "Check that the web app is running and reachable",
    );

    CheckResult {
        name: "Arc Web".to_string(),
        status: check_status,
        summary: status.url.clone(),
        details,
        remediation,
    }
}

// ---------------------------------------------------------------------------
// Orchestrator (does real I/O)
// ---------------------------------------------------------------------------

async fn probe_daytona() -> Option<Result<(), String>> {
    if std::env::var("DAYTONA_API_KEY").is_err() {
        return None;
    }
    Some(
        daytona_sdk::Client::new()
            .await
            .map(|_| ())
            .map_err(|e| e.to_string()),
    )
}

async fn probe_docker() -> Option<Result<(), String>> {
    let docker = bollard::Docker::connect_with_local_defaults()
        .map_err(|e| e.to_string())
        .ok()?;
    Some(docker.ping().await.map(|_| ()).map_err(|e| e.to_string()))
}

fn cheapest_model(provider: Provider) -> String {
    let models = arc_llm::catalog::list_models(Some(provider.as_str()));
    models
        .iter()
        .min_by(|a, b| {
            let cost_a = a.input_cost_per_million.unwrap_or(f64::MAX);
            let cost_b = b.input_cost_per_million.unwrap_or(f64::MAX);
            cost_a.total_cmp(&cost_b)
        })
        .map(|m| m.id.clone())
        .unwrap_or_else(|| format!("unknown-{}", provider.as_str()))
}

async fn probe_llm_provider(
    client: &arc_llm::client::Client,
    provider: Provider,
) -> (Provider, Result<(), String>) {
    let request = arc_llm::types::Request {
        model: cheapest_model(provider),
        messages: vec![arc_llm::types::Message::user("hi")],
        provider: Some(provider.as_str().to_string()),
        tools: None,
        tool_choice: None,
        response_format: None,
        temperature: None,
        top_p: None,
        max_tokens: Some(16),
        stop_sequences: None,
        reasoning_effort: None,
        metadata: None,
        provider_options: None,
    };
    let result = client.complete(&request).await.map(|_| ()).map_err(|e| e.to_string());
    (provider, result)
}

async fn probe_brave_search(http: &reqwest::Client) -> Result<(), String> {
    let api_key = std::env::var("BRAVE_SEARCH_API_KEY")
        .map_err(|_| "BRAVE_SEARCH_API_KEY not set".to_string())?;
    let resp = http
        .get("https://api.search.brave.com/res/v1/web/search?q=test&count=1")
        .header("X-Subscription-Token", api_key)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(format!("HTTP {}", resp.status()))
    }
}

async fn probe_url(http: &reqwest::Client, url: &str) -> Result<(), String> {
    http.get(url)
        .send()
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

pub async fn run_doctor(verbose: bool, live: bool) -> i32 {
    let styles = Styles::detect_stdout();

    // Gather state
    let config_path = dirs::home_dir().map(|h| h.join(".arc").join("arc.toml"));
    let config_exists = config_path
        .as_ref()
        .is_some_and(|p| p.exists());

    let llm_statuses: Vec<(Provider, bool)> = Provider::ALL
        .iter()
        .map(|p| (*p, p.has_api_key()))
        .collect();

    let brave_key_set = std::env::var("BRAVE_SEARCH_API_KEY").is_ok();

    let server_config = arc_api::server_config::load_server_config()
        .unwrap_or_default();

    let api_status = ApiStatus {
        base_url: server_config.api.base_url.clone(),
        authentication_strategies: server_config
            .api
            .authentication_strategies
            .iter()
            .map(|s| match s {
                ApiAuthStrategy::Jwt => "jwt",
                ApiAuthStrategy::Mtls => "mtls",
            })
            .collect::<Vec<_>>()
            .join(", "),
    };

    let web_status = WebStatus {
        url: server_config.web.url.clone(),
        auth_provider: match server_config.web.auth.provider {
            AuthProvider::Github => "github".to_string(),
            AuthProvider::InsecureDisabled => "insecure_disabled".to_string(),
        },
        allowed_usernames_count: server_config.web.auth.allowed_usernames.len(),
    };

    let github_status = GithubAppStatus {
        app_id: server_config.git.app_id.is_some(),
        client_id: server_config.git.client_id.is_some(),
        client_secret: std::env::var("GITHUB_APP_CLIENT_SECRET").is_ok(),
        webhook_secret: std::env::var("GITHUB_APP_WEBHOOK_SECRET").is_ok(),
        private_key: std::env::var("GITHUB_APP_PRIVATE_KEY").is_ok(),
    };

    let daytona_configured = std::env::var("DAYTONA_API_KEY").is_ok();

    let dep_results = probe_system_deps();

    // Live probes (only when --live is set)
    let sandbox_status;
    let llm_live_results: Option<Vec<(Provider, Result<(), String>)>>;
    let brave_live_result: Option<Result<(), String>>;
    let api_live_result: Option<Result<(), String>>;
    let web_live_result: Option<Result<(), String>>;

    if live {
        let http = reqwest::Client::new();

        // Build LLM client — may fail if no keys are set
        let llm_client = arc_llm::client::Client::from_env().await.ok();

        let configured_providers: Vec<Provider> = llm_statuses
            .iter()
            .filter(|(_, set)| *set)
            .map(|(p, _)| *p)
            .collect();

        let llm_fut = async {
            if let Some(client) = &llm_client {
                let futures: Vec<_> = configured_providers
                    .iter()
                    .map(|p| probe_llm_provider(client, *p))
                    .collect();
                Some(futures::future::join_all(futures).await)
            } else {
                None
            }
        };

        let sandbox_fut = async {
            let (daytona_probe, docker_probe) = tokio::join!(probe_daytona(), probe_docker());
            SandboxStatus {
                daytona_configured,
                daytona_probe,
                docker_probe,
            }
        };
        let brave_fut = probe_brave_search(&http);
        let api_url = format!("{}/runs", server_config.api.base_url);
        let api_fut = probe_url(&http, &api_url);
        let web_fut = probe_url(&http, &server_config.web.url);

        let (sandbox, llm, brave, api, web) =
            tokio::join!(sandbox_fut, llm_fut, brave_fut, api_fut, web_fut);

        sandbox_status = sandbox;
        llm_live_results = llm;
        brave_live_result = Some(brave);
        api_live_result = Some(api);
        web_live_result = Some(web);
    } else {
        sandbox_status = SandboxStatus {
            daytona_configured,
            daytona_probe: None,
            docker_probe: None,
        };
        llm_live_results = None;
        brave_live_result = None;
        api_live_result = None;
        web_live_result = None;
    }

    // Run pure checks
    let report = DoctorReport {
        checks: vec![
            check_config(if config_exists { config_path } else { None }),
            check_system_deps(&dep_results),
            check_api(&api_status, api_live_result.as_ref()),
            check_web(&web_status, web_live_result.as_ref()),
            check_llm_providers(&llm_statuses, llm_live_results.as_deref()),
            check_brave_search(brave_key_set, brave_live_result.as_ref()),
            check_sandbox(&sandbox_status),
            check_github_app(&github_status),
        ],
    };

    print!("{}", report.render(&styles, verbose, live));

    if report.has_errors() { 1 } else { 0 }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn pass_check(name: &str) -> CheckResult {
        CheckResult {
            name: name.to_string(),
            status: CheckStatus::Pass,
            summary: "all good".to_string(),
            details: vec![CheckDetail {
                text: "everything is fine".to_string(),
            }],
            remediation: None,
        }
    }

    fn warning_check(name: &str) -> CheckResult {
        CheckResult {
            name: name.to_string(),
            status: CheckStatus::Warning,
            summary: "not configured".to_string(),
            details: vec![CheckDetail {
                text: "missing something".to_string(),
            }],
            remediation: Some("fix it".to_string()),
        }
    }

    fn error_check(name: &str) -> CheckResult {
        CheckResult {
            name: name.to_string(),
            status: CheckStatus::Error,
            summary: "broken".to_string(),
            details: vec![CheckDetail {
                text: "something is wrong".to_string(),
            }],
            remediation: Some("repair it".to_string()),
        }
    }

    // -- render: all-pass, no color --

    #[test]
    fn render_all_pass_no_color() {
        let report = DoctorReport {
            checks: vec![pass_check("Test")],
        };
        let out = report.render(&Styles::new(false), false, false);
        assert!(out.contains("[✓]"));
        assert!(out.contains("All checks passed."));
        assert!(out.contains("Arc Doctor"));
    }

    // -- render: warning footer --

    #[test]
    fn render_warning_footer() {
        let report = DoctorReport {
            checks: vec![warning_check("Optional")],
        };
        let out = report.render(&Styles::new(false), false, false);
        assert!(out.contains("[!]"));
        assert!(out.contains("Doctor found issues in 1 category."));
        assert!(out.contains("Warnings:"));
        assert!(out.contains("fix it"));
    }

    // -- render: error footer --

    #[test]
    fn render_error_footer() {
        let report = DoctorReport {
            checks: vec![error_check("Broken")],
        };
        let out = report.render(&Styles::new(false), false, false);
        assert!(out.contains("[✗]"));
        assert!(out.contains("Errors:"));
        assert!(out.contains("repair it"));
    }

    // -- render: verbose mode --

    #[test]
    fn render_verbose_shows_details() {
        let report = DoctorReport {
            checks: vec![pass_check("Verbose")],
        };
        let out = report.render(&Styles::new(false), true, false);
        assert!(out.contains("•"));
        assert!(out.contains("everything is fine"));
    }

    #[test]
    fn render_default_hides_details() {
        let report = DoctorReport {
            checks: vec![pass_check("Verbose")],
        };
        let out = report.render(&Styles::new(false), false, false);
        assert!(!out.contains("everything is fine"));
    }

    // -- render: color --

    #[test]
    fn render_color_pass_green() {
        let report = DoctorReport {
            checks: vec![pass_check("Color")],
        };
        let out = report.render(&Styles::new(true), false, false);
        assert!(out.contains("\x1b[32m")); // green
    }

    #[test]
    fn render_color_warning_yellow() {
        let report = DoctorReport {
            checks: vec![warning_check("Color")],
        };
        let out = report.render(&Styles::new(true), false, false);
        assert!(out.contains("\x1b[33m")); // yellow
    }

    #[test]
    fn render_color_error_red() {
        let report = DoctorReport {
            checks: vec![error_check("Color")],
        };
        let out = report.render(&Styles::new(true), false, false);
        assert!(out.contains("\x1b[31m")); // red
    }

    // -- has_errors / issue_count --

    #[test]
    fn has_errors_false_for_warnings_only() {
        let report = DoctorReport {
            checks: vec![pass_check("OK"), warning_check("Warn")],
        };
        assert!(!report.has_errors());
    }

    #[test]
    fn has_errors_true_when_error_present() {
        let report = DoctorReport {
            checks: vec![pass_check("OK"), error_check("Broken")],
        };
        assert!(report.has_errors());
    }

    #[test]
    fn issue_count_counts_warnings_and_errors() {
        let report = DoctorReport {
            checks: vec![
                pass_check("OK"),
                warning_check("Warn"),
                error_check("Broken"),
            ],
        };
        assert_eq!(report.issue_count(), 2);
    }

    // -- check_config --

    #[test]
    fn check_config_pass_with_path() {
        let result = check_config(Some(PathBuf::from("/home/user/.arc/arc.toml")));
        assert_eq!(result.status, CheckStatus::Pass);
        assert!(result.summary.contains(".arc/arc.toml"));
    }

    #[test]
    fn check_config_warning_without_path() {
        let result = check_config(None);
        assert_eq!(result.status, CheckStatus::Warning);
        assert!(result.remediation.is_some());
    }

    // -- check_llm_providers --

    #[test]
    fn check_llm_all_configured() {
        let statuses: Vec<(Provider, bool)> =
            Provider::ALL.iter().map(|p| (*p, true)).collect();
        let result = check_llm_providers(&statuses, None);
        assert_eq!(result.status, CheckStatus::Pass);
        assert!(result.summary.contains("7 of 7"));
    }

    #[test]
    fn check_llm_some_configured() {
        let mut statuses: Vec<(Provider, bool)> =
            Provider::ALL.iter().map(|p| (*p, false)).collect();
        statuses[0].1 = true; // Anthropic
        statuses[1].1 = true; // OpenAi
        statuses[2].1 = true; // Gemini
        statuses[3].1 = true; // Kimi
        statuses[4].1 = true; // Zai
        let result = check_llm_providers(&statuses, None);
        assert_eq!(result.status, CheckStatus::Pass);
        assert!(result.summary.contains("5 of 7"));
    }

    #[test]
    fn check_llm_none_configured() {
        let statuses: Vec<(Provider, bool)> =
            Provider::ALL.iter().map(|p| (*p, false)).collect();
        let result = check_llm_providers(&statuses, None);
        assert_eq!(result.status, CheckStatus::Error);
        assert!(result.summary.contains("0 of 7"));
    }

    #[test]
    fn check_llm_live_ok() {
        let statuses = vec![(Provider::Anthropic, true)];
        let live = vec![(Provider::Anthropic, Ok(()))];
        let result = check_llm_providers(&statuses, Some(&live));
        assert_eq!(result.status, CheckStatus::Pass);
        assert!(result.details.iter().any(|d| d.text.contains("connectivity: OK")));
    }

    #[test]
    fn check_llm_live_error() {
        let statuses = vec![(Provider::Anthropic, true)];
        let live = vec![(Provider::Anthropic, Err("timeout".to_string()))];
        let result = check_llm_providers(&statuses, Some(&live));
        assert_eq!(result.status, CheckStatus::Warning);
        assert!(result.details.iter().any(|d| d.text.contains("timeout")));
    }

    // -- check_brave_search --

    #[test]
    fn check_brave_configured() {
        let result = check_brave_search(true, None);
        assert_eq!(result.status, CheckStatus::Pass);
    }

    #[test]
    fn check_brave_not_configured() {
        let result = check_brave_search(false, None);
        assert_eq!(result.status, CheckStatus::Warning);
        assert!(result.remediation.is_some());
    }

    #[test]
    fn check_brave_live_ok() {
        let live = Ok(());
        let result = check_brave_search(true, Some(&live));
        assert_eq!(result.status, CheckStatus::Pass);
        assert!(result.summary.contains("connected"));
    }

    #[test]
    fn check_brave_live_error() {
        let live = Err("HTTP 401".to_string());
        let result = check_brave_search(true, Some(&live));
        assert_eq!(result.status, CheckStatus::Warning);
        assert!(result.details.iter().any(|d| d.text.contains("HTTP 401")));
    }

    // -- check_sandbox --

    #[test]
    fn check_sandbox_daytona_probed_ok() {
        let status = SandboxStatus {
            daytona_configured: true,
            daytona_probe: Some(Ok(())),
            docker_probe: None,
        };
        let result = check_sandbox(&status);
        assert_eq!(result.status, CheckStatus::Pass);
        assert!(result.summary.contains("Daytona available"));
    }

    #[test]
    fn check_sandbox_docker_probed_ok() {
        let status = SandboxStatus {
            daytona_configured: false,
            daytona_probe: None,
            docker_probe: Some(Ok(())),
        };
        let result = check_sandbox(&status);
        assert_eq!(result.status, CheckStatus::Pass);
        assert!(result.summary.contains("Docker available"));
    }

    #[test]
    fn check_sandbox_both_probed_ok() {
        let status = SandboxStatus {
            daytona_configured: true,
            daytona_probe: Some(Ok(())),
            docker_probe: Some(Ok(())),
        };
        let result = check_sandbox(&status);
        assert_eq!(result.status, CheckStatus::Pass);
        assert!(result.summary.contains("Daytona + Docker available"));
    }

    #[test]
    fn check_sandbox_nothing_configured() {
        let status = SandboxStatus {
            daytona_configured: false,
            daytona_probe: None,
            docker_probe: None,
        };
        let result = check_sandbox(&status);
        assert_eq!(result.status, CheckStatus::Warning);
        assert!(result.summary.contains("no sandbox configured"));
    }

    #[test]
    fn check_sandbox_daytona_configured_not_probed() {
        let status = SandboxStatus {
            daytona_configured: true,
            daytona_probe: None,
            docker_probe: None,
        };
        let result = check_sandbox(&status);
        assert_eq!(result.status, CheckStatus::Pass);
        assert!(result.summary.contains("Daytona configured"));
        assert!(result.details.iter().any(|d| d.text.contains("configured")));
    }

    #[test]
    fn check_sandbox_configured_but_broken() {
        let status = SandboxStatus {
            daytona_configured: true,
            daytona_probe: Some(Err("connection refused".to_string())),
            docker_probe: None,
        };
        let result = check_sandbox(&status);
        assert_eq!(result.status, CheckStatus::Error);
    }

    // -- check_github_app --

    #[test]
    fn check_github_all_set() {
        let status = GithubAppStatus {
            app_id: true,
            client_id: true,
            client_secret: true,
            webhook_secret: true,
            private_key: true,
        };
        let result = check_github_app(&status);
        assert_eq!(result.status, CheckStatus::Pass);
    }

    #[test]
    fn check_github_none_set() {
        let status = GithubAppStatus {
            app_id: false,
            client_id: false,
            client_secret: false,
            webhook_secret: false,
            private_key: false,
        };
        let result = check_github_app(&status);
        assert_eq!(result.status, CheckStatus::Warning);
    }

    #[test]
    fn check_github_partial() {
        let status = GithubAppStatus {
            app_id: true,
            client_id: true,
            client_secret: false,
            webhook_secret: false,
            private_key: false,
        };
        let result = check_github_app(&status);
        assert_eq!(result.status, CheckStatus::Error);
        let rem = result.remediation.unwrap();
        assert!(rem.contains("GITHUB_APP_CLIENT_SECRET"));
        assert!(rem.contains("GITHUB_APP_WEBHOOK_SECRET"));
        assert!(rem.contains("GITHUB_APP_PRIVATE_KEY"));
    }

    // -- check_api --

    #[test]
    fn check_api_shows_base_url() {
        let status = ApiStatus {
            base_url: "http://localhost:3000".to_string(),
            authentication_strategies: "jwt".to_string(),
        };
        let result = check_api(&status, None);
        assert_eq!(result.status, CheckStatus::Pass);
        assert_eq!(result.summary, "http://localhost:3000");
    }

    #[test]
    fn check_api_details_show_auth_strategy() {
        let status = ApiStatus {
            base_url: "https://api.example.com".to_string(),
            authentication_strategies: "jwt".to_string(),
        };
        let result = check_api(&status, None);
        assert!(result.details.iter().any(|d| d.text.contains("jwt")));
        assert!(result
            .details
            .iter()
            .any(|d| d.text.contains("https://api.example.com")));
    }

    #[test]
    fn check_api_live_ok() {
        let status = ApiStatus {
            base_url: "http://localhost:3000".to_string(),
            authentication_strategies: "jwt".to_string(),
        };
        let live = Ok(());
        let result = check_api(&status, Some(&live));
        assert_eq!(result.status, CheckStatus::Pass);
        assert!(result.details.iter().any(|d| d.text.contains("Connectivity: OK")));
    }

    #[test]
    fn check_api_live_error() {
        let status = ApiStatus {
            base_url: "http://localhost:3000".to_string(),
            authentication_strategies: "jwt".to_string(),
        };
        let live = Err("connection refused".to_string());
        let result = check_api(&status, Some(&live));
        assert_eq!(result.status, CheckStatus::Warning);
        assert!(result.details.iter().any(|d| d.text.contains("connection refused")));
    }

    // -- check_web --

    #[test]
    fn check_web_shows_url() {
        let status = WebStatus {
            url: "http://localhost:5173".to_string(),
            auth_provider: "github".to_string(),
            allowed_usernames_count: 0,
        };
        let result = check_web(&status, None);
        assert_eq!(result.status, CheckStatus::Pass);
        assert_eq!(result.summary, "http://localhost:5173");
    }

    #[test]
    fn check_web_details_show_auth() {
        let status = WebStatus {
            url: "https://arc.example.com".to_string(),
            auth_provider: "github".to_string(),
            allowed_usernames_count: 3,
        };
        let result = check_web(&status, None);
        assert!(result.details.iter().any(|d| d.text.contains("github")));
        assert!(result
            .details
            .iter()
            .any(|d| d.text.contains("https://arc.example.com")));
        assert!(result
            .details
            .iter()
            .any(|d| d.text.contains("Allowed usernames: 3")));
    }

    #[test]
    fn check_web_live_ok() {
        let status = WebStatus {
            url: "http://localhost:5173".to_string(),
            auth_provider: "github".to_string(),
            allowed_usernames_count: 0,
        };
        let live = Ok(());
        let result = check_web(&status, Some(&live));
        assert_eq!(result.status, CheckStatus::Pass);
        assert!(result.details.iter().any(|d| d.text.contains("Connectivity: OK")));
    }

    #[test]
    fn check_web_live_error() {
        let status = WebStatus {
            url: "http://localhost:5173".to_string(),
            auth_provider: "github".to_string(),
            allowed_usernames_count: 0,
        };
        let live = Err("connection refused".to_string());
        let result = check_web(&status, Some(&live));
        assert_eq!(result.status, CheckStatus::Warning);
        assert!(result.details.iter().any(|d| d.text.contains("connection refused")));
    }

    // -- render: multiple issues --

    #[test]
    fn render_multiple_issues_pluralizes() {
        let report = DoctorReport {
            checks: vec![warning_check("A"), error_check("B")],
        };
        let out = report.render(&Styles::new(false), false, false);
        assert!(out.contains("2 categories"));
    }

    // -- version parsers --

    #[test]
    fn parse_openssl_version_valid() {
        assert_eq!(
            parse_openssl_version("OpenSSL 3.4.1 11 Feb 2025 (Library: OpenSSL 3.4.1 11 Feb 2025)"),
            Some((3, 4, 1)),
        );
    }

    #[test]
    fn parse_openssl_version_libressl() {
        assert_eq!(
            parse_openssl_version("LibreSSL 3.3.6"),
            Some((3, 3, 6)),
        );
    }

    #[test]
    fn parse_openssl_version_garbage() {
        assert_eq!(parse_openssl_version("not a version"), None);
    }

    #[test]
    fn parse_node_version_valid() {
        assert_eq!(parse_node_version("v22.14.0"), Some((22, 14, 0)));
    }

    #[test]
    fn parse_node_version_garbage() {
        assert_eq!(parse_node_version("node not found"), None);
    }

    #[test]
    fn parse_gh_version_valid() {
        assert_eq!(
            parse_gh_version("gh version 2.67.0 (2025-01-31)\nhttps://github.com/cli/cli/releases/tag/v2.67.0"),
            Some((2, 67, 0)),
        );
    }

    #[test]
    fn parse_gh_version_garbage() {
        assert_eq!(parse_gh_version("something else"), None);
    }

    #[test]
    fn parse_dot_version_valid() {
        assert_eq!(
            parse_dot_version("dot - graphviz version 12.2.1 (20241206.2024)"),
            Some((12, 2, 1)),
        );
    }

    #[test]
    fn parse_dot_version_garbage() {
        assert_eq!(parse_dot_version("no version here"), None);
    }

    // -- check_system_deps --

    fn dep(
        name: &'static str,
        required: bool,
        raw_output: Option<&str>,
        success: bool,
        version: Option<(u32, u32, u32)>,
        min_version: (u32, u32, u32),
    ) -> DepProbeResult {
        DepProbeResult {
            name,
            required,
            raw_output: raw_output.map(String::from),
            success,
            version,
            min_version,
        }
    }

    #[test]
    fn check_system_deps_all_present() {
        let deps = vec![
            dep("openssl", true, Some("OpenSSL 3.4.1"), true, Some((3, 4, 1)), (3, 0, 0)),
            dep("node", true, Some("v22.14.0"), true, Some((22, 14, 0)), (20, 0, 0)),
            dep("gh", false, Some("gh version 2.67.0"), true, Some((2, 67, 0)), (2, 0, 0)),
            dep("dot", false, Some("graphviz version 12.2.1"), true, Some((12, 2, 1)), (2, 0, 0)),
        ];
        let result = check_system_deps(&deps);
        assert_eq!(result.status, CheckStatus::Pass);
        assert_eq!(result.summary, "all found");
    }

    #[test]
    fn check_system_deps_required_missing_is_error() {
        let deps = vec![
            dep("openssl", true, None, false, None, (3, 0, 0)),
        ];
        let result = check_system_deps(&deps);
        assert_eq!(result.status, CheckStatus::Error);
        assert!(result.details[0].text.contains("not found (required)"));
    }

    #[test]
    fn check_system_deps_optional_missing_is_warning() {
        let deps = vec![
            dep("gh", false, None, false, None, (2, 0, 0)),
        ];
        let result = check_system_deps(&deps);
        assert_eq!(result.status, CheckStatus::Warning);
        assert!(result.details[0].text.contains("not found (optional)"));
    }

    #[test]
    fn check_system_deps_outdated_is_warning() {
        let deps = vec![
            dep("openssl", true, Some("OpenSSL 1.1.1"), true, Some((1, 1, 1)), (3, 0, 0)),
        ];
        let result = check_system_deps(&deps);
        assert_eq!(result.status, CheckStatus::Warning);
        assert!(result.details[0].text.contains("1.1.1"));
        assert!(result.details[0].text.contains("minimum 3.0.0"));
    }

    #[test]
    fn check_system_deps_unparseable_success_is_pass() {
        let deps = vec![
            dep("openssl", true, Some("weird output"), true, None, (3, 0, 0)),
        ];
        let result = check_system_deps(&deps);
        assert_eq!(result.status, CheckStatus::Pass);
        assert!(result.details[0].text.contains("version unknown"));
    }

    #[test]
    fn check_system_deps_required_command_failed_is_error() {
        let deps = vec![
            dep("node", true, Some("dyld: Library not loaded"), false, None, (20, 0, 0)),
        ];
        let result = check_system_deps(&deps);
        assert_eq!(result.status, CheckStatus::Error);
        assert!(result.details[0].text.contains("command failed (required)"));
    }

    #[test]
    fn check_system_deps_optional_command_failed_is_warning() {
        let deps = vec![
            dep("gh", false, Some("some error"), false, None, (2, 0, 0)),
        ];
        let result = check_system_deps(&deps);
        assert_eq!(result.status, CheckStatus::Warning);
        assert!(result.details[0].text.contains("command failed (optional)"));
    }

    #[test]
    fn check_system_deps_error_beats_warning() {
        let deps = vec![
            dep("openssl", true, None, false, None, (3, 0, 0)),
            dep("gh", false, None, false, None, (2, 0, 0)),
        ];
        let result = check_system_deps(&deps);
        assert_eq!(result.status, CheckStatus::Error);
    }
}
