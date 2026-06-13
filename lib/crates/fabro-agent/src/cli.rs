#[expect(
    clippy::disallowed_types,
    reason = "CLI entry point writes to stdout/stderr; blocking std::io::Write is intentional and \
              scoped to the CLI binary, not to any library code used by Tokio services"
)]
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Context as _;
use clap::{Args, Parser};
use fabro_auth::{CredentialSource, EnvCredentialSource, VaultCredentialSource};
use fabro_config::Storage;
use fabro_config::user::default_storage_dir;
use fabro_llm::Error as LlmError;
use fabro_llm::client::Client;
use fabro_llm::middleware::{Middleware, NextFn, NextStreamFn};
use fabro_llm::provider::StreamEventStream;
use fabro_llm::types::{Request, Response};
use fabro_mcp::config::McpServerSettings;
#[cfg(test)]
use fabro_model::catalog::LlmCatalogSettings;
use fabro_model::{AgentProfileKind, Catalog, ModelHandle, ProviderId};
use fabro_static::EnvVars;
use fabro_util::terminal::Styles;
use fabro_vault::Vault;
use tokio::io::{AsyncWriteExt, stdout};
use tokio::signal;
use tokio::sync::{Mutex as AsyncMutex, RwLock as AsyncRwLock};

use crate::config::{ToolApprovalAdapter, ToolApprovalFn, ToolHookCallback, ToolSecrets};
use crate::error::InterruptReason;
use crate::subagent::{SessionFactory, SubAgentManager};
use crate::tool_permissions::{is_auto_approved, tool_category};
use crate::tools::WebFetchSummarizer;
use crate::{
    AgentEvent, AgentProfile, AnthropicProfile, GeminiProfile, LocalSandbox, Message,
    OpenAiProfile, Sandbox, Session, SessionOptions,
};

#[expect(
    clippy::disallowed_methods,
    reason = "Standalone agent CLI explicitly passes the Brave Search process-env credential into tool configuration."
)]
fn cli_tool_secrets() -> ToolSecrets {
    ToolSecrets {
        brave_search_api_key: std::env::var(EnvVars::BRAVE_SEARCH_API_KEY).ok(),
    }
}

/// Public arguments for the agent command, usable from an external CLI.
#[derive(Args)]
pub struct AgentArgs {
    /// Task prompt
    pub prompt: String,

    /// LLM provider (built-in or configured provider ID)
    #[arg(long)]
    pub provider: Option<String>,

    /// Model name (defaults per provider)
    #[arg(long)]
    pub model: Option<String>,

    /// Permission level for tool execution
    #[arg(long, value_enum)]
    pub permissions: Option<PermissionLevel>,

    /// Skip interactive prompts; deny tools outside permission level
    #[arg(long)]
    pub auto_approve: bool,

    /// Print LLM request/response debug info to stderr
    #[arg(long)]
    pub debug: bool,

    /// Print full LLM request/response JSON to stderr
    #[arg(long)]
    pub verbose: bool,

    /// Directory containing skill files (overrides default discovery)
    #[arg(long)]
    pub skills_dir: Option<String>,

    /// Output format (text for human-readable, json for NDJSON event stream)
    #[arg(long, value_enum)]
    pub output_format: Option<OutputFormat>,
}

#[derive(Parser)]
#[command(name = "fabro-agent")]
struct Cli {
    #[command(flatten)]
    args: AgentArgs,
}

/// Output format for the `fabro exec` / agent CLI.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize, clap::ValueEnum,
)]
#[serde(rename_all = "kebab-case")]
pub enum OutputFormat {
    Text,
    Json,
}

pub use fabro_types::{AgentToolCategory, PermissionLevel};

impl AgentArgs {
    /// Fill `None` fields from settings.toml values, then hardcoded defaults.
    pub fn apply_cli_defaults(
        &mut self,
        provider: Option<&str>,
        model: Option<&str>,
        permissions: Option<PermissionLevel>,
        output_format: Option<OutputFormat>,
    ) {
        self.provider = self
            .provider
            .take()
            .or_else(|| provider.map(String::from))
            .or_else(|| Some("anthropic".to_string()));
        self.model = self.model.take().or_else(|| model.map(String::from));
        self.permissions = self
            .permissions
            .or(permissions)
            .or(Some(PermissionLevel::ReadWrite));
        self.output_format = self
            .output_format
            .or(output_format)
            .or(Some(OutputFormat::Text));
    }
}

#[allow(
    clippy::print_stderr,
    reason = "Interactive approval prompts belong on stderr, not assistant output."
)]
#[expect(
    clippy::disallowed_methods,
    reason = "Interactive tool approval blocks on stdin and stderr by design."
)]
fn build_tool_approval(
    permissions: PermissionLevel,
    is_interactive: bool,
    styles: &'static Styles,
) -> ToolApprovalFn {
    let level = Arc::new(Mutex::new(permissions));

    Arc::new(move |tool_name: &str, _args: &serde_json::Value| {
        let current_level = *level.lock().expect("permission lock poisoned");

        if is_auto_approved(current_level, tool_category(tool_name)) {
            return Ok(());
        }

        if !is_interactive {
            return Err(format!(
                "{tool_name} tool denied at current permission level"
            ));
        }

        // Interactive prompt on stderr
        let category = tool_category(tool_name);
        eprint!(
            "Allow {} ({category})? [y]es / [n]o / [a]lways: ",
            styles.bold.apply_to(tool_name),
        );
        // `AgentToolCategory` derives strum::Display so it renders as the
        // canonical snake_case label (e.g. "read", "write").
        std::io::stderr().flush().ok();

        let mut input = String::new();
        std::io::stdin()
            .read_line(&mut input)
            .map_err(|e| format!("Failed to read input: {e}"))?;

        match input.trim().to_lowercase().as_str() {
            "y" | "yes" => Ok(()),
            "a" | "always" => {
                let mut lvl = level.lock().expect("permission lock poisoned");
                *lvl = if category == AgentToolCategory::Write {
                    PermissionLevel::ReadWrite
                } else {
                    PermissionLevel::Full
                };
                Ok(())
            }
            _ => Err(format!("{tool_name} tool denied by user")),
        }
    })
}

fn summarizer_model_id(
    provider_id: &ProviderId,
    catalog: &Catalog,
    selected_model: &str,
) -> ModelHandle {
    ModelHandle::ByName {
        provider: provider_id.clone(),
        model:    catalog
            .default_for_provider(provider_id)
            .map_or_else(
                || match provider_id.as_str() {
                    ProviderId::ANTHROPIC => "claude-haiku-4-5",
                    ProviderId::GEMINI => "gemini-2.0-flash",
                    _ => selected_model,
                },
                |model| model.id.as_str(),
            )
            .to_string(),
    }
}

fn build_summarizer(
    provider_id: &ProviderId,
    model: &str,
    catalog: &Catalog,
    llm_client: Client,
) -> WebFetchSummarizer {
    WebFetchSummarizer {
        client:   llm_client,
        model_id: summarizer_model_id(provider_id, catalog, model),
    }
}

fn build_profile(
    profile_kind: AgentProfileKind,
    provider_id: ProviderId,
    model: &str,
    summarizer: Option<WebFetchSummarizer>,
    catalog: Arc<Catalog>,
) -> Box<dyn AgentProfile> {
    match profile_kind {
        AgentProfileKind::OpenAi => Box::new(
            OpenAiProfile::with_summarizer(model, summarizer)
                .with_provider_id(provider_id)
                .with_catalog(catalog),
        ),
        AgentProfileKind::Gemini => Box::new(
            GeminiProfile::with_summarizer(model, summarizer)
                .with_provider_id(provider_id)
                .with_catalog(catalog),
        ),
        AgentProfileKind::Anthropic => Box::new(
            AnthropicProfile::with_summarizer(model, summarizer)
                .with_provider_id(provider_id)
                .with_catalog(catalog),
        ),
    }
}

fn parse_provider(args: &AgentArgs) -> anyhow::Result<ProviderId> {
    let provider_str = args.provider.as_deref().unwrap_or("anthropic");
    Ok(provider_str.parse()?)
}

fn resolve_provider_id(catalog: &Catalog, args: &AgentArgs) -> anyhow::Result<ProviderId> {
    if args.provider.is_some() {
        let requested = parse_provider(args)?;
        return Ok(catalog
            .provider(&requested)
            .map_or(requested, |provider| provider.id.clone()));
    }
    if let Some(model_id) = args.model.as_deref() {
        if let Some(model) = catalog.get(model_id) {
            return Ok(model.provider.clone());
        }
    }
    let requested = parse_provider(args)?;
    Ok(catalog
        .provider(&requested)
        .map_or(requested, |provider| provider.id.clone()))
}

fn standalone_llm_source() -> Arc<dyn CredentialSource> {
    let storage_dir = default_storage_dir();
    match Vault::load(Storage::new(storage_dir).secrets_path()) {
        Ok(vault) => Arc::new(VaultCredentialSource::new(Arc::new(AsyncRwLock::new(
            vault,
        )))),
        Err(_) => Arc::new(EnvCredentialSource::new()),
    }
}

fn profile_kind_for_provider(
    catalog: &Catalog,
    provider_id: &ProviderId,
    model: Option<&str>,
) -> anyhow::Result<AgentProfileKind> {
    catalog
        .effective_agent_profile(provider_id, model)
        .ok_or_else(|| anyhow::anyhow!("provider '{provider_id}' is not configured"))
}

fn ensure_provider_registered(client: &Client, provider_id: &ProviderId) -> anyhow::Result<()> {
    if client
        .provider_names()
        .iter()
        .any(|name| *name == provider_id.as_str())
    {
        return Ok(());
    }

    anyhow::bail!("LLM credentials not configured for provider '{provider_id}'");
}

fn format_tool_args(args: &serde_json::Value, cwd: &str) -> String {
    let cwd_prefix = if cwd.ends_with('/') {
        cwd.to_string()
    } else {
        format!("{cwd}/")
    };
    let Some(obj) = args.as_object() else {
        return args.to_string();
    };
    obj.iter()
        .map(|(k, v)| match v {
            serde_json::Value::String(s) => {
                let s = s.strip_prefix(&cwd_prefix).unwrap_or(s);
                let display = if s.len() > 80 {
                    format!("{}...", &s[..s.floor_char_boundary(77)])
                } else {
                    s.to_string()
                };
                format!("{k}={display:?}")
            }
            other => format!("{k}={other}"),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[allow(
    clippy::print_stdout,
    reason = "Assistant responses are the CLI's primary stdout output."
)]
fn print_output(session: &Session, styles: &Styles) {
    for turn in session.history().turns() {
        if let Message::Assistant { content, .. } = turn {
            if !content.is_empty() {
                println!("{}", styles.render_markdown(content));
            }
        }
    }
}

#[allow(
    clippy::print_stderr,
    reason = "Session summaries are diagnostic metadata, not assistant output."
)]
fn print_summary(session: &Session, styles: &Styles) {
    let (mut turn_count, mut tool_call_count, mut total_tokens) = (0usize, 0usize, 0i64);
    for turn in session.history().turns() {
        if let Message::Assistant {
            tool_calls, usage, ..
        } = turn
        {
            turn_count += 1;
            tool_call_count += tool_calls.len();
            total_tokens += usage.total_tokens();
        }
    }
    let token_str = if total_tokens >= 1_000_000 {
        format!("{:.1}m", total_tokens as f64 / 1_000_000.0)
    } else if total_tokens >= 1000 {
        format!("{}k", total_tokens / 1000)
    } else {
        total_tokens.to_string()
    };
    eprintln!(
        "{}",
        styles.dim.apply_to(format!(
            "Done ({turn_count} turns, {tool_call_count} tools, {token_str} toks)"
        )),
    );
}

/// Middleware that logs LLM request/response summaries to stderr.
struct DebugMiddleware {
    styles: &'static Styles,
}

#[async_trait::async_trait]
impl Middleware for DebugMiddleware {
    #[allow(
        clippy::print_stderr,
        reason = "Debug middleware logs request and response summaries to stderr."
    )]
    async fn handle_complete(&self, request: Request, next: NextFn) -> Result<Response, LlmError> {
        let s = self.styles;
        eprintln!(
            "{}",
            s.dim.apply_to(format!(
                "[debug] request: model={} messages={} tools={}",
                request.model,
                request.messages.len(),
                request.tools.as_ref().map_or(0, Vec::len),
            )),
        );
        let response = next(request).await?;
        eprintln!(
            "{}",
            s.dim.apply_to(format!(
                "[debug] response: model={} finish={:?} usage=({}/{}/{})",
                response.model,
                response.finish_reason,
                response.usage.input_tokens,
                response.usage.output_tokens,
                response.usage.total_tokens(),
            )),
        );
        Ok(response)
    }

    async fn handle_stream(
        &self,
        request: Request,
        next: NextStreamFn,
    ) -> Result<StreamEventStream, LlmError> {
        next(request).await
    }
}

/// Middleware that logs full LLM request/response JSON to stderr.
struct VerboseMiddleware {
    styles: &'static Styles,
}

#[async_trait::async_trait]
impl Middleware for VerboseMiddleware {
    #[allow(
        clippy::print_stderr,
        reason = "Verbose middleware dumps full request and response JSON to stderr."
    )]
    async fn handle_complete(&self, request: Request, next: NextFn) -> Result<Response, LlmError> {
        let s = self.styles;
        eprintln!(
            "{}\n{}",
            s.dim.apply_to("[verbose] request:"),
            serde_json::to_string_pretty(&request)
                .unwrap_or_else(|e| format!("<serialize error: {e}>"))
        );
        let response = next(request).await?;
        eprintln!(
            "{}\n{}",
            s.dim.apply_to("[verbose] response:"),
            serde_json::to_string_pretty(&response)
                .unwrap_or_else(|e| format!("<serialize error: {e}>"))
        );
        Ok(response)
    }

    async fn handle_stream(
        &self,
        request: Request,
        next: NextStreamFn,
    ) -> Result<StreamEventStream, LlmError> {
        next(request).await
    }
}

pub async fn run_with_args(
    args: AgentArgs,
    mcp_servers: Vec<McpServerSettings>,
) -> anyhow::Result<()> {
    let llm_source = standalone_llm_source();
    let catalog =
        Arc::new(Catalog::from_builtin().context("failed to build standalone agent LLM catalog")?);
    run_with_args_and_source_and_catalog(args, llm_source, mcp_servers, catalog).await
}

#[allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "Assistant output stays on stdout while prompts and diagnostics use stderr."
)]
pub async fn run_with_args_and_source(
    args: AgentArgs,
    llm_source: Arc<dyn CredentialSource>,
    mcp_servers: Vec<McpServerSettings>,
) -> anyhow::Result<()> {
    let catalog =
        Arc::new(Catalog::from_builtin().context("failed to build standalone agent LLM catalog")?);
    run_with_args_and_source_and_catalog(args, llm_source, mcp_servers, catalog).await
}

#[allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "Assistant output stays on stdout while prompts and diagnostics use stderr."
)]
pub async fn run_with_args_and_source_and_catalog(
    args: AgentArgs,
    llm_source: Arc<dyn CredentialSource>,
    mcp_servers: Vec<McpServerSettings>,
    catalog: Arc<Catalog>,
) -> anyhow::Result<()> {
    let client = Client::from_source(llm_source.as_ref(), Arc::clone(&catalog))
        .await
        .context("Failed to create LLM client")?;
    run_with_args_and_client_and_catalog(args, client, mcp_servers, catalog).await
}

#[allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "Assistant output stays on stdout while prompts and diagnostics use stderr."
)]
pub async fn run_with_args_and_client(
    args: AgentArgs,
    client: Client,
    mcp_servers: Vec<McpServerSettings>,
) -> anyhow::Result<()> {
    let catalog =
        Arc::new(Catalog::from_builtin().context("failed to build standalone agent LLM catalog")?);
    run_with_args_and_client_and_catalog(args, client, mcp_servers, catalog).await
}

#[allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "Assistant output stays on stdout while prompts and diagnostics use stderr."
)]
pub async fn run_with_args_and_client_and_catalog(
    args: AgentArgs,
    mut client: Client,
    mcp_servers: Vec<McpServerSettings>,
    catalog: Arc<Catalog>,
) -> anyhow::Result<()> {
    // Resolve color support once, leak to get 'static lifetime for use across
    // threads
    let styles: &'static Styles = Box::leak(Box::new(Styles::detect_stderr()));

    let provider_id = resolve_provider_id(&catalog, &args)?;
    ensure_provider_registered(&client, &provider_id)?;

    if args.verbose {
        client.add_middleware(Arc::new(VerboseMiddleware { styles }));
    } else if args.debug {
        client.add_middleware(Arc::new(DebugMiddleware { styles }));
    }

    let model = if let Some(model) = args.model.clone() {
        model
    } else {
        catalog
            .default_for_provider(&provider_id)
            .map(|model| model.id.clone())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "provider '{provider_id}' has no default model in the catalog; pass --model explicitly"
                )
            })?
    };
    let profile_kind = profile_kind_for_provider(&catalog, &provider_id, Some(&model))?;
    eprintln!("{}", styles.dim.apply_to(format!("Using model: {model}")));
    let mut profile = build_profile(
        profile_kind,
        provider_id.clone(),
        &model,
        Some(build_summarizer(
            &provider_id,
            &model,
            &catalog,
            client.clone(),
        )),
        Arc::clone(&catalog),
    );

    // Build sandbox
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let cwd_str = cwd.to_string_lossy().to_string();
    let env: Arc<dyn Sandbox> = Arc::new(crate::ReadBeforeWriteSandbox::new(Arc::new(
        LocalSandbox::new(cwd),
    )));

    // Build tool approval callback
    let permissions = args.permissions.unwrap_or(PermissionLevel::ReadWrite);
    #[expect(
        clippy::disallowed_methods,
        reason = "is_terminal() on stdin is a non-blocking fstat; no actual I/O performed"
    )]
    let is_interactive = std::io::stdin().is_terminal() && !args.auto_approve;
    let tool_approval = build_tool_approval(permissions, is_interactive, styles);
    let tool_hooks: Arc<dyn ToolHookCallback> = Arc::new(ToolApprovalAdapter(tool_approval));

    let config = SessionOptions {
        tool_hooks: Some(tool_hooks.clone()),
        permission_level: Some(permissions),
        skill_dirs: args.skills_dir.map(|d| vec![d]),
        mcp_servers,
        tool_secrets: cli_tool_secrets(),
        ..SessionOptions::default()
    };

    // Register subagent tools
    let manager = Arc::new(AsyncMutex::new(SubAgentManager::new(
        config.max_subagent_depth,
    )));
    let manager_for_callback = manager.clone();
    let factory_client = client.clone();
    let factory_model = model.clone();
    let factory_catalog = Arc::clone(&catalog);
    let factory_provider_id = provider_id.clone();
    let factory_profile_kind = profile_kind;
    let factory_env = Arc::clone(&env);
    let factory_hooks = config.tool_hooks.clone();
    let factory_permission_level = config.permission_level;
    let factory_tool_secrets = config.tool_secrets.clone();
    let factory: SessionFactory = Arc::new(move || {
        let child_summarizer = Some(build_summarizer(
            &factory_provider_id,
            &factory_model,
            &factory_catalog,
            factory_client.clone(),
        ));
        let child_profile: Arc<dyn AgentProfile> = Arc::from(build_profile(
            factory_profile_kind,
            factory_provider_id.clone(),
            &factory_model,
            child_summarizer,
            Arc::clone(&factory_catalog),
        ));
        Session::new(
            factory_client.clone(),
            child_profile,
            Arc::clone(&factory_env),
            SessionOptions {
                tool_hooks: factory_hooks.clone(),
                permission_level: factory_permission_level,
                tool_secrets: factory_tool_secrets.clone(),
                ..SessionOptions::default()
            },
            None,
        )
    });
    profile.register_subagent_tools(manager, factory, 0);
    let profile: Arc<dyn AgentProfile> = Arc::from(profile);

    let mut session = Session::new(
        client,
        profile,
        env,
        config,
        Some(manager_for_callback.clone()),
    );

    // Wire subagent event callback to parent session's emitter
    manager_for_callback
        .lock()
        .await
        .set_event_callback(session.sub_agent_event_callback());

    // SIGINT handler
    let cancel_token = session.cancel_token();
    let interrupt_reason = session.interrupt_reason_handle();
    tokio::spawn(async move {
        signal::ctrl_c().await.ok();
        {
            let mut guard = interrupt_reason
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if guard.is_none() {
                *guard = Some(InterruptReason::Cancelled);
            }
        }
        cancel_token.cancel();
    });

    // Subscribe to events
    let verbose = args.verbose;
    let output_format = args.output_format.unwrap_or(OutputFormat::Text);
    let mut rx = session.subscribe();
    tokio::spawn(async move {
        match output_format {
            OutputFormat::Json => {
                let mut stdout = stdout();
                while let Ok(event) = rx.recv().await {
                    if let Ok(json) = serde_json::to_string(&event) {
                        let _ = stdout.write_all(json.as_bytes()).await;
                        let _ = stdout.write_all(b"\n").await;
                        let _ = stdout.flush().await;
                    }
                }
            }
            OutputFormat::Text => {
                let s = styles;
                while let Ok(event) = rx.recv().await {
                    let child_prefix = if event.parent_session_id.is_some() {
                        format!("[child {}] ", event.session_id)
                    } else {
                        String::new()
                    };
                    match &event.event {
                        AgentEvent::ToolCallStarted {
                            tool_name,
                            arguments,
                            ..
                        } => {
                            eprintln!(
                                "  {} {}{}",
                                s.dim.apply_to("\u{25cf}"),
                                s.bold_cyan.apply_to(format!("{child_prefix}{tool_name}")),
                                s.dim.apply_to(format!(
                                    "({})",
                                    format_tool_args(arguments, &cwd_str)
                                )),
                            );
                        }
                        AgentEvent::ToolCallCompleted {
                            tool_name,
                            output,
                            is_error,
                            ..
                        } if verbose => {
                            let label = if *is_error {
                                "tool error"
                            } else {
                                "tool result"
                            };
                            eprintln!(
                                "  {}\n{}",
                                s.dim
                                    .apply_to(format!("[{label}] {child_prefix}{tool_name}:")),
                                serde_json::to_string_pretty(output)
                                    .unwrap_or_else(|_| output.to_string()),
                            );
                        }
                        AgentEvent::Error { error } => {
                            eprintln!(
                                "  {}",
                                s.red.apply_to(format!("\u{2717} {child_prefix}{error}")),
                            );
                        }
                        AgentEvent::SubAgentSpawned {
                            agent_id,
                            depth,
                            task,
                            ..
                        } => {
                            let task_preview = if task.len() > 60 {
                                &task[..task.floor_char_boundary(60)]
                            } else {
                                task
                            };
                            eprintln!(
                                "  {}",
                                s.dim.apply_to(format!(
                                    "{child_prefix}\u{25b6} subagent {agent_id} spawned (depth={depth}) task={task_preview:?}"
                                )),
                            );
                        }
                        AgentEvent::SubAgentCompleted {
                            agent_id,
                            depth,
                            success,
                            turns_used,
                        } => {
                            eprintln!(
                                "  {}",
                                s.dim.apply_to(format!(
                                    "{child_prefix}\u{25a0} subagent {agent_id} completed (depth={depth}, success={success}, turns={turns_used})"
                                )),
                            );
                        }
                        AgentEvent::SubAgentFailed {
                            agent_id,
                            depth,
                            error,
                        } => {
                            eprintln!(
                                "  {}",
                                s.red.apply_to(format!(
                                    "{child_prefix}\u{2717} subagent {agent_id} failed (depth={depth}): {error}"
                                )),
                            );
                        }
                        AgentEvent::SubAgentClosed { agent_id, depth } => {
                            eprintln!(
                                "  {}",
                                s.dim.apply_to(format!(
                                    "{child_prefix}\u{25a0} subagent {agent_id} closed (depth={depth})"
                                )),
                            );
                        }
                        _ => {}
                    }
                }
            }
        }
    });

    // Initialize and run
    session.initialize().await?;
    let result = session.process_input(&args.prompt).await;

    if matches!(output_format, OutputFormat::Text) {
        // Print assistant text to stdout
        print_output(&session, styles);

        // Print completion summary to stderr
        print_summary(&session, styles);
    }

    // Propagate errors for exit code
    result?;
    Ok(())
}

pub async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let mut args = cli.args;
    args.apply_cli_defaults(None, None, None, None);
    run_with_args(args, Vec::new()).await
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use fabro_model::catalog::{
        ModelCatalogSettings, ProviderCatalogSettings, SettingsModelFeatures, SettingsModelLimits,
    };
    use serde_json::json;

    use super::*;

    static NO_COLOR: std::sync::LazyLock<Styles> = std::sync::LazyLock::new(|| Styles::new(false));

    // tool_category tests

    #[test]
    fn tool_category_read_tools() {
        assert_eq!(tool_category("read_file"), AgentToolCategory::Read);
        assert_eq!(tool_category("read_many_files"), AgentToolCategory::Read);
        assert_eq!(tool_category("grep"), AgentToolCategory::Read);
        assert_eq!(tool_category("glob"), AgentToolCategory::Read);
        assert_eq!(tool_category("list_dir"), AgentToolCategory::Read);
    }

    #[test]
    fn tool_category_write_tools() {
        assert_eq!(tool_category("write_file"), AgentToolCategory::Write);
        assert_eq!(tool_category("edit_file"), AgentToolCategory::Write);
        assert_eq!(tool_category("apply_patch"), AgentToolCategory::Write);
    }

    #[test]
    fn tool_category_shell() {
        assert_eq!(tool_category("shell"), AgentToolCategory::Shell);
    }

    #[test]
    fn tool_category_subagent_tools() {
        assert_eq!(tool_category("spawn_agent"), AgentToolCategory::Subagent);
        assert_eq!(tool_category("send_input"), AgentToolCategory::Subagent);
        assert_eq!(tool_category("wait"), AgentToolCategory::Subagent);
        assert_eq!(tool_category("close_agent"), AgentToolCategory::Subagent);
    }

    #[test]
    fn tool_category_unknown_defaults_to_shell() {
        assert_eq!(tool_category("some_random_tool"), AgentToolCategory::Shell);
    }

    // is_auto_approved tests

    #[test]
    fn is_auto_approved_read_only() {
        assert!(is_auto_approved(
            PermissionLevel::ReadOnly,
            AgentToolCategory::Read
        ));
        assert!(is_auto_approved(
            PermissionLevel::ReadOnly,
            AgentToolCategory::Subagent
        ));
        assert!(!is_auto_approved(
            PermissionLevel::ReadOnly,
            AgentToolCategory::Write
        ));
        assert!(!is_auto_approved(
            PermissionLevel::ReadOnly,
            AgentToolCategory::Shell
        ));
    }

    #[test]
    fn is_auto_approved_read_write() {
        assert!(is_auto_approved(
            PermissionLevel::ReadWrite,
            AgentToolCategory::Read
        ));
        assert!(is_auto_approved(
            PermissionLevel::ReadWrite,
            AgentToolCategory::Subagent
        ));
        assert!(is_auto_approved(
            PermissionLevel::ReadWrite,
            AgentToolCategory::Write
        ));
        assert!(!is_auto_approved(
            PermissionLevel::ReadWrite,
            AgentToolCategory::Shell
        ));
    }

    #[test]
    fn is_auto_approved_full() {
        assert!(is_auto_approved(
            PermissionLevel::Full,
            AgentToolCategory::Read
        ));
        assert!(is_auto_approved(
            PermissionLevel::Full,
            AgentToolCategory::Subagent
        ));
        assert!(is_auto_approved(
            PermissionLevel::Full,
            AgentToolCategory::Write
        ));
        assert!(is_auto_approved(
            PermissionLevel::Full,
            AgentToolCategory::Shell
        ));
    }

    // build_tool_approval non-interactive tests

    #[test]
    fn build_tool_approval_read_only_allows_read() {
        let approval_fn = build_tool_approval(PermissionLevel::ReadOnly, false, &NO_COLOR);
        assert!(approval_fn("read_file", &json!({})).is_ok());
    }

    #[test]
    fn build_tool_approval_read_only_denies_write() {
        let approval_fn = build_tool_approval(PermissionLevel::ReadOnly, false, &NO_COLOR);
        let result = approval_fn("write_file", &json!({}));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("denied"));
    }

    #[test]
    fn build_tool_approval_read_write_denies_shell() {
        let approval_fn = build_tool_approval(PermissionLevel::ReadWrite, false, &NO_COLOR);
        let result = approval_fn("shell", &json!({}));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("denied"));
    }

    #[test]
    fn build_tool_approval_full_allows_shell() {
        let approval_fn = build_tool_approval(PermissionLevel::Full, false, &NO_COLOR);
        assert!(approval_fn("shell", &json!({})).is_ok());
    }

    // build_profile tests

    fn test_catalog() -> Arc<Catalog> {
        Arc::new(Catalog::from_builtin().unwrap())
    }

    #[test]
    fn build_profile_anthropic() {
        let profile = build_profile(
            AgentProfileKind::Anthropic,
            ProviderId::anthropic(),
            "model",
            None,
            test_catalog(),
        );
        assert_eq!(profile.profile_kind(), AgentProfileKind::Anthropic);
        assert_eq!(profile.provider_id(), ProviderId::anthropic());
    }

    #[test]
    fn build_profile_openai() {
        let profile = build_profile(
            AgentProfileKind::OpenAi,
            ProviderId::openai(),
            "model",
            None,
            test_catalog(),
        );
        assert_eq!(profile.profile_kind(), AgentProfileKind::OpenAi);
        assert_eq!(profile.provider_id(), ProviderId::openai());
    }

    #[test]
    fn ensure_provider_registered_reports_missing_credentials() {
        let client = Client::new(HashMap::new(), None, vec![]);
        let error = ensure_provider_registered(&client, &ProviderId::anthropic()).unwrap_err();
        assert_eq!(
            error.to_string(),
            "LLM credentials not configured for provider 'anthropic'"
        );
    }

    #[test]
    fn build_profile_gemini() {
        let profile = build_profile(
            AgentProfileKind::Gemini,
            ProviderId::gemini(),
            "model",
            None,
            test_catalog(),
        );
        assert_eq!(profile.profile_kind(), AgentProfileKind::Gemini);
        assert_eq!(profile.provider_id(), ProviderId::gemini());
    }

    #[test]
    fn profile_kind_accepts_custom_catalog_provider() {
        let mut settings = LlmCatalogSettings::default();
        settings
            .providers
            .insert("bedrock".to_string(), ProviderCatalogSettings {
                display_name: Some("Bedrock".to_string()),
                adapter: Some("openai_compatible".to_string()),
                base_url: Some("https://example.invalid/v1".to_string()),
                agent_profile: Some(AgentProfileKind::OpenAi),
                ..ProviderCatalogSettings::default()
            });
        let catalog = Catalog::from_builtin_with_overrides(&settings).unwrap();
        let args = AgentArgs {
            prompt:        "test".to_string(),
            provider:      Some("bedrock".to_string()),
            model:         None,
            permissions:   None,
            auto_approve:  false,
            debug:         false,
            verbose:       false,
            skills_dir:    None,
            output_format: None,
        };

        let provider_id = parse_provider(&args).unwrap();
        assert_eq!(provider_id, ProviderId::new("bedrock"));
        assert_eq!(
            profile_kind_for_provider(&catalog, &provider_id, None).unwrap(),
            AgentProfileKind::OpenAi
        );
    }

    #[test]
    fn standalone_provider_resolution_uses_catalog_model_provider_when_provider_omitted() {
        let mut settings = LlmCatalogSettings::default();
        settings
            .providers
            .insert("bedrock".to_string(), ProviderCatalogSettings {
                display_name: Some("Bedrock".to_string()),
                adapter: Some("openai_compatible".to_string()),
                base_url: Some("https://example.invalid/v1".to_string()),
                agent_profile: Some(AgentProfileKind::OpenAi),
                ..ProviderCatalogSettings::default()
            });
        settings
            .models
            .insert("bedrock-claude".to_string(), ModelCatalogSettings {
                provider: Some("bedrock".to_string()),
                display_name: Some("Bedrock Claude".to_string()),
                family: Some("claude".to_string()),
                default: Some(true),
                limits: Some(SettingsModelLimits {
                    context_window: Some(1000),
                    max_output:     None,
                }),
                features: Some(SettingsModelFeatures {
                    tools:            Some(true),
                    vision:           Some(false),
                    reasoning:        Some(false),
                    reasoning_effort: None,
                    prompt_cache:     None,
                    sampling_params:  None,
                }),
                ..ModelCatalogSettings::default()
            });
        let catalog = Catalog::from_builtin_with_overrides(&settings).unwrap();
        let args = AgentArgs {
            prompt:        "test".to_string(),
            provider:      None,
            model:         Some("bedrock-claude".to_string()),
            permissions:   None,
            auto_approve:  false,
            debug:         false,
            verbose:       false,
            skills_dir:    None,
            output_format: None,
        };

        assert_eq!(
            resolve_provider_id(&catalog, &args).unwrap(),
            ProviderId::new("bedrock")
        );
    }

    #[test]
    fn standalone_provider_resolution_canonicalizes_explicit_provider_alias() {
        let mut settings = LlmCatalogSettings::default();
        settings
            .providers
            .insert("bedrock".to_string(), ProviderCatalogSettings {
                display_name: Some("Bedrock".to_string()),
                adapter: Some("openai_compatible".to_string()),
                base_url: Some("https://example.invalid/v1".to_string()),
                agent_profile: Some(AgentProfileKind::OpenAi),
                aliases: Some(vec!["br".to_string()]),
                ..ProviderCatalogSettings::default()
            });
        let catalog = Catalog::from_builtin_with_overrides(&settings).unwrap();
        let args = AgentArgs {
            prompt:        "test".to_string(),
            provider:      Some("br".to_string()),
            model:         None,
            permissions:   None,
            auto_approve:  false,
            debug:         false,
            verbose:       false,
            skills_dir:    None,
            output_format: None,
        };

        assert_eq!(
            resolve_provider_id(&catalog, &args).unwrap(),
            ProviderId::new("bedrock")
        );
    }

    #[test]
    fn standalone_profile_kind_uses_model_agent_profile_override() {
        let mut settings = LlmCatalogSettings::default();
        settings
            .providers
            .insert("bedrock".to_string(), ProviderCatalogSettings {
                display_name: Some("Bedrock".to_string()),
                adapter: Some("openai_compatible".to_string()),
                base_url: Some("https://example.invalid/v1".to_string()),
                agent_profile: Some(AgentProfileKind::OpenAi),
                ..ProviderCatalogSettings::default()
            });
        settings
            .models
            .insert("bedrock-claude".to_string(), ModelCatalogSettings {
                provider: Some("bedrock".to_string()),
                display_name: Some("Bedrock Claude".to_string()),
                family: Some("claude".to_string()),
                default: Some(true),
                agent_profile: Some(AgentProfileKind::Anthropic),
                limits: Some(SettingsModelLimits {
                    context_window: Some(1000),
                    max_output:     None,
                }),
                features: Some(SettingsModelFeatures {
                    tools:            Some(true),
                    vision:           Some(false),
                    reasoning:        Some(false),
                    reasoning_effort: None,
                    prompt_cache:     None,
                    sampling_params:  None,
                }),
                ..ModelCatalogSettings::default()
            });
        let catalog = Catalog::from_builtin_with_overrides(&settings).unwrap();

        assert_eq!(
            profile_kind_for_provider(
                &catalog,
                &ProviderId::new("bedrock"),
                Some("bedrock-claude")
            )
            .unwrap(),
            AgentProfileKind::Anthropic
        );
    }

    #[test]
    fn summarizer_model_id_uses_selected_model_for_custom_provider_without_default() {
        let mut settings = LlmCatalogSettings::default();
        settings
            .providers
            .insert("bedrock".to_string(), ProviderCatalogSettings {
                display_name: Some("Bedrock".to_string()),
                adapter: Some("openai_compatible".to_string()),
                base_url: Some("https://example.invalid/v1".to_string()),
                agent_profile: Some(AgentProfileKind::OpenAi),
                ..ProviderCatalogSettings::default()
            });
        let catalog = Catalog::from_builtin_with_overrides(&settings).unwrap();
        let provider_id = ProviderId::new("bedrock");

        let model_id = summarizer_model_id(&provider_id, &catalog, "bedrock-claude-sonnet-4-6");

        assert_eq!(model_id.provider(), &provider_id);
        assert_eq!(model_id.model_id(), "bedrock-claude-sonnet-4-6");
    }

    #[test]
    fn summarizer_model_id_ignores_profile_for_custom_provider_without_default() {
        let mut settings = LlmCatalogSettings::default();
        settings
            .providers
            .insert("bedrock".to_string(), ProviderCatalogSettings {
                display_name: Some("Bedrock".to_string()),
                adapter: Some("openai_compatible".to_string()),
                base_url: Some("https://example.invalid/v1".to_string()),
                agent_profile: Some(AgentProfileKind::Anthropic),
                ..ProviderCatalogSettings::default()
            });
        let catalog = Catalog::from_builtin_with_overrides(&settings).unwrap();
        let provider_id = ProviderId::new("bedrock");

        let model_id = summarizer_model_id(&provider_id, &catalog, "bedrock-claude-sonnet-4-6");

        assert_eq!(model_id.provider(), &provider_id);
        assert_eq!(model_id.model_id(), "bedrock-claude-sonnet-4-6");
    }

    // subagent tool registration tests

    #[test]
    fn build_profile_can_register_subagent_tools() {
        let mut profile = build_profile(
            AgentProfileKind::Anthropic,
            ProviderId::anthropic(),
            "model",
            None,
            test_catalog(),
        );
        let manager = Arc::new(AsyncMutex::new(SubAgentManager::new(1)));
        let factory: SessionFactory = Arc::new(|| {
            panic!("factory should not be called in this test");
        });
        profile.register_subagent_tools(manager, factory, 0);

        let names = profile.tool_registry().names();
        assert!(names.contains(&"spawn_agent".to_string()));
        assert!(names.contains(&"send_input".to_string()));
        assert!(names.contains(&"wait".to_string()));
        assert!(names.contains(&"close_agent".to_string()));
    }
}
