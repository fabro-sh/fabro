#![expect(
    clippy::disallowed_methods,
    reason = "CLI `install` command: sync file I/O in install command handler; not on a Tokio hot path"
)]

use std::future::Future;
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
use fabro_client::{AuthEntry, AuthStore, DevTokenEntry, ServerTarget};
use fabro_config::bind::Bind;
use fabro_config::daemon::ServerDaemon;
use fabro_config::user::{SETTINGS_CONFIG_FILENAME, default_storage_dir};
use fabro_config::{Storage, envfile};
use fabro_install::{
    InstallListenConfig, PendingSettingsWrite, merge_server_settings as merge_server_settings_impl,
    persist_install_outputs_direct, write_github_app_settings, write_token_settings,
};
use fabro_model::Provider;
use fabro_server::serve;
use fabro_store::ArtifactStore;
use fabro_types::ServerSettings;
use fabro_types::settings::server::ServerAuthMethod;
use fabro_util::printer::Printer;
use fabro_util::terminal::Styles;
use fabro_util::version::FABRO_VERSION;
use fabro_util::{browser, dev_token, path, session_secret};
use fabro_vault::{SecretType as VaultSecretType, Vault};
use futures::future::BoxFuture;
use rand::Rng;
use tokio::net::TcpListener;
use tokio::process::Command as TokioCommand;
use tokio::sync::oneshot;
use tokio::task::spawn_blocking;

use super::doctor;
use crate::args::{
    DoctorArgs, InstallArgs, InstallCommand, InstallGitHubStrategyArg, InstallGithubArgs,
    InstallNonInteractiveArgs, ServerTargetArgs,
};
use crate::command_context::CommandContext;
use crate::commands::server::{start, stop};
use crate::gh::GhCli;
use crate::shared::provider_auth::{
    ApiKeySource, authenticate_provider, authenticate_provider_with_api_key_source,
    authenticate_provider_with_method, prompt_confirm, prompt_password, provider_display_name,
};
use crate::{local_server, server_client};

const GITHUB_TOKEN_SECRET_KEY: &str = "GITHUB_TOKEN";
const GITHUB_APP_PRIVATE_KEY_KEY: &str = "GITHUB_APP_PRIVATE_KEY";
const GITHUB_APP_CLIENT_SECRET_KEY: &str = "GITHUB_APP_CLIENT_SECRET";
const GITHUB_APP_WEBHOOK_SECRET_KEY: &str = "GITHUB_APP_WEBHOOK_SECRET";

// ---------------------------------------------------------------------------
// Auth status display
// ---------------------------------------------------------------------------

fn print_auth_status(
    methods: &[ServerAuthMethod],
    dev_token: Option<&str>,
    s: &Styles,
    printer: Printer,
) {
    if let Some(token) = dev_token.filter(|_| methods.contains(&ServerAuthMethod::DevToken)) {
        fabro_util::printerr!(
            printer,
            "  {} Auth (Dev Token): {}",
            s.green.apply_to("✔"),
            token
        );
    } else {
        let names: Vec<&str> = methods
            .iter()
            .map(|m| match m {
                ServerAuthMethod::DevToken => "dev-token",
                ServerAuthMethod::Github => "github",
            })
            .collect();
        fabro_util::printerr!(
            printer,
            "  {} Auth: {}",
            s.green.apply_to("✔"),
            names.join(", ")
        );
    }
}

// ---------------------------------------------------------------------------
// Config TOML generation
// ---------------------------------------------------------------------------

/// Default web URL used by `fabro install` when `--web-url` is omitted.
pub(crate) fn default_web_url() -> String {
    format!("http://127.0.0.1:{}", serve::DEFAULT_TCP_PORT)
}

fn merge_server_settings(doc: &mut toml::Value, web_url: &str) -> Result<()> {
    // Extract host:port from a URL like "http://127.0.0.1:32276"
    let authority = web_url
        .split("://")
        .nth(1)
        .unwrap_or(web_url)
        .split('/')
        .next()
        .unwrap_or(web_url);
    merge_server_settings_impl(
        doc,
        web_url,
        &InstallListenConfig::Tcp(authority.to_string()),
    )
}

#[cfg(test)]
fn format_config_toml() -> String {
    let mut doc = toml::Value::Table(toml::Table::default());
    merge_server_settings(&mut doc, &default_web_url())
        .expect("default server config should be valid");
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
        .is_ok_and(|s| s.success())
}

// ---------------------------------------------------------------------------
// Interactive setup
// ---------------------------------------------------------------------------

fn prompt_input(prompt: &str) -> Result<String> {
    Ok(dialoguer::Input::with_theme(&ColorfulTheme::default())
        .with_prompt(prompt)
        .interact_on(&Term::stderr())?)
}

fn prompt_select(prompt: &str, items: &[String], default: usize) -> Result<usize> {
    Ok(Select::with_theme(&ColorfulTheme::default())
        .with_prompt(prompt)
        .default(default)
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
    Token {
        token: String,
    },
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
    #[allow(
        clippy::print_stdout,
        reason = "Install event lines stream on stdout for machine consumers."
    )]
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
                move || prompt_select("Choose your first LLM provider", &labels, 0)
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
        let gh_available = GhCli::detect().await.is_some();

        let token_label = if gh_available {
            "Personal Access Token — use your existing `gh` login"
        } else {
            "Personal Access Token"
        };
        let strategy_options = vec![
            token_label.to_string(),
            "GitHub App — recommended for teams".to_string(),
        ];
        let strategy = spawn_blocking({
            let options = strategy_options.clone();
            move || prompt_select("How should Fabro authenticate with GitHub?", &options, 0)
        })
        .await??;

        match strategy {
            0 => {
                let token = if gh_available {
                    fabro_github::gh_auth_token().await.map_err(|err| {
                        anyhow!("{err}. Run `gh auth login` and rerun `fabro install`.")
                    })?
                } else {
                    spawn_blocking(|| prompt_password("GitHub Personal Access Token")).await??
                };
                Ok(GitHubInstallSelection::Token { token })
            }
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
            Some(InstallGitHubStrategyArg::Token) => {
                let token = fabro_github::gh_auth_token().await.map_err(|err| {
                    anyhow!("{err}. Run `gh auth login` and rerun `fabro install`.")
                })?;
                Ok(GitHubInstallSelection::Token { token })
            }
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

fn validate_install_github_non_interactive(
    github_args: &InstallGithubArgs,
    non_interactive: bool,
) -> Result<()> {
    if !non_interactive {
        if github_args.strategy.is_some() {
            bail!("--strategy requires --non-interactive");
        }
        if github_args.owner.is_some() {
            bail!("--owner requires --non-interactive");
        }
        return Ok(());
    }

    match github_args.strategy {
        Some(InstallGitHubStrategyArg::Token) => {
            anyhow::ensure!(
                github_args.owner.is_none(),
                "--owner is only supported with --strategy app"
            );
        }
        Some(InstallGitHubStrategyArg::App) => {
            let owner = github_args
                .owner
                .as_deref()
                .context("install github --non-interactive requires --owner for --strategy app")?;
            GitHubAppOwner::parse_scripted(owner)?;
        }
        None => bail!("install github --non-interactive requires --strategy"),
    }

    Ok(())
}

async fn choose_install_github_selection(
    install_args: &InstallArgs,
    github_args: &InstallGithubArgs,
    s: &Styles,
    printer: Printer,
) -> Result<GitHubInstallSelection> {
    validate_install_github_non_interactive(github_args, install_args.non_interactive)?;

    if !install_args.non_interactive {
        let input = InteractiveInstallInputSource;
        return input.choose_github_install(s, printer).await;
    }

    match github_args.strategy {
        Some(InstallGitHubStrategyArg::Token) => {
            let token = fabro_github::gh_auth_token().await.map_err(|err| {
                anyhow!("{err}. Run `gh auth login` and rerun `fabro install github`.")
            })?;
            Ok(GitHubInstallSelection::Token { token })
        }
        Some(InstallGitHubStrategyArg::App) => Ok(GitHubInstallSelection::App {
            owner:    GitHubAppOwner::parse_scripted(github_args.owner.as_deref().context(
                "install github --non-interactive requires --owner for --strategy app",
            )?)?,
            username: best_effort_github_username().await,
        }),
        None => bail!("install github --non-interactive requires --strategy"),
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
                    let mut rng = rand::rng();
                    let suffix: String = (0..6).fold(String::with_capacity(6), |mut s, _| {
                        use std::fmt::Write;
                        let _ = write!(s, "{:x}", rng.random::<u8>() % 16);
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
        move || prompt_select("Where should the GitHub App be created?", &items, 0)
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
            if let Err(e) = browser::try_open(&url) {
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
    client: &server_client::Client,
    secrets: &[CreateSecretRequest],
) -> Result<()> {
    for secret in secrets {
        client
            .create_secret(CreateSecretRequest {
                name:        secret.name.clone(),
                value:       secret.value.clone(),
                type_:       secret.type_,
                description: secret.description.clone(),
            })
            .await?;
    }

    Ok(())
}

async fn persist_vault_secrets_with(
    storage_dir: &Path,
    secrets: &[CreateSecretRequest],
    server_was_running: bool,
    connect_server: impl for<'a> Fn(&'a Path) -> BoxFuture<'a, Result<server_client::Client>>,
    stop_server: impl for<'a> Fn(&'a Path, Duration) -> BoxFuture<'a, bool>,
) -> Result<()> {
    if secrets.is_empty() {
        return Ok(());
    }

    let client = match connect_server(storage_dir).await {
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

fn server_env_updates(secrets: &[(String, String)]) -> Vec<envfile::EnvFileUpdate> {
    secrets
        .iter()
        .map(|(key, value)| envfile::EnvFileUpdate {
            key:     key.clone(),
            value:   value.clone(),
            comment: None,
        })
        .collect()
}

fn server_env_removals(keys: &[&'static str]) -> Vec<envfile::EnvFileRemoval> {
    keys.iter()
        .map(|key| envfile::EnvFileRemoval {
            key:     (*key).to_string(),
            comment: None,
        })
        .collect()
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
        |path| Box::pin(server_client::connect_server(path)),
        |path, timeout| {
            Box::pin(async move { stop::stop_server(path, timeout).await.unwrap_or(false) })
        },
    )
    .await
}

struct PendingGitHubInstallWrite<'a> {
    settings_write:    PendingSettingsWrite<'a>,
    server_env_set:    Vec<(String, String)>,
    server_env_remove: Vec<&'static str>,
    vault_set:         Vec<(String, String)>,
    vault_remove:      Vec<&'static str>,
}

fn restore_optional_file(path: &Path, previous_contents: Option<&str>) -> Result<()> {
    match previous_contents {
        Some(contents) => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating directory {}", parent.display()))?;
            }
            std::fs::write(path, contents)
                .with_context(|| format!("restoring {}", path.display()))?;
        }
        None => match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(anyhow::Error::new(err).context(format!("removing {}", path.display())));
            }
        },
    }

    Ok(())
}

fn persist_github_install_changes(
    storage_dir: &Path,
    writes: &PendingGitHubInstallWrite<'_>,
) -> Result<()> {
    let storage = Storage::new(storage_dir);
    let server_env_path = storage.runtime_directory().env_path();
    let vault_path = storage.secrets_path();
    let previous_server_env = std::fs::read_to_string(&server_env_path).ok();
    let previous_vault = std::fs::read_to_string(&vault_path).ok();

    let result = (|| -> Result<()> {
        let server_env_writes = server_env_updates(&writes.server_env_set);
        let server_env_removals = server_env_removals(&writes.server_env_remove);
        persist_install_outputs_direct(
            storage_dir,
            &server_env_writes,
            &server_env_removals,
            &[],
            Some(&writes.settings_write),
        )?;

        let mut vault = Vault::load(vault_path.clone()).map_err(anyhow::Error::from)?;
        for key in &writes.vault_remove {
            match vault.remove(key) {
                Ok(()) | Err(fabro_vault::Error::NotFound(_)) => {}
                Err(err) => return Err(err.into()),
            }
        }
        for (key, value) in &writes.vault_set {
            vault
                .set(key, value, VaultSecretType::Environment, None)
                .map_err(anyhow::Error::from)?;
        }

        Ok(())
    })();

    if let Err(err) = result {
        restore_optional_file(
            writes.settings_write.path,
            writes.settings_write.previous_contents,
        )?;
        restore_optional_file(&server_env_path, previous_server_env.as_deref())?;
        restore_optional_file(&vault_path, previous_vault.as_deref())?;
        return Err(err);
    }

    Ok(())
}

async fn write_artifact_store_metadata(
    settings: &ServerSettings,
    fabro_version: &str,
) -> Result<()> {
    let (object_store, prefix) = serve::build_artifact_object_store(&settings.server)?;
    let artifact_store = ArtifactStore::new(object_store, prefix);
    artifact_store.write_metadata(fabro_version).await?;
    Ok(())
}

async fn persist_install_outputs_with_settings(
    storage_dir: &Path,
    server_env_secrets: &[(String, String)],
    vault_secrets: &[CreateSecretRequest],
    settings_write: Option<PendingSettingsWrite<'_>>,
    server_was_running: bool,
    connect_server: impl for<'a> Fn(&'a Path) -> BoxFuture<'a, Result<server_client::Client>>,
    stop_server: impl for<'a> Fn(&'a Path, Duration) -> BoxFuture<'a, bool>,
) -> Result<()> {
    let server_env_path = Storage::new(storage_dir).runtime_directory().env_path();
    let previous_server_env = std::fs::read_to_string(&server_env_path).ok();
    let settings_write_ref = settings_write.as_ref();
    persist_install_outputs_direct(
        storage_dir,
        &server_env_updates(server_env_secrets),
        &[],
        &[],
        settings_write_ref,
    )?;

    let persist_result = persist_vault_secrets_with(
        storage_dir,
        vault_secrets,
        server_was_running,
        connect_server,
        stop_server,
    )
    .await;

    if let Err(err) = persist_result {
        restore_optional_file(&server_env_path, previous_server_env.as_deref())?;
        if let Some(write) = settings_write {
            match write.previous_contents {
                Some(previous) => std::fs::write(write.path, previous)
                    .with_context(|| format!("restoring settings file {}", write.path.display()))?,
                None if write.path.exists() => std::fs::remove_file(write.path)
                    .with_context(|| format!("removing settings file {}", write.path.display()))?,
                None => {}
            }
        }
        return Err(err);
    }

    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
enum InstallServerRestartOutcome {
    Started(Bind),
    Failed(String),
}

async fn restart_server_after_install_with(
    storage_dir: &Path,
    config_path: &Path,
    stop_server: impl for<'a> Fn(&'a Path, Duration) -> BoxFuture<'a, bool>,
    ensure_server_running: impl for<'a> Fn(&'a Path, &'a Path) -> BoxFuture<'a, Result<Bind>>,
) -> InstallServerRestartOutcome {
    stop_server(storage_dir, Duration::from_secs(5)).await;

    match ensure_server_running(storage_dir, config_path).await {
        Ok(bind) => InstallServerRestartOutcome::Started(bind),
        Err(err) => InstallServerRestartOutcome::Failed(err.to_string()),
    }
}

async fn restart_server_after_install(
    storage_dir: &Path,
    config_path: &Path,
) -> InstallServerRestartOutcome {
    restart_server_after_install_with(
        storage_dir,
        config_path,
        |path, timeout| {
            Box::pin(async move { stop::stop_server(path, timeout).await.unwrap_or(false) })
        },
        |storage_dir, config_path| {
            Box::pin(start::ensure_server_running_for_storage(
                storage_dir,
                config_path,
            ))
        },
    )
    .await
}

async fn maybe_restart_server_after_github_install(
    storage_dir: &Path,
    config_path: &Path,
    server_was_running: bool,
) -> Option<InstallServerRestartOutcome> {
    if !server_was_running {
        return None;
    }

    Some(restart_server_after_install(storage_dir, config_path).await)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallDoctorOutcome {
    SkippedServerRestartFailure,
    SkippedUserDeclined,
    Ran,
}

async fn maybe_run_install_doctor_with<
    ShouldRunDoctor,
    ShouldRunDoctorFuture,
    RunDoctor,
    RunDoctorFuture,
>(
    restart_succeeded: bool,
    should_run_doctor: ShouldRunDoctor,
    run_doctor: RunDoctor,
) -> Result<InstallDoctorOutcome>
where
    ShouldRunDoctor: FnOnce() -> ShouldRunDoctorFuture,
    ShouldRunDoctorFuture: Future<Output = Result<bool>>,
    RunDoctor: FnOnce() -> RunDoctorFuture,
    RunDoctorFuture: Future<Output = Result<i32>>,
{
    if !restart_succeeded {
        return Ok(InstallDoctorOutcome::SkippedServerRestartFailure);
    }

    if !should_run_doctor().await? {
        return Ok(InstallDoctorOutcome::SkippedUserDeclined);
    }

    let _ = run_doctor().await?;
    Ok(InstallDoctorOutcome::Ran)
}

pub(crate) async fn execute(
    args: &InstallArgs,
    command: Option<InstallCommand>,
    ctx: &CommandContext,
) -> Result<()> {
    match command {
        None => run_install(args, ctx).await,
        Some(InstallCommand::Github(github_args)) => {
            run_install_github_command(args, &github_args, ctx).await
        }
    }
}

async fn run_install_github_command(
    args: &InstallArgs,
    github_args: &InstallGithubArgs,
    ctx: &CommandContext,
) -> Result<()> {
    let json = ctx.json_output();
    if ctx.explicit_json_requested() && !args.non_interactive {
        bail!("--json is only supported for install with --non-interactive");
    }

    let result = Box::pin(run_install_github_inner(
        args,
        github_args,
        json,
        ctx.printer(),
    ))
    .await;
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

async fn run_install_github_inner(
    args: &InstallArgs,
    github_args: &InstallGithubArgs,
    json_output: bool,
    printer: Printer,
) -> Result<()> {
    let s = Styles::detect_stderr();
    let fabro_dir = fabro_util::Home::from_env().root().to_path_buf();
    let config_path = fabro_dir.join(SETTINGS_CONFIG_FILENAME);
    if !config_path.exists() {
        bail!("No settings.toml found. Run `fabro install` first.");
    }

    let existing_config_contents =
        std::fs::read_to_string(&config_path).context("failed to read existing settings.toml")?;
    let storage_dir = args
        .storage_dir
        .clone_path()
        .or_else(|| local_server::storage_dir_from_toml(&existing_config_contents).ok())
        .unwrap_or_else(default_storage_dir);
    let server_was_running =
        ServerDaemon::load_running(&Storage::new(&storage_dir).runtime_directory())?.is_some();
    let mut doc: toml::Value = toml::from_str(&existing_config_contents)
        .context("failed to parse existing settings.toml")?;

    let selection = choose_install_github_selection(args, github_args, &s, printer).await?;
    let mut server_env_set = Vec::new();
    let mut server_env_remove = Vec::new();
    let mut vault_set = Vec::new();
    let mut vault_remove = Vec::new();

    match selection {
        GitHubInstallSelection::Token { token } => {
            write_token_settings(&mut doc)?;
            vault_set.push((GITHUB_TOKEN_SECRET_KEY.to_string(), token));
            server_env_remove.extend([
                GITHUB_APP_PRIVATE_KEY_KEY,
                GITHUB_APP_CLIENT_SECRET_KEY,
                GITHUB_APP_WEBHOOK_SECRET_KEY,
            ]);
        }
        GitHubInstallSelection::App { owner, username } => {
            let allowed_username = username.clone().context(
                "GitHub App install requires an authenticated GitHub username; run `gh auth login` and rerun `fabro install github`",
            )?;
            server_env_remove.extend([
                GITHUB_APP_PRIVATE_KEY_KEY,
                GITHUB_APP_CLIENT_SECRET_KEY,
                GITHUB_APP_WEBHOOK_SECRET_KEY,
            ]);
            let registration = setup_github_app(
                &s,
                &args.web_url,
                &owner,
                username.as_deref(),
                if args.non_interactive {
                    GitHubAppHandoffMode::Manual
                } else {
                    GitHubAppHandoffMode::Interactive
                },
                json_output,
                printer,
            )
            .await?;
            server_env_set.extend(registration.env_pairs);
            vault_remove.push(GITHUB_TOKEN_SECRET_KEY);
            write_github_app_settings(
                &mut doc,
                &registration.app_id,
                &registration.slug,
                &registration.client_id,
                &[allowed_username],
            )?;
        }
    }

    let settings_toml = toml::to_string_pretty(&doc)?;
    persist_github_install_changes(&storage_dir, &PendingGitHubInstallWrite {
        settings_write: PendingSettingsWrite {
            path:              &config_path,
            contents:          settings_toml.as_str(),
            previous_contents: Some(existing_config_contents.as_str()),
        },
        server_env_set,
        server_env_remove,
        vault_set,
        vault_remove,
    })?;

    if let Some(restart_outcome) =
        maybe_restart_server_after_github_install(&storage_dir, &config_path, server_was_running)
            .await
    {
        match restart_outcome {
            InstallServerRestartOutcome::Started(bind) => {
                fabro_util::printerr!(
                    printer,
                    "  {} Server running at http://{}",
                    s.green.apply_to("✔"),
                    bind
                );
                let methods = fabro_config::ServerSettingsBuilder::from_toml(&settings_toml)
                    .ok()
                    .map(|settings| settings.server.auth.methods)
                    .unwrap_or_default();
                let token = methods
                    .contains(&ServerAuthMethod::DevToken)
                    .then(|| {
                        dev_token::read_dev_token_file(
                            &Storage::new(&storage_dir)
                                .runtime_directory()
                                .dev_token_path(),
                        )
                    })
                    .flatten();
                print_auth_status(&methods, token.as_deref(), &s, printer);
                fabro_util::printerr!(printer, "");
            }
            InstallServerRestartOutcome::Failed(err) => {
                fabro_util::printerr!(
                    printer,
                    "  {} Failed to restart server: {err}",
                    s.yellow.apply_to("Warning:")
                );
            }
        }
    }

    Ok(())
}

pub(crate) async fn run_install(args: &InstallArgs, ctx: &CommandContext) -> Result<()> {
    let json = ctx.json_output();
    if ctx.explicit_json_requested() && !args.non_interactive {
        bail!("--json is only supported for install with --non-interactive");
    }

    let result = Box::pin(run_install_inner(args, ctx)).await;
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

async fn run_install_inner(args: &InstallArgs, ctx: &CommandContext) -> Result<()> {
    let _cli = &ctx.user_settings().cli;
    let printer = ctx.printer();
    let json = ctx.json_output();
    let web_url = &args.web_url;
    let s = Styles::detect_stderr();
    let emoji = console::Emoji("⚒️  ", "");
    let local_config =
        local_server::LocalServerConfig::load_with_storage_dir(args.storage_dir.as_deref())?;
    let storage_dir = local_config.storage_dir().to_path_buf();
    let server_was_running =
        ServerDaemon::load_running(&Storage::new(&storage_dir).runtime_directory())?.is_some();
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

    std::fs::create_dir_all(&fabro_dir)
        .with_context(|| format!("creating fabro home directory {}", fabro_dir.display()))?;

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
        GitHubInstallSelection::Token { token } => {
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
                merge_server_settings(&mut doc, web_url)?;
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

        toml::to_string_pretty(&doc)?
    };

    let install_server_settings = fabro_config::ServerSettingsBuilder::from_toml(&settings_toml)?;

    // Secrets and auth material
    let mut dev_token_for_auth_store = None;
    {
        let session_secret = session_secret::generate_session_secret();
        fabro_util::printerr!(
            printer,
            "  {} Session secret generated",
            s.green.apply_to("✔")
        );

        let dev_token = if install_server_settings
            .server
            .auth
            .methods
            .contains(&ServerAuthMethod::DevToken)
        {
            let dev_token_path = Storage::new(&storage_dir)
                .runtime_directory()
                .dev_token_path();
            let token = dev_token::read_or_mint_dev_token_for_install(&dev_token_path)?;
            dev_token_for_auth_store = Some(token.clone());
            fabro_util::printerr!(
                printer,
                "  {} Development token generated",
                s.green.apply_to("✔")
            );
            Some(token)
        } else {
            None
        };

        let mut generated_server_env_pairs = vec![("SESSION_SECRET".to_string(), session_secret)];
        if let Some(token) = dev_token {
            generated_server_env_pairs.push(("FABRO_DEV_TOKEN".to_string(), token));
        }
        server_env_pairs.extend(generated_server_env_pairs);
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
    if let Some(token) = dev_token_for_auth_store {
        let target = ServerTarget::http_url(&args.web_url)?;
        if let Err(err) = AuthStore::default().put(
            &target,
            AuthEntry::DevToken(DevTokenEntry {
                token,
                logged_in_at: chrono::Utc::now(),
            }),
        ) {
            fabro_util::printerr!(
                printer,
                "  {} Installed successfully, but failed to save CLI auth: {err}",
                s.yellow.apply_to("Warning:")
            );
        }
    }
    if let Err(err) = write_artifact_store_metadata(&install_server_settings, FABRO_VERSION).await {
        fabro_util::printerr!(
            printer,
            "  {} failed to write artifact store metadata: {err}",
            s.yellow.apply_to("Warning:")
        );
    }
    fabro_util::printerr!(
        printer,
        "  {} Saved {} runtime secrets to {}",
        s.green.apply_to("✔"),
        server_env_pairs.len(),
        path::contract_tilde(&Storage::new(&storage_dir).runtime_directory().env_path()).display()
    );
    fabro_util::printerr!(
        printer,
        "  {} Saved {} workflow-visible secrets to {}",
        s.green.apply_to("✔"),
        vault_secrets.len(),
        path::contract_tilde(&Storage::new(&storage_dir).secrets_path()).display()
    );
    fabro_util::printerr!(
        printer,
        "  {} Wrote {}",
        s.green.apply_to("✔"),
        path::contract_tilde(&config_path).display()
    );
    fabro_util::printerr!(printer, "");
    let restart_succeeded = match restart_server_after_install(&storage_dir, &config_path).await {
        InstallServerRestartOutcome::Started(bind) => {
            fabro_util::printerr!(
                printer,
                "  {} Server running at http://{}",
                s.green.apply_to("✔"),
                bind
            );
            let methods = install_server_settings.server.auth.methods.as_slice();
            let token = methods
                .contains(&ServerAuthMethod::DevToken)
                .then(|| {
                    dev_token::read_dev_token_file(
                        &Storage::new(&storage_dir)
                            .runtime_directory()
                            .dev_token_path(),
                    )
                })
                .flatten();
            print_auth_status(methods, token.as_deref(), &s, printer);
            fabro_util::printerr!(printer, "");
            true
        }
        InstallServerRestartOutcome::Failed(err) => {
            fabro_util::printerr!(
                printer,
                "  {} Failed to start server: {err}",
                s.yellow.apply_to("Warning:")
            );
            fabro_util::printerr!(
                printer,
                "  To start manually, run: {}",
                s.bold_cyan.apply_to("fabro server start")
            );
            false
        }
    };

    match maybe_run_install_doctor_with(
        restart_succeeded,
        || input_source.should_run_doctor(),
        || async {
            fabro_util::printerr!(printer, "");
            let doctor_args = DoctorArgs {
                target:  ServerTargetArgs::default(),
                verbose: false,
            };
            doctor::run_doctor(&doctor_args, ctx).await
        },
    )
    .await?
    {
        InstallDoctorOutcome::SkippedServerRestartFailure => {
            fabro_util::printerr!(
                printer,
                "  {}",
                s.dim
                    .apply_to("Skipping fabro doctor because the server did not restart.")
            );
        }
        InstallDoctorOutcome::SkippedUserDeclined | InstallDoctorOutcome::Ran => {}
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
    #![allow(
        clippy::absolute_paths,
        reason = "This test module prefers explicit type paths over extra imports."
    )]

    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use httpmock::Method::POST;
    use httpmock::MockServer;

    use super::*;

    fn install_args(non_interactive: bool, scripted: InstallNonInteractiveArgs) -> InstallArgs {
        InstallArgs {
            storage_dir: crate::args::StorageDirArgs::default(),
            web_url: default_web_url(),
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

    // -- Config TOML generation --

    #[test]
    fn config_toml_roundtrips() {
        let toml_str = format_config_toml();
        let cfg = fabro_config::ServerSettingsBuilder::from_toml(&toml_str)
            .expect("generated config should resolve");
        let methods = cfg.server.auth.methods;
        assert_eq!(methods, vec![
            fabro_types::settings::ServerAuthMethod::DevToken
        ]);
    }

    #[test]
    fn config_toml_has_auth_strategies() {
        let toml_str = format_config_toml();
        let cfg = fabro_config::ServerSettingsBuilder::from_toml(&toml_str)
            .expect("generated config should resolve");
        assert_eq!(cfg.server.auth.methods, vec![
            fabro_types::settings::ServerAuthMethod::DevToken
        ]);
    }

    #[test]
    fn config_toml_has_tcp_listen_address() {
        let toml_str = format_config_toml();
        let cfg: toml::Value = toml::from_str(&toml_str).expect("generated config should parse");
        assert_eq!(
            cfg.get("server")
                .and_then(toml::Value::as_table)
                .and_then(|server| server.get("listen"))
                .and_then(toml::Value::as_table)
                .and_then(|listen| listen.get("type"))
                .and_then(toml::Value::as_str),
            Some("tcp")
        );
        assert_eq!(
            cfg.get("server")
                .and_then(toml::Value::as_table)
                .and_then(|server| server.get("listen"))
                .and_then(toml::Value::as_table)
                .and_then(|listen| listen.get("address"))
                .and_then(toml::Value::as_str),
            Some("127.0.0.1:32276")
        );
    }

    #[test]
    fn config_toml_has_cli_target_matching_listen_address() {
        let toml_str = format_config_toml();
        let cfg: toml::Value = toml::from_str(&toml_str).expect("generated config should parse");
        assert_eq!(
            cfg.get("cli")
                .and_then(toml::Value::as_table)
                .and_then(|cli| cli.get("target"))
                .and_then(toml::Value::as_table)
                .and_then(|target| target.get("type"))
                .and_then(toml::Value::as_str),
            Some("http")
        );
        assert_eq!(
            cfg.get("cli")
                .and_then(toml::Value::as_table)
                .and_then(|cli| cli.get("target"))
                .and_then(toml::Value::as_table)
                .and_then(|target| target.get("url"))
                .and_then(toml::Value::as_str),
            Some("http://127.0.0.1:32276")
        );
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

        merge_server_settings(&mut doc, &default_web_url()).unwrap();

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

    fn auth_methods(source: &str) -> Option<Vec<String>> {
        toml::from_str::<toml::Value>(source)
            .expect("install settings fixture should parse")
            .get("server")
            .and_then(toml::Value::as_table)
            .and_then(|server| server.get("auth"))
            .and_then(toml::Value::as_table)
            .and_then(|auth| auth.get("methods"))
            .and_then(toml::Value::as_array)
            .map(|methods| {
                methods
                    .iter()
                    .filter_map(toml::Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
    }

    #[test]
    fn dev_token_auth_enabled_when_methods_include_dev_token() {
        let methods = auth_methods(
            r#"
_version = 1

[server.auth]
methods = ["dev-token"]
"#,
        );
        assert_eq!(methods, Some(vec!["dev-token".to_string()]));
    }

    #[test]
    fn dev_token_auth_enabled_when_mixed_with_github() {
        let methods = auth_methods(
            r#"
_version = 1

[server.auth]
methods = ["dev-token", "github"]
"#,
        );
        assert_eq!(
            methods,
            Some(vec!["dev-token".to_string(), "github".to_string()])
        );
    }

    #[test]
    fn dev_token_auth_enabled_false_for_github_only() {
        let methods = auth_methods(
            r#"
_version = 1

[server.auth]
methods = ["github"]
"#,
        );
        assert_eq!(methods, Some(vec!["github".to_string()]));
    }

    #[test]
    fn dev_token_auth_enabled_false_when_methods_absent() {
        let methods = auth_methods(
            "
_version = 1

[server.auth]
",
        );
        assert_eq!(methods, None);
    }

    #[test]
    fn write_token_settings_uses_server_integrations_github() {
        let mut doc: toml::Value = toml::from_str(
            r#"
_version = 1

[server.auth]
methods = ["dev-token", "github"]

[server.auth.github]
allowed_usernames = ["alice"]

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
    fn write_token_settings_removes_github_auth_state() {
        let mut doc: toml::Value = toml::from_str(
            r#"
_version = 1

[server.auth]
methods = ["dev-token", "github"]

[server.auth.github]
allowed_usernames = ["alice"]

[server.integrations.github]
strategy = "app"
app_id = "123"
slug = "fabro-app"
client_id = "client-id"
"#,
        )
        .unwrap();

        write_token_settings(&mut doc).unwrap();

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
            vec!["dev-token"]
        );
        assert!(
            doc.get("server")
                .and_then(toml::Value::as_table)
                .and_then(|server| server.get("auth"))
                .and_then(toml::Value::as_table)
                .and_then(|auth| auth.get("github"))
                .is_none(),
            "server.auth.github should be removed"
        );
    }

    #[test]
    fn write_github_app_settings_uses_server_integrations_github() {
        let mut doc = toml::Value::Table(toml::Table::default());
        merge_server_settings(&mut doc, &default_web_url()).unwrap();

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
            vec!["github"]
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
        let server_env_pairs = [("SESSION_SECRET".to_string(), "session".to_string())];
        let vault_secrets = [
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

        let env_path = Storage::new(dir.path()).runtime_directory().env_path();
        envfile::merge_env_file(&env_path, server_env_pairs.iter().cloned()).unwrap();
        persist_vault_secrets_with(
            dir.path(),
            &vault_secrets,
            false,
            |_| {
                let client = server_client::Client::new_no_proxy(&server.base_url()).unwrap();
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
            std::fs::read_to_string(Storage::new(dir.path()).runtime_directory().env_path())
                .unwrap();
        assert!(server_env.contains("SESSION_SECRET=session"));
        assert_eq!(created.calls_async().await, 2);
        assert!(stop_called.load(Ordering::SeqCst));
        assert!(!Storage::new(dir.path()).secrets_path().exists());
    }

    #[tokio::test]
    async fn persist_vault_secrets_with_leaves_running_server_up() {
        let dir = tempfile::tempdir().unwrap();
        let vault_secrets = [CreateSecretRequest {
            name:        "GITHUB_TOKEN".to_string(),
            value:       "gh-token".to_string(),
            type_:       ApiSecretType::Environment,
            description: None,
        }];
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

        persist_vault_secrets_with(
            dir.path(),
            &vault_secrets,
            true,
            |_| {
                let client = server_client::Client::new_no_proxy(&server.base_url()).unwrap();
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

        assert_eq!(created.calls_async().await, 1);
        assert!(!stop_called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn restart_server_after_install_returns_started_bind_on_success() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("settings.toml");
        let stop_called = Arc::new(AtomicBool::new(false));
        let start_called = Arc::new(AtomicBool::new(false));

        let outcome = restart_server_after_install_with(
            dir.path(),
            &config_path,
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
            {
                let start_called = Arc::clone(&start_called);
                move |_, _| {
                    let start_called = Arc::clone(&start_called);
                    Box::pin(async move {
                        start_called.store(true, Ordering::SeqCst);
                        Ok(Bind::Tcp("127.0.0.1:32276".parse::<SocketAddr>().unwrap()))
                    })
                }
            },
        )
        .await;

        assert_eq!(
            outcome,
            InstallServerRestartOutcome::Started(Bind::Tcp(
                "127.0.0.1:32276".parse::<SocketAddr>().unwrap()
            ))
        );
        assert!(stop_called.load(Ordering::SeqCst));
        assert!(start_called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn restart_server_after_install_returns_failed_on_error() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("settings.toml");
        let stop_called = Arc::new(AtomicBool::new(false));
        let start_called = Arc::new(AtomicBool::new(false));

        let outcome = restart_server_after_install_with(
            dir.path(),
            &config_path,
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
            {
                let start_called = Arc::clone(&start_called);
                move |_, _| {
                    let start_called = Arc::clone(&start_called);
                    Box::pin(async move {
                        start_called.store(true, Ordering::SeqCst);
                        Err(anyhow!("boom"))
                    })
                }
            },
        )
        .await;

        assert_eq!(
            outcome,
            InstallServerRestartOutcome::Failed("boom".to_string())
        );
        assert!(stop_called.load(Ordering::SeqCst));
        assert!(start_called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn maybe_run_install_doctor_skips_prompt_when_restart_failed() {
        let prompt_called = Arc::new(AtomicBool::new(false));
        let doctor_called = Arc::new(AtomicBool::new(false));

        let outcome = maybe_run_install_doctor_with(
            false,
            {
                let prompt_called = Arc::clone(&prompt_called);
                move || {
                    let prompt_called = Arc::clone(&prompt_called);
                    async move {
                        prompt_called.store(true, Ordering::SeqCst);
                        Ok(true)
                    }
                }
            },
            {
                let doctor_called = Arc::clone(&doctor_called);
                move || {
                    let doctor_called = Arc::clone(&doctor_called);
                    async move {
                        doctor_called.store(true, Ordering::SeqCst);
                        Ok(0)
                    }
                }
            },
        )
        .await
        .unwrap();

        assert_eq!(outcome, InstallDoctorOutcome::SkippedServerRestartFailure);
        assert!(!prompt_called.load(Ordering::SeqCst));
        assert!(!doctor_called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn maybe_run_install_doctor_runs_when_restart_succeeds_and_user_accepts() {
        let prompt_called = Arc::new(AtomicBool::new(false));
        let doctor_called = Arc::new(AtomicBool::new(false));

        let outcome = maybe_run_install_doctor_with(
            true,
            {
                let prompt_called = Arc::clone(&prompt_called);
                move || {
                    let prompt_called = Arc::clone(&prompt_called);
                    async move {
                        prompt_called.store(true, Ordering::SeqCst);
                        Ok(true)
                    }
                }
            },
            {
                let doctor_called = Arc::clone(&doctor_called);
                move || {
                    let doctor_called = Arc::clone(&doctor_called);
                    async move {
                        doctor_called.store(true, Ordering::SeqCst);
                        Ok(0)
                    }
                }
            },
        )
        .await
        .unwrap();

        assert_eq!(outcome, InstallDoctorOutcome::Ran);
        assert!(prompt_called.load(Ordering::SeqCst));
        assert!(doctor_called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn persist_install_outputs_with_settings_rolls_back_new_files_on_secret_failure() {
        let dir = tempfile::tempdir().unwrap();
        let server_env_pairs = [("SESSION_SECRET".to_string(), "session".to_string())];
        let vault_secrets = [CreateSecretRequest {
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
        assert!(
            !Storage::new(dir.path())
                .runtime_directory()
                .env_path()
                .exists()
        );
        assert!(!settings_path.exists());
        assert!(stop_called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn persist_install_outputs_with_settings_restores_previous_contents_on_secret_failure() {
        let dir = tempfile::tempdir().unwrap();
        let server_env_pairs = [("SESSION_SECRET".to_string(), "session".to_string())];
        let vault_secrets = [CreateSecretRequest {
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
    fn persist_github_install_changes_replaces_app_env_keys_with_token_secret() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::new(dir.path());
        let server_env_path = storage.runtime_directory().env_path();
        envfile::write_env_file(
            &server_env_path,
            &std::collections::HashMap::from([
                (
                    GITHUB_APP_PRIVATE_KEY_KEY.to_string(),
                    "private".to_string(),
                ),
                (
                    GITHUB_APP_CLIENT_SECRET_KEY.to_string(),
                    "client".to_string(),
                ),
                (
                    GITHUB_APP_WEBHOOK_SECRET_KEY.to_string(),
                    "webhook".to_string(),
                ),
                ("KEEP_ME".to_string(), "1".to_string()),
            ]),
        )
        .unwrap();

        let settings_path = dir.path().join(SETTINGS_CONFIG_FILENAME);
        std::fs::write(&settings_path, "before").unwrap();

        persist_github_install_changes(dir.path(), &PendingGitHubInstallWrite {
            settings_write:    PendingSettingsWrite {
                path:              &settings_path,
                contents:          "after",
                previous_contents: Some("before"),
            },
            server_env_set:    Vec::new(),
            server_env_remove: vec![
                GITHUB_APP_PRIVATE_KEY_KEY,
                GITHUB_APP_CLIENT_SECRET_KEY,
                GITHUB_APP_WEBHOOK_SECRET_KEY,
            ],
            vault_set:         vec![(GITHUB_TOKEN_SECRET_KEY.to_string(), "token".to_string())],
            vault_remove:      Vec::new(),
        })
        .unwrap();

        let server_env = envfile::read_env_file(&server_env_path).unwrap();
        assert_eq!(server_env.get("KEEP_ME").map(String::as_str), Some("1"));
        assert!(!server_env.contains_key(GITHUB_APP_PRIVATE_KEY_KEY));
        assert!(!server_env.contains_key(GITHUB_APP_CLIENT_SECRET_KEY));
        assert!(!server_env.contains_key(GITHUB_APP_WEBHOOK_SECRET_KEY));

        let vault = Vault::load(storage.secrets_path()).unwrap();
        assert_eq!(vault.get(GITHUB_TOKEN_SECRET_KEY), Some("token"));
        assert_eq!(
            vault
                .get_entry(GITHUB_TOKEN_SECRET_KEY)
                .map(|entry| entry.secret_type),
            Some(VaultSecretType::Environment)
        );
        assert_eq!(std::fs::read_to_string(&settings_path).unwrap(), "after");
    }

    #[test]
    fn persist_github_install_changes_replaces_token_secret_with_app_env_keys() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::new(dir.path());
        let server_env_path = storage.runtime_directory().env_path();
        envfile::write_env_file(
            &server_env_path,
            &std::collections::HashMap::from([("KEEP_ME".to_string(), "1".to_string())]),
        )
        .unwrap();

        let mut vault = Vault::load(storage.secrets_path()).unwrap();
        vault
            .set(
                GITHUB_TOKEN_SECRET_KEY,
                "token",
                VaultSecretType::Environment,
                None,
            )
            .unwrap();

        let settings_path = dir.path().join(SETTINGS_CONFIG_FILENAME);
        std::fs::write(&settings_path, "before").unwrap();

        persist_github_install_changes(dir.path(), &PendingGitHubInstallWrite {
            settings_write:    PendingSettingsWrite {
                path:              &settings_path,
                contents:          "after",
                previous_contents: Some("before"),
            },
            server_env_set:    vec![
                (
                    GITHUB_APP_PRIVATE_KEY_KEY.to_string(),
                    "private".to_string(),
                ),
                (
                    GITHUB_APP_CLIENT_SECRET_KEY.to_string(),
                    "client".to_string(),
                ),
            ],
            server_env_remove: vec![
                GITHUB_APP_PRIVATE_KEY_KEY,
                GITHUB_APP_CLIENT_SECRET_KEY,
                GITHUB_APP_WEBHOOK_SECRET_KEY,
            ],
            vault_set:         Vec::new(),
            vault_remove:      vec![GITHUB_TOKEN_SECRET_KEY],
        })
        .unwrap();

        let server_env = envfile::read_env_file(&server_env_path).unwrap();
        assert_eq!(server_env.get("KEEP_ME").map(String::as_str), Some("1"));
        assert_eq!(
            server_env
                .get(GITHUB_APP_PRIVATE_KEY_KEY)
                .map(String::as_str),
            Some("private")
        );
        assert_eq!(
            server_env
                .get(GITHUB_APP_CLIENT_SECRET_KEY)
                .map(String::as_str),
            Some("client")
        );
        assert!(!server_env.contains_key(GITHUB_APP_WEBHOOK_SECRET_KEY));

        let vault = Vault::load(storage.secrets_path()).unwrap();
        assert_eq!(vault.get(GITHUB_TOKEN_SECRET_KEY), None);
        assert_eq!(std::fs::read_to_string(&settings_path).unwrap(), "after");
    }

    #[tokio::test]
    async fn write_artifact_store_metadata_creates_marker_in_resolved_store() {
        let dir = tempfile::tempdir().unwrap();
        let settings = fabro_config::ServerSettingsBuilder::from_toml(&format!(
            r#"
_version = 1

[server.auth]
methods = ["dev-token"]

[server.storage]
root = "{}"
"#,
            dir.path().display()
        ))
        .unwrap();

        write_artifact_store_metadata(&settings, "test-version")
            .await
            .unwrap();

        let value: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(
                dir.path()
                    .join("objects")
                    .join("artifacts")
                    .join("store-metadata.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(value["fabro_version"], "test-version");
        assert!(value["created_at"].as_str().is_some());
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

    #[test]
    fn validate_install_github_non_interactive_rejects_owner_for_token() {
        let err = validate_install_github_non_interactive(
            &InstallGithubArgs {
                strategy: Some(InstallGitHubStrategyArg::Token),
                owner:    Some("personal".to_string()),
            },
            true,
        )
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("--owner is only supported with --strategy app")
        );
    }

    #[test]
    fn validate_install_github_non_interactive_requires_owner_for_app() {
        let err = validate_install_github_non_interactive(
            &InstallGithubArgs {
                strategy: Some(InstallGitHubStrategyArg::App),
                owner:    None,
            },
            true,
        )
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("install github --non-interactive requires --owner for --strategy app")
        );
    }
}
