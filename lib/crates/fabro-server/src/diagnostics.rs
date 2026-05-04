use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use fabro_auth::auth_issue_message;
use fabro_llm::client::Client as LlmClient;
use fabro_llm::types::{Message, Request};
use fabro_model::{Catalog, Provider};
use fabro_sandbox::daytona;
use fabro_static::EnvVars;
use fabro_types::settings::server::GithubIntegrationStrategy;
use fabro_types::settings::{InterpString, ServerAuthMethod};
use fabro_util::check_report::{CheckDetail, CheckResult, CheckSection, CheckStatus};
use fabro_util::dev_token::validate_dev_token_format;
use fabro_util::session_secret;
use fabro_util::version::FABRO_VERSION;
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
    let result = match state.resolve_llm_client().await {
        Ok(result) => result,
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
    if result.client.provider_names().is_empty() && result.auth_issues.is_empty() {
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
    for (provider, issue) in &result.auth_issues {
        let message = auth_issue_message(*provider, issue);
        failures.push(ProviderFailure {
            name:  provider.to_string(),
            short: short_error_line(&message),
        });
        details.push(CheckDetail::new(message));
    }
    for provider_name in result.client.provider_names() {
        let Ok(provider) = provider_name.parse::<Provider>() else {
            continue;
        };
        let probe_result = timeout(
            Duration::from_secs(30),
            probe_llm_provider(&result.client, provider),
        )
        .await;
        match probe_result {
            Ok(Ok(())) => details.push(CheckDetail::new(format!("{provider}: OK"))),
            Ok(Err(err)) => {
                let rendered = render_error_chain(&err);
                failures.push(ProviderFailure {
                    name:  provider.to_string(),
                    short: short_error_line(&rendered),
                });
                details.push(CheckDetail::new(format!("{provider}: {rendered}")));
            }
            Err(_) => {
                failures.push(ProviderFailure {
                    name:  provider.to_string(),
                    short: "timeout (30s)".to_string(),
                });
                details.push(CheckDetail::new(format!("{provider}: timeout (30s)")));
            }
        }
    }

    if failures.is_empty() {
        return CheckResult {
            name: "LLM Providers".to_string(),
            status: CheckStatus::Pass,
            summary: format!("{} configured", result.client.provider_names().len()),
            details,
            remediation: None,
        };
    }

    let summary = if failures.len() == 1 {
        format!("{} failed", failures[0].name)
    } else {
        format!("{} providers failed", failures.len())
    };
    let remediation = failures
        .iter()
        .map(|f| format!("{}: {}", f.name, f.short))
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
    name:  String,
    short: String,
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
        format!("{cutoff}…")
    } else {
        first.to_string()
    }
}

fn render_error_chain(err: &dyn std::error::Error) -> String {
    let mut out = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        out.push_str(": ");
        out.push_str(&cause.to_string());
        source = cause.source();
    }
    out
}

fn probe_model(provider: Provider) -> String {
    Catalog::builtin()
        .probe_for_provider(provider)
        .map_or_else(|| format!("unknown-{provider}"), |m| m.id.clone())
}

async fn probe_llm_provider(client: &LlmClient, provider: Provider) -> fabro_llm::Result<()> {
    let request = Request {
        model:            probe_model(provider),
        messages:         vec![Message::user("hi")],
        provider:         Some(provider.to_string()),
        tools:            None,
        tool_choice:      None,
        response_format:  None,
        temperature:      None,
        top_p:            None,
        max_tokens:       Some(16),
        stop_sequences:   None,
        reasoning_effort: None,
        speed:            None,
        metadata:         None,
        provider_options: None,
    };
    client.complete(&request).await.map(|_| ())
}

async fn check_github_app(state: &AppState) -> CheckResult {
    let settings = state.server_settings();
    if settings.server.integrations.github.strategy == GithubIntegrationStrategy::Token {
        let token = match state.github_credentials(&settings.server.integrations.github) {
            Ok(Some(fabro_github::GitHubCredentials::Token(token))) => token,
            Ok(Some(_)) => unreachable!("token strategy should not return app credentials"),
            Ok(None) => {
                return CheckResult {
                    name:        "GitHub Token".to_string(),
                    status:      CheckStatus::Warning,
                    summary:     "not configured".to_string(),
                    details:     Vec::new(),
                    remediation: Some("Run fabro install or set GITHUB_TOKEN".to_string()),
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
                    remediation: Some("Run fabro install or update GITHUB_TOKEN".to_string()),
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
                remediation: Some("Check GitHub connectivity and GITHUB_TOKEN".to_string()),
            },
            Ok(Err(err)) => CheckResult {
                name:        "GitHub Token".to_string(),
                status:      CheckStatus::Error,
                summary:     "connectivity error".to_string(),
                details:     vec![CheckDetail::new(err.to_string())],
                remediation: Some("Check GitHub connectivity and GITHUB_TOKEN".to_string()),
            },
            Err(_) => CheckResult {
                name:        "GitHub Token".to_string(),
                status:      CheckStatus::Error,
                summary:     "timeout".to_string(),
                details:     vec![CheckDetail::new("GitHub probe timed out".to_string())],
                remediation: Some("Check GitHub connectivity and GITHUB_TOKEN".to_string()),
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
    let private_key_raw = state.server_secret(EnvVars::GITHUB_APP_PRIVATE_KEY);
    let client_id = settings.server.integrations.github.client_id.is_some();
    let client_secret = state
        .server_secret(EnvVars::GITHUB_APP_CLIENT_SECRET)
        .is_some();
    let webhook_secret = state
        .server_secret(EnvVars::GITHUB_APP_WEBHOOK_SECRET)
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
            remediation: Some("Set GITHUB_APP_PRIVATE_KEY".to_string()),
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
    let Some(api_key) = state.vault_or_env(EnvVars::DAYTONA_API_KEY) else {
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
    let Some(api_key) = state.vault_or_env(EnvVars::BRAVE_SEARCH_API_KEY) else {
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
            .server_secret(EnvVars::GITHUB_APP_CLIENT_SECRET)
            .is_none()
        {
            errors.push("GITHUB_APP_CLIENT_SECRET not set".to_string());
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
    use fabro_auth::{AuthCredential, AuthDetails};
    use fabro_config::RunLayer;
    use fabro_vault::SecretType;
    use httpmock::Method::POST;
    use httpmock::MockServer;
    use serde_json::json;

    use super::*;
    use crate::test_support::{default_test_server_settings, test_app_state_with_env_lookup};

    #[test]
    fn short_error_line_returns_fallback_for_empty_input() {
        assert_eq!(short_error_line(""), "error");
    }

    #[test]
    fn short_error_line_skips_whitespace_only_first_line() {
        let input = "   \n\t\nactual message";
        assert_eq!(short_error_line(input), "actual message");
    }

    #[test]
    fn short_error_line_truncates_long_input_with_ellipsis() {
        let input = "a".repeat(MAX_SHORT_LEN + 50);
        let result = short_error_line(&input);
        let expected = format!("{}…", "a".repeat(MAX_SHORT_LEN));
        assert_eq!(result, expected);
    }

    #[test]
    fn short_error_line_keeps_input_at_exact_max_length() {
        let input = "a".repeat(MAX_SHORT_LEN);
        assert_eq!(short_error_line(&input), input);
    }

    #[test]
    fn short_error_line_truncates_one_char_past_max_length() {
        let input = "a".repeat(MAX_SHORT_LEN + 1);
        let expected = format!("{}…", "a".repeat(MAX_SHORT_LEN));
        assert_eq!(short_error_line(&input), expected);
    }

    #[test]
    fn render_error_chain_includes_underlying_cause_for_typed_llm_error() {
        let inner = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "tcp connect");
        let err = fabro_llm::Error::network("openai request failed", inner);

        let rendered = render_error_chain(&err);

        assert!(
            rendered.contains("openai request failed"),
            "top-level message missing: {rendered}"
        );
        assert!(
            rendered.contains("tcp connect"),
            "underlying cause missing — chain not walked: {rendered}"
        );
    }

    #[test]
    fn short_error_line_returns_first_non_empty_trimmed_line() {
        let input = "  first line  \nsecond line";
        assert_eq!(short_error_line(input), "first line");
    }

    #[test]
    fn short_error_line_returns_short_input_unchanged() {
        assert_eq!(short_error_line("connection refused"), "connection refused");
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
        let base_url = server.url("/v1");
        let state = test_app_state_with_env_lookup(
            default_test_server_settings(),
            RunLayer::default(),
            5,
            move |name| match name {
                "OPENAI_BASE_URL" => Some(base_url.clone()),
                _ => None,
            },
        );
        let credential = AuthCredential {
            provider: Provider::OpenAi,
            details:  AuthDetails::ApiKey {
                key: "vault-openai-key".to_string(),
            },
        };
        state
            .vault
            .write()
            .await
            .set(
                "openai_codex",
                &serde_json::to_string(&credential).unwrap(),
                SecretType::Credential,
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
