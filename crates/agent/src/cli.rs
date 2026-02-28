use crate::{
    AgentEvent, AnthropicProfile, GeminiProfile, LocalExecutionEnvironment, OpenAiProfile,
    ProviderProfile, Session, SessionConfig, ToolApprovalFn, Turn,
    subagent::{SessionFactory, SubAgentManager},
};
use clap::{Parser, ValueEnum};
use llm::client::Client;
use llm::provider::{ModelId, Provider};
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use util::terminal::Styles;

/// Minimal CLI for the agent agentic loop.
#[derive(Parser)]
#[command(name = "agent")]
struct Cli {
    /// Task prompt
    prompt: String,

    /// LLM provider (anthropic, openai, gemini)
    #[arg(long, default_value = "anthropic")]
    provider: String,

    /// Model name (defaults per provider)
    #[arg(long)]
    model: Option<String>,

    /// Permission level for tool execution
    #[arg(long, default_value = "read-write", value_enum)]
    permissions: PermissionLevel,

    /// Skip interactive prompts; deny tools outside permission level
    #[arg(long)]
    auto_approve: bool,

    /// Print LLM request/response debug info to stderr
    #[arg(long)]
    debug: bool,

    /// Print full LLM request/response JSON to stderr
    #[arg(long)]
    verbose: bool,

    /// Directory containing skill files (overrides default discovery)
    #[arg(long)]
    skills_dir: Option<String>,

    /// Output format (text for human-readable, json for NDJSON event stream)
    #[arg(long, default_value = "text", value_enum)]
    output_format: OutputFormat,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum OutputFormat {
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum PermissionLevel {
    ReadOnly,
    ReadWrite,
    Full,
}

fn default_model(provider: Provider) -> &'static str {
    match provider {
        Provider::OpenAi => "gpt-5.2-codex",
        Provider::Gemini => "gemini-3.1-pro-preview",
        Provider::Anthropic => "claude-opus-4-6",
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
        (_, "read")
            | (_, "subagent")
            | (PermissionLevel::ReadWrite | PermissionLevel::Full, "write")
            | (PermissionLevel::Full, "shell")
    )
}

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
            "Allow {}{tool_name}{} ({category})? [y]es / [n]o / [a]lways: ",
            styles.bold, styles.reset,
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

fn summarizer_model_id(provider: Provider) -> ModelId {
    match provider {
        Provider::OpenAi => ModelId::new(Provider::OpenAi, "gpt-4o-mini"),
        Provider::Gemini => ModelId::new(Provider::Gemini, "gemini-2.0-flash"),
        Provider::Anthropic => ModelId::new(Provider::Anthropic, "claude-haiku-4-5-20251001"),
    }
}

fn build_summarizer(provider: Provider, llm_client: Option<Client>) -> Option<crate::tools::WebFetchSummarizer> {
    let client = llm_client?;
    Some(crate::tools::WebFetchSummarizer {
        client,
        model_id: summarizer_model_id(provider),
    })
}

fn build_profile(provider: Provider, model: &str, llm_client: Option<Client>) -> Box<dyn ProviderProfile> {
    let summarizer = build_summarizer(provider, llm_client);
    match provider {
        Provider::OpenAi => Box::new(OpenAiProfile::with_summarizer(model, summarizer)),
        Provider::Gemini => Box::new(GeminiProfile::with_summarizer(model, summarizer)),
        Provider::Anthropic => Box::new(AnthropicProfile::with_summarizer(model, summarizer)),
    }
}

fn validate_api_key(provider: Provider) -> bool {
    match provider {
        Provider::Anthropic => std::env::var("ANTHROPIC_API_KEY").is_ok(),
        Provider::OpenAi => std::env::var("OPENAI_API_KEY").is_ok(),
        Provider::Gemini => {
            std::env::var("GEMINI_API_KEY").is_ok() || std::env::var("GOOGLE_API_KEY").is_ok()
        }
    }
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
                    format!("{}...", &s[..77])
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

fn print_output(session: &Session) {
    for turn in session.history().turns() {
        if let Turn::Assistant { content, .. } = turn {
            if !content.is_empty() {
                println!("{content}");
            }
        }
    }
}

fn print_summary(session: &Session, styles: &Styles) {
    let (mut turn_count, mut tool_call_count, mut total_tokens) = (0usize, 0usize, 0i64);
    for turn in session.history().turns() {
        if let Turn::Assistant {
            tool_calls, usage, ..
        } = turn
        {
            turn_count += 1;
            tool_call_count += tool_calls.len();
            total_tokens += usage.total_tokens;
        }
    }
    let token_str = if total_tokens >= 1000 {
        format!("{}k tokens", total_tokens / 1000)
    } else {
        format!("{total_tokens} tokens")
    };
    eprintln!(
        "{}Done ({turn_count} turns, {tool_call_count} tool calls, {token_str}){}",
        styles.dim, styles.reset,
    );
}

/// Middleware that logs LLM request/response summaries to stderr.
struct DebugMiddleware {
    styles: &'static Styles,
}

#[async_trait::async_trait]
impl llm::middleware::Middleware for DebugMiddleware {
    async fn handle_complete(
        &self,
        request: llm::types::Request,
        next: llm::middleware::NextFn,
    ) -> Result<llm::types::Response, llm::error::SdkError> {
        let s = self.styles;
        eprintln!(
            "{}[debug] request: model={} messages={} tools={}{}",
            s.dim,
            request.model,
            request.messages.len(),
            request.tools.as_ref().map_or(0, Vec::len),
            s.reset,
        );
        let response = next(request).await?;
        eprintln!(
            "{}[debug] response: model={} finish={:?} usage=({}/{}/{}){}",
            s.dim,
            response.model,
            response.finish_reason,
            response.usage.input_tokens,
            response.usage.output_tokens,
            response.usage.total_tokens,
            s.reset,
        );
        Ok(response)
    }

    async fn handle_stream(
        &self,
        request: llm::types::Request,
        next: llm::middleware::NextStreamFn,
    ) -> Result<llm::provider::StreamEventStream, llm::error::SdkError> {
        next(request).await
    }
}

/// Middleware that logs full LLM request/response JSON to stderr.
struct VerboseMiddleware {
    styles: &'static Styles,
}

#[async_trait::async_trait]
impl llm::middleware::Middleware for VerboseMiddleware {
    async fn handle_complete(
        &self,
        request: llm::types::Request,
        next: llm::middleware::NextFn,
    ) -> Result<llm::types::Response, llm::error::SdkError> {
        let s = self.styles;
        eprintln!(
            "{}[verbose] request:{}\n{}",
            s.dim,
            s.reset,
            serde_json::to_string_pretty(&request).unwrap_or_else(|e| format!("<serialize error: {e}>"))
        );
        let response = next(request).await?;
        eprintln!(
            "{}[verbose] response:{}\n{}",
            s.dim,
            s.reset,
            serde_json::to_string_pretty(&response).unwrap_or_else(|e| format!("<serialize error: {e}>"))
        );
        Ok(response)
    }

    async fn handle_stream(
        &self,
        request: llm::types::Request,
        next: llm::middleware::NextStreamFn,
    ) -> Result<llm::provider::StreamEventStream, llm::error::SdkError> {
        next(request).await
    }
}

pub async fn run() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();

    // Resolve color support once, leak to get 'static lifetime for use across threads
    let styles: &'static Styles = Box::leak(Box::new(Styles::detect_stderr()));

    // Parse provider string to enum early for compile-time safety
    let provider: Provider = cli.provider.parse().map_err(|e: String| anyhow::anyhow!("{e}"))?;

    // Validate provider API key
    if !validate_api_key(provider) {
        anyhow::bail!("API key not set for provider '{provider}'");
    }

    // Build LLM client
    let mut client = Client::from_env()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to create LLM client: {e}"))?;

    if cli.verbose {
        client.add_middleware(Arc::new(VerboseMiddleware { styles }));
    } else if cli.debug {
        client.add_middleware(Arc::new(DebugMiddleware { styles }));
    }

    // Resolve model and build profile
    let model = cli
        .model
        .as_deref()
        .unwrap_or_else(|| default_model(provider));
    eprintln!(
        "{}Using model: {model}{}",
        styles.dim, styles.reset,
    );
    let mut profile = build_profile(provider, model, Some(client.clone()));

    // Build execution environment
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let cwd_str = cwd.to_string_lossy().to_string();
    let env: Arc<dyn crate::ExecutionEnvironment> = Arc::new(LocalExecutionEnvironment::new(cwd));

    // Build tool approval callback
    let is_interactive = std::io::stdin().is_terminal() && !cli.auto_approve;
    let tool_approval = build_tool_approval(cli.permissions, is_interactive, styles);

    let config = SessionConfig {
        tool_approval: Some(tool_approval),
        skill_dirs: cli.skills_dir.map(|d| vec![d]),
        ..SessionConfig::default()
    };

    // Register subagent tools
    let manager = Arc::new(tokio::sync::Mutex::new(
        SubAgentManager::new(config.max_subagent_depth),
    ));
    let manager_for_callback = manager.clone();
    let factory_client = client.clone();
    let factory_model = model.to_string();
    let factory_env = Arc::clone(&env);
    let factory_approval = config.tool_approval.clone();
    let factory: SessionFactory = Arc::new(move || {
        let child_summarizer = build_summarizer(provider, Some(factory_client.clone()));
        let child_profile: Arc<dyn ProviderProfile> = match provider {
            Provider::OpenAi => Arc::new(OpenAiProfile::with_summarizer(&factory_model, child_summarizer)),
            Provider::Gemini => Arc::new(GeminiProfile::with_summarizer(&factory_model, child_summarizer)),
            Provider::Anthropic => Arc::new(AnthropicProfile::with_summarizer(&factory_model, child_summarizer)),
        };
        Session::new(
            factory_client.clone(),
            child_profile,
            Arc::clone(&factory_env),
            SessionConfig {
                tool_approval: factory_approval.clone(),
                ..SessionConfig::default()
            },
        )
    });
    profile.register_subagent_tools(manager, factory, 0);
    let profile: Arc<dyn ProviderProfile> = Arc::from(profile);

    let mut session = Session::new(client, profile, env, config);

    // Wire subagent event callback to parent session's emitter
    manager_for_callback.lock().await.set_event_callback(session.event_callback());

    // SIGINT handler
    let cancel_token = session.cancel_token();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        cancel_token.cancel();
    });

    // Subscribe to events
    let verbose = cli.verbose;
    let output_format = cli.output_format;
    let mut rx = session.subscribe();
    tokio::spawn(async move {
        match output_format {
            OutputFormat::Json => {
                while let Ok(event) = rx.recv().await {
                    if let Ok(json) = serde_json::to_string(&event) {
                        let mut stdout = std::io::stdout().lock();
                        let _ = writeln!(stdout, "{json}");
                        let _ = stdout.flush();
                    }
                }
            }
            OutputFormat::Text => {
                let s = styles;
                while let Ok(event) = rx.recv().await {
                    match &event.event {
                        AgentEvent::ToolCallStarted { tool_name, arguments, .. } => {
                            eprintln!(
                                "  {dim}\u{25cf}{reset} {bold}{cyan}{tool_name}{reset}{dim}({args}){reset}",
                                dim = s.dim,
                                reset = s.reset,
                                bold = s.bold,
                                cyan = s.cyan,
                                args = format_tool_args(arguments, &cwd_str),
                            );
                        }
                        AgentEvent::ToolCallCompleted {
                            tool_name, output, is_error, ..
                        } if verbose => {
                            let label = if *is_error { "tool error" } else { "tool result" };
                            eprintln!(
                                "  {}[{label}] {tool_name}:{}\n{}",
                                s.dim,
                                s.reset,
                                serde_json::to_string_pretty(output)
                                    .unwrap_or_else(|_| output.to_string()),
                            );
                        }
                        AgentEvent::Error { error } => {
                            eprintln!(
                                "  {red}\u{2717} {error}{reset}",
                                red = s.red,
                                reset = s.reset,
                            );
                        }
                        AgentEvent::SubAgentSpawned { agent_id, depth, task, .. } => {
                            let short_id = &agent_id[..8.min(agent_id.len())];
                            let task_preview = if task.len() > 60 { &task[..60] } else { task };
                            eprintln!(
                                "  {dim}\u{25b6} subagent {short_id} spawned (depth={depth}) task={task_preview:?}{reset}",
                                dim = s.dim, reset = s.reset,
                            );
                        }
                        AgentEvent::SubAgentCompleted { agent_id, depth, success, turns_used } => {
                            let short_id = &agent_id[..8.min(agent_id.len())];
                            eprintln!(
                                "  {dim}\u{25a0} subagent {short_id} completed (depth={depth}, success={success}, turns={turns_used}){reset}",
                                dim = s.dim, reset = s.reset,
                            );
                        }
                        AgentEvent::SubAgentFailed { agent_id, depth, error } => {
                            let short_id = &agent_id[..8.min(agent_id.len())];
                            eprintln!(
                                "  {red}\u{2717} subagent {short_id} failed (depth={depth}): {error}{reset}",
                                red = s.red, reset = s.reset,
                            );
                        }
                        AgentEvent::SubAgentClosed { agent_id, depth } => {
                            let short_id = &agent_id[..8.min(agent_id.len())];
                            eprintln!(
                                "  {dim}\u{25a0} subagent {short_id} closed (depth={depth}){reset}",
                                dim = s.dim, reset = s.reset,
                            );
                        }
                        AgentEvent::SubAgentEvent { agent_id, event: child_event, .. } if verbose => {
                            let short_id = &agent_id[..8.min(agent_id.len())];
                            eprintln!(
                                "  {dim}[subagent {short_id}] {child_event:?}{reset}",
                                dim = s.dim, reset = s.reset,
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
    let result = session.process_input(&cli.prompt).await;

    if matches!(output_format, OutputFormat::Text) {
        // Print assistant text to stdout
        print_output(&session);

        // Print completion summary to stderr
        print_summary(&session, styles);
    }

    // Propagate errors for exit code
    result?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use llm::provider::Provider;
    use serde_json::json;

    static NO_COLOR: Styles = Styles::new(false);

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

    // default_model tests

    #[test]
    fn default_model_anthropic() {
        assert_eq!(default_model(Provider::Anthropic), "claude-opus-4-6");
    }

    #[test]
    fn default_model_openai() {
        assert_eq!(default_model(Provider::OpenAi), "gpt-5.2-codex");
    }

    #[test]
    fn default_model_gemini() {
        assert_eq!(default_model(Provider::Gemini), "gemini-3.1-pro-preview");
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
    fn build_profile_gemini() {
        let profile = build_profile(Provider::Gemini, "model", None);
        assert_eq!(profile.provider(), Provider::Gemini);
    }

    // subagent tool registration tests

    #[test]
    fn build_profile_can_register_subagent_tools() {
        let mut profile = build_profile(Provider::Anthropic, "model", None);
        let manager = Arc::new(tokio::sync::Mutex::new(SubAgentManager::new(1)));
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
