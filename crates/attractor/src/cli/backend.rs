use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use agent::{
    AgentEvent, AnthropicProfile, ExecutionEnvironment, GeminiProfile, OpenAiProfile,
    ProviderProfile, Session, SessionConfig, Turn,
    subagent::{SessionFactory, SubAgentManager},
};
use llm::client::Client;
use llm::provider::Provider;
use util::terminal::Styles;

use crate::context::Context;
use crate::error::AttractorError;
use crate::graph::Node;
use crate::handler::codergen::{CodergenBackend, CodergenResult};
use crate::outcome::StageUsage;

/// LLM backend that delegates to an `agent` Session per invocation.
///
/// For `full` fidelity nodes sharing a thread key, sessions are cached
/// and reused so the LLM sees the full conversation history.
pub struct AgentBackend {
    model: String,
    provider: Provider,
    verbose: u8,
    styles: &'static Styles,
    sessions: Mutex<HashMap<String, Session>>,
}

impl AgentBackend {
    #[must_use]
    pub fn new(
        model: String,
        provider: Provider,
        verbose: u8,
        styles: &'static Styles,
    ) -> Self {
        Self {
            model,
            provider,
            verbose,
            styles,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    async fn create_session(
        &self,
        node: &Node,
        execution_env: &Arc<dyn ExecutionEnvironment>,
    ) -> Result<Session, AttractorError> {
        let client = Client::from_env()
            .await
            .map_err(|e| AttractorError::Handler(format!("Failed to create LLM client: {e}")))?;

        let mut profile = self.build_profile();

        let config = SessionConfig {
            reasoning_effort: Some(node.reasoning_effort().to_string()),
            ..SessionConfig::default()
        };

        let manager = Arc::new(tokio::sync::Mutex::new(
            SubAgentManager::new(config.max_subagent_depth),
        ));
        let manager_for_callback = manager.clone();

        // Build factory that creates child sessions WITHOUT subagent tools
        let factory_client = client.clone();
        let factory_provider = self.provider;
        let factory_model = self.model.clone();
        let factory_env = Arc::clone(execution_env);
        let factory: SessionFactory = Arc::new(move || {
            let child_profile: Arc<dyn ProviderProfile> = match factory_provider {
                Provider::OpenAi => Arc::new(OpenAiProfile::new(&factory_model)),
                Provider::Gemini => Arc::new(GeminiProfile::new(&factory_model)),
                Provider::Anthropic => Arc::new(AnthropicProfile::new(&factory_model)),
            };
            Session::new(
                factory_client.clone(),
                child_profile,
                Arc::clone(&factory_env),
                SessionConfig::default(),
            )
        });

        profile.register_subagent_tools(manager, factory, 0);
        let profile: Arc<dyn ProviderProfile> = Arc::from(profile);

        let session = Session::new(client, profile, Arc::clone(execution_env), config);

        // Wire subagent event callback to parent session's emitter
        manager_for_callback.lock().await.set_event_callback(session.event_callback());

        Ok(session)
    }

    fn build_profile(&self) -> Box<dyn ProviderProfile> {
        match self.provider {
            Provider::OpenAi => Box::new(OpenAiProfile::new(&self.model)),
            Provider::Gemini => Box::new(GeminiProfile::new(&self.model)),
            Provider::Anthropic => Box::new(AnthropicProfile::new(&self.model)),
        }
    }
}

#[async_trait]
impl CodergenBackend for AgentBackend {
    async fn one_shot(
        &self,
        node: &Node,
        prompt: &str,
        stage_dir: &std::path::Path,
    ) -> Result<CodergenResult, AttractorError> {
        let client = Client::from_env()
            .await
            .map_err(|e| AttractorError::Handler(format!("Failed to create LLM client: {e}")))?;

        let model = node.llm_model().unwrap_or(&self.model);
        let provider = node
            .llm_provider()
            .map(String::from)
            .or_else(|| Some(self.provider.as_str().to_string()));

        let request = llm::types::Request {
            model: model.to_string(),
            messages: vec![llm::types::Message::user(prompt)],
            provider,
            reasoning_effort: Some(node.reasoning_effort().to_string()),
            tools: None,
            tool_choice: None,
            response_format: None,
            temperature: None,
            top_p: None,
            max_tokens: None,
            stop_sequences: None,
            metadata: None,
            provider_options: None,
        };

        let _ = tokio::fs::create_dir_all(stage_dir).await;
        if let Ok(json) = serde_json::to_string_pretty(&request) {
            let _ = tokio::fs::write(stage_dir.join("api_request.json"), json).await;
        }

        let response = client
            .complete(&request)
            .await
            .map_err(|e| AttractorError::Handler(format!("one_shot LLM call failed: {e}")))?;

        if let Ok(json) = serde_json::to_string_pretty(&response) {
            let _ = tokio::fs::write(stage_dir.join("api_response.json"), json).await;
        }

        let provider_used = serde_json::json!({
            "mode": "one_shot",
            "provider": request.provider.as_deref().unwrap_or("anthropic"),
            "model": &request.model,
        });
        if let Ok(json) = serde_json::to_string_pretty(&provider_used) {
            let _ = tokio::fs::write(stage_dir.join("provider_used.json"), json).await;
        }

        let mut stage_usage = StageUsage {
            model: model.to_string(),
            input_tokens: response.usage.input_tokens,
            output_tokens: response.usage.output_tokens,
            cache_read_tokens: response.usage.cache_read_tokens,
            cache_write_tokens: response.usage.cache_write_tokens,
            reasoning_tokens: response.usage.reasoning_tokens,
            cost: None,
        };
        stage_usage.cost = super::compute_stage_cost(&stage_usage);

        Ok(CodergenResult::Text {
            text: response.text(),
            usage: Some(stage_usage),
            files_touched: Vec::new(),
        })
    }

    async fn run(
        &self,
        node: &Node,
        prompt: &str,
        context: &Context,
        thread_id: Option<&str>,
        emitter: &Arc<crate::event::EventEmitter>,
        stage_dir: &std::path::Path,
        execution_env: &Arc<dyn ExecutionEnvironment>,
    ) -> Result<CodergenResult, AttractorError> {
        let fidelity = context.get_string("internal.fidelity", "");
        let reuse_key = if fidelity == "full" {
            thread_id.map(String::from)
        } else {
            None
        };

        // Take a cached session if reusing, otherwise create a new one.
        let (mut session, is_reused) = if let Some(ref key) = reuse_key {
            let existing = self.sessions.lock().unwrap().remove(key);
            if let Some(s) = existing {
                (s, true)
            } else {
                (self.create_session(node, execution_env).await?, false)
            }
        } else {
            (self.create_session(node, execution_env).await?, false)
        };

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
                // Track file changes from tool calls
                match &event.event {
                    AgentEvent::ToolCallStarted {
                        tool_name,
                        tool_call_id,
                        arguments,
                    } => {
                        if tool_name == "write_file" || tool_name == "edit_file" {
                            if let Some(path) = arguments.get("file_path").and_then(|v| v.as_str()) {
                                pending_clone.lock().unwrap().insert(
                                    tool_call_id.clone(),
                                    path.to_string(),
                                );
                            }
                        }
                    }
                    AgentEvent::ToolCallCompleted {
                        tool_call_id,
                        is_error,
                        ..
                    } => {
                        if !*is_error {
                            if let Some(path) = pending_clone.lock().unwrap().remove(tool_call_id) {
                                files_clone.lock().unwrap().insert(path);
                            }
                        } else {
                            pending_clone.lock().unwrap().remove(tool_call_id);
                        }
                    }
                    _ => {}
                }

                // Forward non-streaming agent events to pipeline
                if !matches!(
                    &event.event,
                    AgentEvent::SessionStarted
                        | AgentEvent::SessionEnded
                        | AgentEvent::AssistantTextStart
                        | AgentEvent::TextDelta { .. }
                        | AgentEvent::ToolCallOutputDelta { .. }
                        | AgentEvent::SkillExpanded { .. }
                ) {
                    pipeline_emitter.emit(&PipelineEvent::Agent {
                        stage: node_id.clone(),
                        event: event.event.clone(),
                    });
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

        // Record turn count before processing so we only aggregate new usage.
        let turns_before = session.history().turns().len();

        if !is_reused {
            session.initialize().await;
        }

        let result = session.process_input(prompt).await.map_err(|e| {
            AttractorError::Handler(format!("Agent session failed: {e}"))
        });

        // On error, drop the session (don't cache failed state).
        result?;

        // Aggregate token usage only from new turns (prevents double-counting on reuse).
        let (mut turn_count, mut tool_call_count) = (0usize, 0usize);
        let mut total_usage = llm::types::Usage::default();
        for turn in &session.history().turns()[turns_before..] {
            if let Turn::Assistant {
                tool_calls, usage, ..
            } = turn
            {
                turn_count += 1;
                tool_call_count += tool_calls.len();
                total_usage = total_usage + usage.clone();
            }
        }

        let mut stage_usage = StageUsage {
            model: self.model.clone(),
            input_tokens: total_usage.input_tokens,
            output_tokens: total_usage.output_tokens,
            cache_read_tokens: total_usage.cache_read_tokens,
            cache_write_tokens: total_usage.cache_write_tokens,
            reasoning_tokens: total_usage.reasoning_tokens,
            cost: None,
        };
        stage_usage.cost = super::compute_stage_cost(&stage_usage);

        // Print session summary to stderr.
        if self.verbose >= 1 {
            let total_tokens = total_usage.input_tokens + total_usage.output_tokens;
            let token_str = if total_tokens >= 1000 {
                format!("{}k tokens", total_tokens / 1000)
            } else {
                format!("{total_tokens} tokens")
            };
            let reuse_label = if is_reused { " (reused session)" } else { "" };
            eprintln!(
                "{dim}[{node_id}] Done ({turn_count} turns, {tool_call_count} tool calls, {token_str}{reuse_label}){reset}",
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

        let provider_used = serde_json::json!({
            "mode": "agent_loop",
            "provider": self.provider.as_str(),
            "model": &self.model,
        });
        if let Ok(json) = serde_json::to_string_pretty(&provider_used) {
            let _ = std::fs::write(stage_dir.join("provider_used.json"), json);
        }

        // Cache session back for reuse on success.
        if let Some(key) = reuse_key {
            self.sessions.lock().unwrap().insert(key, session);
        }

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

#[cfg(test)]
mod tests {
    use super::*;
    use agent::subagent::SessionFactory;

    #[test]
    fn agent_backend_stores_config() {
        let styles = Box::leak(Box::new(Styles::new(false)));
        let backend = AgentBackend::new(
            "claude-opus-4-6".to_string(),
            Provider::OpenAi,
            2,
            styles,
        );
        assert_eq!(backend.model, "claude-opus-4-6");
        assert_eq!(backend.provider, Provider::OpenAi);
        assert_eq!(backend.verbose, 2);
    }

    #[test]
    fn agent_backend_initializes_empty_sessions() {
        let styles = Box::leak(Box::new(Styles::new(false)));
        let backend = AgentBackend::new(
            "claude-opus-4-6".to_string(),
            Provider::Anthropic,
            0,
            styles,
        );
        assert!(backend.sessions.lock().unwrap().is_empty());
    }

    #[test]
    fn build_profile_can_register_subagent_tools() {
        let styles = Box::leak(Box::new(Styles::new(false)));
        let backend = AgentBackend::new(
            "claude-opus-4-6".to_string(),
            Provider::Anthropic,
            0,
            styles,
        );
        let mut profile = backend.build_profile();
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
