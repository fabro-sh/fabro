use std::path::PathBuf;
#[cfg(feature = "server")]
use std::process::Command;
#[cfg(feature = "server")]
use std::sync::LazyLock;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
#[cfg(feature = "server")]
use fabro_config::server::{ApiAuthStrategy, AuthProvider};
use fabro_llm::client::Client as LlmClient;
use fabro_llm::types::{Message, Request};
use fabro_model::{Catalog, Provider};
pub use fabro_util::check_report::{
    CheckDetail, CheckReport, CheckResult, CheckSection, CheckStatus,
};
use fabro_util::terminal::Styles;
use futures::future::join_all;
#[cfg(feature = "server")]
use regex::Regex;
#[cfg(feature = "server")]
use semver::Version;

use crate::cli_config::load_cli_settings;

// ---------------------------------------------------------------------------
// System dependency types and parsers (server mode only)
// ---------------------------------------------------------------------------

#[cfg(feature = "server")]
pub struct DepSpec {
    pub name: &'static str,
    command: &'static [&'static str],
    pub required: bool,
    pub min_version: Version,
    pattern: &'static LazyLock<Regex>,
}

#[cfg(feature = "server")]
#[derive(Debug, Clone, PartialEq)]
pub enum ProbeOutcome {
    NotFound,
    Failed,
    Ok { version: Option<Version> },
}

#[cfg(feature = "server")]
static OPENSSL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:OpenSSL|LibreSSL)\s+(\d+)\.(\d+)\.(\d+)").unwrap());
#[cfg(feature = "server")]
static NODE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"v(\d+)\.(\d+)\.(\d+)").unwrap());
#[cfg(feature = "server")]
static DOT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"graphviz version (\d+)\.(\d+)\.(\d+)").unwrap());

#[cfg(feature = "server")]
fn parse_version(re: &Regex, output: &str) -> Option<Version> {
    let caps = re.captures(output)?;
    Some(Version::new(
        caps[1].parse().ok()?,
        caps[2].parse().ok()?,
        caps[3].parse().ok()?,
    ))
}

#[cfg(feature = "server")]
pub const DEP_SPECS: &[DepSpec] = &[
    DepSpec {
        name: "openssl",
        command: &["openssl", "version"],
        required: true,
        min_version: Version::new(3, 0, 0),
        pattern: &OPENSSL_RE,
    },
    DepSpec {
        name: "node",
        command: &["node", "--version"],
        required: true,
        min_version: Version::new(20, 0, 0),
        pattern: &NODE_RE,
    },
    DepSpec {
        name: "dot",
        command: &["dot", "-V"],
        required: false,
        min_version: Version::new(2, 0, 0),
        pattern: &DOT_RE,
    },
];

#[cfg(feature = "server")]
pub fn probe_system_deps() -> Vec<ProbeOutcome> {
    DEP_SPECS
        .iter()
        .map(|spec| {
            let result = Command::new(spec.command[0])
                .args(&spec.command[1..])
                .output()
                .ok();

            match result {
                None => ProbeOutcome::NotFound,
                Some(o) if !o.status.success() => ProbeOutcome::Failed,
                Some(o) => {
                    let stdout = String::from_utf8_lossy(&o.stdout);
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    let version = parse_version(spec.pattern, &stdout)
                        .or_else(|| parse_version(spec.pattern, &stderr));
                    ProbeOutcome::Ok { version }
                }
            }
        })
        .collect()
}

#[cfg(feature = "server")]
fn dep_issue(name: &str, issue: &str, required: bool) -> (CheckStatus, String) {
    let severity = if required { "required" } else { "optional" };
    let status = if required {
        CheckStatus::Error
    } else {
        CheckStatus::Warning
    };
    (status, format!("{name}: {issue} ({severity})"))
}

#[cfg(feature = "server")]
pub fn check_system_deps(specs: &[DepSpec], outcomes: &[ProbeOutcome]) -> CheckResult {
    let mut details = Vec::new();
    let mut worst_status = CheckStatus::Pass;

    for (spec, outcome) in specs.iter().zip(outcomes) {
        let (status, text) = match outcome {
            ProbeOutcome::NotFound => dep_issue(spec.name, "not found", spec.required),
            ProbeOutcome::Failed => dep_issue(spec.name, "command failed", spec.required),
            ProbeOutcome::Ok { version: None } => {
                (CheckStatus::Pass, format!("{}: version unknown", spec.name))
            }
            ProbeOutcome::Ok { version: Some(v) } => {
                if v < &spec.min_version {
                    (
                        CheckStatus::Warning,
                        format!("{}: {v} (minimum {})", spec.name, spec.min_version),
                    )
                } else {
                    (CheckStatus::Pass, format!("{}: {v}", spec.name))
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
            details.push(CheckDetail::new("Connectivity: OK".to_string()));
            (CheckStatus::Pass, None)
        }
        Some(Err(e)) => {
            details.push(CheckDetail::new(format!("Connectivity: {e}")));
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
            details: vec![CheckDetail::new(format!("Loaded from {}", p.display()))],
            remediation: None,
        },
        None => CheckResult {
            name: "Configuration".to_string(),
            status: CheckStatus::Warning,
            summary: "no config file found".to_string(),
            details: vec![CheckDetail::new(
                "Create ~/.fabro/cli.toml to configure Arc".to_string(),
            )],
            remediation: Some("Create ~/.fabro/cli.toml".to_string()),
        },
    }
}

pub fn check_llm_providers(
    statuses: &[(Provider, bool)],
    live_results: Option<&[(Provider, Result<(), String>)]>,
) -> CheckResult {
    let count = statuses.iter().filter(|(_, set)| *set).count();

    let mut details: Vec<CheckDetail> = statuses
        .iter()
        .filter(|(_, set)| *set)
        .map(|(provider, _)| {
            let env_vars = provider.api_key_env_vars().join(" or ");
            CheckDetail::new(format!("{provider} ({env_vars}): set"))
        })
        .collect();

    let mut failed_providers: Vec<&Provider> = Vec::new();
    if let Some(results) = live_results {
        for (provider, result) in results {
            match result {
                Ok(()) => details.push(CheckDetail::new(format!("{provider} connectivity: OK",))),
                Err(e) => {
                    failed_providers.push(provider);
                    details.push(CheckDetail::new(format!("{provider} connectivity: {e}",)));
                }
            }
        }
    }

    if count == 0 {
        CheckResult {
            name: "LLM providers".to_string(),
            status: CheckStatus::Error,
            summary: "none configured".to_string(),
            details,
            remediation: Some("Set at least one provider API key".to_string()),
        }
    } else if !failed_providers.is_empty() {
        let names: Vec<_> = failed_providers.iter().map(|p| p.to_string()).collect();
        CheckResult {
            name: "LLM providers".to_string(),
            status: CheckStatus::Warning,
            summary: format!("{count} configured (connectivity issues)"),
            details,
            remediation: Some(format!("Connectivity issues with: {}", names.join(", "))),
        }
    } else {
        CheckResult {
            name: "LLM providers".to_string(),
            status: CheckStatus::Pass,
            summary: format!("{count} configured"),
            details,
            remediation: None,
        }
    }
}

pub fn check_brave_search(
    api_key_set: bool,
    live_result: Option<&Result<(), String>>,
) -> CheckResult {
    let mut details = vec![CheckDetail::new(format!(
        "BRAVE_SEARCH_API_KEY is {}",
        if api_key_set { "set" } else { "not set" }
    ))];

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
}

pub fn check_sandbox(status: &SandboxStatus) -> CheckResult {
    let mut details = Vec::new();

    match &status.daytona_probe {
        Some(Ok(())) => {
            details.push(CheckDetail::new(
                "Daytona (DAYTONA_API_KEY): available".to_string(),
            ));
            return CheckResult {
                name: "Cloud sandbox".to_string(),
                status: CheckStatus::Pass,
                summary: "Daytona available".to_string(),
                details,
                remediation: None,
            };
        }
        Some(Err(e)) => {
            details.push(CheckDetail::new(format!(
                "Daytona (DAYTONA_API_KEY): error — {e}",
            )));
            return CheckResult {
                name: "Cloud sandbox".to_string(),
                status: CheckStatus::Error,
                summary: format!("Daytona: {e}"),
                details,
                remediation: Some("Fix sandbox configuration errors".to_string()),
            };
        }
        None if status.daytona_configured => {
            details.push(CheckDetail::new(
                "Daytona (DAYTONA_API_KEY): configured".to_string(),
            ));
            return CheckResult {
                name: "Cloud sandbox".to_string(),
                status: CheckStatus::Pass,
                summary: "Daytona configured".to_string(),
                details,
                remediation: None,
            };
        }
        None => {
            details.push(CheckDetail::new(
                "Daytona (DAYTONA_API_KEY): not configured".to_string(),
            ));
        }
    }

    CheckResult {
        name: "Cloud sandbox".to_string(),
        status: CheckStatus::Warning,
        summary: "no sandbox configured".to_string(),
        details,
        remediation: Some("Set DAYTONA_API_KEY to enable cloud sandbox execution".to_string()),
    }
}

pub struct GithubAppStatus {
    pub app_id: Option<String>,
    pub slug: Option<String>,
    pub private_key_set: bool,
    /// Result of attempting to sign a JWT with the configured credentials.
    /// `None` if app_id or private key is missing.
    pub sign_result: Option<Result<(), String>>,
    #[cfg(feature = "server")]
    pub client_id: bool,
    #[cfg(feature = "server")]
    pub client_secret: bool,
    #[cfg(feature = "server")]
    pub webhook_secret: bool,
}

impl GithubAppStatus {
    fn core_set(&self) -> bool {
        self.app_id.is_some() && self.private_key_set
    }

    fn none_set(&self) -> bool {
        let core_none = self.app_id.is_none() && !self.private_key_set;
        #[cfg(feature = "server")]
        {
            core_none && !self.client_id && !self.client_secret && !self.webhook_secret
        }
        #[cfg(not(feature = "server"))]
        {
            core_none
        }
    }

    #[cfg(feature = "server")]
    fn all_set(&self) -> bool {
        self.core_set() && self.client_id && self.client_secret && self.webhook_secret
    }
}

pub fn check_github_app(status: &GithubAppStatus) -> CheckResult {
    let mut details: Vec<CheckDetail> = Vec::new();

    match (&status.app_id, &status.slug) {
        (Some(id), Some(slug)) => details.push(CheckDetail::new(format!("App: {slug} (ID {id})"))),
        (Some(id), None) => details.push(CheckDetail::new(format!("App ID: {id}"))),
        _ => details.push(CheckDetail::new("git.app_id: not set".to_string())),
    }

    details.push(CheckDetail::new(format!(
        "GITHUB_APP_PRIVATE_KEY: {}",
        if status.private_key_set {
            "set"
        } else {
            "not set"
        }
    )));

    #[allow(unused_mut)]
    let mut fields: Vec<(&str, bool)> = vec![
        ("git.app_id", status.app_id.is_some()),
        ("GITHUB_APP_PRIVATE_KEY", status.private_key_set),
    ];

    #[cfg(feature = "server")]
    {
        let server_fields: Vec<(&str, bool)> = vec![
            ("git.client_id", status.client_id),
            ("GITHUB_APP_CLIENT_SECRET", status.client_secret),
            ("GITHUB_APP_WEBHOOK_SECRET", status.webhook_secret),
        ];
        for (name, set) in &server_fields {
            details.push(CheckDetail::new(format!(
                "{name}: {}",
                if *set { "set" } else { "not set" }
            )));
        }
        fields.extend(server_fields);
    }

    // Add key validation detail
    if let Some(ref result) = status.sign_result {
        match result {
            Ok(()) => details.push(CheckDetail::new(
                "Private key: valid (JWT signing OK)".to_string(),
            )),
            Err(e) => details.push(CheckDetail::new(format!("Private key: invalid ({e})"))),
        }
    }

    let has_sign_error = matches!(&status.sign_result, Some(Err(_)));

    if has_sign_error {
        let msg = status
            .sign_result
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap_err()
            .clone();
        return CheckResult {
            name: "GitHub App".to_string(),
            status: CheckStatus::Error,
            summary: "private key invalid".to_string(),
            details,
            remediation: Some(format!(
                "GITHUB_APP_PRIVATE_KEY failed JWT signing: {msg}. \
                 Generate a new private key from your GitHub App settings."
            )),
        };
    }

    if status.none_set() {
        return CheckResult {
            name: "GitHub App".to_string(),
            status: CheckStatus::Warning,
            summary: "not configured".to_string(),
            details,
            remediation: Some(
                "Configure GitHub App in server.toml and set env vars to enable GitHub integration"
                    .to_string(),
            ),
        };
    }

    #[cfg(feature = "server")]
    if status.all_set() {
        return CheckResult {
            name: "GitHub App".to_string(),
            status: CheckStatus::Pass,
            summary: "fully configured".to_string(),
            details,
            remediation: None,
        };
    }

    #[cfg(not(feature = "server"))]
    if status.core_set() {
        return CheckResult {
            name: "GitHub App".to_string(),
            status: CheckStatus::Pass,
            summary: "configured".to_string(),
            details,
            remediation: None,
        };
    }

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

#[cfg(feature = "server")]
pub struct ApiStatus {
    pub base_url: String,
    pub authentication_strategies: Vec<ApiAuthStrategy>,
}

#[cfg(feature = "server")]
fn format_auth_strategies(strategies: &[ApiAuthStrategy]) -> String {
    strategies
        .iter()
        .map(|s| match s {
            ApiAuthStrategy::Jwt => "jwt",
            ApiAuthStrategy::Mtls => "mtls",
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(feature = "server")]
pub fn check_api(status: &ApiStatus, live_result: Option<&Result<(), String>>) -> CheckResult {
    let mut details = vec![
        CheckDetail::new(format!("Base URL: {}", status.base_url)),
        CheckDetail::new(format!(
            "Authentication: {}",
            format_auth_strategies(&status.authentication_strategies)
        )),
    ];

    let (check_status, remediation) = apply_live_result(
        live_result,
        &mut details,
        "Check that the API server is running and reachable",
    );

    CheckResult {
        name: "Fabro API".to_string(),
        status: check_status,
        summary: status.base_url.clone(),
        details,
        remediation,
    }
}

#[cfg(feature = "server")]
pub struct WebStatus {
    pub url: String,
    pub auth_provider: AuthProvider,
    pub allowed_usernames_count: usize,
}

#[cfg(feature = "server")]
fn format_auth_provider(provider: &AuthProvider) -> &'static str {
    match provider {
        AuthProvider::Github => "github",
        AuthProvider::InsecureDisabled => "insecure_disabled",
    }
}

#[cfg(feature = "server")]
pub fn check_web(status: &WebStatus, live_result: Option<&Result<(), String>>) -> CheckResult {
    let mut details = vec![
        CheckDetail::new(format!("URL: {}", status.url)),
        CheckDetail::new(format!(
            "Auth provider: {}",
            format_auth_provider(&status.auth_provider)
        )),
        CheckDetail::new(format!(
            "Allowed usernames: {}",
            status.allowed_usernames_count
        )),
    ];

    let (check_status, remediation) = apply_live_result(
        live_result,
        &mut details,
        "Check that the web app is running and reachable",
    );

    CheckResult {
        name: "Fabro Web".to_string(),
        status: check_status,
        summary: status.url.clone(),
        details,
        remediation,
    }
}

// ---------------------------------------------------------------------------
// Cryptographic key validation
// ---------------------------------------------------------------------------

#[cfg(feature = "server")]
pub struct TlsCheckInput {
    pub cert_pem: String,
    pub key_pem: String,
    pub ca_pem: String,
}

#[cfg(feature = "server")]
pub struct CryptoInput {
    pub auth_strategies: Vec<ApiAuthStrategy>,
    pub tls_files: Option<Result<TlsCheckInput, String>>,
    pub jwt_public_key: Option<String>,
    pub jwt_private_key: Option<String>,
    pub session_secret: Option<String>,
    pub now_epoch: i64,
}

#[cfg(feature = "server")]
fn decode_pem_value(name: &str, value: &str) -> Result<String, String> {
    if value.starts_with("-----") {
        return Ok(value.to_string());
    }
    let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, value)
        .map_err(|e| format!("{name} is not valid PEM or base64: {e}"))?;
    String::from_utf8(bytes).map_err(|e| format!("{name} base64 decoded to invalid UTF-8: {e}"))
}

#[cfg(feature = "server")]
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

#[cfg(feature = "server")]
fn validate_tls_private_key(pem: &str) -> Result<(), String> {
    let mut reader = std::io::Cursor::new(pem.as_bytes());
    rustls_pemfile::private_key(&mut reader)
        .map_err(|e| format!("failed to parse private key PEM: {e}"))?
        .ok_or_else(|| "no private key found in PEM".to_string())?;
    Ok(())
}

#[cfg(feature = "server")]
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

#[cfg(feature = "server")]
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

#[cfg(feature = "server")]
struct CryptoCheckState {
    details: Vec<CheckDetail>,
    errors: Vec<String>,
    worst: CheckStatus,
}

#[cfg(feature = "server")]
impl CryptoCheckState {
    fn new() -> Self {
        Self {
            details: Vec::new(),
            errors: Vec::new(),
            worst: CheckStatus::Pass,
        }
    }

    /// Record a validation result. Ok(suffix) becomes "{label}: {suffix}" detail,
    /// Err(msg) becomes an error detail and is accumulated for remediation.
    fn record(&mut self, label: &str, result: Result<String, String>) {
        match result {
            Ok(suffix) => self
                .details
                .push(CheckDetail::new(format!("{label}: {suffix}"))),
            Err(e) => {
                self.worst = CheckStatus::Error;
                let text = format!("{label}: {e}");
                self.errors.push(text.clone());
                self.details.push(CheckDetail::new(text));
            }
        }
    }

    fn record_unit(&mut self, label: &str, result: Result<(), String>) {
        self.record(label, result.map(|()| "valid".to_string()));
    }

    fn push_error(&mut self, text: String) {
        self.worst = CheckStatus::Error;
        self.errors.push(text.clone());
        self.details.push(CheckDetail::new(text));
    }
}

#[cfg(feature = "server")]
pub fn check_crypto(input: &CryptoInput) -> CheckResult {
    let has_jwt = input.auth_strategies.contains(&ApiAuthStrategy::Jwt);
    let has_mtls = input.auth_strategies.contains(&ApiAuthStrategy::Mtls);

    let mut state = CryptoCheckState::new();

    // mTLS certs
    if has_mtls {
        match &input.tls_files {
            Some(Ok(tls)) => {
                state.record(
                    "TLS cert",
                    validate_tls_cert(&tls.cert_pem, input.now_epoch)
                        .map(|info| format!("valid ({info})")),
                );
                state.record_unit("TLS key", validate_tls_private_key(&tls.key_pem));
                state.record_unit("TLS CA", validate_tls_ca(&tls.ca_pem));
            }
            Some(Err(e)) => state.push_error(format!("TLS files: {e}")),
            None => state.push_error("mTLS configured but [api.tls] not set".to_string()),
        }
    }

    // JWT public key
    if has_jwt {
        let result = input
            .jwt_public_key
            .as_deref()
            .ok_or_else(|| "JWT configured but FABRO_JWT_PUBLIC_KEY not set".to_string())
            .and_then(|raw| decode_pem_value("FABRO_JWT_PUBLIC_KEY", raw))
            .and_then(|pem| {
                jsonwebtoken::DecodingKey::from_ed_pem(pem.as_bytes())
                    .map(|_| ())
                    .map_err(|e| format!("invalid Ed25519 — {e}"))
            });
        state.record_unit("JWT public key", result);
    }

    // JWT private key (only when set)
    if let Some(raw) = &input.jwt_private_key {
        let result = decode_pem_value("FABRO_JWT_PRIVATE_KEY", raw).and_then(|pem| {
            jsonwebtoken::EncodingKey::from_ed_pem(pem.as_bytes())
                .map(|_| ())
                .map_err(|e| format!("invalid Ed25519 — {e}"))
        });
        state.record_unit("JWT private key", result);
    }

    // Session secret (only when set)
    if let Some(secret) = &input.session_secret {
        state.record_unit("Session secret", validate_session_secret(secret));
    }

    // No auth at all
    if !has_jwt && !has_mtls && input.jwt_private_key.is_none() && input.session_secret.is_none() {
        return CheckResult {
            name: "Cryptographic keys".to_string(),
            status: CheckStatus::Warning,
            summary: "no authentication configured".to_string(),
            details: vec![CheckDetail::new(
                "No authentication strategies or keys configured".to_string(),
            )],
            remediation: Some(
                "Configure authentication_strategies in [api] section of server.toml".to_string(),
            ),
        };
    }

    let summary = match state.worst {
        CheckStatus::Pass => "all keys valid".to_string(),
        CheckStatus::Warning => "some issues".to_string(),
        CheckStatus::Error => "invalid keys found".to_string(),
    };

    CheckResult {
        name: "Cryptographic keys".to_string(),
        status: state.worst,
        summary,
        details: state.details,
        remediation: if state.errors.is_empty() {
            None
        } else {
            Some(state.errors.join("; "))
        },
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

pub(crate) fn probe_model(provider: Provider) -> String {
    Catalog::builtin()
        .probe_for_provider(provider)
        .map(|m| m.id.clone())
        .unwrap_or_else(|| format!("unknown-{}", provider.as_str()))
}

async fn probe_llm_provider(
    client: &LlmClient,
    provider: Provider,
) -> (Provider, Result<(), String>) {
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
    let result = client
        .complete(&request)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string());
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

#[cfg(feature = "server")]
async fn probe_url(http: &reqwest::Client, url: &str) -> Result<(), String> {
    http.get(url)
        .send()
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

pub async fn run_doctor(verbose: bool, live: bool) -> i32 {
    let styles = Styles::detect_stdout();

    let spinner = indicatif::ProgressBar::new_spinner();
    spinner.set_style(
        indicatif::ProgressStyle::with_template("{spinner:.cyan} {msg}")
            .expect("valid template")
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", ""]),
    );
    spinner.set_message("Running checks…");
    spinner.enable_steady_tick(std::time::Duration::from_millis(80));

    // Gather state
    let cli_config = load_cli_settings(None).unwrap_or_default();

    let config_path = dirs::home_dir().map(|h| h.join(".fabro").join("cli.toml"));
    let config_exists = config_path.as_ref().is_some_and(|p| p.exists());

    let llm_statuses: Vec<(Provider, bool)> = Provider::ALL
        .iter()
        .map(|p| (*p, p.has_api_key()))
        .collect();

    let brave_key_set = std::env::var("BRAVE_SEARCH_API_KEY").is_ok();

    let daytona_configured = std::env::var("DAYTONA_API_KEY").is_ok();

    #[cfg(feature = "server")]
    let server_config = fabro_config::server::load_server_settings(None).unwrap_or_default();

    #[cfg(feature = "server")]
    let api_status = {
        let api = server_config.api.clone().unwrap_or_default();
        ApiStatus {
            base_url: api.base_url.clone(),
            authentication_strategies: api.authentication_strategies.clone(),
        }
    };

    #[cfg(feature = "server")]
    let web_status = {
        let web = server_config.web.clone().unwrap_or_default();
        WebStatus {
            url: web.url.clone(),
            auth_provider: web.auth.provider.clone(),
            allowed_usernames_count: web.auth.allowed_usernames.len(),
        }
    };

    #[cfg(feature = "server")]
    let server_git = server_config.git.clone().unwrap_or_default();

    #[cfg(feature = "server")]
    let server_api = server_config.api.clone().unwrap_or_default();

    #[cfg(feature = "server")]
    let server_web = server_config.web.clone().unwrap_or_default();

    let git_app_id = cli_config.app_id().map(str::to_owned);
    let private_key_raw = std::env::var("GITHUB_APP_PRIVATE_KEY").ok();
    let sign_result = match (&git_app_id, &private_key_raw) {
        (Some(app_id), Some(raw)) => {
            let pem = if raw.starts_with("-----") {
                Ok(raw.clone())
            } else {
                BASE64_STANDARD
                    .decode(raw)
                    .map_err(|e| format!("base64 decode failed: {e}"))
                    .and_then(|bytes| {
                        String::from_utf8(bytes)
                            .map_err(|e| format!("decoded key is not valid UTF-8: {e}"))
                    })
            };
            match pem {
                Ok(pem) => Some(
                    fabro_github::sign_app_jwt(app_id, &pem)
                        .map(|_| ())
                        .map_err(|e| e.to_string()),
                ),
                Err(e) => Some(Err(e)),
            }
        }
        _ => None,
    };
    let github_status = GithubAppStatus {
        app_id: git_app_id,
        slug: cli_config.slug().map(str::to_owned),
        private_key_set: private_key_raw.is_some(),
        sign_result,
        #[cfg(feature = "server")]
        client_id: server_git.client_id.is_some(),
        #[cfg(feature = "server")]
        client_secret: std::env::var("GITHUB_APP_CLIENT_SECRET").is_ok(),
        #[cfg(feature = "server")]
        webhook_secret: std::env::var("GITHUB_APP_WEBHOOK_SECRET").is_ok(),
    };

    #[cfg(feature = "server")]
    let crypto_input = {
        let has_mtls = server_api
            .authentication_strategies
            .contains(&ApiAuthStrategy::Mtls);
        let tls_files = if has_mtls {
            server_api.tls.as_ref().map(|tls| {
                let read = |p: &std::path::Path| -> Result<String, String> {
                    let expanded = fabro_config::expand_tilde(p);
                    std::fs::read_to_string(&expanded)
                        .map_err(|e| format!("{}: {e}", expanded.display()))
                };
                Ok(TlsCheckInput {
                    cert_pem: read(&tls.cert)?,
                    key_pem: read(&tls.key)?,
                    ca_pem: read(&tls.ca)?,
                })
            })
        } else {
            None
        };
        CryptoInput {
            auth_strategies: server_api.authentication_strategies.clone(),
            tls_files,
            jwt_public_key: std::env::var("FABRO_JWT_PUBLIC_KEY").ok(),
            jwt_private_key: std::env::var("FABRO_JWT_PRIVATE_KEY").ok(),
            session_secret: std::env::var("SESSION_SECRET").ok(),
            now_epoch: chrono::Utc::now().timestamp(),
        }
    };

    #[cfg(feature = "server")]
    let dep_results = probe_system_deps();

    // Live probes (only when --live is set)
    let sandbox_status;
    let llm_live_results: Option<Vec<(Provider, Result<(), String>)>>;
    let brave_live_result: Option<Result<(), String>>;
    #[cfg(feature = "server")]
    let api_live_result: Option<Result<(), String>>;
    #[cfg(feature = "server")]
    let web_live_result: Option<Result<(), String>>;

    if live {
        let http = reqwest::Client::new();

        // Build LLM client — may fail if no keys are set
        let llm_client = LlmClient::from_env().await.ok();

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
                Some(join_all(futures).await)
            } else {
                None
            }
        };

        let sandbox_fut = async {
            let daytona_probe = probe_daytona().await;
            SandboxStatus {
                daytona_configured,
                daytona_probe,
            }
        };
        let brave_fut = probe_brave_search(&http);

        #[cfg(feature = "server")]
        {
            let api_url = format!("{}/runs", server_api.base_url);
            let api_fut = probe_url(&http, &api_url);
            let web_fut = probe_url(&http, &server_web.url);

            let (sandbox, llm, brave, api, web) =
                tokio::join!(sandbox_fut, llm_fut, brave_fut, api_fut, web_fut);

            sandbox_status = sandbox;
            llm_live_results = llm;
            brave_live_result = Some(brave);
            api_live_result = Some(api);
            web_live_result = Some(web);
        }

        #[cfg(not(feature = "server"))]
        {
            let (sandbox, llm, brave) = tokio::join!(sandbox_fut, llm_fut, brave_fut);

            sandbox_status = sandbox;
            llm_live_results = llm;
            brave_live_result = Some(brave);
        }
    } else {
        sandbox_status = SandboxStatus {
            daytona_configured,
            daytona_probe: None,
        };
        llm_live_results = None;
        brave_live_result = None;
        #[cfg(feature = "server")]
        {
            api_live_result = None;
            web_live_result = None;
        }
    }

    // Run pure checks
    #[allow(unused_mut)]
    let mut sections = vec![
        CheckSection {
            title: "Required".into(),
            checks: vec![
                check_config(if config_exists { config_path } else { None }),
                check_llm_providers(&llm_statuses, llm_live_results.as_deref()),
                check_github_app(&github_status),
            ],
        },
        CheckSection {
            title: "Optional".into(),
            checks: vec![
                check_sandbox(&sandbox_status),
                check_brave_search(brave_key_set, brave_live_result.as_ref()),
            ],
        },
    ];

    #[cfg(feature = "server")]
    sections.push(CheckSection {
        title: "Server".into(),
        checks: vec![
            check_system_deps(DEP_SPECS, &dep_results),
            check_api(&api_status, api_live_result.as_ref()),
            check_web(&web_status, web_live_result.as_ref()),
            check_crypto(&crypto_input),
        ],
    });

    let report = CheckReport {
        title: "Fabro Doctor".into(),
        sections,
    };

    spinner.finish_and_clear();

    let term_width = console::Term::stderr().size().1;
    print!(
        "{}",
        report.render(&styles, verbose, None, Some(term_width))
    );

    if report.has_errors() { 1 } else { 0 }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- check_config --

    #[test]
    fn check_config_pass_with_path() {
        let result = check_config(Some(PathBuf::from("/home/user/.fabro/cli.toml")));
        assert_eq!(result.status, CheckStatus::Pass);
        assert!(result.summary.contains(".fabro/cli.toml"));
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
        let statuses: Vec<(Provider, bool)> = Provider::ALL.iter().map(|p| (*p, true)).collect();
        let result = check_llm_providers(&statuses, None);
        assert_eq!(result.status, CheckStatus::Pass);
        assert!(result.summary.contains("7 configured"));
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
        assert!(result.summary.contains("5 configured"));
    }

    #[test]
    fn check_llm_none_configured() {
        let statuses: Vec<(Provider, bool)> = Provider::ALL.iter().map(|p| (*p, false)).collect();
        let result = check_llm_providers(&statuses, None);
        assert_eq!(result.status, CheckStatus::Error);
        assert!(result.summary.contains("none configured"));
    }

    #[test]
    fn check_llm_live_ok() {
        let statuses = vec![(Provider::Anthropic, true)];
        let live = vec![(Provider::Anthropic, Ok(()))];
        let result = check_llm_providers(&statuses, Some(&live));
        assert_eq!(result.status, CheckStatus::Pass);
        assert!(
            result
                .details
                .iter()
                .any(|d| d.text.contains("connectivity: OK"))
        );
    }

    #[test]
    fn check_llm_live_error() {
        let statuses = vec![(Provider::Anthropic, true)];
        let live = vec![(Provider::Anthropic, Err("timeout".to_string()))];
        let result = check_llm_providers(&statuses, Some(&live));
        assert_eq!(result.status, CheckStatus::Warning);
        assert!(result.details.iter().any(|d| d.text.contains("timeout")));
        let rem = result.remediation.unwrap();
        assert!(
            rem.contains("anthropic"),
            "remediation should name the failing provider: {rem}"
        );
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
        };
        let result = check_sandbox(&status);
        assert_eq!(result.status, CheckStatus::Pass);
        assert!(result.summary.contains("Daytona available"));
    }

    #[test]
    fn check_sandbox_nothing_configured() {
        let status = SandboxStatus {
            daytona_configured: false,
            daytona_probe: None,
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
        };
        let result = check_sandbox(&status);
        assert_eq!(result.status, CheckStatus::Error);
    }

    // -- check_github_app --

    #[test]
    fn check_github_sign_error_reports_error() {
        let status = GithubAppStatus {
            app_id: Some("12345".to_string()),
            slug: None,
            private_key_set: true,
            sign_result: Some(Err(
                "Signing failed: signature error: UnexpectedError".to_string()
            )),
            #[cfg(feature = "server")]
            client_id: true,
            #[cfg(feature = "server")]
            client_secret: true,
            #[cfg(feature = "server")]
            webhook_secret: true,
        };
        let result = check_github_app(&status);
        assert_eq!(result.status, CheckStatus::Error);
        assert!(
            result.summary.contains("invalid"),
            "got: {}",
            result.summary
        );
        let rem = result.remediation.unwrap();
        assert!(rem.contains("Generate a new private key"), "got: {rem}");
    }

    #[test]
    fn check_github_not_configured() {
        let status = GithubAppStatus {
            app_id: None,
            slug: None,
            private_key_set: false,
            sign_result: None,
            #[cfg(feature = "server")]
            client_id: false,
            #[cfg(feature = "server")]
            client_secret: false,
            #[cfg(feature = "server")]
            webhook_secret: false,
        };
        let result = check_github_app(&status);
        assert_eq!(result.status, CheckStatus::Warning);
    }

    // -- Server-only checks (check_api, check_web, check_crypto) --

    #[cfg(feature = "server")]
    mod server_tests {
        use super::*;

        #[test]
        fn check_github_all_set() {
            let status = GithubAppStatus {
                app_id: Some("12345".to_string()),
                slug: Some("my-app".to_string()),
                private_key_set: true,
                sign_result: Some(Ok(())),
                client_id: true,
                client_secret: true,
                webhook_secret: true,
            };
            let result = check_github_app(&status);
            assert_eq!(result.status, CheckStatus::Pass);
        }

        #[test]
        fn check_github_none_set() {
            let status = GithubAppStatus {
                app_id: None,
                slug: None,
                private_key_set: false,
                sign_result: None,
                client_id: false,
                client_secret: false,
                webhook_secret: false,
            };
            let result = check_github_app(&status);
            assert_eq!(result.status, CheckStatus::Warning);
        }

        #[test]
        fn check_github_partial() {
            let status = GithubAppStatus {
                app_id: Some("12345".to_string()),
                slug: None,
                private_key_set: false,
                sign_result: None,
                client_id: true,
                client_secret: false,
                webhook_secret: false,
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
                authentication_strategies: vec![ApiAuthStrategy::Jwt],
            };
            let result = check_api(&status, None);
            assert_eq!(result.status, CheckStatus::Pass);
            assert_eq!(result.summary, "http://localhost:3000");
        }

        #[test]
        fn check_api_details_show_auth_strategy() {
            let status = ApiStatus {
                base_url: "https://api.example.com".to_string(),
                authentication_strategies: vec![ApiAuthStrategy::Jwt],
            };
            let result = check_api(&status, None);
            assert!(result.details.iter().any(|d| d.text.contains("jwt")));
            assert!(
                result
                    .details
                    .iter()
                    .any(|d| d.text.contains("https://api.example.com"))
            );
        }

        #[test]
        fn check_api_live_ok() {
            let status = ApiStatus {
                base_url: "http://localhost:3000".to_string(),
                authentication_strategies: vec![ApiAuthStrategy::Jwt],
            };
            let live = Ok(());
            let result = check_api(&status, Some(&live));
            assert_eq!(result.status, CheckStatus::Pass);
            assert!(
                result
                    .details
                    .iter()
                    .any(|d| d.text.contains("Connectivity: OK"))
            );
        }

        #[test]
        fn check_api_live_error() {
            let status = ApiStatus {
                base_url: "http://localhost:3000".to_string(),
                authentication_strategies: vec![ApiAuthStrategy::Jwt],
            };
            let live = Err("connection refused".to_string());
            let result = check_api(&status, Some(&live));
            assert_eq!(result.status, CheckStatus::Warning);
            assert!(
                result
                    .details
                    .iter()
                    .any(|d| d.text.contains("connection refused"))
            );
        }

        // -- check_web --

        #[test]
        fn check_web_shows_url() {
            let status = WebStatus {
                url: "http://localhost:5173".to_string(),
                auth_provider: AuthProvider::Github,
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
                auth_provider: AuthProvider::Github,
                allowed_usernames_count: 3,
            };
            let result = check_web(&status, None);
            assert!(result.details.iter().any(|d| d.text.contains("github")));
            assert!(
                result
                    .details
                    .iter()
                    .any(|d| d.text.contains("https://arc.example.com"))
            );
            assert!(
                result
                    .details
                    .iter()
                    .any(|d| d.text.contains("Allowed usernames: 3"))
            );
        }

        #[test]
        fn check_web_live_ok() {
            let status = WebStatus {
                url: "http://localhost:5173".to_string(),
                auth_provider: AuthProvider::Github,
                allowed_usernames_count: 0,
            };
            let live = Ok(());
            let result = check_web(&status, Some(&live));
            assert_eq!(result.status, CheckStatus::Pass);
            assert!(
                result
                    .details
                    .iter()
                    .any(|d| d.text.contains("Connectivity: OK"))
            );
        }

        #[test]
        fn check_web_live_error() {
            let status = WebStatus {
                url: "http://localhost:5173".to_string(),
                auth_provider: AuthProvider::Github,
                allowed_usernames_count: 0,
            };
            let live = Err("connection refused".to_string());
            let result = check_web(&status, Some(&live));
            assert_eq!(result.status, CheckStatus::Warning);
            assert!(
                result
                    .details
                    .iter()
                    .any(|d| d.text.contains("connection refused"))
            );
        }
    } // mod server_tests (check_github_app, check_api, check_web)

    // -- parse_version (server only) --

    #[test]
    #[cfg(feature = "server")]
    fn parse_version_openssl() {
        assert_eq!(
            parse_version(
                &OPENSSL_RE,
                "OpenSSL 3.4.1 11 Feb 2025 (Library: OpenSSL 3.4.1 11 Feb 2025)"
            ),
            Some(Version::new(3, 4, 1)),
        );
    }

    #[test]
    #[cfg(feature = "server")]
    fn parse_version_libressl() {
        assert_eq!(
            parse_version(&OPENSSL_RE, "LibreSSL 3.3.6"),
            Some(Version::new(3, 3, 6))
        );
    }

    #[test]
    #[cfg(feature = "server")]
    fn parse_version_node() {
        assert_eq!(
            parse_version(&NODE_RE, "v22.14.0"),
            Some(Version::new(22, 14, 0))
        );
    }

    #[test]
    #[cfg(feature = "server")]
    fn parse_version_dot() {
        assert_eq!(
            parse_version(&DOT_RE, "dot - graphviz version 12.2.1 (20241206.2024)"),
            Some(Version::new(12, 2, 1)),
        );
    }

    #[test]
    #[cfg(feature = "server")]
    fn parse_version_garbage_returns_none() {
        assert_eq!(parse_version(&OPENSSL_RE, "not a version"), None);
        assert_eq!(parse_version(&NODE_RE, "node not found"), None);
        assert_eq!(parse_version(&DOT_RE, "no version here"), None);
    }

    // -- check_system_deps (server only) --

    #[cfg(feature = "server")]
    static TEST_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"unused").unwrap());

    #[cfg(feature = "server")]
    fn spec(name: &'static str, required: bool, min_version: Version) -> DepSpec {
        DepSpec {
            name,
            command: &["true"],
            required,
            min_version,
            pattern: &TEST_RE,
        }
    }

    #[test]
    #[cfg(feature = "server")]
    fn check_system_deps_all_present() {
        let specs = [
            spec("openssl", true, Version::new(3, 0, 0)),
            spec("node", true, Version::new(20, 0, 0)),
            spec("gh", false, Version::new(2, 0, 0)),
            spec("dot", false, Version::new(2, 0, 0)),
        ];
        let outcomes = [
            ProbeOutcome::Ok {
                version: Some(Version::new(3, 4, 1)),
            },
            ProbeOutcome::Ok {
                version: Some(Version::new(22, 14, 0)),
            },
            ProbeOutcome::Ok {
                version: Some(Version::new(2, 67, 0)),
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
    #[cfg(feature = "server")]
    fn check_system_deps_required_missing_is_error() {
        let specs = [spec("openssl", true, Version::new(3, 0, 0))];
        let outcomes = [ProbeOutcome::NotFound];
        let result = check_system_deps(&specs, &outcomes);
        assert_eq!(result.status, CheckStatus::Error);
        assert!(result.details[0].text.contains("not found (required)"));
    }

    #[test]
    #[cfg(feature = "server")]
    fn check_system_deps_optional_missing_is_warning() {
        let specs = [spec("gh", false, Version::new(2, 0, 0))];
        let outcomes = [ProbeOutcome::NotFound];
        let result = check_system_deps(&specs, &outcomes);
        assert_eq!(result.status, CheckStatus::Warning);
        assert!(result.details[0].text.contains("not found (optional)"));
    }

    #[test]
    #[cfg(feature = "server")]
    fn check_system_deps_outdated_is_warning() {
        let specs = [spec("openssl", true, Version::new(3, 0, 0))];
        let outcomes = [ProbeOutcome::Ok {
            version: Some(Version::new(1, 1, 1)),
        }];
        let result = check_system_deps(&specs, &outcomes);
        assert_eq!(result.status, CheckStatus::Warning);
        assert!(result.details[0].text.contains("1.1.1"));
        assert!(result.details[0].text.contains("minimum 3.0.0"));
    }

    #[test]
    #[cfg(feature = "server")]
    fn check_system_deps_unparseable_success_is_pass() {
        let specs = [spec("openssl", true, Version::new(3, 0, 0))];
        let outcomes = [ProbeOutcome::Ok { version: None }];
        let result = check_system_deps(&specs, &outcomes);
        assert_eq!(result.status, CheckStatus::Pass);
        assert!(result.details[0].text.contains("version unknown"));
    }

    #[test]
    #[cfg(feature = "server")]
    fn check_system_deps_required_command_failed_is_error() {
        let specs = [spec("node", true, Version::new(20, 0, 0))];
        let outcomes = [ProbeOutcome::Failed];
        let result = check_system_deps(&specs, &outcomes);
        assert_eq!(result.status, CheckStatus::Error);
        assert!(result.details[0].text.contains("command failed (required)"));
    }

    #[test]
    #[cfg(feature = "server")]
    fn check_system_deps_optional_command_failed_is_warning() {
        let specs = [spec("gh", false, Version::new(2, 0, 0))];
        let outcomes = [ProbeOutcome::Failed];
        let result = check_system_deps(&specs, &outcomes);
        assert_eq!(result.status, CheckStatus::Warning);
        assert!(result.details[0].text.contains("command failed (optional)"));
    }

    #[test]
    #[cfg(feature = "server")]
    fn check_system_deps_error_beats_warning() {
        let specs = [
            spec("openssl", true, Version::new(3, 0, 0)),
            spec("gh", false, Version::new(2, 0, 0)),
        ];
        let outcomes = [ProbeOutcome::NotFound, ProbeOutcome::NotFound];
        let result = check_system_deps(&specs, &outcomes);
        assert_eq!(result.status, CheckStatus::Error);
    }

    // -- check_crypto --

    #[cfg(feature = "server")]
    mod server_crypto_tests {
        use super::*;

        /// Generate a self-signed cert + private key PEM for TLS tests.
        fn generate_test_tls_cert() -> (String, String) {
            let output = std::process::Command::new("openssl")
                .args([
                    "req",
                    "-x509",
                    "-newkey",
                    "ec",
                    "-pkeyopt",
                    "ec_paramgen_curve:prime256v1",
                    "-keyout",
                    "/dev/stdout",
                    "-out",
                    "/dev/stdout",
                    "-days",
                    "3650",
                    "-nodes",
                    "-subj",
                    "/CN=test-server",
                ])
                .output()
                .expect("openssl must be available for tests");
            let combined = String::from_utf8(output.stdout).unwrap();
            let key_start = combined.find("-----BEGIN PRIVATE KEY-----").unwrap();
            let key_end = combined.find("-----END PRIVATE KEY-----").unwrap()
                + "-----END PRIVATE KEY-----".len();
            let cert_start = combined.find("-----BEGIN CERTIFICATE-----").unwrap();
            let cert_end = combined.find("-----END CERTIFICATE-----").unwrap()
                + "-----END CERTIFICATE-----".len();
            let key_pem = combined[key_start..key_end].to_string();
            let cert_pem = combined[cert_start..cert_end].to_string();
            (cert_pem, key_pem)
        }

        fn generate_test_ed25519_keypair() -> (String, String) {
            let output = std::process::Command::new("openssl")
                .args(["genpkey", "-algorithm", "Ed25519"])
                .output()
                .expect("openssl must be available for tests");
            let private_pem = String::from_utf8(output.stdout).unwrap();
            let output = std::process::Command::new("openssl")
                .args(["pkey", "-pubout"])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .and_then(|mut child| {
                    use std::io::Write;
                    child
                        .stdin
                        .take()
                        .unwrap()
                        .write_all(private_pem.as_bytes())
                        .unwrap();
                    child.wait_with_output()
                })
                .expect("openssl pkey failed");
            let public_pem = String::from_utf8(output.stdout).unwrap();
            (public_pem, private_pem)
        }

        fn crypto_input(auth_strategies: Vec<ApiAuthStrategy>) -> CryptoInput {
            CryptoInput {
                auth_strategies,
                tls_files: None,
                jwt_public_key: None,
                jwt_private_key: None,
                session_secret: None,
                now_epoch: chrono::Utc::now().timestamp(),
            }
        }

        #[test]
        fn crypto_all_keys_valid() {
            let (cert_pem, key_pem) = generate_test_tls_cert();
            let (public_pem, private_pem) = generate_test_ed25519_keypair();
            let input = CryptoInput {
                tls_files: Some(Ok(TlsCheckInput {
                    cert_pem: cert_pem.clone(),
                    key_pem,
                    ca_pem: cert_pem,
                })),
                jwt_public_key: Some(public_pem),
                jwt_private_key: Some(private_pem),
                session_secret: Some("a".repeat(64)),
                ..crypto_input(vec![ApiAuthStrategy::Jwt, ApiAuthStrategy::Mtls])
            };
            let result = check_crypto(&input);
            assert_eq!(result.status, CheckStatus::Pass);
            assert_eq!(result.summary, "all keys valid");
        }

        #[test]
        fn crypto_invalid_cert_pem() {
            let (public_pem, private_pem) = generate_test_ed25519_keypair();
            let input = CryptoInput {
                tls_files: Some(Ok(TlsCheckInput {
                    cert_pem: "not a pem".to_string(),
                    key_pem: "not a pem".to_string(),
                    ca_pem: "not a pem".to_string(),
                })),
                jwt_public_key: Some(public_pem),
                jwt_private_key: Some(private_pem),
                ..crypto_input(vec![ApiAuthStrategy::Jwt, ApiAuthStrategy::Mtls])
            };
            let result = check_crypto(&input);
            assert_eq!(result.status, CheckStatus::Error);
            assert!(result.details.iter().any(|d| d.text.contains("TLS cert")));
        }

        #[test]
        fn crypto_expired_cert() {
            let (cert_pem, _) = generate_test_tls_cert();
            let far_future = i64::MAX / 2;
            let result = validate_tls_cert(&cert_pem, far_future);
            assert!(result.is_err());
            assert!(result.unwrap_err().contains("expired"));
        }

        #[test]
        fn crypto_session_secret_too_short() {
            let input = CryptoInput {
                session_secret: Some("abcdef".to_string()),
                ..crypto_input(vec![])
            };
            let result = check_crypto(&input);
            assert_eq!(result.status, CheckStatus::Error);
            assert!(result.details.iter().any(|d| d.text.contains("too short")));
        }

        #[test]
        fn crypto_session_secret_non_hex() {
            let input = CryptoInput {
                session_secret: Some("z".repeat(64)),
                ..crypto_input(vec![])
            };
            let result = check_crypto(&input);
            assert_eq!(result.status, CheckStatus::Error);
            assert!(result.details.iter().any(|d| d.text.contains("non-hex")));
        }

        #[test]
        fn crypto_no_auth_configured() {
            let result = check_crypto(&crypto_input(vec![]));
            assert_eq!(result.status, CheckStatus::Warning);
            assert!(result.summary.contains("no authentication configured"));
        }

        #[test]
        fn crypto_jwt_configured_but_key_missing() {
            let result = check_crypto(&crypto_input(vec![ApiAuthStrategy::Jwt]));
            assert_eq!(result.status, CheckStatus::Error);
            assert!(
                result
                    .details
                    .iter()
                    .any(|d| d.text.contains("FABRO_JWT_PUBLIC_KEY not set"))
            );
        }

        #[test]
        fn crypto_mtls_configured_but_tls_not_set() {
            let result = check_crypto(&crypto_input(vec![ApiAuthStrategy::Mtls]));
            assert_eq!(result.status, CheckStatus::Error);
            assert!(
                result
                    .details
                    .iter()
                    .any(|d| d.text.contains("[api.tls] not set"))
            );
        }

        #[test]
        fn crypto_mtls_configured_but_files_unreadable() {
            let input = CryptoInput {
                tls_files: Some(Err("Permission denied: /path/to/cert.pem".to_string())),
                ..crypto_input(vec![ApiAuthStrategy::Mtls])
            };
            let result = check_crypto(&input);
            assert_eq!(result.status, CheckStatus::Error);
            assert!(
                result
                    .details
                    .iter()
                    .any(|d| d.text.contains("Permission denied"))
            );
        }

        #[test]
        fn crypto_invalid_jwt_public_key() {
            let input = CryptoInput {
                jwt_public_key: Some(
                    "-----BEGIN PUBLIC KEY-----\nINVALID\n-----END PUBLIC KEY-----".to_string(),
                ),
                ..crypto_input(vec![ApiAuthStrategy::Jwt])
            };
            let result = check_crypto(&input);
            assert_eq!(result.status, CheckStatus::Error);
            assert!(
                result
                    .details
                    .iter()
                    .any(|d| d.text.contains("JWT public key: invalid"))
            );
        }

        #[test]
        fn crypto_invalid_jwt_private_key() {
            let input = CryptoInput {
                jwt_private_key: Some(
                    "-----BEGIN PRIVATE KEY-----\nINVALID\n-----END PRIVATE KEY-----".to_string(),
                ),
                ..crypto_input(vec![])
            };
            let result = check_crypto(&input);
            assert_eq!(result.status, CheckStatus::Error);
            assert!(
                result
                    .details
                    .iter()
                    .any(|d| d.text.contains("JWT private key: invalid"))
            );
        }

        #[test]
        fn crypto_base64_encoded_jwt_key() {
            let (public_pem, _) = generate_test_ed25519_keypair();
            let encoded = base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                public_pem.as_bytes(),
            );
            let input = CryptoInput {
                jwt_public_key: Some(encoded),
                ..crypto_input(vec![ApiAuthStrategy::Jwt])
            };
            let result = check_crypto(&input);
            assert_eq!(result.status, CheckStatus::Pass);
            assert!(
                result
                    .details
                    .iter()
                    .any(|d| d.text.contains("JWT public key: valid"))
            );
        }

        #[test]
        fn crypto_valid_session_secret() {
            let input = CryptoInput {
                session_secret: Some("a1b2c3d4e5f6".repeat(6)),
                ..crypto_input(vec![])
            };
            let result = check_crypto(&input);
            assert_eq!(result.status, CheckStatus::Pass);
        }
    } // mod server_crypto_tests
}
