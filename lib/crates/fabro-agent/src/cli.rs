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
use fabro_model::{Catalog, ModelHandle, Provider};
use fabro_util::terminal::Styles;
use fabro_vault::Vault;
use tokio::io::{AsyncWriteExt, stdout};
use tokio::signal;
use tokio::sync::{Mutex as AsyncMutex, RwLock as AsyncRwLock};

use crate::config::{ToolApprovalAdapter, ToolApprovalFn, ToolHookCallback};
use crate::error::InterruptReason;
use crate::subagent::{SessionFactory, SubAgentManager};
use crate::tools::WebFetchSummarizer;
use crate::{
    AgentEvent, AgentProfile, AnthropicProfile, GeminiProfile, LocalSandbox, OpenAiProfile,
    Sandbox, Session, SessionOptions, Turn,
};

/// Public arguments for the agent command, usable from an external CLI.
#[derive(Args)]
pub struct AgentArgs {
    /// Task prompt
    pub prompt: String,

    /// LLM provider (anthropic, openai, gemini, kimi, zai, minimax, inception)
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

/// Agent tool permission level.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize, clap::ValueEnum,
)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionLevel {
    ReadOnly,
    ReadWrite,
    Full,
}

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

fn tool_category(name: &str) -> &'static str {
    match name {
        "read_file" | "read_many_files" | "grep" | "glob" | "list_dir" => "read",
        "write_file" | "edit_file" | "apply_patch" => "write",
        // subagent tools inherit parent permissions, always allowed
        "spawn_agent" | "send_input" | "wait" | "close_agent" => "subagent",
        // shell and unknown tools require highest permission
        _ => "shell",
    }
}

fn is_auto_approved(level: PermissionLevel, category: &str) -> bool {
    matches!(
        (level, category),
        (_, "read" | "subagent")
            | (PermissionLevel::ReadWrite | PermissionLevel::Full, "write")
            | (PermissionLevel::Full, "shell")
    )
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
        std::io::stderr().flush().ok();

        let mut input = String::new();
        std::io::stdin()
            .read_line(&mut input)
            .map_err(|e| format!("Failed to read input: {e}"))?;

        match input.trim().to_lowercase().as_str() {
            "y" | "yes" => Ok(()),
            "a" | "always" => {
                let mut lvl = level.lock().expect("permission lock poisoned");
                *lvl = if category == "write" {
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

fn summarizer_model_id(provider: Provider) -> ModelHandle {
    let model = match provider {
        Provider::OpenAi | Provider::OpenAiCompatible => "gpt-4o-mini",
        Provider::Gemini => "gemini-2.0-flash",
        Provider::Anthropic => "claude-haiku-4-5",
        Provider::Kimi => "kimi-k2.5",
        Provider::Zai => "glm-4.7",
        Provider::Minimax => "minimax-m2.5",
        Provider::Inception => "mercury",
    };
    ModelHandle::ByName {
        provider: provider.into(),
        model:    model.to_string(),
    }
}

fn build_summarizer(provider: Provider, llm_client: Client) -> WebFetchSummarizer {
    WebFetchSummarizer {
        client:   llm_client,
        model_id: summarizer_model_id(provider),
    }
}

fn build_profile(
    provider: Provider,
    model: &str,
    summarizer: Option<WebFetchSummarizer>,
) -> Box<dyn AgentProfile> {
    match provider {
        Provider::OpenAi => Box::new(OpenAiProfile::with_summarizer(model, summarizer)),
        Provider::Kimi
        | Provider::Zai
        | Provider::Minimax
        | Provider::Inception
        | Provider::OpenAiCompatible => {
            Box::new(OpenAiProfile::with_summarizer(model, summarizer).with_provider(provider))
        }
        Provider::Gemini => Box::new(GeminiProfile::with_summarizer(model, summarizer)),
        Provider::Anthropic => Box::new(AnthropicProfile::with_summarizer(model, summarizer)),
    }
}

fn parse_provider(args: &AgentArgs) -> anyhow::Result<Provider> {
    let provider_str = args.provider.as_deref().unwrap_or("anthropic");
    provider_str
        .parse()
        .map_err(|_| anyhow::anyhow!("unknown provider: {provider_str}"))
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

fn ensure_provider_registered(client: &Client, provider: Provider) -> anyhow::Result<()> {
    if client
        .provider_names()
        .iter()
        .any(|name| *name == <&'static str>::from(provider))
    {
        return Ok(());
    }

    anyhow::bail!("LLM credentials not configured for provider '{provider}'");
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
        if let Turn::Assistant { content, .. } = turn {
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
        if let Turn::Assistant {
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
    run_with_args_and_source(args, llm_source, mcp_servers).await
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
    let provider = parse_provider(&args)?;
    let client = Client::from_source(llm_source.as_ref())
        .await
        .context("Failed to create LLM client")?;
    ensure_provider_registered(&client, provider)?;
    run_with_args_and_client(args, client, mcp_servers).await
}

#[allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "Assistant output stays on stdout while prompts and diagnostics use stderr."
)]
pub async fn run_with_args_and_client(
    args: AgentArgs,
    mut client: Client,
    mcp_servers: Vec<McpServerSettings>,
) -> anyhow::Result<()> {
    // Resolve color support once, leak to get 'static lifetime for use across
    // threads
    let styles: &'static Styles = Box::leak(Box::new(Styles::detect_stderr()));

    let provider = parse_provider(&args)?;
    ensure_provider_registered(&client, provider)?;

    if args.verbose {
        client.add_middleware(Arc::new(VerboseMiddleware { styles }));
    } else if args.debug {
        client.add_middleware(Arc::new(DebugMiddleware { styles }));
    }

    // Resolve model and build profile
    let model = if let Some(model) = args.model.clone() {
        model
    } else {
        Catalog::builtin()
            .default_for_provider(&provider.to_string())
            .map(|model| model.id.clone())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "provider '{provider}' has no default model in the catalog; pass --model explicitly"
                )
            })?
    };
    eprintln!("{}", styles.dim.apply_to(format!("Using model: {model}")));
    let mut profile = build_profile(
        provider,
        &model,
        Some(build_summarizer(provider, client.clone())),
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
        skill_dirs: args.skills_dir.map(|d| vec![d]),
        mcp_servers,
        ..SessionOptions::default()
    };

    // Register subagent tools
    let manager = Arc::new(AsyncMutex::new(SubAgentManager::new(
        config.max_subagent_depth,
    )));
    let manager_for_callback = manager.clone();
    let factory_client = client.clone();
    let factory_model = model.clone();
    let factory_env = Arc::clone(&env);
    let factory_hooks = config.tool_hooks.clone();
    let factory: SessionFactory = Arc::new(move || {
        let child_summarizer = Some(build_summarizer(provider, factory_client.clone()));
        let child_profile: Arc<dyn AgentProfile> = match provider {
            Provider::OpenAi => Arc::new(OpenAiProfile::with_summarizer(
                &factory_model,
                child_summarizer,
            )),
            Provider::Kimi
            | Provider::Zai
            | Provider::Minimax
            | Provider::Inception
            | Provider::OpenAiCompatible => Arc::new(
                OpenAiProfile::with_summarizer(&factory_model, child_summarizer)
                    .with_provider(provider),
            ),
            Provider::Gemini => Arc::new(GeminiProfile::with_summarizer(
                &factory_model,
                child_summarizer,
            )),
            Provider::Anthropic => Arc::new(AnthropicProfile::with_summarizer(
                &factory_model,
                child_summarizer,
            )),
        };
        Session::new(
            factory_client.clone(),
            child_profile,
            Arc::clone(&factory_env),
            SessionOptions {
                tool_hooks: factory_hooks.clone(),
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
    session.initialize().await;
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

    use fabro_model::Provider;
    use serde_json::json;

    use super::*;

    static NO_COLOR: std::sync::LazyLock<Styles> = std::sync::LazyLock::new(|| Styles::new(false));

    // tool_category tests

    #[test]
    fn tool_category_read_tools() {
        assert_eq!(tool_category("read_file"), "read");
        assert_eq!(tool_category("read_many_files"), "read");
        assert_eq!(tool_category("grep"), "read");
        assert_eq!(tool_category("glob"), "read");
        assert_eq!(tool_category("list_dir"), "read");
    }

    #[test]
    fn tool_category_write_tools() {
        assert_eq!(tool_category("write_file"), "write");
        assert_eq!(tool_category("edit_file"), "write");
        assert_eq!(tool_category("apply_patch"), "write");
    }

    #[test]
    fn tool_category_shell() {
        assert_eq!(tool_category("shell"), "shell");
    }

    #[test]
    fn tool_category_subagent_tools() {
        assert_eq!(tool_category("spawn_agent"), "subagent");
        assert_eq!(tool_category("send_input"), "subagent");
        assert_eq!(tool_category("wait"), "subagent");
        assert_eq!(tool_category("close_agent"), "subagent");
    }

    #[test]
    fn tool_category_unknown_defaults_to_shell() {
        assert_eq!(tool_category("some_random_tool"), "shell");
    }

    // is_auto_approved tests

    #[test]
    fn is_auto_approved_read_only() {
        assert!(is_auto_approved(PermissionLevel::ReadOnly, "read"));
        assert!(is_auto_approved(PermissionLevel::ReadOnly, "subagent"));
        assert!(!is_auto_approved(PermissionLevel::ReadOnly, "write"));
        assert!(!is_auto_approved(PermissionLevel::ReadOnly, "shell"));
    }

    #[test]
    fn is_auto_approved_read_write() {
        assert!(is_auto_approved(PermissionLevel::ReadWrite, "read"));
        assert!(is_auto_approved(PermissionLevel::ReadWrite, "subagent"));
        assert!(is_auto_approved(PermissionLevel::ReadWrite, "write"));
        assert!(!is_auto_approved(PermissionLevel::ReadWrite, "shell"));
    }

    #[test]
    fn is_auto_approved_full() {
        assert!(is_auto_approved(PermissionLevel::Full, "read"));
        assert!(is_auto_approved(PermissionLevel::Full, "subagent"));
        assert!(is_auto_approved(PermissionLevel::Full, "write"));
        assert!(is_auto_approved(PermissionLevel::Full, "shell"));
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

    #[test]
    fn build_profile_anthropic() {
        let profile = build_profile(Provider::Anthropic, "model", None);
        assert_eq!(profile.provider(), Provider::Anthropic);
    }

    #[test]
    fn build_profile_openai() {
        let profile = build_profile(Provider::OpenAi, "model", None);
        assert_eq!(profile.provider(), Provider::OpenAi);
    }

    #[test]
    fn ensure_provider_registered_reports_missing_credentials() {
        let client = Client::new(HashMap::new(), None, vec![]);
        let error = ensure_provider_registered(&client, Provider::Anthropic).unwrap_err();
        assert_eq!(
            error.to_string(),
            "LLM credentials not configured for provider 'anthropic'"
        );
    }

    #[test]
    fn build_profile_gemini() {
        let profile = build_profile(Provider::Gemini, "model", None);
        assert_eq!(profile.provider(), Provider::Gemini);
    }

    // subagent tool registration tests

    #[test]
    fn build_profile_can_register_subagent_tools() {
        let mut profile = build_profile(Provider::Anthropic, "model", None);
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
