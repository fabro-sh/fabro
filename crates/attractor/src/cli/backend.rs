use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use agent::{
    AgentEvent, AnthropicProfile, DockerConfig, DockerExecutionEnvironment,
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
        emitter: &Arc<crate::event::EventEmitter>,
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

        // File change tracking: shared between spawned task and main fn.
        let pending_tool_calls: Arc<Mutex<HashMap<String, String>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let files_touched: Arc<Mutex<HashSet<String>>> =
            Arc::new(Mutex::new(HashSet::new()));
        let pending_clone = Arc::clone(&pending_tool_calls);
        let files_clone = Arc::clone(&files_touched);

        // Subscribe to session events: forward to pipeline emitter and optionally print to stderr.
        let verbose = self.verbose;
        let node_id = node.id.clone();
        let styles = self.styles;
        let pipeline_emitter = Arc::clone(emitter);
        let mut rx = session.subscribe();
        tokio::spawn(async move {
            use crate::event::PipelineEvent;
            while let Ok(event) = rx.recv().await {
                // Forward agent events to pipeline events
                match &event.event {
                    AgentEvent::AssistantMessage {
                        text,
                        model,
                        usage,
                        tool_call_count,
                    } => {
                        pipeline_emitter.emit(&PipelineEvent::AssistantMessage {
                            stage: node_id.clone(),
                            text: text.clone(),
                            model: model.clone(),
                            usage: usage.clone(),
                            tool_call_count: *tool_call_count,
                        });
                    }
                    AgentEvent::ToolCallStarted {
                        tool_name,
                        tool_call_id,
                        arguments,
                    } => {
                        // Track file paths from write_file/edit_file tool calls
                        if tool_name == "write_file" || tool_name == "edit_file" {
                            if let Some(path) = arguments.get("file_path").and_then(|v| v.as_str()) {
                                pending_clone.lock().unwrap().insert(
                                    tool_call_id.clone(),
                                    path.to_string(),
                                );
                            }
                        }
                        pipeline_emitter.emit(&PipelineEvent::ToolCallStarted {
                            stage: node_id.clone(),
                            tool_name: tool_name.clone(),
                            tool_call_id: tool_call_id.clone(),
                            arguments: arguments.clone(),
                        });
                    }
                    AgentEvent::ToolCallCompleted {
                        tool_name,
                        tool_call_id,
                        output,
                        is_error,
                    } => {
                        // On successful completion, move file from pending to touched
                        if !*is_error {
                            if let Some(path) = pending_clone.lock().unwrap().remove(tool_call_id) {
                                files_clone.lock().unwrap().insert(path);
                            }
                        } else {
                            pending_clone.lock().unwrap().remove(tool_call_id);
                        }
                        pipeline_emitter.emit(&PipelineEvent::ToolCallCompleted {
                            stage: node_id.clone(),
                            tool_name: tool_name.clone(),
                            tool_call_id: tool_call_id.clone(),
                            output: output.clone(),
                            is_error: *is_error,
                        });
                    }
                    AgentEvent::Error { error } => {
                        pipeline_emitter.emit(&PipelineEvent::SessionError {
                            stage: node_id.clone(),
                            error: error.clone(),
                        });
                    }
                    AgentEvent::ContextWindowWarning {
                        estimated_tokens,
                        context_window_size,
                        usage_percent,
                    } => {
                        pipeline_emitter.emit(&PipelineEvent::ContextWindowWarning {
                            stage: node_id.clone(),
                            estimated_tokens: *estimated_tokens,
                            context_window_size: *context_window_size,
                            usage_percent: *usage_percent,
                        });
                    }
                    AgentEvent::LoopDetected => {
                        pipeline_emitter.emit(&PipelineEvent::LoopDetected {
                            stage: node_id.clone(),
                        });
                    }
                    AgentEvent::TurnLimitReached => {
                        pipeline_emitter.emit(&PipelineEvent::TurnLimitReached {
                            stage: node_id.clone(),
                        });
                    }
                    AgentEvent::CompactionStarted {
                        estimated_tokens,
                        context_window_size,
                    } => {
                        pipeline_emitter.emit(&PipelineEvent::CompactionStarted {
                            stage: node_id.clone(),
                            estimated_tokens: *estimated_tokens,
                            context_window_size: *context_window_size,
                        });
                    }
                    AgentEvent::CompactionCompleted {
                        original_turn_count,
                        preserved_turn_count,
                        summary_token_estimate,
                    } => {
                        pipeline_emitter.emit(&PipelineEvent::CompactionCompleted {
                            stage: node_id.clone(),
                            original_turn_count: *original_turn_count,
                            preserved_turn_count: *preserved_turn_count,
                            summary_token_estimate: *summary_token_estimate,
                        });
                    }
                    // Streaming events and session lifecycle not forwarded
                    _ => {}
                }

                // Verbose stderr printing (gated on verbosity)
                if verbose >= 1 {
                    match &event.event {
                        AgentEvent::ToolCallStarted {
                            tool_name,
                            arguments,
                            ..
                        } => {
                            eprintln!(
                                "{dim}[{node_id}]{reset}   {dim}\u{25cf}{reset} {bold}{cyan}{tool_name}{reset}{dim}({args}){reset}",
                                dim = styles.dim,
                                reset = styles.reset,
                                bold = styles.bold,
                                cyan = styles.cyan,
                                args = format_tool_args(arguments),
                            );
                        }
                        AgentEvent::ToolCallCompleted {
                            tool_name,
                            output,
                            is_error,
                            ..
                        } if verbose >= 2 => {
                            let label = if *is_error { "error" } else { "result" };
                            eprintln!(
                                "{dim}[{node_id}]   [{label}] {tool_name}:{reset}\n{}",
                                serde_json::to_string_pretty(output)
                                    .unwrap_or_else(|_| output.to_string()),
                                dim = styles.dim,
                                reset = styles.reset,
                            );
                        }
                        AgentEvent::Error { error } => {
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
            }
        });

        // Emit Prompt event before processing
        emitter.emit(&crate::event::PipelineEvent::Prompt {
            stage: node.id.clone(),
            text: prompt.to_string(),
        });

        session.initialize().await;
        session.process_input(prompt).await.map_err(|e| {
            AttractorError::Handler(format!("Agent session failed: {e}"))
        })?;

        // Aggregate token usage from all assistant turns.
        let (mut turn_count, mut tool_call_count, mut input_tokens, mut output_tokens) =
            (0usize, 0usize, 0i64, 0i64);
        let (mut cache_read_tokens, mut cache_write_tokens, mut reasoning_tokens): (
            Option<i64>,
            Option<i64>,
            Option<i64>,
        ) = (None, None, None);

        fn add_optional(acc: &mut Option<i64>, val: Option<i64>) {
            if let Some(v) = val {
                *acc = Some(acc.unwrap_or(0) + v);
            }
        }

        for turn in session.history().turns() {
            if let Turn::Assistant {
                tool_calls, usage, ..
            } = turn
            {
                turn_count += 1;
                tool_call_count += tool_calls.len();
                input_tokens += usage.input_tokens;
                output_tokens += usage.output_tokens;
                add_optional(&mut cache_read_tokens, usage.cache_read_tokens);
                add_optional(&mut cache_write_tokens, usage.cache_write_tokens);
                add_optional(&mut reasoning_tokens, usage.reasoning_tokens);
            }
        }

        let stage_usage = StageUsage {
            model: self.model.clone(),
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
            reasoning_tokens,
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

        // Collect files_touched from the shared set.
        let files_touched: Vec<String> = {
            let set = files_touched.lock().unwrap();
            let mut v: Vec<String> = set.iter().cloned().collect();
            v.sort();
            v
        };

        Ok(CodergenResult::Text { text: response, usage: Some(stage_usage), files_touched })
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
