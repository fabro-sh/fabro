use std::path::PathBuf;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use fabro_llm::client::Client as LlmClient;
use fabro_llm::types::{Message, Request};
use fabro_model::{Catalog, Provider};
use fabro_types::settings::server::GithubIntegrationStrategy;
use fabro_types::settings::{InterpString, ServerAuthMethod};
use fabro_util::check_report::{CheckDetail, CheckResult, CheckSection, CheckStatus};
use fabro_util::dev_token::validate_dev_token_format;
use fabro_util::session_secret;
use fabro_util::version::FABRO_VERSION;
use serde::Serialize;
use tokio::time::timeout;

use crate::server::AppState;
use crate::server_secrets::auth_issue_message;

fn http_client_or_check(
    name: &str,
    status: CheckStatus,
) -> Result<fabro_http::HttpClient, CheckResult> {
    fabro_http::http_client().map_err(|err| CheckResult {
        name: name.to_string(),
        status,
        summary: "client error".to_string(),
        details: vec![CheckDetail::new(err.to_string())],
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

fn validate_tls_cert(pem: &str, now_epoch: i64) -> Result<String, String> {
    let mut reader = std::io::Cursor::new(pem.as_bytes());
    let certs: Vec<_> = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("failed to parse certificate PEM: {e}"))?;
    if certs.is_empty() {
        return Err("no certificates found in PEM".to_string());
    }
    let (_, parsed) = x509_parser::parse_x509_certificate(&certs[0])
        .map_err(|e| format!("failed to parse X.509 certificate: {e}"))?;
    let not_after = parsed.validity().not_after.timestamp();
    if not_after <= now_epoch {
        return Err("certificate has expired".to_string());
    }
    let cn = parsed
        .subject()
        .iter_common_name()
        .next()
        .and_then(|cn| cn.as_str().ok())
        .unwrap_or("(no CN)");
    Ok(format!("CN={cn}, valid"))
}

fn validate_tls_private_key(pem: &str) -> Result<(), String> {
    let mut reader = std::io::Cursor::new(pem.as_bytes());
    rustls_pemfile::private_key(&mut reader)
        .map_err(|e| format!("failed to parse private key PEM: {e}"))?
        .ok_or_else(|| "no private key found in PEM".to_string())?;
    Ok(())
}

fn validate_session_secret(value: &str) -> Result<(), String> {
    session_secret::validate_session_secret(value)
}

pub async fn run_all(state: &AppState) -> DiagnosticsReport {
    let (llm, github, brave) = tokio::join!(
        check_llm_providers(state),
        check_github_app(state),
        check_brave_search(state),
    );
    let sandbox = check_sandbox(state);
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
                checks: vec![crypto],
            },
        ],
    }
}

async fn check_llm_providers(state: &AppState) -> CheckResult {
    let result = match state.build_llm_client().await {
        Ok(result) => result,
        Err(err) => {
            return CheckResult {
                name:        "LLM Providers".to_string(),
                status:      CheckStatus::Error,
                summary:     "failed to initialize".to_string(),
                details:     vec![CheckDetail::new(err)],
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

    let mut details = Vec::new();
    let mut failed = Vec::new();
    for (provider, issue) in &result.auth_issues {
        failed.push(provider.to_string());
        details.push(CheckDetail::new(auth_issue_message(*provider, issue)));
    }
    for provider_name in result.client.provider_names() {
        let Ok(provider) = provider_name.parse::<Provider>() else {
            continue;
        };
        let result = timeout(
            Duration::from_secs(30),
            probe_llm_provider(&result.client, provider),
        )
        .await;
        match result {
            Ok(Ok(())) => details.push(CheckDetail::new(format!("{provider} connectivity: OK"))),
            Ok(Err(err)) => {
                failed.push(provider.to_string());
                details.push(CheckDetail::new(format!("{provider} connectivity: {err}")));
            }
            Err(_) => {
                failed.push(provider.to_string());
                details.push(CheckDetail::new(format!(
                    "{provider} connectivity: timeout (30s)"
                )));
            }
        }
    }

    if failed.is_empty() {
        CheckResult {
            name: "LLM Providers".to_string(),
            status: CheckStatus::Pass,
            summary: format!("{} configured", result.client.provider_names().len()),
            details,
            remediation: None,
        }
    } else {
        CheckResult {
            name: "LLM Providers".to_string(),
            status: CheckStatus::Warning,
            summary: "some providers require attention".to_string(),
            details,
            remediation: Some(format!("Connectivity issues with: {}", failed.join(", "))),
        }
    }
}

fn probe_model(provider: Provider) -> String {
    Catalog::builtin().probe_for_provider(provider).map_or_else(
        || format!("unknown-{}", provider.as_str()),
        |m| m.id.clone(),
    )
}

async fn probe_llm_provider(client: &LlmClient, provider: Provider) -> Result<(), String> {
    let request = Request {
        model:            probe_model(provider),
        messages:         vec![Message::user("hi")],
        provider:         Some(provider.as_str().to_string()),
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
    client
        .complete(&request)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

async fn check_github_app(state: &AppState) -> CheckResult {
    let settings = state.server_settings();
    if settings.integrations.github.strategy == GithubIntegrationStrategy::Token {
        let token = match state.github_credentials(&settings.integrations.github) {
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
        .integrations
        .github
        .app_id
        .as_ref()
        .map(InterpString::as_source);
    let slug = settings
        .integrations
        .github
        .slug
        .as_ref()
        .map(InterpString::as_source);
    let private_key_raw = state.server_secret("GITHUB_APP_PRIVATE_KEY");
    let client_id = settings.integrations.github.client_id.is_some();
    let client_secret = state.server_secret("GITHUB_APP_CLIENT_SECRET").is_some();
    let webhook_secret = state.server_secret("GITHUB_APP_WEBHOOK_SECRET").is_some();

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

    let private_key = match decode_pem_value("GITHUB_APP_PRIVATE_KEY", &private_key_raw) {
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
                details:     vec![CheckDetail::new(err.clone())],
                remediation: Some(err),
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
            details:     vec![CheckDetail::new(err.clone())],
            remediation: Some(err),
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

fn check_sandbox(state: &AppState) -> CheckResult {
    if state.vault_or_env("DAYTONA_API_KEY").is_some() {
        CheckResult {
            name:        "Sandbox".to_string(),
            status:      CheckStatus::Pass,
            summary:     "Daytona configured".to_string(),
            details:     Vec::new(),
            remediation: None,
        }
    } else {
        CheckResult {
            name:        "Sandbox".to_string(),
            status:      CheckStatus::Warning,
            summary:     "not configured".to_string(),
            details:     Vec::new(),
            remediation: Some("Set DAYTONA_API_KEY to enable cloud sandbox execution".to_string()),
        }
    }
}

async fn check_brave_search(state: &AppState) -> CheckResult {
    let Some(api_key) = state.vault_or_env("BRAVE_SEARCH_API_KEY") else {
        return CheckResult {
            name:        "Brave Search".to_string(),
            status:      CheckStatus::Warning,
            summary:     "not configured".to_string(),
            details:     Vec::new(),
            remediation: Some("Set BRAVE_SEARCH_API_KEY to enable web search".to_string()),
        };
    };

    let http = match http_client_or_check("Brave Search", CheckStatus::Warning) {
        Ok(http) => http,
        Err(result) => return result,
    };

    let probe = timeout(Duration::from_secs(15), async move {
        http.get("https://api.search.brave.com/res/v1/web/search?q=test&count=1")
            .header("X-Subscription-Token", api_key)
            .send()
            .await
            .map_err(|e| e.to_string())
    })
    .await;

    match probe {
        Ok(Ok(response)) if response.status().is_success() => CheckResult {
            name:        "Brave Search".to_string(),
            status:      CheckStatus::Pass,
            summary:     "configured and reachable".to_string(),
            details:     Vec::new(),
            remediation: None,
        },
        Ok(Ok(response)) => CheckResult {
            name:        "Brave Search".to_string(),
            status:      CheckStatus::Warning,
            summary:     format!("HTTP {}", response.status()),
            details:     Vec::new(),
            remediation: Some("Check BRAVE_SEARCH_API_KEY and network connectivity".to_string()),
        },
        Ok(Err(err)) => CheckResult {
            name:        "Brave Search".to_string(),
            status:      CheckStatus::Warning,
            summary:     "connectivity error".to_string(),
            details:     vec![CheckDetail::new(err.clone())],
            remediation: Some(err),
        },
        Err(_) => CheckResult {
            name:        "Brave Search".to_string(),
            status:      CheckStatus::Warning,
            summary:     "timeout".to_string(),
            details:     vec![CheckDetail::new("Brave Search probe timed out".to_string())],
            remediation: Some("Check BRAVE_SEARCH_API_KEY and network connectivity".to_string()),
        },
    }
}

fn check_crypto(state: &AppState) -> CheckResult {
    let settings_file = state
        .settings
        .read()
        .expect("settings lock poisoned")
        .clone();
    let resolved_server_settings = state.server_settings();

    let mut details = Vec::new();
    let mut errors = Vec::new();

    if resolved_server_settings.web.enabled {
        match state.server_secret("SESSION_SECRET") {
            Some(secret) => {
                if let Err(err) = validate_session_secret(&secret) {
                    errors.push(err);
                }
            }
            None => errors.push("SESSION_SECRET not set".to_string()),
        }
    }

    let methods = &resolved_server_settings.auth.methods;
    if methods.contains(&ServerAuthMethod::DevToken) {
        match state.server_secret("FABRO_DEV_TOKEN") {
            Some(token) if validate_dev_token_format(&token) => {}
            Some(_) => errors.push("FABRO_DEV_TOKEN has invalid format".to_string()),
            None => errors.push("FABRO_DEV_TOKEN not set".to_string()),
        }
    }
    if methods.contains(&ServerAuthMethod::Github) {
        if resolved_server_settings
            .integrations
            .github
            .client_id
            .is_none()
        {
            errors.push("server.integrations.github.client_id is not configured".to_string());
        }
        if state.server_secret("GITHUB_APP_CLIENT_SECRET").is_none() {
            errors.push("GITHUB_APP_CLIENT_SECRET not set".to_string());
        }
    }

    if let Some(raw) = state.server_secret("FABRO_JWT_PUBLIC_KEY") {
        if let Err(err) = decode_pem_value("FABRO_JWT_PUBLIC_KEY", &raw).and_then(|pem| {
            jsonwebtoken::DecodingKey::from_ed_pem(pem.as_bytes())
                .map(|_| ())
                .map_err(|e| format!("invalid JWT public key: {e}"))
        }) {
            errors.push(err);
        } else {
            details.push(CheckDetail::new("FABRO_JWT_PUBLIC_KEY valid".to_string()));
        }
    }

    if let Some(raw) = state.server_secret("FABRO_JWT_PRIVATE_KEY") {
        if let Err(err) = decode_pem_value("FABRO_JWT_PRIVATE_KEY", &raw).and_then(|pem| {
            jsonwebtoken::EncodingKey::from_ed_pem(pem.as_bytes())
                .map(|_| ())
                .map_err(|e| format!("invalid JWT private key: {e}"))
        }) {
            errors.push(err);
        } else {
            details.push(CheckDetail::new("FABRO_JWT_PRIVATE_KEY valid".to_string()));
        }
    }

    if let Some(listen) = settings_file
        .server
        .as_ref()
        .and_then(|s| s.listen.as_ref())
    {
        use fabro_types::settings::server::ServerListenLayer;

        if let ServerListenLayer::Tcp { tls: Some(tls), .. } = listen {
            let read = |raw: Option<String>, label: &str| -> Result<String, String> {
                let Some(path_str) = raw else {
                    return Err(format!("server.listen.tls.{label} is not configured"));
                };
                let path = PathBuf::from(&path_str);
                let expanded = fabro_config::expand_tilde(&path);
                std::fs::read_to_string(&expanded)
                    .map_err(|e| format!("{}: {e}", expanded.display()))
            };
            match (
                read(tls.cert.as_ref().map(InterpString::as_source), "cert"),
                read(tls.key.as_ref().map(InterpString::as_source), "key"),
            ) {
                (Ok(cert_pem), Ok(key_pem)) => {
                    if let Err(err) = validate_tls_cert(&cert_pem, chrono::Utc::now().timestamp()) {
                        errors.push(err);
                    }
                    if let Err(err) = validate_tls_private_key(&key_pem) {
                        errors.push(err);
                    }
                }
                _ => errors.push("failed to read TLS files".to_string()),
            }
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
