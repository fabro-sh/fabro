use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use fabro_auth::auth_issue_message;
use fabro_llm::client::Client as LlmClient;
use fabro_llm::model_test::{ModelTestStatus, run_basic_model_probe};
use fabro_model::{Catalog, ProviderId};
use fabro_redact::redact_string;
use fabro_sandbox::daytona;
use fabro_static::EnvVars;
use fabro_types::settings::server::GithubIntegrationStrategy;
use fabro_types::settings::{InterpString, ServerAuthMethod};
use fabro_util::check_report::{CheckDetail, CheckResult, CheckSection, CheckStatus};
use fabro_util::dev_token::validate_dev_token_format;
use fabro_util::session_secret;
use fabro_util::version::FABRO_VERSION;
use futures_util::future::join_all;
use serde::Serialize;
use tokio::time::timeout;

use crate::server::AppState;

fn http_client_or_check(
    name: &str,
    status: CheckStatus,
) -> Result<fabro_http::HttpClient, CheckResult> {
    fabro_http::http_client().map_err(|err| CheckResult {
        name: name.to_string(),
        status,
        summary: "client error".to_string(),
        details: vec![CheckDetail::new(format!("{err:#}"))],
        remediation: Some(err.to_string()),
    })
}

#[derive(Debug, Serialize)]
pub struct DiagnosticsReport {
    pub version:  String,
    pub sections: Vec<CheckSection>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProviderProbeReport {
    pub data:    Vec<ProviderProbeResult>,
    pub summary: ProviderProbeSummary,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProviderProbeResult {
    pub provider:      ProviderId,
    pub model_id:      Option<String>,
    pub status:        ProviderProbeStatus,
    pub error_message: Option<String>,
    #[serde(skip)]
    diagnostic_detail: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProviderProbeSummary {
    pub status: ProviderProbeStatus,
    pub total:  u32,
    pub passed: u32,
    pub failed: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, strum::Display)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub(crate) enum ProviderProbeStatus {
    Ok,
    Error,
}

fn decode_pem_value(name: &str, value: &str) -> Result<String, String> {
    if value.starts_with("-----") {
        return Ok(value.to_string());
    }
    let bytes = BASE64_STANDARD
        .decode(value)
        .map_err(|e| format!("{name} is not valid PEM or base64: {e}"))?;
    String::from_utf8(bytes).map_err(|e| format!("{name} base64 decoded to invalid UTF-8: {e}"))
}

fn validate_session_secret(value: &str) -> Result<(), String> {
    session_secret::validate_session_secret(value)
}

pub async fn run_all(state: &AppState) -> DiagnosticsReport {
    let (llm, github, sandbox, brave) = tokio::join!(
        check_llm_providers(state),
        check_github_app(state),
        check_sandbox(state),
        check_brave_search(state),
    );
    let crypto = check_crypto(state);

    DiagnosticsReport {
        version:  FABRO_VERSION.to_string(),
        sections: vec![
            CheckSection {
                title:  "Credentials".to_string(),
                checks: vec![llm, github, sandbox, brave],
            },
            CheckSection {
                title:  "Configuration".to_string(),
                checks: vec![crypto, check_storage_dir(state)],
            },
        ],
    }
}

async fn check_llm_providers(state: &AppState) -> CheckResult {
    let report = match test_llm_providers(state).await {
        Ok(report) => report,
        Err(err) => {
            return CheckResult {
                name:        "LLM Providers".to_string(),
                status:      CheckStatus::Error,
                summary:     "failed to initialize".to_string(),
                details:     vec![CheckDetail::new(format!("{err:#}"))],
                remediation: Some("Check configured provider credentials".to_string()),
            };
        }
    };
    if report.data.is_empty() {
        return CheckResult {
            name:        "LLM Providers".to_string(),
            status:      CheckStatus::Error,
            summary:     "none configured".to_string(),
            details:     Vec::new(),
            remediation: Some("Set at least one provider API key".to_string()),
        };
    }

    let mut details: Vec<CheckDetail> = Vec::new();
    let mut failures: Vec<ProviderFailure> = Vec::new();
    for result in &report.data {
        match result.status {
            ProviderProbeStatus::Ok => {
                details.push(CheckDetail::new(format!("{}: OK", result.provider)));
            }
            ProviderProbeStatus::Error => {
                let message = result
                    .error_message
                    .as_deref()
                    .unwrap_or("provider probe failed");
                let detail = result
                    .diagnostic_detail
                    .clone()
                    .unwrap_or_else(|| format!("{}: {message}", result.provider));
                failures.push(ProviderFailure {
                    provider:     result.provider.to_string(),
                    summary_line: short_error_line(message),
                });
                details.push(CheckDetail::new(detail));
            }
        }
    }

    if failures.is_empty() {
        return CheckResult {
            name: "LLM Providers".to_string(),
            status: CheckStatus::Pass,
            summary: format!("{} configured", report.summary.total),
            details,
            remediation: None,
        };
    }

    let summary = if failures.len() == 1 {
        format!("{} failed", failures[0].provider)
    } else {
        format!("{} providers failed", failures.len())
    };
    let remediation = failures
        .iter()
        .map(|f| format!("{}: {}", f.provider, f.summary_line))
        .collect::<Vec<_>>()
        .join("; ");

    CheckResult {
        name: "LLM Providers".to_string(),
        status: CheckStatus::Error,
        summary,
        details,
        remediation: Some(remediation),
    }
}

struct ProviderFailure {
    provider:     String,
    summary_line: String,
}

pub(crate) async fn test_llm_providers(state: &AppState) -> anyhow::Result<ProviderProbeReport> {
    // `configured_providers` already iterates the catalog in order and includes
    // every provider with credential material on disk. Auth and registration
    // issues only arise for those providers, so this list is the complete
    // population to probe.
    let configured_providers = state.configured_llm_provider_ids().await;
    let result = state.resolve_llm_client().await?;
    let catalog = state.catalog();
    let client = Arc::new(result.client);

    let probe_results = join_all(configured_providers.into_iter().map(|provider| {
        let client = Arc::clone(&client);
        let catalog = Arc::clone(&catalog);
        let auth_issue = result
            .auth_issues
            .iter()
            .find(|(issue_provider, _)| issue_provider == &provider)
            .map(|(_, issue)| redact_string(&auth_issue_message(&provider, issue)));
        let registration_issue = result
            .registration_issues
            .iter()
            .find(|issue| issue.provider == provider)
            .map(|issue| redact_string(&issue.error.to_string()));
        async move {
            probe_single_provider(client, &catalog, provider, auth_issue, registration_issue).await
        }
    }))
    .await;

    Ok(provider_probe_report(probe_results))
}

async fn probe_single_provider(
    client: Arc<LlmClient>,
    catalog: &Catalog,
    provider: ProviderId,
    auth_issue: Option<String>,
    registration_issue: Option<String>,
) -> ProviderProbeResult {
    if let Some(message) = auth_issue {
        // `auth_issue_message` already embeds the provider's display name, so the
        // diagnostics detail uses the message as-is rather than re-prefixing.
        return provider_probe_error(provider, None, message.clone(), Some(message));
    }
    if let Some(message) = registration_issue {
        return provider_probe_error(provider, None, message, None);
    }

    let Some(model) = catalog.probe_for_provider(&provider) else {
        return provider_probe_error(
            provider,
            None,
            "no probe model configured for provider".to_string(),
            None,
        );
    };
    let model_id = model.id.clone();

    let outcome = run_basic_model_probe(&model_id, &provider, client).await;
    match outcome.status {
        ModelTestStatus::Ok => ProviderProbeResult {
            provider,
            model_id: Some(model_id),
            status: ProviderProbeStatus::Ok,
            error_message: None,
            diagnostic_detail: None,
        },
        ModelTestStatus::Error => {
            let raw = outcome
                .error_message
                .unwrap_or_else(|| "provider probe failed".to_string());
            provider_probe_error(provider, Some(model_id), redact_string(&raw), None)
        }
    }
}

fn provider_probe_error(
    provider: ProviderId,
    model_id: Option<String>,
    error_message: String,
    diagnostic_detail: Option<String>,
) -> ProviderProbeResult {
    ProviderProbeResult {
        provider,
        model_id,
        status: ProviderProbeStatus::Error,
        error_message: Some(error_message),
        diagnostic_detail,
    }
}

fn provider_probe_report(data: Vec<ProviderProbeResult>) -> ProviderProbeReport {
    let total = u32::try_from(data.len()).unwrap_or(u32::MAX);
    let passed = u32::try_from(
        data.iter()
            .filter(|result| result.status == ProviderProbeStatus::Ok)
            .count(),
    )
    .unwrap_or(u32::MAX);
    let failed = total.saturating_sub(passed);
    let status = if total > 0 && failed == 0 {
        ProviderProbeStatus::Ok
    } else {
        ProviderProbeStatus::Error
    };

    ProviderProbeReport {
        data,
        summary: ProviderProbeSummary {
            status,
            total,
            passed,
            failed,
        },
    }
}

const MAX_SHORT_LEN: usize = 120;

fn short_error_line(rendered: &str) -> String {
    let first = rendered
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("error");
    if first.chars().count() > MAX_SHORT_LEN {
        let cutoff: String = first.chars().take(MAX_SHORT_LEN).collect();
        format!("{cutoff}...")
    } else {
        first.to_string()
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "known leak: GitHub App id/slug passes unresolved; strict resolution scheduled in the \
              interpolation unification (Phase 2)"
)]
async fn check_github_app(state: &AppState) -> CheckResult {
    let settings = state.server_settings();
    if settings.server.integrations.github.strategy == GithubIntegrationStrategy::Token {
        let token = match state.github_credentials(&settings.server.integrations.github) {
            Ok(Some(fabro_github::GitHubCredentials::Pat(token))) => token,
            Ok(Some(fabro_github::GitHubCredentials::Installation(token))) => {
                match token.valid_token() {
                    Ok(token) => token.to_string(),
                    Err(err) => {
                        return CheckResult {
                            name:        "GitHub Token".to_string(),
                            status:      CheckStatus::Error,
                            summary:     "token expired".to_string(),
                            details:     vec![CheckDetail::new(err.to_string())],
                            remediation: Some(
                                "Run fabro install or run `fabro secret set GITHUB_TOKEN`"
                                    .to_string(),
                            ),
                        };
                    }
                }
            }
            Ok(Some(_)) => unreachable!("token strategy should not return app credentials"),
            Ok(None) => {
                return CheckResult {
                    name:        "GitHub Token".to_string(),
                    status:      CheckStatus::Warning,
                    summary:     "not configured".to_string(),
                    details:     Vec::new(),
                    remediation: Some(
                        "Run fabro install or run `fabro secret set GITHUB_TOKEN`".to_string(),
                    ),
                };
            }
            Err(err) => {
                return CheckResult {
                    name:        "GitHub Token".to_string(),
                    status:      CheckStatus::Error,
                    summary:     "missing token".to_string(),
                    details:     vec![CheckDetail::new(err.clone())],
                    remediation: Some(err),
                };
            }
        };

        let http = match http_client_or_check("GitHub Token", CheckStatus::Error) {
            Ok(http) => http,
            Err(result) => return result,
        };
        let probe = timeout(
            Duration::from_secs(15),
            http.get(format!("{}/user", fabro_github::github_api_base_url()))
                .header("Authorization", format!("Bearer {token}"))
                .header("Accept", "application/vnd.github+json")
                .header("User-Agent", "fabro-server")
                .send(),
        )
        .await;

        return match probe {
            Ok(Ok(response)) if response.status().is_success() => CheckResult {
                name:        "GitHub Token".to_string(),
                status:      CheckStatus::Pass,
                summary:     "configured".to_string(),
                details:     Vec::new(),
                remediation: None,
            },
            Ok(Ok(response)) if response.status() == fabro_http::StatusCode::UNAUTHORIZED => {
                CheckResult {
                    name:        "GitHub Token".to_string(),
                    status:      CheckStatus::Error,
                    summary:     "token invalid".to_string(),
                    details:     vec![CheckDetail::new(format!(
                        "GitHub returned {}",
                        response.status()
                    ))],
                    remediation: Some(
                        "Run fabro install or run `fabro secret set GITHUB_TOKEN`".to_string(),
                    ),
                }
            }
            Ok(Ok(response)) => CheckResult {
                name:        "GitHub Token".to_string(),
                status:      CheckStatus::Error,
                summary:     "connectivity error".to_string(),
                details:     vec![CheckDetail::new(format!(
                    "GitHub returned {}",
                    response.status()
                ))],
                remediation: Some(
                    "Check GitHub connectivity and the vault GITHUB_TOKEN".to_string(),
                ),
            },
            Ok(Err(err)) => CheckResult {
                name:        "GitHub Token".to_string(),
                status:      CheckStatus::Error,
                summary:     "connectivity error".to_string(),
                details:     vec![CheckDetail::new(err.to_string())],
                remediation: Some(
                    "Check GitHub connectivity and the vault GITHUB_TOKEN".to_string(),
                ),
            },
            Err(_) => CheckResult {
                name:        "GitHub Token".to_string(),
                status:      CheckStatus::Error,
                summary:     "timeout".to_string(),
                details:     vec![CheckDetail::new("GitHub probe timed out".to_string())],
                remediation: Some(
                    "Check GitHub connectivity and the vault GITHUB_TOKEN".to_string(),
                ),
            },
        };
    }

    let app_id = settings
        .server
        .integrations
        .github
        .app_id
        .as_ref()
        .map(InterpString::as_source);
    let slug = settings
        .server
        .integrations
        .github
        .slug
        .as_ref()
        .map(InterpString::as_source);
    let private_key_raw = state.vault_secret(EnvVars::GITHUB_APP_PRIVATE_KEY);
    let client_id = settings.server.integrations.github.client_id.is_some();
    let client_secret = state
        .vault_secret(EnvVars::GITHUB_APP_CLIENT_SECRET)
        .is_some();
    let webhook_secret = state
        .vault_secret(EnvVars::GITHUB_APP_WEBHOOK_SECRET)
        .is_some();

    if app_id.is_none()
        && private_key_raw.is_none()
        && !client_id
        && !client_secret
        && !webhook_secret
    {
        return CheckResult {
            name:        "GitHub App".to_string(),
            status:      CheckStatus::Warning,
            summary:     "not configured".to_string(),
            details:     Vec::new(),
            remediation: Some("Configure GitHub App settings and secrets".to_string()),
        };
    }

    let Some(app_id) = app_id else {
        return CheckResult {
            name:        "GitHub App".to_string(),
            status:      CheckStatus::Error,
            summary:     "missing app_id".to_string(),
            details:     Vec::new(),
            remediation: Some(
                "Set [server.integrations.github].app_id in settings.toml".to_string(),
            ),
        };
    };
    let Some(private_key_raw) = private_key_raw else {
        return CheckResult {
            name:        "GitHub App".to_string(),
            status:      CheckStatus::Error,
            summary:     "missing private key".to_string(),
            details:     Vec::new(),
            remediation: Some("Run `fabro secret set GITHUB_APP_PRIVATE_KEY`".to_string()),
        };
    };

    let private_key = match decode_pem_value(EnvVars::GITHUB_APP_PRIVATE_KEY, &private_key_raw) {
        Ok(value) => value,
        Err(err) => {
            return CheckResult {
                name:        "GitHub App".to_string(),
                status:      CheckStatus::Error,
                summary:     "private key invalid".to_string(),
                details:     vec![CheckDetail::new(err.clone())],
                remediation: Some(err),
            };
        }
    };

    let jwt = match fabro_github::sign_app_jwt(&app_id, &private_key) {
        Ok(jwt) => jwt,
        Err(err) => {
            return CheckResult {
                name:        "GitHub App".to_string(),
                status:      CheckStatus::Error,
                summary:     "JWT signing failed".to_string(),
                details:     vec![CheckDetail::new(format!("{err:#}"))],
                remediation: Some(err.to_string()),
            };
        }
    };

    let http = match http_client_or_check("GitHub App", CheckStatus::Error) {
        Ok(http) => http,
        Err(result) => return result,
    };
    let auth_result = timeout(
        Duration::from_secs(15),
        fabro_github::get_authenticated_app(&http, &jwt, &fabro_github::github_api_base_url()),
    )
    .await;
    match auth_result {
        Ok(Ok(_app)) => CheckResult {
            name:        "GitHub App".to_string(),
            status:      CheckStatus::Pass,
            summary:     slug.unwrap_or_else(|| "configured".to_string()),
            details:     Vec::new(),
            remediation: None,
        },
        Ok(Err(err)) => CheckResult {
            name:        "GitHub App".to_string(),
            status:      CheckStatus::Error,
            summary:     "connectivity error".to_string(),
            details:     vec![CheckDetail::new(format!("{err:#}"))],
            remediation: Some("Check GitHub App credentials and network connectivity".to_string()),
        },
        Err(_) => CheckResult {
            name:        "GitHub App".to_string(),
            status:      CheckStatus::Error,
            summary:     "timeout".to_string(),
            details:     vec![CheckDetail::new("GitHub probe timed out".to_string())],
            remediation: Some("Check GitHub connectivity and credentials".to_string()),
        },
    }
}

async fn check_sandbox(state: &AppState) -> CheckResult {
    let Some(api_key) = state.vault_secret(EnvVars::DAYTONA_API_KEY) else {
        return CheckResult {
            name:        "Sandbox".to_string(),
            status:      CheckStatus::Warning,
            summary:     "recommended, not configured".to_string(),
            details:     Vec::new(),
            remediation: Some(
                "Run `fabro secret set DAYTONA_API_KEY` to enable cloud sandbox execution"
                    .to_string(),
            ),
        };
    };

    match state.check_daytona_api_key(api_key).await {
        Ok(check) if check.ok() => CheckResult {
            name:        "Sandbox".to_string(),
            status:      CheckStatus::Pass,
            summary:     format!("Daytona configured ({})", check.key_name),
            details:     Vec::new(),
            remediation: None,
        },
        Ok(check) => CheckResult {
            name:        "Sandbox".to_string(),
            status:      CheckStatus::Error,
            summary:     "Daytona API key is missing required scopes".to_string(),
            details:     vec![CheckDetail::new(format!(
                "missing: {}",
                check.missing_display()
            ))],
            remediation: Some(format!(
                "Regenerate the Daytona API key with scopes: {}, then \
                 `fabro secret set DAYTONA_API_KEY`.",
                daytona::required_perms_display()
            )),
        },
        Err(err) => CheckResult {
            name:        "Sandbox".to_string(),
            status:      CheckStatus::Error,
            summary:     "Daytona credential rejected".to_string(),
            details:     vec![CheckDetail::new(format!("{err:#}"))],
            remediation: Some("Verify DAYTONA_API_KEY value and Daytona reachability".to_string()),
        },
    }
}

fn check_storage_dir(state: &AppState) -> CheckResult {
    check_storage_dir_path(&state.server_storage_dir())
}

#[expect(
    clippy::disallowed_methods,
    reason = "Server diagnostics deliberately performs a synchronous local filesystem probe."
)]
fn check_storage_dir_path(path: &std::path::Path) -> CheckResult {
    let exists = path.is_dir();
    let readable = exists && std::fs::read_dir(path).is_ok();
    let writable = exists && tempfile::tempfile_in(path).is_ok();
    let details = vec![
        CheckDetail::new(format!("Exists: {}", if exists { "yes" } else { "no" })),
        CheckDetail::new(format!("Readable: {}", if readable { "yes" } else { "no" })),
        CheckDetail::new(format!("Writable: {}", if writable { "yes" } else { "no" })),
    ];
    let is_healthy = exists && readable && writable;
    let display = path.display();

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
        } else if !exists {
            Some(format!("Create the directory: mkdir -p {display}"))
        } else {
            Some(format!("Fix permissions on {display}"))
        },
    }
}

async fn check_brave_search(state: &AppState) -> CheckResult {
    let Some(api_key) = state.vault_secret(EnvVars::BRAVE_SEARCH_API_KEY) else {
        return CheckResult {
            name:        "Web Search (Brave)".to_string(),
            status:      CheckStatus::Warning,
            summary:     "optional, not configured".to_string(),
            details:     Vec::new(),
            remediation: Some(
                "Run `fabro secret set BRAVE_SEARCH_API_KEY` to enable web search".to_string(),
            ),
        };
    };

    let http = match http_client_or_check("Web Search (Brave)", CheckStatus::Warning) {
        Ok(http) => http,
        Err(result) => return result,
    };

    let probe = timeout(Duration::from_secs(15), async move {
        http.get("https://api.search.brave.com/res/v1/web/search?q=test&count=1")
            .header("X-Subscription-Token", api_key)
            .send()
            .await
            .map_err(anyhow::Error::new)
    })
    .await;

    match probe {
        Ok(Ok(response)) if response.status().is_success() => CheckResult {
            name:        "Web Search (Brave)".to_string(),
            status:      CheckStatus::Pass,
            summary:     "configured and reachable".to_string(),
            details:     Vec::new(),
            remediation: None,
        },
        Ok(Ok(response)) => CheckResult {
            name:        "Web Search (Brave)".to_string(),
            status:      CheckStatus::Warning,
            summary:     format!("HTTP {}", response.status()),
            details:     Vec::new(),
            remediation: Some("Check BRAVE_SEARCH_API_KEY and network connectivity".to_string()),
        },
        Ok(Err(err)) => CheckResult {
            name:        "Web Search (Brave)".to_string(),
            status:      CheckStatus::Warning,
            summary:     "connectivity error".to_string(),
            details:     vec![CheckDetail::new(format!("{err:#}"))],
            remediation: Some("Check BRAVE_SEARCH_API_KEY and network connectivity".to_string()),
        },
        Err(_) => CheckResult {
            name:        "Web Search (Brave)".to_string(),
            status:      CheckStatus::Warning,
            summary:     "timeout".to_string(),
            details:     vec![CheckDetail::new(
                "Web Search (Brave) probe timed out".to_string(),
            )],
            remediation: Some("Check BRAVE_SEARCH_API_KEY and network connectivity".to_string()),
        },
    }
}

fn check_crypto(state: &AppState) -> CheckResult {
    let resolved_server_settings = state.server_settings();

    let mut details = Vec::new();
    let mut errors = Vec::new();

    if resolved_server_settings.server.web.enabled {
        match state.server_secret(EnvVars::SESSION_SECRET) {
            Some(secret) => {
                if let Err(err) = validate_session_secret(&secret) {
                    errors.push(err);
                }
            }
            None => errors.push("SESSION_SECRET not set".to_string()),
        }
    }

    let methods = &resolved_server_settings.server.auth.methods;
    if methods.contains(&ServerAuthMethod::DevToken) {
        match state.server_secret(EnvVars::FABRO_DEV_TOKEN) {
            Some(token) if validate_dev_token_format(&token) => {}
            Some(_) => errors.push("FABRO_DEV_TOKEN has invalid format".to_string()),
            None => errors.push("FABRO_DEV_TOKEN not set".to_string()),
        }
    }
    if methods.contains(&ServerAuthMethod::Github) {
        if resolved_server_settings
            .server
            .integrations
            .github
            .client_id
            .is_none()
        {
            errors.push("server.integrations.github.client_id is not configured".to_string());
        }
        if state
            .vault_secret(EnvVars::GITHUB_APP_CLIENT_SECRET)
            .is_none()
        {
            errors.push("GITHUB_APP_CLIENT_SECRET not configured in vault".to_string());
        }
    }

    if errors.is_empty() {
        CheckResult {
            name: "Crypto".to_string(),
            status: CheckStatus::Pass,
            summary: "all configured auth material valid".to_string(),
            details,
            remediation: None,
        }
    } else {
        for err in &errors {
            details.push(CheckDetail::new(err.clone()));
        }
        CheckResult {
            name: "Crypto".to_string(),
            status: CheckStatus::Error,
            summary: "invalid keys found".to_string(),
            details,
            remediation: Some(errors.join("; ")),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use fabro_config::RunLayer;
    use fabro_vault::SecretType;
    use httpmock::Method::POST;
    use httpmock::MockServer;
    use serde_json::json;

    use super::*;
    use crate::test_support::{TestAppStateBuilder, default_test_server_settings};

    #[test]
    fn short_error_line_returns_fallback_for_empty_input() {
        assert_eq!(short_error_line(""), "error");
    }

    #[test]
    fn short_error_line_returns_first_non_empty_trimmed_line() {
        let input = "   \n\t\n  first line  \nsecond line";
        assert_eq!(short_error_line(input), "first line");
    }

    #[test]
    fn short_error_line_truncates_long_input_with_ascii_ellipsis() {
        let input = "a".repeat(MAX_SHORT_LEN + 50);
        let result = short_error_line(&input);
        let expected = format!("{}...", "a".repeat(MAX_SHORT_LEN));
        assert_eq!(result, expected);
    }

    #[tokio::test]
    async fn check_llm_providers_reports_error_with_typed_remediation_on_probe_failure() {
        let server = MockServer::start_async().await;
        let _mock = server
            .mock_async(|when, then| {
                when.method(POST).path("/v1/responses");
                then.status(401)
                    .header("content-type", "application/json")
                    .json_body(json!({
                        "error": {
                            "message": "invalid api key",
                            "type": "invalid_request_error"
                        }
                    }));
            })
            .await;
        let state = TestAppStateBuilder::new()
            .runtime_settings(default_test_server_settings(), RunLayer::default())
            .max_concurrent_runs(5)
            .provider_base_url("openai", server.url("/v1"))
            .build();
        state
            .vault
            .write()
            .await
            .set(
                "OPENAI_API_KEY",
                "vault-openai-key",
                SecretType::Token,
                None,
            )
            .unwrap();

        let result = check_llm_providers(&state).await;

        assert_eq!(result.status, CheckStatus::Error);
        assert_eq!(result.summary, "openai failed");
        let remediation = result.remediation.expect("remediation set on failure");
        assert!(
            remediation.starts_with("openai: "),
            "expected remediation to start with provider name, got: {remediation}"
        );
        assert!(
            remediation.contains("Authentication"),
            "expected typed Display 'Authentication' in remediation, got: {remediation}"
        );
        assert!(!result.details.is_empty(), "details should be populated");
        assert!(
            result
                .details
                .iter()
                .any(|d| d.text.starts_with("openai: ")),
            "expected a detail line prefixed with 'openai: ', got: {:?}",
            result.details
        );
    }

    #[tokio::test]
    async fn check_llm_providers_reports_none_configured_after_shared_probe() {
        let state = TestAppStateBuilder::new().build();

        let result = check_llm_providers(&state).await;

        assert_eq!(result.status, CheckStatus::Error);
        assert_eq!(result.summary, "none configured");
        assert!(result.details.is_empty());
        assert_eq!(
            result.remediation.as_deref(),
            Some("Set at least one provider API key")
        );
    }

    #[tokio::test]
    async fn check_llm_providers_preserves_pass_summary_after_shared_probe() {
        let server = MockServer::start_async().await;
        let _mock = server
            .mock_async(|when, then| {
                when.method(POST).path("/v1/responses");
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(json!({
                        "id": "resp_1",
                        "model": "gpt-5.4-mini",
                        "output": [
                            {
                                "type": "message",
                                "role": "assistant",
                                "content": [
                                    {
                                        "type": "output_text",
                                        "text": "OK"
                                    }
                                ]
                            }
                        ],
                        "status": "completed"
                    }));
            })
            .await;
        let state = TestAppStateBuilder::new()
            .provider_base_url("openai", server.url("/v1"))
            .build();
        state
            .vault
            .write()
            .await
            .set(
                "OPENAI_API_KEY",
                "vault-openai-key",
                SecretType::Token,
                None,
            )
            .unwrap();

        let result = check_llm_providers(&state).await;

        assert_eq!(result.status, CheckStatus::Pass);
        assert_eq!(result.summary, "1 configured");
        assert_eq!(result.remediation, None);
        assert!(
            result
                .details
                .iter()
                .any(|detail| detail.text == "openai: OK"),
            "expected openai OK detail, got: {:?}",
            result.details
        );
    }

    #[tokio::test]
    async fn check_sandbox_ignores_env_backed_daytona_api_key() {
        let state = TestAppStateBuilder::new()
            .env_lookup(|name| {
                (name == EnvVars::DAYTONA_API_KEY).then(|| "dtn_from_env".to_string())
            })
            .build();

        let result = check_sandbox(&state).await;

        assert_eq!(result.status, CheckStatus::Warning);
        assert_eq!(result.summary, "recommended, not configured");
        assert_eq!(
            result.remediation.as_deref(),
            Some("Run `fabro secret set DAYTONA_API_KEY` to enable cloud sandbox execution")
        );
    }

    #[tokio::test]
    async fn check_brave_search_ignores_env_backed_api_key() {
        let state = TestAppStateBuilder::new()
            .env_lookup(|name| {
                (name == EnvVars::BRAVE_SEARCH_API_KEY).then(|| "brave-from-env".to_string())
            })
            .build();

        let result = check_brave_search(&state).await;

        assert_eq!(result.status, CheckStatus::Warning);
        assert_eq!(result.summary, "optional, not configured");
        assert_eq!(
            result.remediation.as_deref(),
            Some("Run `fabro secret set BRAVE_SEARCH_API_KEY` to enable web search")
        );
    }

    #[test]
    fn check_crypto_requires_github_client_secret_from_vault() {
        let settings = fabro_config::ServerSettingsBuilder::from_toml(
            r#"
_version = 1

[server.auth]
methods = ["github"]

[server.auth.github]
allowed_usernames = ["octocat"]

[server.integrations.github]
client_id = "Iv1.test"
"#,
        )
        .expect("github settings should parse");
        let state = TestAppStateBuilder::new()
            .runtime_settings(settings, RunLayer::default())
            .server_secret_env(HashMap::from([(
                EnvVars::GITHUB_APP_CLIENT_SECRET.to_string(),
                "server-env-client-secret".to_string(),
            )]))
            .build();

        let result = check_crypto(&state);

        assert_eq!(result.status, CheckStatus::Error);
        assert!(result.details.iter().any(|detail| {
            detail
                .text
                .contains("GITHUB_APP_CLIENT_SECRET not configured in vault")
        }));
    }

    #[test]
    fn check_storage_dir_path_passes_for_readable_writable_directory() {
        let dir = tempfile::tempdir().unwrap();

        let result = check_storage_dir_path(dir.path());

        assert_eq!(result.name, "Storage directory");
        assert_eq!(result.status, CheckStatus::Pass);
        assert_eq!(result.summary, dir.path().display().to_string());
        assert!(result.remediation.is_none());
    }

    #[test]
    fn check_storage_dir_path_errors_for_missing_directory() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");

        let result = check_storage_dir_path(&missing);

        assert_eq!(result.name, "Storage directory");
        assert_eq!(result.status, CheckStatus::Error);
        assert_eq!(result.summary, missing.display().to_string());
        assert_eq!(
            result.remediation,
            Some(format!(
                "Create the directory: mkdir -p {}",
                missing.display()
            ))
        );
    }
}
