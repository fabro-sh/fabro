use std::path::PathBuf;
use std::process::Command;
use std::sync::LazyLock;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use fabro_llm::client::Client as LlmClient;
use fabro_llm::types::{Message, Request};
use fabro_model::{Catalog, Provider};
use fabro_util::check_report::{CheckDetail, CheckResult, CheckSection, CheckStatus};
use fabro_util::version::FABRO_VERSION;
use regex::Regex;
use semver::Version;
use serde::Serialize;
use tokio::time::timeout;

use crate::server::AppState;

#[derive(Debug, Serialize)]
pub struct DiagnosticsReport {
    pub version: String,
    pub sections: Vec<CheckSection>,
}

#[derive(Debug, Clone, PartialEq)]
enum ProbeOutcome {
    NotFound,
    Failed,
    Ok { version: Option<Version> },
}

static DOT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"graphviz version (\d+)\.(\d+)\.(\d+)").unwrap());

fn parse_version(re: &Regex, output: &str) -> Option<Version> {
    let caps = re.captures(output)?;
    Some(Version::new(
        caps[1].parse().ok()?,
        caps[2].parse().ok()?,
        caps[3].parse().ok()?,
    ))
}

fn probe_dot() -> ProbeOutcome {
    let result = Command::new("dot").arg("-V").output().ok();
    match result {
        None => ProbeOutcome::NotFound,
        Some(output) if !output.status.success() => ProbeOutcome::Failed,
        Some(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let version =
                parse_version(&DOT_RE, &stdout).or_else(|| parse_version(&DOT_RE, &stderr));
            ProbeOutcome::Ok { version }
        }
    }
}

fn check_dot() -> CheckResult {
    let outcome = probe_dot();
    let (status, summary, remediation) = match &outcome {
        ProbeOutcome::NotFound => (
            CheckStatus::Warning,
            "not installed".to_string(),
            Some("Install Graphviz to enable workflow graph rendering".to_string()),
        ),
        ProbeOutcome::Failed => (
            CheckStatus::Warning,
            "command failed".to_string(),
            Some("Check that `dot -V` succeeds on the server host".to_string()),
        ),
        ProbeOutcome::Ok {
            version: Some(version),
        } => (CheckStatus::Pass, format!("dot {version}"), None),
        ProbeOutcome::Ok { version: None } => {
            (CheckStatus::Pass, "dot available".to_string(), None)
        }
    };
    CheckResult {
        name: "dot".to_string(),
        status,
        summary,
        details: Vec::new(),
        remediation,
    }
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

fn validate_tls_ca(pem: &str) -> Result<(), String> {
    let mut reader = std::io::Cursor::new(pem.as_bytes());
    let certs: Vec<_> = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("failed to parse CA certificate PEM: {e}"))?;
    if certs.is_empty() {
        return Err("no CA certificates found in PEM".to_string());
    }
    Ok(())
}

fn validate_session_secret(value: &str) -> Result<(), String> {
    if value.len() < 64 {
        return Err(format!(
            "too short ({} chars, need at least 64 hex chars for 256-bit entropy)",
            value.len()
        ));
    }
    if !value.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("contains non-hex characters".to_string());
    }
    Ok(())
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
        version: FABRO_VERSION.to_string(),
        sections: vec![
            CheckSection {
                title: "Credentials".to_string(),
                checks: vec![llm, github, sandbox, brave],
            },
            CheckSection {
                title: "System".to_string(),
                checks: vec![check_dot()],
            },
            CheckSection {
                title: "Configuration".to_string(),
                checks: vec![crypto],
            },
        ],
    }
}

async fn check_llm_providers(state: &AppState) -> CheckResult {
    let configured: Vec<Provider> = Provider::ALL
        .iter()
        .copied()
        .filter(|provider| {
            provider
                .api_key_env_vars()
                .iter()
                .any(|name| state.secret_or_env(name).is_some())
        })
        .collect();

    if configured.is_empty() {
        return CheckResult {
            name: "LLM Providers".to_string(),
            status: CheckStatus::Error,
            summary: "none configured".to_string(),
            details: Vec::new(),
            remediation: Some("Set at least one provider API key".to_string()),
        };
    }

    let client = match state.build_llm_client().await {
        Ok(client) => client,
        Err(err) => {
            return CheckResult {
                name: "LLM Providers".to_string(),
                status: CheckStatus::Error,
                summary: "failed to initialize".to_string(),
                details: vec![CheckDetail::new(err)],
                remediation: Some("Check configured provider credentials".to_string()),
            };
        }
    };

    let mut details = Vec::new();
    let mut failed = Vec::new();
    for provider in configured {
        let result = timeout(
            Duration::from_secs(30),
            probe_llm_provider(&client, provider),
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
            summary: format!("{} configured", details.len()),
            details,
            remediation: None,
        }
    } else {
        CheckResult {
            name: "LLM Providers".to_string(),
            status: CheckStatus::Warning,
            summary: "connectivity issues".to_string(),
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
        model: probe_model(provider),
        messages: vec![Message::user("hi")],
        provider: Some(provider.as_str().to_string()),
        tools: None,
        tool_choice: None,
        response_format: None,
        temperature: None,
        top_p: None,
        max_tokens: Some(16),
        stop_sequences: None,
        reasoning_effort: None,
        speed: None,
        metadata: None,
        provider_options: None,
    };
    client
        .complete(&request)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

async fn check_github_app(state: &AppState) -> CheckResult {
    let settings = state
        .settings
        .read()
        .expect("settings lock poisoned")
        .clone();
    let app_id = settings.github_app_id_str();
    let slug = settings.github_slug_str();
    let private_key_raw = state.secret_or_env("GITHUB_APP_PRIVATE_KEY");
    let client_id = settings.github_client_id_str().is_some();
    let client_secret = state.secret_or_env("GITHUB_APP_CLIENT_SECRET").is_some();
    let webhook_secret = state.secret_or_env("GITHUB_APP_WEBHOOK_SECRET").is_some();

    if app_id.is_none()
        && private_key_raw.is_none()
        && !client_id
        && !client_secret
        && !webhook_secret
    {
        return CheckResult {
            name: "GitHub App".to_string(),
            status: CheckStatus::Warning,
            summary: "not configured".to_string(),
            details: Vec::new(),
            remediation: Some("Configure GitHub App settings and secrets".to_string()),
        };
    }

    let Some(app_id) = app_id else {
        return CheckResult {
            name: "GitHub App".to_string(),
            status: CheckStatus::Error,
            summary: "missing app_id".to_string(),
            details: Vec::new(),
            remediation: Some("Set git.app_id in settings.toml".to_string()),
        };
    };
    let Some(private_key_raw) = private_key_raw else {
        return CheckResult {
            name: "GitHub App".to_string(),
            status: CheckStatus::Error,
            summary: "missing private key".to_string(),
            details: Vec::new(),
            remediation: Some("Set GITHUB_APP_PRIVATE_KEY".to_string()),
        };
    };

    let private_key = match decode_pem_value("GITHUB_APP_PRIVATE_KEY", &private_key_raw) {
        Ok(value) => value,
        Err(err) => {
            return CheckResult {
                name: "GitHub App".to_string(),
                status: CheckStatus::Error,
                summary: "private key invalid".to_string(),
                details: vec![CheckDetail::new(err.clone())],
                remediation: Some(err),
            };
        }
    };

    let jwt = match fabro_github::sign_app_jwt(&app_id, &private_key) {
        Ok(jwt) => jwt,
        Err(err) => {
            return CheckResult {
                name: "GitHub App".to_string(),
                status: CheckStatus::Error,
                summary: "JWT signing failed".to_string(),
                details: vec![CheckDetail::new(err.clone())],
                remediation: Some(err),
            };
        }
    };

    let http = reqwest::Client::new();
    let auth_result = timeout(
        Duration::from_secs(15),
        fabro_github::get_authenticated_app(&http, &jwt, &fabro_github::github_api_base_url()),
    )
    .await;
    match auth_result {
        Ok(Ok(_app)) => CheckResult {
            name: "GitHub App".to_string(),
            status: CheckStatus::Pass,
            summary: slug.unwrap_or_else(|| "configured".to_string()),
            details: Vec::new(),
            remediation: None,
        },
        Ok(Err(err)) => CheckResult {
            name: "GitHub App".to_string(),
            status: CheckStatus::Error,
            summary: "connectivity error".to_string(),
            details: vec![CheckDetail::new(err.clone())],
            remediation: Some(err),
        },
        Err(_) => CheckResult {
            name: "GitHub App".to_string(),
            status: CheckStatus::Error,
            summary: "timeout".to_string(),
            details: vec![CheckDetail::new("GitHub probe timed out".to_string())],
            remediation: Some("Check GitHub connectivity and credentials".to_string()),
        },
    }
}

fn check_sandbox(state: &AppState) -> CheckResult {
    if state.secret_or_env("DAYTONA_API_KEY").is_some() {
        CheckResult {
            name: "Sandbox".to_string(),
            status: CheckStatus::Pass,
            summary: "Daytona configured".to_string(),
            details: Vec::new(),
            remediation: None,
        }
    } else {
        CheckResult {
            name: "Sandbox".to_string(),
            status: CheckStatus::Warning,
            summary: "not configured".to_string(),
            details: Vec::new(),
            remediation: Some("Set DAYTONA_API_KEY to enable cloud sandbox execution".to_string()),
        }
    }
}

async fn check_brave_search(state: &AppState) -> CheckResult {
    let Some(api_key) = state.secret_or_env("BRAVE_SEARCH_API_KEY") else {
        return CheckResult {
            name: "Brave Search".to_string(),
            status: CheckStatus::Warning,
            summary: "not configured".to_string(),
            details: Vec::new(),
            remediation: Some("Set BRAVE_SEARCH_API_KEY to enable web search".to_string()),
        };
    };

    let probe = timeout(Duration::from_secs(15), async {
        reqwest::Client::new()
            .get("https://api.search.brave.com/res/v1/web/search?q=test&count=1")
            .header("X-Subscription-Token", api_key)
            .send()
            .await
            .map_err(|e| e.to_string())
    })
    .await;

    match probe {
        Ok(Ok(response)) if response.status().is_success() => CheckResult {
            name: "Brave Search".to_string(),
            status: CheckStatus::Pass,
            summary: "configured and reachable".to_string(),
            details: Vec::new(),
            remediation: None,
        },
        Ok(Ok(response)) => CheckResult {
            name: "Brave Search".to_string(),
            status: CheckStatus::Warning,
            summary: format!("HTTP {}", response.status()),
            details: Vec::new(),
            remediation: Some("Check BRAVE_SEARCH_API_KEY and network connectivity".to_string()),
        },
        Ok(Err(err)) => CheckResult {
            name: "Brave Search".to_string(),
            status: CheckStatus::Warning,
            summary: "connectivity error".to_string(),
            details: vec![CheckDetail::new(err.clone())],
            remediation: Some(err),
        },
        Err(_) => CheckResult {
            name: "Brave Search".to_string(),
            status: CheckStatus::Warning,
            summary: "timeout".to_string(),
            details: vec![CheckDetail::new("Brave Search probe timed out".to_string())],
            remediation: Some("Check BRAVE_SEARCH_API_KEY and network connectivity".to_string()),
        },
    }
}

fn check_crypto(state: &AppState) -> CheckResult {
    use fabro_types::settings::interp::InterpString;

    let settings_file = state
        .settings
        .read()
        .expect("settings lock poisoned")
        .clone();
    let auth_api = settings_file
        .server
        .as_ref()
        .and_then(|s| s.auth.as_ref())
        .and_then(|a| a.api.as_ref());
    let has_jwt = auth_api
        .and_then(|api| api.jwt.as_ref())
        .is_some_and(|jwt| jwt.enabled.unwrap_or(true));
    let has_mtls = auth_api
        .and_then(|api| api.mtls.as_ref())
        .is_some_and(|mtls| mtls.enabled.unwrap_or(true));

    if !has_jwt && !has_mtls {
        return CheckResult {
            name: "Crypto".to_string(),
            status: CheckStatus::Warning,
            summary: "no authentication configured".to_string(),
            details: Vec::new(),
            remediation: Some(
                "Configure strategies under [server.auth.api.jwt] or [server.auth.api.mtls]"
                    .to_string(),
            ),
        };
    }

    let mut details = Vec::new();
    let mut errors = Vec::new();

    if has_mtls {
        use fabro_types::settings::server::ServerListenLayer;
        let listen_tls = settings_file
            .server
            .as_ref()
            .and_then(|s| s.listen.as_ref())
            .and_then(|listen| match listen {
                ServerListenLayer::Tcp { tls, .. } => tls.as_ref(),
                ServerListenLayer::Unix { .. } => None,
            });
        if let Some(listen_tls) = listen_tls {
            let read = |raw: Option<String>, label: &str| -> Result<String, String> {
                let Some(path_str) = raw else {
                    return Err(format!("server.listen.tls.{label} is not configured"));
                };
                let path = PathBuf::from(&path_str);
                let expanded = fabro_config::expand_tilde(&path);
                std::fs::read_to_string(&expanded)
                    .map_err(|e| format!("{}: {e}", expanded.display()))
            };
            let cert = read(
                listen_tls.cert.as_ref().map(InterpString::as_source),
                "cert",
            );
            let key = read(listen_tls.key.as_ref().map(InterpString::as_source), "key");
            let ca = read(listen_tls.ca.as_ref().map(InterpString::as_source), "ca");
            match (cert, key, ca) {
                (Ok(cert_pem), Ok(key_pem), Ok(ca_pem)) => {
                    if let Err(err) = validate_tls_cert(&cert_pem, chrono::Utc::now().timestamp()) {
                        errors.push(err);
                    }
                    if let Err(err) = validate_tls_private_key(&key_pem) {
                        errors.push(err);
                    }
                    if let Err(err) = validate_tls_ca(&ca_pem) {
                        errors.push(err);
                    }
                }
                _ => errors.push("failed to read mTLS files".to_string()),
            }
        } else {
            errors.push("mTLS configured but [server.listen.tls] is missing".to_string());
        }
    }

    if has_jwt {
        match state.secret_or_env("FABRO_JWT_PUBLIC_KEY") {
            Some(raw) => {
                if let Err(err) = decode_pem_value("FABRO_JWT_PUBLIC_KEY", &raw).and_then(|pem| {
                    jsonwebtoken::DecodingKey::from_ed_pem(pem.as_bytes())
                        .map(|_| ())
                        .map_err(|e| format!("invalid JWT public key: {e}"))
                }) {
                    errors.push(err);
                }
            }
            None => errors.push("FABRO_JWT_PUBLIC_KEY not set".to_string()),
        }
    }

    if let Some(raw) = state.secret_or_env("FABRO_JWT_PRIVATE_KEY") {
        if let Err(err) = decode_pem_value("FABRO_JWT_PRIVATE_KEY", &raw).and_then(|pem| {
            jsonwebtoken::EncodingKey::from_ed_pem(pem.as_bytes())
                .map(|_| ())
                .map_err(|e| format!("invalid JWT private key: {e}"))
        }) {
            errors.push(err);
        }
    }

    if let Some(secret) = state.secret_or_env("SESSION_SECRET") {
        if let Err(err) = validate_session_secret(&secret) {
            errors.push(err);
        }
    }

    if errors.is_empty() {
        CheckResult {
            name: "Crypto".to_string(),
            status: CheckStatus::Pass,
            summary: "all keys valid".to_string(),
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
