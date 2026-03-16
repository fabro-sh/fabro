use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use fabro_agent::{
    subagent::{SessionFactory, SubAgentManager},
    AgentEvent, AnthropicProfile, GeminiProfile, OpenAiProfile, ProviderProfile, Sandbox, Session,
    SessionConfig, Turn,
};
use fabro_llm::catalog::FallbackTarget;
use fabro_llm::client::Client;
use fabro_llm::provider::Provider;

use crate::context::Context;
use crate::error::FabroError;
use crate::event::WorkflowRunEvent;
use crate::graph::Node;
use crate::handler::agent::{CodergenBackend, CodergenResult};
use crate::outcome::StageUsage;

fn build_profile(model: &str, provider: Provider) -> Box<dyn ProviderProfile> {
    match provider {
        Provider::OpenAi => Box::new(OpenAiProfile::new(model)),
        Provider::Kimi | Provider::Zai | Provider::Minimax | Provider::Inception => {
            Box::new(OpenAiProfile::new(model).with_provider(provider))
        }
        Provider::Gemini => Box::new(GeminiProfile::new(model)),
        Provider::Anthropic => Box::new(AnthropicProfile::new(model)),
    }
}

/// Recursively extract file-tracking events from agent events, including
/// those wrapped in one or more layers of `SubAgentEvent`.
fn track_file_event(
    event: &AgentEvent,
    pending_tool_calls: &Arc<Mutex<HashMap<String, String>>>,
    files_touched: &Arc<Mutex<HashSet<String>>>,
    last_file_touched: &Arc<Mutex<Option<String>>>,
) {
    match event {
        AgentEvent::ToolCallStarted {
            tool_name,
            tool_call_id,
            arguments,
        } => {
            if tool_name == "write_file" || tool_name == "edit_file" {
                if let Some(path) = arguments.get("file_path").and_then(|v| v.as_str()) {
                    pending_tool_calls
                        .lock()
                        .unwrap()
                        .insert(tool_call_id.clone(), path.to_string());
                }
            }
        }
        AgentEvent::ToolCallCompleted {
            tool_call_id,
            is_error,
            ..
        } => {
            if !*is_error {
                if let Some(path) = pending_tool_calls.lock().unwrap().remove(tool_call_id) {
                    files_touched.lock().unwrap().insert(path.clone());
                    *last_file_touched.lock().unwrap() = Some(path);
                }
            } else {
                pending_tool_calls.lock().unwrap().remove(tool_call_id);
            }
        }
        AgentEvent::SubAgentEvent { event: inner, .. } => {
            track_file_event(inner, pending_tool_calls, files_touched, last_file_touched);
        }
        _ => {}
    }
}

/// Spawn a task that subscribes to session events and:
/// 1. Tracks file changes (write_file/edit_file tool calls) into shared state.
/// 2. Forwards non-streaming agent events to the pipeline emitter.
fn spawn_event_forwarder(
    session: &Session,
    node_id: String,
    emitter: Arc<crate::event::EventEmitter>,
    pending_tool_calls: Arc<Mutex<HashMap<String, String>>>,
    files_touched: Arc<Mutex<HashSet<String>>>,
    last_file_touched: Arc<Mutex<Option<String>>>,
) {
    let mut rx = session.subscribe();
    tokio::spawn(async move {
        while let Ok(event) = rx.recv().await {
            // Reset watchdog on every event, including streaming deltas
            emitter.touch();

            // Track file changes from tool calls (including sub-agent events)
            track_file_event(
                &event.event,
                &pending_tool_calls,
                &files_touched,
                &last_file_touched,
            );

            // Forward non-streaming agent events to pipeline
            if !matches!(
                &event.event,
                AgentEvent::SessionStarted
                    | AgentEvent::SessionEnded
                    | AgentEvent::AssistantTextStart
                    | AgentEvent::TextDelta { .. }
                    | AgentEvent::ReasoningDelta { .. }
                    | AgentEvent::ToolCallOutputDelta { .. }
                    | AgentEvent::SkillExpanded { .. }
            ) {
                emitter.emit(&WorkflowRunEvent::Agent {
                    stage: node_id.clone(),
                    event: event.event.clone(),
                });
            }
        }
    });
}

/// LLM backend that delegates to an `agent` Session per invocation.
///
/// For `full` fidelity nodes sharing a thread key, sessions are cached
/// and reused so the LLM sees the full conversation history.
pub struct AgentApiBackend {
    model: String,
    provider: Provider,
    fallback_chain: Vec<FallbackTarget>,
    sessions: Mutex<HashMap<String, Session>>,
    env: HashMap<String, String>,
    mcp_servers: Vec<fabro_mcp::config::McpServerConfig>,
}

impl AgentApiBackend {
    #[must_use]
    pub fn new(model: String, provider: Provider, fallback_chain: Vec<FallbackTarget>) -> Self {
        Self {
            model,
            provider,
            fallback_chain,
            sessions: Mutex::new(HashMap::new()),
            env: HashMap::new(),
            mcp_servers: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_env(mut self, env: HashMap<String, String>) -> Self {
        self.env = env;
        self
    }

    #[must_use]
    pub fn with_mcp_servers(mut self, servers: Vec<fabro_mcp::config::McpServerConfig>) -> Self {
        self.mcp_servers = servers;
        self
    }

    async fn create_session(
        &self,
        node: &Node,
        sandbox: &Arc<dyn Sandbox>,
        tool_hooks: Option<Arc<dyn fabro_agent::ToolHookCallback>>,
    ) -> Result<Session, FabroError> {
        Self::create_session_for(
            &self.model,
            self.provider,
            node,
            sandbox,
            &self.env,
            tool_hooks,
            self.mcp_servers.clone(),
        )
        .await
    }

    async fn create_session_for(
        model: &str,
        provider: Provider,
        node: &Node,
        sandbox: &Arc<dyn Sandbox>,
        env: &HashMap<String, String>,
        tool_hooks: Option<Arc<dyn fabro_agent::ToolHookCallback>>,
        mcp_servers: Vec<fabro_mcp::config::McpServerConfig>,
    ) -> Result<Session, FabroError> {
        let client = Client::from_env()
            .await
            .map_err(|e| FabroError::handler(format!("Failed to create LLM client: {e}")))?;

        let mut profile = build_profile(model, provider);

        let config = SessionConfig {
            max_tokens: node.max_tokens(),
            reasoning_effort: Some(node.reasoning_effort().to_string()),
            tool_hooks,
            mcp_servers,
            ..SessionConfig::default()
        };

        let manager = Arc::new(tokio::sync::Mutex::new(SubAgentManager::new(
            config.max_subagent_depth,
        )));
        let manager_for_callback = manager.clone();

        // Build factory that creates child sessions WITHOUT subagent tools
        let factory_client = client.clone();
        let factory_model = model.to_string();
        let factory_env = Arc::clone(sandbox);
        let factory_tool_env = env.clone();
        let factory: SessionFactory = Arc::new(move || {
            let child_profile: Arc<dyn ProviderProfile> = match provider {
                Provider::OpenAi => Arc::new(OpenAiProfile::new(&factory_model)),
                Provider::Kimi | Provider::Zai | Provider::Minimax | Provider::Inception => {
                    Arc::new(OpenAiProfile::new(&factory_model).with_provider(provider))
                }
                Provider::Gemini => Arc::new(GeminiProfile::new(&factory_model)),
                Provider::Anthropic => Arc::new(AnthropicProfile::new(&factory_model)),
            };
            let mut session = Session::new(
                factory_client.clone(),
                child_profile,
                Arc::clone(&factory_env),
                SessionConfig::default(),
            );
            if !factory_tool_env.is_empty() {
                session.set_tool_env(factory_tool_env.clone());
            }
            session
        });

        profile.register_subagent_tools(manager, factory, 0);
        let profile: Arc<dyn ProviderProfile> = Arc::from(profile);

        let mut session = Session::new(client, profile, Arc::clone(sandbox), config);
        if !env.is_empty() {
            session.set_tool_env(env.clone());
        }

        // Wire subagent event callback to parent session's emitter
        manager_for_callback
            .lock()
            .await
            .set_event_callback(session.event_callback());

        Ok(session)
    }
}

#[async_trait]
impl CodergenBackend for AgentApiBackend {
    async fn one_shot(
        &self,
        node: &Node,
        prompt: &str,
        system_prompt: Option<&str>,
        stage_dir: &std::path::Path,
    ) -> Result<CodergenResult, FabroError> {
        let client = Client::from_env()
            .await
            .map_err(|e| FabroError::handler(format!("Failed to create LLM client: {e}")))?;

        let model = node.model().unwrap_or(&self.model);
        let provider = node
            .provider()
            .map(String::from)
            .or_else(|| Some(self.provider.as_str().to_string()));

        let max_tokens = node.max_tokens().or_else(|| {
            fabro_llm::catalog::get_model_info(model).and_then(|m| m.limits.max_output)
        });

        let mut messages = Vec::new();
        if let Some(sys) = system_prompt {
            messages.push(fabro_llm::types::Message::system(sys));
        }
        messages.push(fabro_llm::types::Message::user(prompt));

        let request = fabro_llm::types::Request {
            model: model.to_string(),
            messages,
            provider,
            reasoning_effort: Some(node.reasoning_effort().to_string()),
            tools: None,
            tool_choice: None,
            response_format: None,
            temperature: None,
            top_p: None,
            max_tokens,
            stop_sequences: None,
            metadata: None,
            provider_options: None,
        };

        let _ = tokio::fs::create_dir_all(stage_dir).await;
        if let Ok(json) = serde_json::to_string_pretty(&request) {
            let _ = tokio::fs::write(stage_dir.join("api_request.json"), json).await;
        }

        // Build per-request fallback chain: if the node overrides the provider,
        // no failover is available; otherwise use the backend's.
        let fallback_chain: &[FallbackTarget] = if node.provider().is_some() {
            &[]
        } else {
            &self.fallback_chain
        };

        let result = client.complete(&request).await;

        let default_provider = self.provider.as_str().to_string();

        let (response, actual_model, actual_provider) = match result {
            Ok(resp) => (
                resp,
                request.model.clone(),
                request
                    .provider
                    .clone()
                    .unwrap_or_else(|| default_provider.clone()),
            ),
            Err(sdk_err) if sdk_err.failover_eligible() && !fallback_chain.is_empty() => {
                let error_msg = sdk_err.to_string();
                let from_provider = request
                    .provider
                    .clone()
                    .unwrap_or_else(|| default_provider.clone());
                let from_model = request.model.clone();

                let mut last_err = sdk_err;
                let mut found = None;

                for target in fallback_chain {
                    tracing::warn!(
                        stage = node.id.as_str(),
                        from_provider = from_provider.as_str(),
                        from_model = from_model.as_str(),
                        to_provider = target.provider.as_str(),
                        to_model = target.model.as_str(),
                        error = error_msg.as_str(),
                        "LLM provider failover (prompt)"
                    );

                    let max_tokens = node.max_tokens().or_else(|| {
                        fabro_llm::catalog::get_model_info(&target.model)
                            .and_then(|m| m.limits.max_output)
                    });

                    let fallback_request = fabro_llm::types::Request {
                        model: target.model.clone(),
                        provider: Some(target.provider.clone()),
                        max_tokens,
                        ..request.clone()
                    };

                    match client.complete(&fallback_request).await {
                        Ok(resp) => {
                            found = Some((resp, target.model.clone(), target.provider.clone()));
                            break;
                        }
                        Err(err) if err.failover_eligible() => {
                            last_err = err;
                        }
                        Err(err) => return Err(FabroError::Llm(err)),
                    }
                }

                match found {
                    Some(triple) => triple,
                    None => return Err(FabroError::Llm(last_err)),
                }
            }
            Err(sdk_err) => return Err(FabroError::Llm(sdk_err)),
        };

        if let Ok(json) = serde_json::to_string_pretty(&response) {
            let _ = tokio::fs::write(stage_dir.join("api_response.json"), json).await;
        }

        let provider_used = serde_json::json!({
            "mode": "prompt",
            "provider": &actual_provider,
            "model": &actual_model,
        });
        if let Ok(json) = serde_json::to_string_pretty(&provider_used) {
            let _ = tokio::fs::write(stage_dir.join("provider_used.json"), json).await;
        }

        let mut stage_usage = StageUsage {
            model: actual_model,
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
            last_file_touched: None,
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
        sandbox: &Arc<dyn Sandbox>,
        tool_hooks: Option<Arc<dyn fabro_agent::ToolHookCallback>>,
    ) -> Result<CodergenResult, FabroError> {
        let fidelity = context.fidelity();
        let reuse_key = if fidelity == crate::context::keys::Fidelity::Full {
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
                (
                    self.create_session(node, sandbox, tool_hooks.clone())
                        .await?,
                    false,
                )
            }
        } else {
            (
                self.create_session(node, sandbox, tool_hooks.clone())
                    .await?,
                false,
            )
        };

        tracing::debug!(
            node = %node.id,
            fidelity = %fidelity,
            reused = is_reused,
            "Agent session ready"
        );

        // File change tracking: shared between spawned task and main fn.
        let pending_tool_calls: Arc<Mutex<HashMap<String, String>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let files_touched: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
        let last_file_touched: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

        // Subscribe to session events: forward to pipeline emitter + track files.
        spawn_event_forwarder(
            &session,
            node.id.clone(),
            Arc::clone(emitter),
            Arc::clone(&pending_tool_calls),
            Arc::clone(&files_touched),
            Arc::clone(&last_file_touched),
        );

        // Emit Prompt event before processing
        emitter.emit(&crate::event::WorkflowRunEvent::Prompt {
            stage: node.id.clone(),
            text: prompt.to_string(),
        });

        // Record turn count before processing so we only aggregate new usage.
        let turns_before = session.history().turns().len();

        if !is_reused {
            session.initialize().await;
        }

        let result = session.process_input(prompt).await;

        // On failover-eligible error, try fallback providers.
        let result = match result {
            Ok(()) => Ok(()),
            Err(fabro_agent::AgentError::Llm(ref sdk_err))
                if sdk_err.failover_eligible() && !self.fallback_chain.is_empty() =>
            {
                let error_msg = sdk_err.to_string();
                let from_provider = self.provider.as_str().to_string();
                let from_model = self.model.clone();

                let mut last_err = FabroError::Llm(sdk_err.clone());
                let mut succeeded = false;

                for target in &self.fallback_chain {
                    emitter.emit(&WorkflowRunEvent::Failover {
                        stage: node.id.clone(),
                        from_provider: from_provider.clone(),
                        from_model: from_model.clone(),
                        to_provider: target.provider.clone(),
                        to_model: target.model.clone(),
                        error: error_msg.clone(),
                    });

                    let target_provider: Provider = match target.provider.parse() {
                        Ok(p) => p,
                        Err(_) => continue,
                    };

                    let new_session = match Self::create_session_for(
                        &target.model,
                        target_provider,
                        node,
                        sandbox,
                        &self.env,
                        tool_hooks.clone(),
                        self.mcp_servers.clone(),
                    )
                    .await
                    {
                        Ok(s) => s,
                        Err(e) => {
                            last_err = e;
                            continue;
                        }
                    };
                    session = new_session;

                    // Re-subscribe to forward events + track files from the new session
                    spawn_event_forwarder(
                        &session,
                        node.id.clone(),
                        Arc::clone(emitter),
                        Arc::clone(&pending_tool_calls),
                        Arc::clone(&files_touched),
                        Arc::clone(&last_file_touched),
                    );

                    session.initialize().await;
                    match session.process_input(prompt).await {
                        Ok(()) => {
                            succeeded = true;
                            break;
                        }
                        Err(fabro_agent::AgentError::Llm(err)) if err.failover_eligible() => {
                            last_err = FabroError::Llm(err);
                        }
                        Err(fabro_agent::AgentError::Llm(err)) => return Err(FabroError::Llm(err)),
                        Err(fabro_agent::AgentError::Aborted(_)) => {
                            return Err(FabroError::Cancelled)
                        }
                        Err(other) => {
                            return Err(FabroError::handler(format!(
                                "Agent session failed: {other}"
                            )));
                        }
                    }
                }

                if succeeded {
                    Ok(())
                } else {
                    Err(last_err)
                }
            }
            Err(fabro_agent::AgentError::Llm(sdk_err)) => Err(FabroError::Llm(sdk_err)),
            Err(fabro_agent::AgentError::Aborted(_)) => Err(FabroError::Cancelled),
            Err(other) => Err(FabroError::handler(format!(
                "Agent session failed: {other}"
            ))),
        };

        // On error, drop the session (don't cache failed state).
        result?;

        // Aggregate token usage only from new turns (prevents double-counting on reuse).
        let mut total_usage = fabro_llm::types::Usage::default();
        for turn in &session.history().turns()[turns_before..] {
            if let Turn::Assistant { usage, .. } = turn {
                total_usage = total_usage + *usage.clone();
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
            "mode": "agent",
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

        let last_file_touched = last_file_touched.lock().unwrap().clone();

        Ok(CodergenResult::Text {
            text: response,
            usage: Some(stage_usage),
            files_touched,
            last_file_touched,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fabro_agent::subagent::SessionFactory;

    #[test]
    fn agent_backend_stores_config() {
        let backend =
            AgentApiBackend::new("claude-opus-4-6".to_string(), Provider::OpenAi, Vec::new());
        assert_eq!(backend.model, "claude-opus-4-6");
        assert_eq!(backend.provider, Provider::OpenAi);
    }

    #[test]
    fn agent_backend_initializes_empty_sessions() {
        let backend = AgentApiBackend::new(
            "claude-opus-4-6".to_string(),
            Provider::Anthropic,
            Vec::new(),
        );
        assert!(backend.sessions.lock().unwrap().is_empty());
    }

    #[test]
    fn track_file_event_records_top_level_write() {
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let touched = Arc::new(Mutex::new(HashSet::new()));
        let last = Arc::new(Mutex::new(None));

        let mut args = serde_json::Map::new();
        args.insert(
            "file_path".to_string(),
            serde_json::Value::String("/tmp/foo.rs".to_string()),
        );

        track_file_event(
            &AgentEvent::ToolCallStarted {
                tool_name: "write_file".to_string(),
                tool_call_id: "tc1".to_string(),
                arguments: serde_json::Value::Object(args),
            },
            &pending,
            &touched,
            &last,
        );
        assert_eq!(pending.lock().unwrap().get("tc1").unwrap(), "/tmp/foo.rs");

        track_file_event(
            &AgentEvent::ToolCallCompleted {
                tool_call_id: "tc1".to_string(),
                tool_name: "write_file".to_string(),
                is_error: false,
                output: serde_json::Value::String("ok".to_string()),
            },
            &pending,
            &touched,
            &last,
        );
        assert!(touched.lock().unwrap().contains("/tmp/foo.rs"));
        assert_eq!(last.lock().unwrap().as_deref(), Some("/tmp/foo.rs"));
    }

    #[test]
    fn track_file_event_unwraps_sub_agent_edit() {
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let touched = Arc::new(Mutex::new(HashSet::new()));
        let last = Arc::new(Mutex::new(None));

        let mut args = serde_json::Map::new();
        args.insert(
            "file_path".to_string(),
            serde_json::Value::String("/src/lib.rs".to_string()),
        );

        // ToolCallStarted wrapped in SubAgentEvent
        track_file_event(
            &AgentEvent::SubAgentEvent {
                agent_id: "sub-1".to_string(),
                depth: 1,
                event: Box::new(AgentEvent::ToolCallStarted {
                    tool_name: "edit_file".to_string(),
                    tool_call_id: "tc-sub".to_string(),
                    arguments: serde_json::Value::Object(args),
                }),
            },
            &pending,
            &touched,
            &last,
        );
        assert_eq!(
            pending.lock().unwrap().get("tc-sub").unwrap(),
            "/src/lib.rs"
        );

        // ToolCallCompleted wrapped in SubAgentEvent
        track_file_event(
            &AgentEvent::SubAgentEvent {
                agent_id: "sub-1".to_string(),
                depth: 1,
                event: Box::new(AgentEvent::ToolCallCompleted {
                    tool_call_id: "tc-sub".to_string(),
                    tool_name: "edit_file".to_string(),
                    is_error: false,
                    output: serde_json::Value::String("ok".to_string()),
                }),
            },
            &pending,
            &touched,
            &last,
        );
        assert!(touched.lock().unwrap().contains("/src/lib.rs"));
        assert_eq!(last.lock().unwrap().as_deref(), Some("/src/lib.rs"));
    }

    #[test]
    fn track_file_event_unwraps_nested_sub_sub_agent() {
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let touched = Arc::new(Mutex::new(HashSet::new()));
        let last = Arc::new(Mutex::new(None));

        let mut args = serde_json::Map::new();
        args.insert(
            "file_path".to_string(),
            serde_json::Value::String("/deep/file.rs".to_string()),
        );

        // Double-wrapped SubAgentEvent → SubAgentEvent → ToolCallStarted
        track_file_event(
            &AgentEvent::SubAgentEvent {
                agent_id: "sub-outer".to_string(),
                depth: 1,
                event: Box::new(AgentEvent::SubAgentEvent {
                    agent_id: "sub-inner".to_string(),
                    depth: 2,
                    event: Box::new(AgentEvent::ToolCallStarted {
                        tool_name: "write_file".to_string(),
                        tool_call_id: "tc-deep".to_string(),
                        arguments: serde_json::Value::Object(args),
                    }),
                }),
            },
            &pending,
            &touched,
            &last,
        );
        assert!(pending.lock().unwrap().contains_key("tc-deep"));

        track_file_event(
            &AgentEvent::SubAgentEvent {
                agent_id: "sub-outer".to_string(),
                depth: 1,
                event: Box::new(AgentEvent::SubAgentEvent {
                    agent_id: "sub-inner".to_string(),
                    depth: 2,
                    event: Box::new(AgentEvent::ToolCallCompleted {
                        tool_call_id: "tc-deep".to_string(),
                        tool_name: "write_file".to_string(),
                        is_error: false,
                        output: serde_json::Value::String("ok".to_string()),
                    }),
                }),
            },
            &pending,
            &touched,
            &last,
        );
        assert!(touched.lock().unwrap().contains("/deep/file.rs"));
    }

    #[test]
    fn track_file_event_error_removes_pending() {
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let touched = Arc::new(Mutex::new(HashSet::new()));
        let last = Arc::new(Mutex::new(None));

        let mut args = serde_json::Map::new();
        args.insert(
            "file_path".to_string(),
            serde_json::Value::String("/err.rs".to_string()),
        );

        track_file_event(
            &AgentEvent::SubAgentEvent {
                agent_id: "sub-1".to_string(),
                depth: 1,
                event: Box::new(AgentEvent::ToolCallStarted {
                    tool_name: "edit_file".to_string(),
                    tool_call_id: "tc-err".to_string(),
                    arguments: serde_json::Value::Object(args),
                }),
            },
            &pending,
            &touched,
            &last,
        );

        track_file_event(
            &AgentEvent::SubAgentEvent {
                agent_id: "sub-1".to_string(),
                depth: 1,
                event: Box::new(AgentEvent::ToolCallCompleted {
                    tool_call_id: "tc-err".to_string(),
                    tool_name: "edit_file".to_string(),
                    is_error: true,
                    output: serde_json::Value::String("failed".to_string()),
                }),
            },
            &pending,
            &touched,
            &last,
        );
        assert!(pending.lock().unwrap().is_empty());
        assert!(!touched.lock().unwrap().contains("/err.rs"));
    }

    #[test]
    fn build_profile_can_register_subagent_tools() {
        let mut profile = build_profile("claude-opus-4-6", Provider::Anthropic);
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
