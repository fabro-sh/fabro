use crate::{
    AnthropicProfile, EventData, EventKind, GeminiProfile, LocalExecutionEnvironment, OpenAiProfile,
    ProviderProfile, Session, SessionConfig, ToolApprovalFn, Turn,
};
use clap::{Parser, ValueEnum};
use llm::client::Client;
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

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
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum PermissionLevel {
    ReadOnly,
    ReadWrite,
    Full,
}

fn default_model(provider: &str) -> &'static str {
    match provider {
        "openai" => "gpt-5.2",
        "gemini" => "gemini-3-pro-preview",
        // anthropic and unknown providers
        _ => "claude-sonnet-4-5",
    }
}

fn tool_category(name: &str) -> &'static str {
    match name {
        "read_file" | "read_many_files" | "grep" | "glob" | "list_dir" => "read",
        "write_file" | "edit_file" | "apply_patch" => "write",
        // shell and unknown tools require highest permission
        _ => "shell",
    }
}

fn is_auto_approved(level: PermissionLevel, category: &str) -> bool {
    matches!(
        (level, category),
        (_, "read")
            | (PermissionLevel::ReadWrite | PermissionLevel::Full, "write")
            | (PermissionLevel::Full, "shell")
    )
}

fn build_tool_approval(permissions: PermissionLevel, is_interactive: bool) -> ToolApprovalFn {
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
        eprint!("Allow {tool_name} ({category})? [y]es / [n]o / [a]lways: ");
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

fn build_profile(provider: &str, model: &str) -> Arc<dyn ProviderProfile> {
    match provider {
        "openai" => Arc::new(OpenAiProfile::new(model)),
        "gemini" => Arc::new(GeminiProfile::new(model)),
        // anthropic and unknown providers
        _ => Arc::new(AnthropicProfile::new(model)),
    }
}

fn validate_api_key(provider: &str) -> bool {
    match provider {
        "anthropic" => std::env::var("ANTHROPIC_API_KEY").is_ok(),
        "openai" => std::env::var("OPENAI_API_KEY").is_ok(),
        "gemini" => {
            std::env::var("GEMINI_API_KEY").is_ok() || std::env::var("GOOGLE_API_KEY").is_ok()
        }
        _ => false,
    }
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

fn print_summary(session: &Session) {
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
    eprintln!("Done ({turn_count} turns, {tool_call_count} tool calls, {token_str})");
}

/// Middleware that logs LLM request/response summaries to stderr.
struct DebugMiddleware;

#[async_trait::async_trait]
impl llm::middleware::Middleware for DebugMiddleware {
    async fn handle_complete(
        &self,
        request: llm::types::Request,
        next: llm::middleware::NextFn,
    ) -> Result<llm::types::Response, llm::error::SdkError> {
        eprintln!(
            "[debug] request: model={} messages={} tools={}",
            request.model,
            request.messages.len(),
            request.tools.as_ref().map_or(0, Vec::len),
        );
        let response = next(request).await?;
        eprintln!(
            "[debug] response: model={} finish={:?} usage=({}/{}/{})",
            response.model,
            response.finish_reason,
            response.usage.input_tokens,
            response.usage.output_tokens,
            response.usage.total_tokens,
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

    // Validate provider API key
    if !validate_api_key(&cli.provider) {
        anyhow::bail!("API key not set for provider '{}'", cli.provider);
    }

    // Build LLM client
    let mut client = Client::from_env()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to create LLM client: {e}"))?;

    if cli.debug {
        client.add_middleware(Arc::new(DebugMiddleware));
    }

    // Resolve model and build profile
    let model = cli
        .model
        .as_deref()
        .unwrap_or_else(|| default_model(&cli.provider));
    let profile = build_profile(&cli.provider, model);

    // Build execution environment
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let env = Arc::new(LocalExecutionEnvironment::new(cwd));

    // Build tool approval callback
    let is_interactive = std::io::stdin().is_terminal() && !cli.auto_approve;
    let tool_approval = build_tool_approval(cli.permissions, is_interactive);

    let config = SessionConfig {
        tool_approval: Some(tool_approval),
        ..SessionConfig::default()
    };

    let mut session = Session::new(client, profile, env, config);

    // SIGINT handler
    let abort_flag = session.abort_flag_handle();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        abort_flag.store(true, Ordering::SeqCst);
    });

    // Subscribe to events for real-time tool status on stderr
    let mut rx = session.subscribe();
    tokio::spawn(async move {
        while let Ok(event) = rx.recv().await {
            match (&event.kind, &event.data) {
                (EventKind::ToolCallStart, EventData::ToolCall { tool_name, .. }) => {
                    eprintln!("[tool] {tool_name}");
                }
                (EventKind::Error, EventData::Error { error }) => {
                    eprintln!("[error] {error}");
                }
                _ => {}
            }
        }
    });

    // Initialize and run
    session.initialize().await;
    let result = session.process_input(&cli.prompt).await;

    // Print assistant text to stdout
    print_output(&session);

    // Print completion summary to stderr
    print_summary(&session);

    // Propagate errors for exit code
    result?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
    fn tool_category_unknown_defaults_to_shell() {
        assert_eq!(tool_category("some_random_tool"), "shell");
    }

    // is_auto_approved tests

    #[test]
    fn is_auto_approved_read_only() {
        assert!(is_auto_approved(PermissionLevel::ReadOnly, "read"));
        assert!(!is_auto_approved(PermissionLevel::ReadOnly, "write"));
        assert!(!is_auto_approved(PermissionLevel::ReadOnly, "shell"));
    }

    #[test]
    fn is_auto_approved_read_write() {
        assert!(is_auto_approved(PermissionLevel::ReadWrite, "read"));
        assert!(is_auto_approved(PermissionLevel::ReadWrite, "write"));
        assert!(!is_auto_approved(PermissionLevel::ReadWrite, "shell"));
    }

    #[test]
    fn is_auto_approved_full() {
        assert!(is_auto_approved(PermissionLevel::Full, "read"));
        assert!(is_auto_approved(PermissionLevel::Full, "write"));
        assert!(is_auto_approved(PermissionLevel::Full, "shell"));
    }

    // default_model tests

    #[test]
    fn default_model_anthropic() {
        assert_eq!(default_model("anthropic"), "claude-sonnet-4-5");
    }

    #[test]
    fn default_model_openai() {
        assert_eq!(default_model("openai"), "gpt-5.2");
    }

    #[test]
    fn default_model_gemini() {
        assert_eq!(default_model("gemini"), "gemini-3-pro-preview");
    }

    // validate_api_key tests

    #[test]
    fn validate_api_key_unknown_provider() {
        assert!(!validate_api_key("unknown"));
    }

    // build_tool_approval non-interactive tests

    #[test]
    fn build_tool_approval_read_only_allows_read() {
        let approval_fn = build_tool_approval(PermissionLevel::ReadOnly, false);
        assert!(approval_fn("read_file", &json!({})).is_ok());
    }

    #[test]
    fn build_tool_approval_read_only_denies_write() {
        let approval_fn = build_tool_approval(PermissionLevel::ReadOnly, false);
        let result = approval_fn("write_file", &json!({}));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("denied"));
    }

    #[test]
    fn build_tool_approval_read_write_denies_shell() {
        let approval_fn = build_tool_approval(PermissionLevel::ReadWrite, false);
        let result = approval_fn("shell", &json!({}));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("denied"));
    }

    #[test]
    fn build_tool_approval_full_allows_shell() {
        let approval_fn = build_tool_approval(PermissionLevel::Full, false);
        assert!(approval_fn("shell", &json!({})).is_ok());
    }

    // build_profile tests

    #[test]
    fn build_profile_anthropic() {
        let profile = build_profile("anthropic", "model");
        assert_eq!(profile.id(), "anthropic");
    }

    #[test]
    fn build_profile_openai() {
        let profile = build_profile("openai", "model");
        assert_eq!(profile.id(), "openai");
    }

    #[test]
    fn build_profile_gemini() {
        let profile = build_profile("gemini", "model");
        assert_eq!(profile.id(), "gemini");
    }
}
