use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

use agent::{
    AnthropicProfile, DockerConfig, DockerExecutionEnvironment, EventData, EventKind,
    ExecutionEnvironment, GeminiProfile, LocalExecutionEnvironment, OpenAiProfile, ProviderProfile,
    Session, SessionConfig, Turn,
};
use llm::client::Client;
use terminal::Styles;

use crate::context::Context;
use crate::error::AttractorError;
use crate::graph::Node;
use crate::handler::codergen::{CodergenBackend, CodergenResult};
use crate::outcome::StageUsage;

/// LLM backend that delegates to an `agent` Session per invocation.
pub struct AgentBackend {
    model: String,
    provider: Option<String>,
    verbose: u8,
    styles: &'static Styles,
    docker: bool,
}

impl AgentBackend {
    #[must_use]
    pub const fn new(
        model: String,
        provider: Option<String>,
        verbose: u8,
        styles: &'static Styles,
        docker: bool,
    ) -> Self {
        Self {
            model,
            provider,
            verbose,
            styles,
            docker,
        }
    }

    fn build_profile(&self) -> Arc<dyn ProviderProfile> {
        let provider = self.provider.as_deref().unwrap_or("anthropic");
        match provider {
            "openai" => Arc::new(OpenAiProfile::new(&self.model)),
            "gemini" => Arc::new(GeminiProfile::new(&self.model)),
            _ => Arc::new(AnthropicProfile::new(&self.model)),
        }
    }
}

#[async_trait]
impl CodergenBackend for AgentBackend {
    async fn run(
        &self,
        node: &Node,
        prompt: &str,
        _context: &Context,
        _thread_id: Option<&str>,
    ) -> Result<CodergenResult, AttractorError> {
        let client = Client::from_env()
            .await
            .map_err(|e| AttractorError::Handler(format!("Failed to create LLM client: {e}")))?;

        let profile = self.build_profile();
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        let exec_env: Arc<dyn ExecutionEnvironment> = if self.docker {
            let config = DockerConfig {
                host_working_directory: cwd.to_string_lossy().to_string(),
                ..DockerConfig::default()
            };
            Arc::new(
                DockerExecutionEnvironment::new(config)
                    .map_err(|e| AttractorError::Handler(format!("Failed to create Docker environment: {e}")))?,
            )
        } else {
            Arc::new(LocalExecutionEnvironment::new(cwd))
        };

        let config = SessionConfig {
            reasoning_effort: Some(node.reasoning_effort().to_string()),
            ..SessionConfig::default()
        };

        let mut session = Session::new(client, profile, exec_env, config);

        // Subscribe to session events for real-time tool status on stderr.
        let verbose = self.verbose;
        if verbose >= 1 {
            let node_id = node.id.clone();
            let styles = self.styles;
            let mut rx = session.subscribe();
            tokio::spawn(async move {
                while let Ok(event) = rx.recv().await {
                    match (&event.kind, &event.data) {
                        (
                            EventKind::ToolCallStart,
                            EventData::ToolCall {
                                tool_name,
                                arguments,
                                ..
                            },
                        ) => {
                            eprintln!(
                                "{dim}[{node_id}]{reset}   {dim}\u{25cf}{reset} {bold}{cyan}{tool_name}{reset}{dim}({args}){reset}",
                                dim = styles.dim,
                                reset = styles.reset,
                                bold = styles.bold,
                                cyan = styles.cyan,
                                args = format_tool_args(arguments),
                            );
                        }
                        (
                            EventKind::ToolCallEnd,
                            EventData::ToolCallEnd {
                                tool_name,
                                output,
                                is_error,
                                ..
                            },
                        ) if verbose >= 2 => {
                            let label = if *is_error { "error" } else { "result" };
                            eprintln!(
                                "{dim}[{node_id}]   [{label}] {tool_name}:{reset}\n{}",
                                serde_json::to_string_pretty(output)
                                    .unwrap_or_else(|_| output.to_string()),
                                dim = styles.dim,
                                reset = styles.reset,
                            );
                        }
                        (EventKind::Error, EventData::Error { error }) => {
                            eprintln!(
                                "{dim}[{node_id}]{reset}   {red}\u{2717} {error}{reset}",
                                dim = styles.dim,
                                red = styles.red,
                                reset = styles.reset,
                            );
                        }
                        _ => {}
                    }
                }
            });
        }

        session.initialize().await;
        session.process_input(prompt).await.map_err(|e| {
            AttractorError::Handler(format!("Agent session failed: {e}"))
        })?;

        // Aggregate token usage from all assistant turns.
        let (mut turn_count, mut tool_call_count, mut input_tokens, mut output_tokens) =
            (0usize, 0usize, 0i64, 0i64);
        for turn in session.history().turns() {
            if let Turn::Assistant {
                tool_calls, usage, ..
            } = turn
            {
                turn_count += 1;
                tool_call_count += tool_calls.len();
                input_tokens += usage.input_tokens;
                output_tokens += usage.output_tokens;
            }
        }

        let stage_usage = StageUsage {
            model: self.model.clone(),
            input_tokens,
            output_tokens,
        };

        // Print session summary to stderr.
        if self.verbose >= 1 {
            let total_tokens = input_tokens + output_tokens;
            let token_str = if total_tokens >= 1000 {
                format!("{}k tokens", total_tokens / 1000)
            } else {
                format!("{total_tokens} tokens")
            };
            eprintln!(
                "{dim}[{node_id}] Done ({turn_count} turns, {tool_call_count} tool calls, {token_str}){reset}",
                node_id = node.id,
                dim = self.styles.dim,
                reset = self.styles.reset,
            );
        }

        // Extract last assistant response from the session history.
        let response = session
            .history()
            .turns()
            .iter()
            .rev()
            .find_map(|turn| {
                if let Turn::Assistant { content, .. } = turn {
                    if !content.is_empty() {
                        return Some(content.clone());
                    }
                }
                None
            })
            .unwrap_or_default();

        Ok(CodergenResult::Text { text: response, usage: Some(stage_usage) })
    }
}

fn format_tool_args(args: &serde_json::Value) -> String {
    let Some(obj) = args.as_object() else {
        return args.to_string();
    };
    obj.iter()
        .map(|(k, v)| match v {
            serde_json::Value::String(s) => {
                let display = if s.len() > 80 {
                    format!("{}...", &s[..77])
                } else {
                    s.clone()
                };
                format!("{k}={display:?}")
            }
            other => format!("{k}={other}"),
        })
        .collect::<Vec<_>>()
        .join(", ")
}
