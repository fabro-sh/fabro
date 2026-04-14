use std::net::SocketAddr;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use axum::extract::Query;
use axum::response::Html;
use axum::routing::get;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use dialoguer::console::Term;
use dialoguer::theme::ColorfulTheme;
use dialoguer::{MultiSelect, Select};
use fabro_api::types::{CreateSecretRequest, SecretType as ApiSecretType};
use fabro_auth::{AuthCredential, AuthMethod, codex_oauth_config, credential_id_for};
use fabro_config::user::SETTINGS_CONFIG_FILENAME;
use fabro_config::{Storage, envfile, legacy_env};
use fabro_model::Provider;
use fabro_types::settings::CliSettings;
use fabro_types::settings::cli::{CliLayer, OutputFormat};
use fabro_util::printer::Printer;
use fabro_util::terminal::Styles;
use fabro_util::{dev_token, session_secret};
use futures::future::BoxFuture;
use rand::Rng;
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair as _};
use tokio::net::TcpListener;
use tokio::process::Command as TokioCommand;
use tokio::sync::oneshot;
use tokio::task::spawn_blocking;

use super::doctor;
use crate::args::{
    DoctorArgs, InstallArgs, InstallGitHubStrategyArg, InstallNonInteractiveArgs, ServerTargetArgs,
};
use crate::commands::server::{record, stop};
use crate::gh::GhCli;
use crate::shared::provider_auth::{
    ApiKeySource, authenticate_provider, authenticate_provider_with_api_key_source,
    authenticate_provider_with_method, prompt_confirm, provider_display_name,
};
use crate::{server_client, user_config};

// ---------------------------------------------------------------------------
// JWT keypair generation
// ---------------------------------------------------------------------------

const ED25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2A, 0x30, 0x05, 0x06, 0x03, 0x2B, 0x65, 0x70, 0x03, 0x21, 0x00,
];
const ED25519_PUBLIC_KEY_LEN: usize = 32;

fn pem_encode(label: &str, bytes: &[u8]) -> String {
    let body = BASE64_STANDARD.encode(bytes);
    let mut pem = String::new();
    pem.push_str("-----BEGIN ");
    pem.push_str(label);
    pem.push_str("-----\n");
    for chunk in body.as_bytes().chunks(64) {
        pem.push_str(std::str::from_utf8(chunk).expect("base64 output should be valid UTF-8"));
        pem.push('\n');
    }
    pem.push_str("-----END ");
    pem.push_str(label);
    pem.push_str("-----\n");
    pem
}

fn ed25519_public_key_spki(public_key: &[u8]) -> Result<Vec<u8>> {
    if public_key.len() != ED25519_PUBLIC_KEY_LEN {
        bail!("generated Ed25519 public key had unexpected length");
    }

    let mut spki = Vec::with_capacity(ED25519_SPKI_PREFIX.len() + public_key.len());
    spki.extend_from_slice(&ED25519_SPKI_PREFIX);
    spki.extend_from_slice(public_key);
    Ok(spki)
}

fn generate_jwt_keypair() -> Result<(String, String)> {
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
        .map_err(|_| anyhow!("failed to generate Ed25519 keypair"))?;
    let keypair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref())
        .map_err(|_| anyhow!("failed to parse generated Ed25519 keypair"))?;
    let public_der = ed25519_public_key_spki(keypair.public_key().as_ref())?;

    Ok((
        pem_encode("PRIVATE KEY", pkcs8.as_ref()),
        pem_encode("PUBLIC KEY", &public_der),
    ))
}

// ---------------------------------------------------------------------------
// Config TOML generation
// ---------------------------------------------------------------------------

fn root_table_mut(doc: &mut toml::Value) -> Result<&mut toml::Table> {
    doc.as_table_mut()
        .context("settings.toml root is not a table")
}

fn ensure_table<'a>(table: &'a mut toml::Table, key: &str) -> Result<&'a mut toml::Table> {
    table
        .entry(key.to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::default()))
        .as_table_mut()
        .with_context(|| format!("settings.toml [{key}] is not a table"))
}

fn merge_server_settings(doc: &mut toml::Value) -> Result<()> {
    let root = root_table_mut(doc)?;
    root.insert("_version".to_string(), toml::Value::Integer(1));

    let server = ensure_table(root, "server")?;

    let api = ensure_table(server, "api")?;
    api.insert(
        "url".to_string(),
        toml::Value::String("http://127.0.0.1:32276/api/v1".to_string()),
    );

    let listen = ensure_table(server, "listen")?;
    listen.insert("type".to_string(), toml::Value::String("tcp".to_string()));
    listen.insert(
        "address".to_string(),
        toml::Value::String("127.0.0.1:32276".to_string()),
    );

    let web = ensure_table(server, "web")?;
    web.insert("enabled".to_string(), toml::Value::Boolean(true));
    web.insert(
        "url".to_string(),
        toml::Value::String("http://127.0.0.1:32276".to_string()),
    );

    let auth = ensure_table(server, "auth")?;
    auth.insert(
        "methods".to_string(),
        toml::Value::Array(vec![toml::Value::String("dev-token".to_string())]),
    );

    Ok(())
}

fn github_integration_table(doc: &mut toml::Value) -> Result<&mut toml::Table> {
    let root = doc
        .as_table_mut()
        .context("settings.toml root is not a table")?;
    let server = root
        .entry("server")
        .or_insert(toml::Value::Table(toml::Table::default()));
    let server_table = server
        .as_table_mut()
        .context("settings.toml [server] is not a table")?;
    let integrations = server_table
        .entry("integrations")
        .or_insert(toml::Value::Table(toml::Table::default()));
    let integrations_table = integrations
        .as_table_mut()
        .context("settings.toml [server.integrations] is not a table")?;
    let github = integrations_table
        .entry("github")
        .or_insert(toml::Value::Table(toml::Table::default()));
    github
        .as_table_mut()
        .context("settings.toml [server.integrations.github] is not a table")
}

fn write_token_settings(doc: &mut toml::Value) -> Result<()> {
    let github = github_integration_table(doc)?;
    github.insert("strategy".into(), toml::Value::String("token".to_string()));
    github.remove("app_id");
    github.remove("slug");
    github.remove("client_id");
    Ok(())
}

fn write_github_app_settings(
    doc: &mut toml::Value,
    app_id: &str,
    slug: &str,
    client_id: &str,
    allowed_usernames: &[String],
) -> Result<()> {
    anyhow::ensure!(
        !allowed_usernames.is_empty(),
        "GitHub App install requires at least one allowed GitHub username"
    );

    let root = root_table_mut(doc)?;
    let server = ensure_table(root, "server")?;
    let auth = ensure_table(server, "auth")?;
    let methods = auth
        .entry("methods".to_string())
        .or_insert_with(|| toml::Value::Array(Vec::new()))
        .as_array_mut()
        .context("settings.toml [server.auth].methods is not an array")?;
    if !methods.iter().any(|value| value.as_str() == Some("github")) {
        methods.push(toml::Value::String("github".to_string()));
    }
    let github_auth = ensure_table(auth, "github")?;
    github_auth.insert(
        "allowed_usernames".to_string(),
        toml::Value::Array(
            allowed_usernames
                .iter()
                .cloned()
                .map(toml::Value::String)
                .collect(),
        ),
    );

    let github = github_integration_table(doc)?;
    github.insert("strategy".into(), toml::Value::String("app".to_string()));
    github.insert("app_id".into(), toml::Value::String(app_id.to_string()));
    github.insert("slug".into(), toml::Value::String(slug.to_string()));
    github.insert(
        "client_id".into(),
        toml::Value::String(client_id.to_string()),
    );
    Ok(())
}

#[cfg(test)]
fn format_config_toml() -> String {
    let mut doc = toml::Value::Table(toml::Table::default());
    merge_server_settings(&mut doc).expect("default server config should be valid");
    toml::to_string_pretty(&doc).expect("default server config should serialize")
}

// ---------------------------------------------------------------------------
// Binary detection
// ---------------------------------------------------------------------------

/// Check if a binary exists on PATH using the doctor.rs pattern.
async fn detect_binary_on_path(binary: &str) -> bool {
    TokioCommand::new(binary)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Interactive setup
// ---------------------------------------------------------------------------

fn prompt_input(prompt: &str) -> Result<String> {
    Ok(dialoguer::Input::with_theme(&ColorfulTheme::default())
        .with_prompt(prompt)
        .interact_on(&Term::stderr())?)
}

fn prompt_select(prompt: &str, items: &[String]) -> Result<usize> {
    Ok(Select::with_theme(&ColorfulTheme::default())
        .with_prompt(prompt)
        .items(items)
        .interact_on(&Term::stderr())?)
}

fn prompt_multiselect(prompt: &str, items: &[String]) -> Result<Vec<usize>> {
    Ok(MultiSelect::with_theme(&ColorfulTheme::default())
        .with_prompt(prompt)
        .items(items)
        .interact_on(&Term::stderr())?)
}

impl InstallNonInteractiveArgs {
    fn has_any(&self) -> bool {
        self.llm_provider.is_some()
            || self.llm_api_key_stdin
            || self.llm_api_key_env.is_some()
            || self.github_strategy.is_some()
            || self.github_owner.is_some()
            || self.github_username.is_some()
            || self.overwrite_settings
            || self.keep_existing_settings
            || self.run_doctor
    }

    fn first_flag_name(&self) -> Option<&'static str> {
        if self.llm_provider.is_some() {
            Some("--llm-provider")
        } else if self.llm_api_key_stdin {
            Some("--llm-api-key-stdin")
        } else if self.llm_api_key_env.is_some() {
            Some("--llm-api-key-env")
        } else if self.github_strategy.is_some() {
            Some("--github-strategy")
        } else if self.github_owner.is_some() {
            Some("--github-owner")
        } else if self.github_username.is_some() {
            Some("--github-username")
        } else if self.overwrite_settings {
            Some("--overwrite-settings")
        } else if self.keep_existing_settings {
            Some("--keep-existing-settings")
        } else if self.run_doctor {
            Some("--run-doctor")
        } else {
            None
        }
    }
}

fn non_interactive_install_usage() -> &'static str {
    r#"Non-interactive install requires additional flags.

Non-interactive usage:
  fabro install --non-interactive \
    --llm-provider anthropic \
    --llm-api-key-env ANTHROPIC_API_KEY \
    --github-strategy token \
    --github-username brynary

  printf '%s\n' "$ANTHROPIC_API_KEY" | fabro install --non-interactive \
    --llm-provider anthropic \
    --llm-api-key-stdin \
    --github-strategy token \
    --github-username brynary

  fabro install --non-interactive \
    --llm-provider anthropic \
    --llm-api-key-env ANTHROPIC_API_KEY \
    --github-strategy app \
    --github-owner personal

Hidden non-interactive flags:
  --llm-provider <PROVIDER>
  --llm-api-key-stdin
  --llm-api-key-env <ENV_VAR>
  --github-strategy <token|app>
  --github-owner <personal|org:SLUG>
  --github-username <USERNAME>
  --overwrite-settings
  --keep-existing-settings
  --run-doctor

Notes:
  - Only one API-key-based LLM provider is supported in non-interactive mode.
  - GitHub App setup prints a local handoff URL and waits for the browser callback."#
}

#[derive(Debug, Clone)]
struct InstallFacts {
    codex_detected: bool,
}

#[derive(Debug)]
struct LlmInstallSelection {
    credentials: Vec<AuthCredential>,
}

#[derive(Debug)]
enum GitHubInstallSelection {
    Token,
    App {
        owner:    GitHubAppOwner,
        username: Option<String>,
    },
}

#[derive(Debug)]
enum ServerConfigSelection {
    KeepExisting,
    Write,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GitHubAppHandoffMode {
    Interactive,
    Manual,
}

fn install_json_event_line(value: &serde_json::Value) -> Result<String> {
    serde_json::to_string(&value).context("failed to serialize install JSON event")
}

fn emit_install_json_event(value: &serde_json::Value) -> Result<()> {
    let line = install_json_event_line(value)?;
    #[allow(clippy::print_stdout)]
    {
        println!("{line}");
    }
    Ok(())
}

fn install_complete_event() -> serde_json::Value {
    serde_json::json!({
        "event": "install_complete",
        "status": "success",
    })
}

fn install_error_event(message: &str) -> serde_json::Value {
    serde_json::json!({
        "event": "install_error",
        "status": "error",
        "message": message,
    })
}

fn install_github_app_handoff_event(url: &str, owner: &GitHubAppOwner) -> serde_json::Value {
    serde_json::json!({
        "event": "github_app_handoff",
        "url": url,
        "owner": owner.scripted_value(),
    })
}

#[async_trait]
trait InstallInputSource {
    async fn collect_llm_selection(
        &self,
        facts: &InstallFacts,
        s: &Styles,
        printer: Printer,
    ) -> Result<LlmInstallSelection>;

    async fn choose_github_install(
        &self,
        s: &Styles,
        printer: Printer,
    ) -> Result<GitHubInstallSelection>;

    async fn choose_server_config(&self, config_exists: bool) -> Result<ServerConfigSelection>;

    async fn should_run_doctor(&self) -> Result<bool>;
}

struct InteractiveInstallInputSource;

#[async_trait]
impl InstallInputSource for InteractiveInstallInputSource {
    async fn collect_llm_selection(
        &self,
        facts: &InstallFacts,
        s: &Styles,
        printer: Printer,
    ) -> Result<LlmInstallSelection> {
        let mut credentials = Vec::new();
        let mut configured_providers: Vec<Provider> = Vec::new();
        let mut openai_configured = false;

        if facts.codex_detected {
            tracing::debug!("Codex binary detected on PATH");
            let use_device_auth = spawn_blocking(|| {
                prompt_confirm(
                    "OpenAI (Codex) detected. Set up OpenAI with device code login?",
                    true,
                )
            })
            .await??;

            if use_device_auth {
                let credential = authenticate_provider_with_method(
                    Provider::OpenAi,
                    AuthMethod::CodexDevice(codex_oauth_config()),
                    s,
                    printer,
                )
                .await?;
                credentials.push(credential);
                configured_providers.push(Provider::OpenAi);
                openai_configured = true;
            }
        }

        if !openai_configured {
            let primary_providers = [Provider::Anthropic, Provider::OpenAi, Provider::Gemini];
            let primary_labels: Vec<String> = primary_providers
                .iter()
                .map(|p| provider_display_name(*p).to_string())
                .collect();
            let primary_idx: usize = spawn_blocking({
                let labels = primary_labels.clone();
                move || prompt_select("Choose your first LLM provider", &labels)
            })
            .await??;

            let first_provider = primary_providers[primary_idx];
            credentials.push(authenticate_provider(first_provider, s, printer).await?);
            configured_providers.push(first_provider);
        }

        let add_more =
            spawn_blocking(|| prompt_confirm("Set up additional LLM providers?", false)).await??;

        if add_more {
            let remaining_labels: Vec<String> = Provider::ALL
                .iter()
                .filter(|p| !configured_providers.contains(p))
                .map(|p| {
                    let env_vars = p.api_key_env_vars().join(" / ");
                    format!("{} ({})", provider_display_name(*p), env_vars)
                })
                .collect();
            let remaining_providers: Vec<Provider> = Provider::ALL
                .iter()
                .filter(|p| !configured_providers.contains(p))
                .copied()
                .collect();

            let selected_indices: Vec<usize> = spawn_blocking({
                let labels = remaining_labels.clone();
                move || prompt_multiselect("Which additional LLM providers?", &labels)
            })
            .await??;

            for idx in selected_indices {
                let provider = remaining_providers[idx];
                credentials.push(authenticate_provider(provider, s, printer).await?);
            }
        }

        Ok(LlmInstallSelection { credentials })
    }

    async fn choose_github_install(
        &self,
        s: &Styles,
        _printer: Printer,
    ) -> Result<GitHubInstallSelection> {
        let strategy_options = vec![
            "GitHub CLI — use your existing `gh` login".to_string(),
            "GitHub App — recommended for teams".to_string(),
        ];
        let strategy = spawn_blocking({
            let options = strategy_options.clone();
            move || prompt_select("How should Fabro authenticate with GitHub?", &options)
        })
        .await??;

        match strategy {
            0 => Ok(GitHubInstallSelection::Token),
            1 => {
                let (owner, username) = prompt_github_app_owner(s).await?;
                Ok(GitHubInstallSelection::App { owner, username })
            }
            _ => unreachable!("prompt_select returned an out-of-range index"),
        }
    }

    async fn choose_server_config(&self, config_exists: bool) -> Result<ServerConfigSelection> {
        let write_config = if config_exists {
            spawn_blocking(|| {
                prompt_confirm("~/.fabro/settings.toml already exists. Overwrite?", false)
            })
            .await??
        } else {
            true
        };

        if write_config {
            Ok(ServerConfigSelection::Write)
        } else {
            Ok(ServerConfigSelection::KeepExisting)
        }
    }

    async fn should_run_doctor(&self) -> Result<bool> {
        spawn_blocking(|| prompt_confirm("Run fabro doctor to verify?", true)).await?
    }
}

#[derive(Debug)]
struct NonInteractiveInstallInputSource {
    args: InstallNonInteractiveArgs,
}

impl NonInteractiveInstallInputSource {
    fn new(args: &InstallArgs) -> Result<Option<Self>> {
        if !args.non_interactive {
            if let Some(flag) = args.scripted.first_flag_name() {
                bail!("{flag} requires --non-interactive");
            }
            return Ok(None);
        }

        if !args.scripted.has_any() {
            bail!("{}", non_interactive_install_usage());
        }

        anyhow::ensure!(
            args.scripted.llm_api_key_stdin ^ args.scripted.llm_api_key_env.is_some(),
            "non-interactive install requires exactly one of --llm-api-key-stdin or --llm-api-key-env"
        );
        anyhow::ensure!(
            !(args.scripted.overwrite_settings && args.scripted.keep_existing_settings),
            "--overwrite-settings and --keep-existing-settings cannot be used together"
        );

        Ok(Some(Self {
            args: args.scripted.clone(),
        }))
    }

    fn validate(&self, config_exists: bool) -> Result<()> {
        anyhow::ensure!(
            self.args.llm_provider.is_some(),
            "non-interactive install requires --llm-provider"
        );

        match self.args.github_strategy {
            Some(InstallGitHubStrategyArg::Token) => {
                anyhow::ensure!(
                    self.args.github_owner.is_none(),
                    "--github-owner is only supported with --github-strategy app"
                );
            }
            Some(InstallGitHubStrategyArg::App) => {
                let owner = self.args.github_owner.as_deref().context(
                    "non-interactive install requires --github-owner for --github-strategy app",
                )?;
                GitHubAppOwner::parse_scripted(owner)?;
                anyhow::ensure!(
                    self.args.github_username.is_none(),
                    "--github-username is only supported with --github-strategy token"
                );
            }
            None => bail!("non-interactive install requires --github-strategy"),
        }

        if config_exists {
            anyhow::ensure!(
                self.args.keep_existing_settings || self.args.overwrite_settings,
                "settings.toml already exists; pass --overwrite-settings or --keep-existing-settings"
            );

            if self.args.keep_existing_settings {
                return Ok(());
            }
        }

        if matches!(
            self.args.github_strategy,
            Some(InstallGitHubStrategyArg::Token)
        ) {
            anyhow::ensure!(
                self.args.github_username.is_some(),
                "non-interactive install requires --github-username for --github-strategy token"
            );
        }

        Ok(())
    }

    fn api_key_source(&self) -> Result<ApiKeySource> {
        if self.args.llm_api_key_stdin {
            Ok(ApiKeySource::Stdin)
        } else if let Some(name) = &self.args.llm_api_key_env {
            Ok(ApiKeySource::EnvVar(name.clone()))
        } else {
            bail!(
                "non-interactive install requires exactly one of --llm-api-key-stdin or --llm-api-key-env"
            )
        }
    }
}

#[async_trait]
impl InstallInputSource for NonInteractiveInstallInputSource {
    async fn collect_llm_selection(
        &self,
        _facts: &InstallFacts,
        s: &Styles,
        printer: Printer,
    ) -> Result<LlmInstallSelection> {
        let provider = self
            .args
            .llm_provider
            .context("non-interactive install requires --llm-provider")?;
        let credential =
            authenticate_provider_with_api_key_source(provider, self.api_key_source()?, s, printer)
                .await?;
        Ok(LlmInstallSelection {
            credentials: vec![credential],
        })
    }

    async fn choose_github_install(
        &self,
        _s: &Styles,
        _printer: Printer,
    ) -> Result<GitHubInstallSelection> {
        match self.args.github_strategy {
            Some(InstallGitHubStrategyArg::Token) => Ok(GitHubInstallSelection::Token),
            Some(InstallGitHubStrategyArg::App) => Ok(GitHubInstallSelection::App {
                owner:    GitHubAppOwner::parse_scripted(
                    self.args.github_owner.as_deref().context(
                        "non-interactive install requires --github-owner for --github-strategy app",
                    )?,
                )?,
                username: best_effort_github_username().await,
            }),
            None => bail!("non-interactive install requires --github-strategy"),
        }
    }

    async fn choose_server_config(&self, config_exists: bool) -> Result<ServerConfigSelection> {
        if config_exists {
            if self.args.keep_existing_settings {
                return Ok(ServerConfigSelection::KeepExisting);
            }
            anyhow::ensure!(
                self.args.overwrite_settings,
                "settings.toml already exists; pass --overwrite-settings or --keep-existing-settings"
            );
        }

        Ok(ServerConfigSelection::Write)
    }

    async fn should_run_doctor(&self) -> Result<bool> {
        Ok(self.args.run_doctor)
    }
}

// ---------------------------------------------------------------------------
// GitHub App owner selection
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum GitHubAppOwner {
    Personal,
    Organization(String),
}

impl GitHubAppOwner {
    fn parse_scripted(value: &str) -> Result<Self> {
        if value == "personal" {
            return Ok(Self::Personal);
        }

        let Some(org) = value.strip_prefix("org:") else {
            bail!("--github-owner must be 'personal' or 'org:<slug>'");
        };
        anyhow::ensure!(
            !org.trim().is_empty(),
            "--github-owner organization slug cannot be empty"
        );
        Ok(Self::Organization(org.to_string()))
    }

    fn manifest_form_action(&self) -> String {
        match self {
            Self::Personal => "https://github.com/settings/apps/new".to_string(),
            Self::Organization(org) => {
                format!("https://github.com/organizations/{org}/settings/apps/new")
            }
        }
    }

    fn scripted_value(&self) -> String {
        match self {
            Self::Personal => "personal".to_string(),
            Self::Organization(org) => format!("org:{org}"),
        }
    }

    fn app_name(&self, username: Option<&str>) -> String {
        match self {
            Self::Organization(org) => format!("{org}-fabro"),
            Self::Personal => {
                if let Some(user) = username {
                    format!("{user}-fabro")
                } else {
                    let mut rng = rand::thread_rng();
                    let suffix: String = (0..6).fold(String::with_capacity(6), |mut s, _| {
                        use std::fmt::Write;
                        let _ = write!(s, "{:x}", rng.gen::<u8>() % 16);
                        s
                    });
                    format!("Fabro-{suffix}")
                }
            }
        }
    }
}

async fn best_effort_github_username() -> Option<String> {
    let gh = GhCli::detect().await?;
    gh.authenticated_user().await
}

/// Ask the user where to create the GitHub App.
///
/// Uses the `gh` CLI to discover the username and admin orgs. If `gh` is
/// unavailable or the user has no admin orgs, falls back gracefully.
/// Always offers a manual "Other" option so org app managers can enter a slug.
///
/// Returns `(owner, username)`.
async fn prompt_github_app_owner(_s: &Styles) -> Result<(GitHubAppOwner, Option<String>)> {
    let spinner = indicatif::ProgressBar::new_spinner();
    spinner.set_style(
        indicatif::ProgressStyle::with_template("{spinner:.cyan} {msg}")
            .expect("valid template")
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", ""]),
    );
    spinner.set_message("Checking GitHub CLI...");
    spinner.enable_steady_tick(std::time::Duration::from_millis(80));

    let Some(gh) = GhCli::detect().await else {
        spinner.finish_and_clear();
        return Ok((GitHubAppOwner::Personal, None));
    };

    let (username, orgs) = tokio::join!(gh.authenticated_user(), gh.list_admin_orgs());
    spinner.finish_and_clear();

    // Build the selection menu
    let personal_label = match &username {
        Some(user) => format!("Personal account ({user})"),
        None => "Personal account".to_string(),
    };
    let mut items = vec![personal_label];
    for org in &orgs {
        items.push(format!("Organization: {org}"));
    }
    items.push("Other (enter organization name)".to_string());

    let selected: usize = spawn_blocking({
        let items = items.clone();
        move || prompt_select("Where should the GitHub App be created?", &items)
    })
    .await??;

    let other_index = 1 + orgs.len();
    let owner = if selected == 0 {
        GitHubAppOwner::Personal
    } else if selected == other_index {
        let org_slug: String = spawn_blocking(|| prompt_input("Organization name")).await??;
        GitHubAppOwner::Organization(org_slug)
    } else {
        GitHubAppOwner::Organization(orgs[selected - 1].clone())
    };

    Ok((owner, username))
}

// ---------------------------------------------------------------------------
// GitHub App manifest flow
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct CallbackParams {
    code: String,
}

fn build_github_app_manifest(app_name: &str, port: u16, web_url: &str) -> serde_json::Value {
    serde_json::json!({
        "name": app_name,
        "url": "https://fabro.sh",
        "redirect_url": format!("http://127.0.0.1:{port}/callback"),
        "callback_urls": [format!("{web_url}/auth/callback/github")],
        "setup_url": format!("{web_url}/setup"),
        "public": false,
        "default_permissions": {
            "contents": "write",
            "metadata": "read",
            "pull_requests": "write",
            "checks": "write",
            "issues": "write",
            "emails": "read"
        },
        "default_events": []
    })
}

/// Run the GitHub App manifest registration flow via a temporary local server.
/// Returns the app metadata and secret pairs to persist for the local server.
struct GitHubAppRegistration {
    app_id:    String,
    slug:      String,
    client_id: String,
    env_pairs: Vec<(String, String)>,
}

enum PendingGitHubSettings {
    Token,
    App {
        app_id:            String,
        slug:              String,
        client_id:         String,
        allowed_usernames: Vec<String>,
    },
}

#[derive(Clone, Copy)]
struct PendingSettingsWrite<'a> {
    path:              &'a Path,
    contents:          &'a str,
    previous_contents: Option<&'a str>,
}

async fn setup_github_app(
    s: &Styles,
    web_url: &str,
    owner: &GitHubAppOwner,
    username: Option<&str>,
    handoff_mode: GitHubAppHandoffMode,
    json_output: bool,
    printer: Printer,
) -> Result<GitHubAppRegistration> {
    let app_name = owner.app_name(username);

    // Bind to random port
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("failed to bind local server")?;
    let addr: SocketAddr = listener.local_addr()?;
    let port = addr.port();

    let manifest = build_github_app_manifest(&app_name, port, web_url);
    let manifest_json = serde_json::to_string(&manifest)?;
    let escaped_manifest = manifest_json
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;");

    // Channel to receive the code from the callback
    let (code_tx, code_rx) = oneshot::channel::<String>();
    // Channel to trigger graceful shutdown
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    let code_tx = std::sync::Arc::new(std::sync::Mutex::new(Some(code_tx)));
    let shutdown_tx = std::sync::Arc::new(std::sync::Mutex::new(Some(shutdown_tx)));

    let form_action = owner.manifest_form_action();
    let index_html = format!(
        r#"<!DOCTYPE html>
<html>
<body>
  <p>Redirecting to GitHub...</p>
  <form id="f" method="post" action="{form_action}">
    <input type="hidden" name="manifest" value="{escaped_manifest}">
  </form>
  <script>document.getElementById('f').submit();</script>
</body>
</html>"#
    );

    let app = axum::Router::new()
        .route(
            "/",
            get(move || async move { Html(index_html.clone()) }),
        )
        .route(
            "/callback",
            get(move |Query(params): Query<CallbackParams>| async move {
                if let Some(tx) = code_tx.lock().unwrap().take() {
                    let _ = tx.send(params.code);
                }
                if let Some(tx) = shutdown_tx.lock().unwrap().take() {
                    let _ = tx.send(());
                }
                Html(r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<title>Fabro Setup</title>
<style>
  body { font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Helvetica, Arial, sans-serif; display: flex; justify-content: center; align-items: center; min-height: 100vh; margin: 0; background: #f6f8fa; color: #1f2328; }
  .card { text-align: center; background: #fff; border: 1px solid #d1d9e0; border-radius: 12px; padding: 48px; max-width: 420px; }
  .check { font-size: 48px; margin-bottom: 16px; }
  h1 { font-size: 20px; font-weight: 600; margin: 0 0 8px; }
  p { font-size: 14px; color: #59636e; margin: 0; }
</style>
</head>
<body>
<div class="card">
  <div class="check">&#10003;</div>
  <h1>GitHub App created</h1>
  <p>You can close this tab and return to your terminal.</p>
</div>
</body>
</html>"#.to_string())
            }),
        );

    // Spawn server with graceful shutdown
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
            .ok();
    });

    let url = format!("http://127.0.0.1:{port}/");
    if json_output {
        emit_install_json_event(&install_github_app_handoff_event(&url, owner))?;
    }

    match handoff_mode {
        GitHubAppHandoffMode::Interactive => {
            fabro_util::printerr!(printer, "  {}", s.dim.apply_to("Opening browser..."));
            if let Err(e) = open::that(&url) {
                fabro_util::printerr!(printer, "  Could not open browser automatically: {e}");
                fabro_util::printerr!(printer, "  Please open this URL manually: {url}");
            }
        }
        GitHubAppHandoffMode::Manual => {
            if !json_output {
                fabro_util::printerr!(printer, "  Open this URL manually to continue setup:");
                fabro_util::printerr!(printer, "  {url}");
            }
        }
    }

    if !json_output {
        fabro_util::printerr!(
            printer,
            "  {}",
            s.dim.apply_to("Waiting for GitHub... (Ctrl+C to cancel)")
        );
    }

    // Wait for the code
    let code = code_rx
        .await
        .context("did not receive callback from GitHub (was the browser flow completed?)")?;

    // Exchange code for app credentials
    fabro_util::printerr!(
        printer,
        "  {}",
        s.dim.apply_to("Exchanging code with GitHub...")
    );
    let client = fabro_http::http_client()?;
    let resp = client
        .post(format!(
            "https://api.github.com/app-manifests/{code}/conversions"
        ))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "fabro-cli")
        .send()
        .await
        .context("failed to exchange code with GitHub")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("GitHub manifest conversion failed ({status}): {body}");
    }

    let body: serde_json::Value = resp.json().await.context("invalid JSON from GitHub")?;

    let app_id = body["id"]
        .as_i64()
        .context("missing 'id' in GitHub response")?
        .to_string();
    let slug = body["slug"]
        .as_str()
        .context("missing 'slug' in GitHub response")?
        .to_string();
    let client_id = body["client_id"]
        .as_str()
        .context("missing 'client_id' in GitHub response")?
        .to_string();
    let client_secret = body["client_secret"]
        .as_str()
        .context("missing 'client_secret' in GitHub response")?
        .to_string();
    let webhook_secret = body["webhook_secret"].as_str().map(String::from);
    let pem = body["pem"]
        .as_str()
        .context("missing 'pem' in GitHub response")?
        .to_string();

    fabro_util::printerr!(
        printer,
        "  {}",
        s.dim
            .apply_to(format!("App: https://github.com/apps/{slug}"))
    );

    // Return secret pairs
    let pem_b64 = BASE64_STANDARD.encode(pem.as_bytes());

    let mut env_pairs = vec![
        ("GITHUB_APP_PRIVATE_KEY".to_string(), pem_b64),
        ("GITHUB_APP_CLIENT_SECRET".to_string(), client_secret),
    ];
    if let Some(secret) = webhook_secret {
        env_pairs.push(("GITHUB_APP_WEBHOOK_SECRET".to_string(), secret));
    }

    Ok(GitHubAppRegistration {
        app_id,
        slug,
        client_id,
        env_pairs,
    })
}

async fn persist_vault_secrets_via_server(
    client: &fabro_api::Client,
    secrets: &[CreateSecretRequest],
) -> Result<()> {
    for secret in secrets {
        client
            .create_secret()
            .body(CreateSecretRequest {
                name:        secret.name.clone(),
                value:       secret.value.clone(),
                type_:       secret.type_,
                description: secret.description.clone(),
            })
            .send()
            .await?;
    }

    Ok(())
}

async fn persist_vault_secrets_with(
    storage_dir: &Path,
    secrets: &[CreateSecretRequest],
    server_was_running: bool,
    connect_api_client: impl for<'a> Fn(&'a Path) -> BoxFuture<'a, Result<fabro_api::Client>>,
    stop_server: impl for<'a> Fn(&'a Path, Duration) -> BoxFuture<'a, bool>,
) -> Result<()> {
    if secrets.is_empty() {
        return Ok(());
    }

    let client = match connect_api_client(storage_dir).await {
        Ok(client) => client,
        Err(err) => {
            if !server_was_running {
                stop_server(storage_dir, Duration::from_secs(5)).await;
            }
            return Err(err);
        }
    };
    let result = persist_vault_secrets_via_server(&client, secrets).await;
    if !server_was_running {
        stop_server(storage_dir, Duration::from_secs(5)).await;
    }
    result
}

fn credential_secret_request(credential: &AuthCredential) -> Result<CreateSecretRequest> {
    Ok(CreateSecretRequest {
        name:        credential_id_for(credential).map_err(anyhow::Error::msg)?,
        value:       serde_json::to_string(credential)?,
        type_:       ApiSecretType::Credential,
        description: None,
    })
}

fn persist_server_env_secrets(storage_dir: &Path, secrets: &[(String, String)]) -> Result<()> {
    if secrets.is_empty() {
        return Ok(());
    }

    envfile::merge_env_file(
        &Storage::new(storage_dir).server_state().env_path(),
        secrets.iter().cloned(),
    )?;
    Ok(())
}

async fn persist_install_outputs(
    storage_dir: &Path,
    server_env_secrets: &[(String, String)],
    vault_secrets: &[CreateSecretRequest],
    settings_write: Option<PendingSettingsWrite<'_>>,
    server_was_running: bool,
) -> Result<()> {
    persist_install_outputs_with_settings(
        storage_dir,
        server_env_secrets,
        vault_secrets,
        settings_write,
        server_was_running,
        |path| Box::pin(server_client::connect_api_client(path)),
        |path, timeout| Box::pin(stop::stop_server(path, timeout)),
    )
    .await
}

async fn persist_install_outputs_with_settings(
    storage_dir: &Path,
    server_env_secrets: &[(String, String)],
    vault_secrets: &[CreateSecretRequest],
    settings_write: Option<PendingSettingsWrite<'_>>,
    server_was_running: bool,
    connect_api_client: impl for<'a> Fn(&'a Path) -> BoxFuture<'a, Result<fabro_api::Client>>,
    stop_server: impl for<'a> Fn(&'a Path, Duration) -> BoxFuture<'a, bool>,
) -> Result<()> {
    persist_server_env_secrets(storage_dir, server_env_secrets)?;

    if let Some(write) = settings_write {
        std::fs::write(write.path, write.contents)?;
    }

    let persist_result = persist_vault_secrets_with(
        storage_dir,
        vault_secrets,
        server_was_running,
        connect_api_client,
        stop_server,
    )
    .await;

    if let Err(err) = persist_result {
        if let Some(write) = settings_write {
            match write.previous_contents {
                Some(previous) => std::fs::write(write.path, previous)?,
                None if write.path.exists() => std::fs::remove_file(write.path)?,
                None => {}
            }
        }
        return Err(err);
    }

    Ok(())
}

pub(crate) async fn run_install(
    args: &InstallArgs,
    cli: &CliSettings,
    cli_layer: &CliLayer,
    process_local_json: bool,
    printer: Printer,
) -> Result<()> {
    let json = cli.output.format == OutputFormat::Json;
    if process_local_json && !args.non_interactive {
        bail!("--json is only supported for install with --non-interactive");
    }

    let result = Box::pin(run_install_inner(args, cli, cli_layer, printer)).await;
    if json {
        let emit_result = match &result {
            Ok(()) => emit_install_json_event(&install_complete_event()),
            Err(err) => emit_install_json_event(&install_error_event(&err.to_string())),
        };
        if result.is_ok() {
            emit_result?;
        }
    }

    result
}

async fn run_install_inner(
    args: &InstallArgs,
    cli: &CliSettings,
    cli_layer: &CliLayer,
    printer: Printer,
) -> Result<()> {
    let json = cli.output.format == OutputFormat::Json;
    let web_url = &args.web_url;
    let s = Styles::detect_stderr();
    let emoji = console::Emoji("⚒️  ", "");
    let cli_settings = user_config::load_settings_with_storage_dir(args.storage_dir.as_deref())?;
    let storage_dir = user_config::storage_dir(&cli_settings)?;
    let server_was_running = record::active_server_record(&storage_dir).is_some();
    let fabro_dir = fabro_util::Home::from_env().root().to_path_buf();
    let config_path = fabro_dir.join(SETTINGS_CONFIG_FILENAME);
    let existing_config_contents = std::fs::read_to_string(&config_path).ok();
    let config_existed_before_install = config_path.exists();
    let input_source: Box<dyn InstallInputSource + Send + Sync> =
        match NonInteractiveInstallInputSource::new(args)? {
            Some(source) => {
                source.validate(config_existed_before_install)?;
                Box::new(source)
            }
            None => Box::new(InteractiveInstallInputSource),
        };

    fabro_util::printerr!(printer, "");
    fabro_util::printerr!(printer, "  {}{}", emoji, s.bold.apply_to("Fabro Install"));
    fabro_util::printerr!(printer, "");
    fabro_util::printerr!(
        printer,
        "  {}",
        s.dim
            .apply_to("Let's get Fabro set up. This will configure your")
    );
    fabro_util::printerr!(
        printer,
        "  {}",
        s.dim.apply_to("LLM providers and GitHub access.")
    );
    fabro_util::printerr!(printer, "");

    std::fs::create_dir_all(&fabro_dir)?;

    {
        let env_path = legacy_env::legacy_env_file_path();
        if env_path.exists() {
            fabro_util::printerr!(
                printer,
                "  Warning: {} is no longer read by fabro server. This install will persist runtime secrets in server.env and workflow-visible credentials in the vault instead.",
                env_path.display()
            );
            fabro_util::printerr!(printer, "");
        }
    }

    let facts = InstallFacts {
        codex_detected: detect_binary_on_path("codex").await,
    };

    // Step 1: LLM Providers
    fabro_util::printerr!(printer, "  {}", s.bold.apply_to("Step 1 · LLM Providers"));
    fabro_util::printerr!(printer, "  {}", s.dim.apply_to("──────────────────────"));
    fabro_util::printerr!(printer, "");

    let mut vault_secrets: Vec<CreateSecretRequest> = Vec::new();
    let mut server_env_pairs: Vec<(String, String)> = Vec::new();
    let llm_selection = input_source
        .collect_llm_selection(&facts, &s, printer)
        .await?;
    for credential in llm_selection.credentials {
        vault_secrets.push(credential_secret_request(&credential)?);
    }
    fabro_util::printerr!(printer, "");

    // Step 2: GitHub
    fabro_util::printerr!(printer, "  {}", s.bold.apply_to("Step 2 · GitHub"));
    fabro_util::printerr!(printer, "  {}", s.dim.apply_to("───────────────"));
    fabro_util::printerr!(printer, "");

    let pending_github_settings = match input_source.choose_github_install(&s, printer).await? {
        GitHubInstallSelection::Token => {
            let token = fabro_github::gh_auth_token()
                .await
                .map_err(|err| anyhow!("{err}. Run `gh auth login` and rerun `fabro install`."))?;
            fabro_util::printerr!(
                printer,
                "  {} GitHub token configured",
                s.green.apply_to("✔")
            );
            vault_secrets.push(CreateSecretRequest {
                name:        "GITHUB_TOKEN".to_string(),
                value:       token,
                type_:       ApiSecretType::Environment,
                description: None,
            });
            Some(PendingGitHubSettings::Token)
        }
        GitHubInstallSelection::App { owner, username } => {
            let allowed_username = username.clone().context(
                "GitHub App install requires an authenticated GitHub username; run `gh auth login` and rerun `fabro install`",
            )?;
            let registration = setup_github_app(
                &s,
                web_url,
                &owner,
                username.as_deref(),
                if args.non_interactive {
                    GitHubAppHandoffMode::Manual
                } else {
                    GitHubAppHandoffMode::Interactive
                },
                json,
                printer,
            )
            .await?;
            fabro_util::printerr!(
                printer,
                "  {} GitHub App registered ({})",
                s.green.apply_to("✔"),
                registration.slug
            );
            server_env_pairs.extend(registration.env_pairs.iter().cloned());
            Some(PendingGitHubSettings::App {
                app_id:            registration.app_id,
                slug:              registration.slug,
                client_id:         registration.client_id,
                allowed_usernames: vec![allowed_username],
            })
        }
    };
    fabro_util::printerr!(printer, "");

    // Server configuration
    let settings_toml = {
        fabro_util::printerr!(printer, "  {}", s.bold.apply_to("Server · Configuration"));
        fabro_util::printerr!(printer, "  {}", s.dim.apply_to("─────────────────────"));
        fabro_util::printerr!(printer, "");

        let existing = existing_config_contents.clone().unwrap_or_default();
        let mut doc: toml::Value = if existing.is_empty() {
            toml::Value::Table(toml::Table::default())
        } else {
            toml::from_str(&existing).context("failed to parse existing settings.toml")?
        };

        match input_source
            .choose_server_config(config_existed_before_install)
            .await?
        {
            ServerConfigSelection::KeepExisting => {
                fabro_util::printerr!(
                    printer,
                    "  {}",
                    s.dim.apply_to("Keeping existing settings.toml")
                );
            }
            ServerConfigSelection::Write => {
                merge_server_settings(&mut doc)?;
            }
        }

        match pending_github_settings {
            Some(PendingGitHubSettings::Token) => {
                write_token_settings(&mut doc)?;
            }
            Some(PendingGitHubSettings::App {
                app_id,
                slug,
                client_id,
                allowed_usernames,
            }) => {
                write_github_app_settings(
                    &mut doc,
                    &app_id,
                    &slug,
                    &client_id,
                    &allowed_usernames,
                )?;
            }
            None => {}
        }

        fabro_util::printerr!(printer, "");
        toml::to_string_pretty(&doc)?
    };

    // Secrets and auth material
    {
        fabro_util::printerr!(
            printer,
            "  {}",
            s.dim.apply_to("Generating secrets and auth material...")
        );

        let session_secret = session_secret::generate_session_secret();
        fabro_util::printerr!(
            printer,
            "  {} Session secret generated",
            s.green.apply_to("✔")
        );

        let (jwt_private_pem, jwt_public_pem) = generate_jwt_keypair()?;
        fabro_util::printerr!(
            printer,
            "  {} Ed25519 JWT keypair generated",
            s.green.apply_to("✔")
        );

        let dev_token =
            dev_token::load_or_create_dev_token(&fabro_util::Home::from_env().dev_token_path())?;
        dev_token::write_dev_token(
            &Storage::new(&storage_dir).server_state().dev_token_path(),
            &dev_token,
        )?;
        fabro_util::printerr!(
            printer,
            "  {} Development token generated",
            s.green.apply_to("✔")
        );

        let jwt_private_b64 = BASE64_STANDARD.encode(jwt_private_pem.as_bytes());
        let jwt_public_b64 = BASE64_STANDARD.encode(jwt_public_pem.as_bytes());

        let generated_server_env_pairs = vec![
            ("FABRO_JWT_PRIVATE_KEY".to_string(), jwt_private_b64),
            ("FABRO_JWT_PUBLIC_KEY".to_string(), jwt_public_b64),
            ("SESSION_SECRET".to_string(), session_secret),
            ("FABRO_DEV_TOKEN".to_string(), dev_token),
        ];
        server_env_pairs.extend(generated_server_env_pairs);
        fabro_util::printerr!(printer, "");

        fabro_util::printerr!(printer, "  To start Fabro, run these commands:");
        fabro_util::printerr!(printer, "");
        fabro_util::printerr!(printer, "    fabro server start");
        fabro_util::printerr!(printer, "");
    }

    persist_install_outputs(
        &storage_dir,
        &server_env_pairs,
        &vault_secrets,
        Some(PendingSettingsWrite {
            path:              &config_path,
            contents:          settings_toml.as_str(),
            previous_contents: existing_config_contents.as_deref(),
        }),
        server_was_running,
    )
    .await?;
    fabro_util::printerr!(
        printer,
        "  {} Saved {} runtime secrets to {}",
        s.green.apply_to("✔"),
        server_env_pairs.len(),
        Storage::new(&storage_dir)
            .server_state()
            .env_path()
            .display()
    );
    fabro_util::printerr!(
        printer,
        "  {} Saved {} workflow-visible secrets to {}",
        s.green.apply_to("✔"),
        vault_secrets.len(),
        Storage::new(&storage_dir).secrets_path().display()
    );
    fabro_util::printerr!(
        printer,
        "  {} Wrote {}",
        s.green.apply_to("✔"),
        config_path.display()
    );
    if server_was_running {
        fabro_util::printerr!(
            printer,
            "  Warning: the local fabro server was already running. Restart it to pick up the new server.env values."
        );
    }
    fabro_util::printerr!(printer, "");

    // Verify setup
    let run_doctor = input_source.should_run_doctor().await?;

    if run_doctor {
        fabro_util::printerr!(printer, "");
        let doctor_args = DoctorArgs {
            target:  ServerTargetArgs::default(),
            verbose: true,
        };
        let _ = doctor::run_doctor(&doctor_args, true, cli, cli_layer, printer).await?;
    }

    fabro_util::printerr!(printer, "");
    fabro_util::printerr!(
        printer,
        "  Setup complete! Go to your project and run {} to get started.",
        s.bold_cyan.apply_to("fabro repo init")
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    #![allow(clippy::absolute_paths)]

    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use httpmock::Method::POST;
    use httpmock::MockServer;

    use super::*;

    fn install_args(non_interactive: bool, scripted: InstallNonInteractiveArgs) -> InstallArgs {
        InstallArgs {
            storage_dir: crate::args::StorageDirArgs::default(),
            web_url: "http://localhost:3000".to_string(),
            non_interactive,
            scripted,
        }
    }

    // -- Binary detection --

    #[tokio::test]
    async fn detect_binary_finds_existing_command() {
        assert!(detect_binary_on_path("git").await);
    }

    #[tokio::test]
    async fn detect_binary_returns_false_for_nonexistent() {
        assert!(!detect_binary_on_path("arc_nonexistent_xyz").await);
    }

    // -- Session secret --

    #[test]
    fn session_secret_length() {
        let secret = fabro_util::session_secret::generate_session_secret();
        assert_eq!(secret.len(), 64);
    }

    #[test]
    fn session_secret_is_hex() {
        let secret = fabro_util::session_secret::generate_session_secret();
        assert!(secret.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn session_secret_is_lowercase() {
        let secret = fabro_util::session_secret::generate_session_secret();
        assert!(secret.chars().all(|c| !c.is_ascii_uppercase()));
    }

    // -- JWT keypair --

    #[tokio::test]
    async fn jwt_keypair_private_pem_header() {
        let (private, _) = generate_jwt_keypair().unwrap();
        assert!(
            private.starts_with("-----BEGIN PRIVATE KEY-----"),
            "private PEM: {private}"
        );
    }

    #[tokio::test]
    async fn jwt_keypair_public_pem_header() {
        let (_, public) = generate_jwt_keypair().unwrap();
        assert!(
            public.starts_with("-----BEGIN PUBLIC KEY-----"),
            "public PEM: {public}"
        );
    }

    #[tokio::test]
    async fn jwt_keypair_public_parses() {
        let (_, public) = generate_jwt_keypair().unwrap();
        jsonwebtoken::DecodingKey::from_ed_pem(public.as_bytes()).expect("public key should parse");
    }

    #[tokio::test]
    async fn jwt_keypair_private_parses() {
        let (private, _) = generate_jwt_keypair().unwrap();
        jsonwebtoken::EncodingKey::from_ed_pem(private.as_bytes())
            .expect("private key should parse");
    }

    // -- Config TOML generation --

    #[test]
    fn config_toml_roundtrips() {
        use fabro_types::settings::SettingsLayer;
        let toml_str = format_config_toml();
        let cfg: SettingsLayer = fabro_config::parse_settings_layer(&toml_str)
            .expect("generated config should parse as v2");
        let methods = cfg
            .server
            .as_ref()
            .and_then(|s| s.auth.as_ref())
            .and_then(|a| a.methods.clone())
            .expect("server.auth.methods should be set");
        assert_eq!(methods, vec![
            fabro_types::settings::ServerAuthMethod::DevToken
        ]);
    }

    #[test]
    fn config_toml_has_auth_strategies() {
        use fabro_types::settings::SettingsLayer;
        let toml_str = format_config_toml();
        let cfg: SettingsLayer = fabro_config::parse_settings_layer(&toml_str).unwrap();
        let auth = cfg
            .server
            .as_ref()
            .and_then(|s| s.auth.as_ref())
            .expect("server.auth should be set");
        assert_eq!(
            auth.methods,
            Some(vec![fabro_types::settings::ServerAuthMethod::DevToken])
        );
    }

    #[test]
    fn config_toml_has_tcp_listen_address() {
        use fabro_types::settings::SettingsLayer;
        use fabro_types::settings::server::ServerListenLayer;
        let toml_str = format_config_toml();
        let cfg: SettingsLayer = fabro_config::parse_settings_layer(&toml_str).unwrap();
        let listen = cfg
            .server
            .as_ref()
            .and_then(|s| s.listen.as_ref())
            .expect("server.listen should be set");
        match listen {
            ServerListenLayer::Tcp { address, tls } => {
                assert_eq!(
                    address
                        .as_ref()
                        .map(fabro_types::settings::InterpString::as_source),
                    Some("127.0.0.1:32276".to_string())
                );
                assert!(tls.is_none());
            }
            ServerListenLayer::Unix { .. } => panic!("expected tcp listen"),
        }
    }

    #[test]
    fn merge_server_settings_preserves_existing_top_level_sections() {
        let mut doc: toml::Value = toml::from_str(
            r#"
_version = 1

[project]
name = "custom"
"#,
        )
        .unwrap();

        merge_server_settings(&mut doc).unwrap();

        // Existing top-level [project] stays.
        assert_eq!(
            doc.get("project")
                .and_then(toml::Value::as_table)
                .and_then(|p| p.get("name"))
                .and_then(toml::Value::as_str),
            Some("custom")
        );
        // New server.auth.methods is added.
        assert_eq!(
            doc.get("server")
                .and_then(toml::Value::as_table)
                .and_then(|s| s.get("auth"))
                .and_then(toml::Value::as_table)
                .and_then(|a| a.get("methods"))
                .and_then(toml::Value::as_array)
                .and_then(|methods| methods.first())
                .and_then(toml::Value::as_str),
            Some("dev-token")
        );
    }

    #[test]
    fn write_token_settings_uses_server_integrations_github() {
        let mut doc: toml::Value = toml::from_str(
            r#"
_version = 1

[server.integrations.github]
strategy = "app"
app_id = "123"
slug = "fabro-app"
client_id = "client-id"
"#,
        )
        .unwrap();

        write_token_settings(&mut doc).unwrap();

        let github = doc
            .get("server")
            .and_then(toml::Value::as_table)
            .and_then(|server| server.get("integrations"))
            .and_then(toml::Value::as_table)
            .and_then(|integrations| integrations.get("github"))
            .and_then(toml::Value::as_table)
            .expect("server.integrations.github should exist");

        assert_eq!(
            github.get("strategy").and_then(toml::Value::as_str),
            Some("token")
        );
        assert!(!github.contains_key("app_id"));
        assert!(!github.contains_key("slug"));
        assert!(!github.contains_key("client_id"));
    }

    #[test]
    fn write_github_app_settings_uses_server_integrations_github() {
        let mut doc = toml::Value::Table(toml::Table::default());
        merge_server_settings(&mut doc).unwrap();

        write_github_app_settings(&mut doc, "123", "fabro-app", "client-id", &[
            "brynary".to_string()
        ])
        .unwrap();

        let github = doc
            .get("server")
            .and_then(toml::Value::as_table)
            .and_then(|server| server.get("integrations"))
            .and_then(toml::Value::as_table)
            .and_then(|integrations| integrations.get("github"))
            .and_then(toml::Value::as_table)
            .expect("server.integrations.github should exist");

        assert_eq!(
            github.get("strategy").and_then(toml::Value::as_str),
            Some("app")
        );
        assert_eq!(
            github.get("app_id").and_then(toml::Value::as_str),
            Some("123")
        );
        assert_eq!(
            github.get("slug").and_then(toml::Value::as_str),
            Some("fabro-app")
        );
        assert_eq!(
            github.get("client_id").and_then(toml::Value::as_str),
            Some("client-id")
        );

        let methods = doc
            .get("server")
            .and_then(toml::Value::as_table)
            .and_then(|server| server.get("auth"))
            .and_then(toml::Value::as_table)
            .and_then(|auth| auth.get("methods"))
            .and_then(toml::Value::as_array)
            .expect("server.auth.methods should exist");

        assert_eq!(
            methods
                .iter()
                .map(|value| value.as_str().expect("auth method should be a string"))
                .collect::<Vec<_>>(),
            vec!["dev-token", "github"]
        );

        let allowed_usernames = doc
            .get("server")
            .and_then(toml::Value::as_table)
            .and_then(|server| server.get("auth"))
            .and_then(toml::Value::as_table)
            .and_then(|auth| auth.get("github"))
            .and_then(toml::Value::as_table)
            .and_then(|github| github.get("allowed_usernames"))
            .and_then(toml::Value::as_array)
            .expect("server.auth.github.allowed_usernames should exist");

        assert_eq!(
            allowed_usernames
                .iter()
                .map(|value| value.as_str().expect("username should be a string"))
                .collect::<Vec<_>>(),
            vec!["brynary"]
        );
    }

    #[test]
    fn write_github_app_settings_requires_allowed_usernames() {
        let mut doc = toml::Value::Table(toml::Table::default());
        let err =
            write_github_app_settings(&mut doc, "123", "fabro-app", "client-id", &[]).unwrap_err();
        assert!(
            err.to_string()
                .contains("GitHub App install requires at least one allowed GitHub username")
        );
    }

    // -- GitHub App owner --

    #[test]
    fn github_app_owner_personal_url() {
        let owner = GitHubAppOwner::Personal;
        assert_eq!(
            owner.manifest_form_action(),
            "https://github.com/settings/apps/new"
        );
    }

    #[test]
    fn github_app_owner_org_url() {
        let owner = GitHubAppOwner::Organization("my-org".to_string());
        assert_eq!(
            owner.manifest_form_action(),
            "https://github.com/organizations/my-org/settings/apps/new"
        );
    }

    #[test]
    fn github_app_owner_parses_personal_scripted_value() {
        assert_eq!(
            GitHubAppOwner::parse_scripted("personal").unwrap(),
            GitHubAppOwner::Personal
        );
    }

    #[test]
    fn github_app_owner_parses_org_scripted_value() {
        assert_eq!(
            GitHubAppOwner::parse_scripted("org:acme").unwrap(),
            GitHubAppOwner::Organization("acme".to_string())
        );
    }

    #[test]
    fn github_app_owner_rejects_invalid_scripted_value() {
        let err = GitHubAppOwner::parse_scripted("acme").unwrap_err();
        assert!(
            err.to_string()
                .contains("--github-owner must be 'personal' or 'org:<slug>'")
        );
    }

    #[test]
    fn github_app_owner_app_name_with_org() {
        let owner = GitHubAppOwner::Organization("acme-corp".to_string());
        assert_eq!(owner.app_name(Some("alice")), "acme-corp-fabro");
    }

    #[test]
    fn github_app_owner_app_name_personal_with_username() {
        let owner = GitHubAppOwner::Personal;
        assert_eq!(owner.app_name(Some("brynary")), "brynary-fabro");
    }

    #[test]
    fn github_app_owner_app_name_personal_without_username() {
        let owner = GitHubAppOwner::Personal;
        let name = owner.app_name(None);
        assert!(name.starts_with("Fabro-"), "expected Fabro- prefix: {name}");
        assert_eq!(name.len(), 12); // "Fabro-" (6) + 6 hex chars
    }

    #[test]
    fn install_json_event_line_serializes_handoff_event() {
        let event =
            install_github_app_handoff_event("http://127.0.0.1:1234/", &GitHubAppOwner::Personal);
        let line = install_json_event_line(&event).unwrap();
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["event"], "github_app_handoff");
        assert_eq!(value["url"], "http://127.0.0.1:1234/");
        assert_eq!(value["owner"], "personal");
    }

    #[test]
    fn install_error_event_contains_message() {
        let value = install_error_event("boom");
        assert_eq!(value["event"], "install_error");
        assert_eq!(value["status"], "error");
        assert_eq!(value["message"], "boom");
    }

    // -- GitHub App manifest --

    #[test]
    fn manifest_includes_callback_urls_and_setup_url() {
        let web_url = "https://app.example.com";
        let manifest = build_github_app_manifest("Fabro-test", 12345, web_url);

        assert_eq!(manifest["url"], serde_json::json!("https://fabro.sh"),);
        assert_eq!(
            manifest["callback_urls"],
            serde_json::json!(["https://app.example.com/auth/callback/github"]),
        );
        assert_eq!(
            manifest["setup_url"],
            serde_json::json!("https://app.example.com/setup"),
        );
    }

    #[tokio::test]
    async fn persist_install_outputs_persists_vault_secrets_via_server_when_autostarting() {
        let dir = tempfile::tempdir().unwrap();
        let server_env_pairs = vec![
            ("SESSION_SECRET".to_string(), "session".to_string()),
            ("FABRO_JWT_PUBLIC_KEY".to_string(), "public-key".to_string()),
        ];
        let vault_secrets = vec![
            CreateSecretRequest {
                name:        "GITHUB_TOKEN".to_string(),
                value:       "gh-token".to_string(),
                type_:       ApiSecretType::Environment,
                description: None,
            },
            credential_secret_request(&AuthCredential {
                provider: Provider::Anthropic,
                details:  fabro_auth::AuthDetails::ApiKey {
                    key: "anthropic-key".to_string(),
                },
            })
            .unwrap(),
        ];
        let server = MockServer::start_async().await;
        let created = server
            .mock_async(|when, then| {
                when.method(POST).path("/api/v1/secrets");
                then.status(200)
                    .header("content-type", "application/json")
                    .body(
                        serde_json::json!({
                            "name": "persisted",
                            "type": "environment",
                            "created_at": "2026-01-01T00:00:00Z",
                            "updated_at": "2026-01-01T00:00:00Z"
                        })
                        .to_string(),
                    );
            })
            .await;
        let stop_called = Arc::new(AtomicBool::new(false));

        persist_server_env_secrets(dir.path(), &server_env_pairs).unwrap();
        persist_vault_secrets_with(
            dir.path(),
            &vault_secrets,
            false,
            |_| {
                let client = fabro_api::Client::new_with_client(
                    &server.base_url(),
                    fabro_test::test_http_client(),
                );
                Box::pin(async move { Ok(client) })
            },
            {
                let stop_called = Arc::clone(&stop_called);
                move |_, _| {
                    let stop_called = Arc::clone(&stop_called);
                    Box::pin(async move {
                        stop_called.store(true, Ordering::SeqCst);
                        true
                    })
                }
            },
        )
        .await
        .unwrap();

        let server_env =
            std::fs::read_to_string(Storage::new(dir.path()).server_state().env_path()).unwrap();
        assert!(server_env.contains("SESSION_SECRET=session"));
        assert!(server_env.contains("FABRO_JWT_PUBLIC_KEY=public-key"));
        assert_eq!(created.calls_async().await, 2);
        assert!(stop_called.load(Ordering::SeqCst));
        assert!(!Storage::new(dir.path()).secrets_path().exists());
    }

    #[tokio::test]
    async fn persist_install_outputs_with_settings_does_not_write_settings_on_secret_failure() {
        let dir = tempfile::tempdir().unwrap();
        let server_env_pairs = vec![("SESSION_SECRET".to_string(), "session".to_string())];
        let vault_secrets = vec![CreateSecretRequest {
            name:        "GITHUB_CLI_TOKEN".to_string(),
            value:       "gh-token".to_string(),
            type_:       ApiSecretType::Environment,
            description: None,
        }];
        let settings_path = dir.path().join(SETTINGS_CONFIG_FILENAME);
        let stop_called = Arc::new(AtomicBool::new(false));

        let result = persist_install_outputs_with_settings(
            dir.path(),
            &server_env_pairs,
            &vault_secrets,
            Some(PendingSettingsWrite {
                path:              &settings_path,
                contents:          "_version = 1\n",
                previous_contents: None,
            }),
            false,
            |_| Box::pin(async move { Err(anyhow!("boom")) }),
            {
                let stop_called = Arc::clone(&stop_called);
                move |_, _| {
                    let stop_called = Arc::clone(&stop_called);
                    Box::pin(async move {
                        stop_called.store(true, Ordering::SeqCst);
                        true
                    })
                }
            },
        )
        .await;

        assert!(result.is_err());
        assert!(Storage::new(dir.path()).server_state().env_path().exists());
        assert!(!settings_path.exists());
        assert!(stop_called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn persist_install_outputs_with_settings_restores_previous_contents_on_secret_failure() {
        let dir = tempfile::tempdir().unwrap();
        let server_env_pairs = vec![("SESSION_SECRET".to_string(), "session".to_string())];
        let vault_secrets = vec![CreateSecretRequest {
            name:        "GITHUB_CLI_TOKEN".to_string(),
            value:       "gh-token".to_string(),
            type_:       ApiSecretType::Environment,
            description: None,
        }];
        let settings_path = dir.path().join(SETTINGS_CONFIG_FILENAME);
        std::fs::write(&settings_path, "_version = 1\n[server]\n").unwrap();

        let result = persist_install_outputs_with_settings(
            dir.path(),
            &server_env_pairs,
            &vault_secrets,
            Some(PendingSettingsWrite {
                path:              &settings_path,
                contents:          "_version = 1\n[server]\nfoo = \"bar\"\n",
                previous_contents: Some("_version = 1\n[server]\n"),
            }),
            false,
            |_| Box::pin(async move { Err(anyhow!("boom")) }),
            |_, _| Box::pin(async move { true }),
        )
        .await;

        assert!(result.is_err());
        assert_eq!(
            std::fs::read_to_string(&settings_path).unwrap(),
            "_version = 1\n[server]\n"
        );
    }

    #[test]
    fn non_interactive_source_rejects_missing_scripted_inputs() {
        let args = install_args(true, InstallNonInteractiveArgs::default());
        let err = NonInteractiveInstallInputSource::new(&args).unwrap_err();
        assert!(
            err.to_string()
                .contains("Non-interactive install requires additional flags")
        );
    }

    #[test]
    fn non_interactive_source_rejects_hidden_args_without_switch() {
        let args = install_args(false, InstallNonInteractiveArgs {
            llm_provider: Some(Provider::Anthropic),
            ..InstallNonInteractiveArgs::default()
        });
        let err = NonInteractiveInstallInputSource::new(&args).unwrap_err();
        assert!(
            err.to_string()
                .contains("--llm-provider requires --non-interactive")
        );
    }

    #[test]
    fn non_interactive_source_rejects_conflicting_api_key_inputs() {
        let args = install_args(true, InstallNonInteractiveArgs {
            llm_provider: Some(Provider::Anthropic),
            llm_api_key_stdin: true,
            llm_api_key_env: Some("ANTHROPIC_API_KEY".to_string()),
            github_strategy: Some(InstallGitHubStrategyArg::Token),
            github_username: Some("brynary".to_string()),
            ..InstallNonInteractiveArgs::default()
        });
        let err = NonInteractiveInstallInputSource::new(&args).unwrap_err();
        assert!(
            err.to_string()
                .contains("requires exactly one of --llm-api-key-stdin or --llm-api-key-env")
        );
    }

    #[test]
    fn non_interactive_source_rejects_missing_llm_provider() {
        let source = NonInteractiveInstallInputSource {
            args: InstallNonInteractiveArgs {
                llm_api_key_env: Some("ANTHROPIC_API_KEY".to_string()),
                github_strategy: Some(InstallGitHubStrategyArg::Token),
                github_username: Some("brynary".to_string()),
                ..InstallNonInteractiveArgs::default()
            },
        };

        let err = source.validate(false).unwrap_err();
        assert!(
            err.to_string()
                .contains("non-interactive install requires --llm-provider")
        );
    }

    #[test]
    fn non_interactive_source_rejects_missing_github_strategy() {
        let source = NonInteractiveInstallInputSource {
            args: InstallNonInteractiveArgs {
                llm_provider: Some(Provider::Anthropic),
                llm_api_key_env: Some("ANTHROPIC_API_KEY".to_string()),
                github_username: Some("brynary".to_string()),
                ..InstallNonInteractiveArgs::default()
            },
        };

        let err = source.validate(false).unwrap_err();
        assert!(
            err.to_string()
                .contains("non-interactive install requires --github-strategy")
        );
    }

    #[test]
    fn non_interactive_source_rejects_missing_github_username_for_new_config() {
        let source = NonInteractiveInstallInputSource {
            args: InstallNonInteractiveArgs {
                llm_provider: Some(Provider::Anthropic),
                llm_api_key_env: Some("ANTHROPIC_API_KEY".to_string()),
                github_strategy: Some(InstallGitHubStrategyArg::Token),
                ..InstallNonInteractiveArgs::default()
            },
        };

        let err = source.validate(false).unwrap_err();
        assert!(err.to_string().contains(
            "non-interactive install requires --github-username for --github-strategy token"
        ));
    }

    #[test]
    fn non_interactive_source_allows_keep_existing_settings_without_username() {
        let source = NonInteractiveInstallInputSource {
            args: InstallNonInteractiveArgs {
                llm_provider: Some(Provider::Anthropic),
                llm_api_key_env: Some("ANTHROPIC_API_KEY".to_string()),
                github_strategy: Some(InstallGitHubStrategyArg::Token),
                keep_existing_settings: true,
                ..InstallNonInteractiveArgs::default()
            },
        };

        source.validate(true).unwrap();
    }

    #[test]
    fn non_interactive_source_rejects_missing_github_owner_for_app() {
        let source = NonInteractiveInstallInputSource {
            args: InstallNonInteractiveArgs {
                llm_provider: Some(Provider::Anthropic),
                llm_api_key_env: Some("ANTHROPIC_API_KEY".to_string()),
                github_strategy: Some(InstallGitHubStrategyArg::App),
                ..InstallNonInteractiveArgs::default()
            },
        };

        let err = source.validate(false).unwrap_err();
        assert!(
            err.to_string().contains(
                "non-interactive install requires --github-owner for --github-strategy app"
            )
        );
    }

    #[test]
    fn non_interactive_source_rejects_github_owner_for_token() {
        let source = NonInteractiveInstallInputSource {
            args: InstallNonInteractiveArgs {
                llm_provider: Some(Provider::Anthropic),
                llm_api_key_env: Some("ANTHROPIC_API_KEY".to_string()),
                github_strategy: Some(InstallGitHubStrategyArg::Token),
                github_owner: Some("personal".to_string()),
                github_username: Some("brynary".to_string()),
                ..InstallNonInteractiveArgs::default()
            },
        };

        let err = source.validate(false).unwrap_err();
        assert!(
            err.to_string()
                .contains("--github-owner is only supported with --github-strategy app")
        );
    }

    #[test]
    fn non_interactive_source_rejects_github_username_for_app() {
        let source = NonInteractiveInstallInputSource {
            args: InstallNonInteractiveArgs {
                llm_provider: Some(Provider::Anthropic),
                llm_api_key_env: Some("ANTHROPIC_API_KEY".to_string()),
                github_strategy: Some(InstallGitHubStrategyArg::App),
                github_owner: Some("personal".to_string()),
                github_username: Some("brynary".to_string()),
                ..InstallNonInteractiveArgs::default()
            },
        };

        let err = source.validate(false).unwrap_err();
        assert!(
            err.to_string()
                .contains("--github-username is only supported with --github-strategy token")
        );
    }

    #[test]
    fn non_interactive_source_allows_github_app_setup() {
        let source = NonInteractiveInstallInputSource {
            args: InstallNonInteractiveArgs {
                llm_provider: Some(Provider::Anthropic),
                llm_api_key_env: Some("ANTHROPIC_API_KEY".to_string()),
                github_strategy: Some(InstallGitHubStrategyArg::App),
                github_owner: Some("personal".to_string()),
                ..InstallNonInteractiveArgs::default()
            },
        };

        source.validate(false).unwrap();
    }

    #[tokio::test]
    async fn non_interactive_source_requires_config_choice_when_settings_exist() {
        let source = NonInteractiveInstallInputSource {
            args: InstallNonInteractiveArgs {
                llm_provider: Some(Provider::Anthropic),
                llm_api_key_env: Some("ANTHROPIC_API_KEY".to_string()),
                github_strategy: Some(InstallGitHubStrategyArg::Token),
                github_username: Some("brynary".to_string()),
                ..InstallNonInteractiveArgs::default()
            },
        };

        let err = source.choose_server_config(true).await.unwrap_err();
        assert!(err.to_string().contains(
            "settings.toml already exists; pass --overwrite-settings or --keep-existing-settings"
        ));
    }
}
