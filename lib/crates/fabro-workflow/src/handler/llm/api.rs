use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fabro_agent::subagent::{SessionFactory, SubAgentManager};
use fabro_agent::{
    AgentEvent, AgentProfile, AnthropicProfile, GeminiProfile, OpenAiProfile, Sandbox, Session,
    SessionOptions, Turn,
};
use fabro_auth::{CredentialSource, EnvCredentialSource};
use fabro_graphviz::graph::Node;
use fabro_llm::client::Client;
use fabro_llm::types::{Message, Request, TokenCounts};
use fabro_mcp::config::McpServerSettings;
use fabro_model::{FallbackTarget, Provider};
use tokio::sync::Mutex as TokioMutex;

use super::super::agent::{CodergenBackend, CodergenResult};
use crate::context::keys::Fidelity;
use crate::context::{Context, WorkflowContext};
use crate::error::Error;
use crate::event::{Emitter, Event, StageScope};
use crate::outcome::billed_model_usage_from_llm;

fn build_profile(model: &str, provider: Provider) -> Box<dyn AgentProfile> {
    match provider {
        Provider::OpenAi => Box::new(OpenAiProfile::new(model)),
        Provider::Kimi
        | Provider::Zai
        | Provider::Minimax
        | Provider::Inception
        | Provider::OpenAiCompatible => Box::new(OpenAiProfile::new(model).with_provider(provider)),
        Provider::Gemini => Box::new(GeminiProfile::new(model)),
        Provider::Anthropic => Box::new(AnthropicProfile::new(model)),
    }
}

fn credential_providers(primary: Provider, fallback_chain: &[FallbackTarget]) -> Vec<Provider> {
    let mut providers = vec![primary];
    for target in fallback_chain {
        if let Ok(provider) = target.provider.parse::<Provider>() {
            if !providers.contains(&provider) {
                providers.push(provider);
            }
        }
    }
    providers
}

/// Shared state for tracking file modifications from agent tool calls.
struct FileTracking {
    /// Maps tool_call_id → file_path for in-flight write/edit calls.
    pending: HashMap<String, String>,
    /// Set of all file paths successfully written/edited.
    touched: HashSet<String>,
    /// Most recently modified file path.
    last:    Option<String>,
}

fn track_file_event(event: &AgentEvent, state: &mut FileTracking) {
    match event {
        AgentEvent::ToolCallStarted {
            tool_name,
            tool_call_id,
            arguments,
        } if tool_name == "write_file" || tool_name == "edit_file" => {
            if let Some(path) = arguments.get("file_path").and_then(|v| v.as_str()) {
                state.pending.insert(tool_call_id.clone(), path.to_string());
            }
        }
        AgentEvent::ToolCallCompleted {
            tool_call_id,
            is_error,
            ..
        } => {
            if let Some(path) = state.pending.remove(tool_call_id) {
                if !*is_error {
                    state.touched.insert(path.clone());
                    state.last = Some(path);
                }
            }
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
    scope: StageScope,
    emitter: Arc<Emitter>,
    file_tracking: Arc<Mutex<FileTracking>>,
) {
    let mut rx = session.subscribe();
    tokio::spawn(async move {
        while let Ok(event) = rx.recv().await {
            // Reset watchdog on every event, including streaming deltas
            emitter.touch();

            // Track file changes from tool calls (including sub-agent events)
            track_file_event(&event.event, &mut file_tracking.lock().unwrap());

            // Forward non-streaming agent events to pipeline
            if !event.event.is_streaming_noise()
                && !matches!(&event.event, AgentEvent::ProcessingEnd)
            {
                emitter.emit_scoped(
                    &Event::Agent {
                        stage:             node_id.clone(),
                        visit:             scope.visit,
                        event:             event.event.clone(),
                        session_id:        Some(event.session_id.clone()),
                        parent_session_id: event.parent_session_id.clone(),
                    },
                    &scope,
                );
            }
        }
    });
}

/// LLM backend that delegates to an `agent` Session per invocation.
///
/// For `full` fidelity nodes sharing a thread key, sessions are cached
/// and reused so the LLM sees the full conversation history.
pub struct AgentApiBackend {
    model:          String,
    provider:       Provider,
    fallback_chain: Vec<FallbackTarget>,
    sessions:       Mutex<HashMap<String, Session>>,
    env:            HashMap<String, String>,
    mcp_servers:    Vec<McpServerSettings>,
    source:         Arc<dyn CredentialSource>,
}

impl AgentApiBackend {
    #[must_use]
    pub fn new(
        model: String,
        provider: Provider,
        fallback_chain: Vec<FallbackTarget>,
        source: Arc<dyn CredentialSource>,
    ) -> Self {
        Self {
            model,
            provider,
            fallback_chain,
            sessions: Mutex::new(HashMap::new()),
            env: HashMap::new(),
            mcp_servers: Vec::new(),
            source,
        }
    }

    #[must_use]
    pub fn new_from_env(
        model: String,
        provider: Provider,
        fallback_chain: Vec<FallbackTarget>,
    ) -> Self {
        Self::new(
            model,
            provider,
            fallback_chain,
            Arc::new(EnvCredentialSource::new()),
        )
    }

    #[must_use]
    pub fn with_env(mut self, env: HashMap<String, String>) -> Self {
        self.env = env;
        self
    }

    #[must_use]
    pub fn with_mcp_servers(mut self, servers: Vec<McpServerSettings>) -> Self {
        self.mcp_servers = servers;
        self
    }

    async fn create_session(
        &self,
        node: &Node,
        sandbox: &Arc<dyn Sandbox>,
        tool_hooks: Option<Arc<dyn fabro_agent::ToolHookCallback>>,
    ) -> Result<Session, Error> {
        let model = node.model().unwrap_or(&self.model);
        let provider = node
            .provider()
            .and_then(|p| p.parse::<Provider>().ok())
            .unwrap_or(self.provider);
        Self::create_session_for(
            model,
            provider,
            node,
            sandbox,
            self.source.as_ref(),
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
        source: &dyn CredentialSource,
        env: &HashMap<String, String>,
        tool_hooks: Option<Arc<dyn fabro_agent::ToolHookCallback>>,
        mcp_servers: Vec<McpServerSettings>,
    ) -> Result<Session, Error> {
        let client = Client::from_source_for(source, &[provider])
            .await
            .map_err(|e| Error::handler(format!("Failed to create LLM client: {e}")))?;

        let mut profile = build_profile(model, provider);

        let config = SessionOptions {
            max_tokens: node.max_tokens(),
            reasoning_effort: node.reasoning_effort().parse().ok(),
            speed: node.speed().map(String::from),
            tool_hooks,
            mcp_servers,
            ..SessionOptions::default()
        };

        let manager = Arc::new(TokioMutex::new(SubAgentManager::new(
            config.max_subagent_depth,
        )));
        let manager_for_callback = manager.clone();

        // Build factory that creates child sessions WITHOUT subagent tools
        let factory_client = client.clone();
        let factory_model = model.to_string();
        let factory_env = Arc::clone(sandbox);
        let factory_tool_env = env.clone();
        let factory: SessionFactory = Arc::new(move || {
            let child_profile: Arc<dyn AgentProfile> = match provider {
                Provider::OpenAi => Arc::new(OpenAiProfile::new(&factory_model)),
                Provider::Kimi
                | Provider::Zai
                | Provider::Minimax
                | Provider::Inception
                | Provider::OpenAiCompatible => {
                    Arc::new(OpenAiProfile::new(&factory_model).with_provider(provider))
                }
                Provider::Gemini => Arc::new(GeminiProfile::new(&factory_model)),
                Provider::Anthropic => Arc::new(AnthropicProfile::new(&factory_model)),
            };
            let mut session = Session::new(
                factory_client.clone(),
                child_profile,
                Arc::clone(&factory_env),
                SessionOptions::default(),
                None,
            );
            if !factory_tool_env.is_empty() {
                session.set_tool_env(factory_tool_env.clone());
            }
            session
        });

        profile.register_subagent_tools(manager, factory, 0);
        let profile: Arc<dyn AgentProfile> = Arc::from(profile);

        let mut session = Session::new(
            client,
            profile,
            Arc::clone(sandbox),
            config,
            Some(manager_for_callback.clone()),
        );
        if !env.is_empty() {
            session.set_tool_env(env.clone());
        }

        // Wire subagent event callback to parent session's emitter
        manager_for_callback
            .lock()
            .await
            .set_event_callback(session.sub_agent_event_callback());

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
    ) -> Result<CodergenResult, Error> {
        let model = node.model().unwrap_or(&self.model);
        let provider = node
            .provider()
            .map(String::from)
            .or_else(|| Some(self.provider.as_str().to_string()));

        // Build per-request fallback chain: if the node overrides the provider,
        // no failover is available; otherwise use the backend's.
        let fallback_chain: &[FallbackTarget] = if node.provider().is_some() {
            &[]
        } else {
            &self.fallback_chain
        };
        let primary_provider = provider
            .as_deref()
            .and_then(|provider| provider.parse::<Provider>().ok())
            .unwrap_or(self.provider);
        let providers = credential_providers(primary_provider, fallback_chain);
        let client = Client::from_source_for(self.source.as_ref(), &providers)
            .await
            .map_err(|e| Error::handler(format!("Failed to create LLM client: {e}")))?;

        let max_tokens = node.max_tokens().or_else(|| {
            fabro_model::Catalog::builtin()
                .get(model)
                .and_then(|m| m.limits.max_output)
        });

        let mut messages = Vec::new();
        if let Some(sys) = system_prompt {
            messages.push(Message::system(sys));
        }
        messages.push(Message::user(prompt));

        let request = Request {
            model: model.to_string(),
            messages,
            provider,
            reasoning_effort: node.reasoning_effort().parse().ok(),
            speed: node.speed().map(String::from),
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
                        fabro_model::Catalog::builtin()
                            .get(&target.model)
                            .and_then(|m| m.limits.max_output)
                    });

                    let fallback_request = Request {
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
                        Err(err) => return Err(Error::Llm(err)),
                    }
                }

                match found {
                    Some(triple) => triple,
                    None => return Err(Error::Llm(last_err)),
                }
            }
            Err(sdk_err) => return Err(Error::Llm(sdk_err)),
        };

        let actual_provider = actual_provider.parse::<Provider>().unwrap_or(self.provider);
        let stage_usage = billed_model_usage_from_llm(
            &actual_model,
            actual_provider,
            node.speed(),
            &response.usage,
        );

        Ok(CodergenResult::Text {
            text:              response.text(),
            usage:             Some(stage_usage),
            files_touched:     Vec::new(),
            last_file_touched: None,
        })
    }

    async fn run(
        &self,
        node: &Node,
        prompt: &str,
        context: &Context,
        thread_id: Option<&str>,
        emitter: &Arc<Emitter>,
        sandbox: &Arc<dyn Sandbox>,
        tool_hooks: Option<Arc<dyn fabro_agent::ToolHookCallback>>,
    ) -> Result<CodergenResult, Error> {
        let actual_model = node.model().unwrap_or(&self.model).to_string();
        let _actual_provider = node
            .provider()
            .and_then(|p| p.parse::<Provider>().ok())
            .unwrap_or(self.provider);

        let fidelity = context.fidelity();
        let reuse_key = if fidelity == Fidelity::Full {
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
        let file_tracking = Arc::new(Mutex::new(FileTracking {
            pending: HashMap::new(),
            touched: HashSet::new(),
            last:    None,
        }));
        let stage_scope = StageScope::for_handler(context, &node.id);

        // Subscribe to session events: forward to pipeline emitter + track files.
        spawn_event_forwarder(
            &session,
            node.id.clone(),
            stage_scope.clone(),
            Arc::clone(emitter),
            Arc::clone(&file_tracking),
        );

        // Record turn count before processing so we only aggregate new usage.
        let turns_before = session.history().turns().len();

        if !is_reused {
            session.initialize().await;
        }

        let result = session.process_input(prompt).await;

        // On failover-eligible error, try fallback providers.
        let result = match result {
            Ok(()) => Ok(()),
            Err(fabro_agent::Error::Llm(ref sdk_err))
                if sdk_err.failover_eligible() && !self.fallback_chain.is_empty() =>
            {
                let error_msg = sdk_err.to_string();
                let from_provider = self.provider.as_str().to_string();
                let from_model = self.model.clone();

                let mut last_err = Error::Llm(sdk_err.clone());
                let mut succeeded = false;

                for target in &self.fallback_chain {
                    emitter.emit_scoped(
                        &Event::Failover {
                            stage:         node.id.clone(),
                            from_provider: from_provider.clone(),
                            from_model:    from_model.clone(),
                            to_provider:   target.provider.clone(),
                            to_model:      target.model.clone(),
                            error:         error_msg.clone(),
                        },
                        &stage_scope,
                    );

                    let target_provider: Provider = match target.provider.parse() {
                        Ok(p) => p,
                        Err(_) => continue,
                    };

                    let new_session = match Self::create_session_for(
                        &target.model,
                        target_provider,
                        node,
                        sandbox,
                        self.source.as_ref(),
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
                        stage_scope.clone(),
                        Arc::clone(emitter),
                        Arc::clone(&file_tracking),
                    );

                    session.initialize().await;
                    match session.process_input(prompt).await {
                        Ok(()) => {
                            succeeded = true;
                            break;
                        }
                        Err(fabro_agent::Error::Llm(err)) if err.failover_eligible() => {
                            last_err = Error::Llm(err);
                        }
                        Err(fabro_agent::Error::Llm(err)) => return Err(Error::Llm(err)),
                        Err(fabro_agent::Error::Interrupted(_)) => {
                            return Err(Error::Cancelled);
                        }
                        Err(other) => {
                            return Err(Error::handler(format!("Agent session failed: {other}")));
                        }
                    }
                }

                if succeeded { Ok(()) } else { Err(last_err) }
            }
            Err(fabro_agent::Error::Llm(sdk_err)) => Err(Error::Llm(sdk_err)),
            Err(fabro_agent::Error::Interrupted(_)) => Err(Error::Cancelled),
            Err(other) => Err(Error::handler(format!("Agent session failed: {other}"))),
        };

        // On error, drop the session (don't cache failed state).
        result?;

        // Aggregate token usage only from new turns (prevents double-counting on
        // reuse).
        let mut total_usage = TokenCounts::default();
        for turn in &session.history().turns()[turns_before..] {
            if let Turn::Assistant { usage, .. } = turn {
                total_usage += *usage.clone();
            }
        }

        let stage_usage = billed_model_usage_from_llm(
            &actual_model,
            _actual_provider,
            node.speed(),
            &total_usage,
        );

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

        // Collect files_touched from the shared tracking state.
        let (files_touched, last_file_touched) = {
            let s = file_tracking.lock().unwrap();
            let mut v: Vec<String> = s.touched.iter().cloned().collect();
            v.sort();
            (v, s.last.clone())
        };

        // Cache session back for reuse on success.
        if let Some(key) = reuse_key {
            self.sessions.lock().unwrap().insert(key, session);
        }

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
    use fabro_agent::subagent::SessionFactory;
    use fabro_auth::{AuthCredential, AuthDetails, VaultCredentialSource};
    use fabro_vault::Vault;
    use tokio::sync::RwLock as AsyncRwLock;

    use super::*;

    #[test]
    fn agent_backend_stores_config() {
        let backend = AgentApiBackend::new_from_env(
            "claude-opus-4-6".to_string(),
            Provider::OpenAi,
            Vec::new(),
        );
        assert_eq!(backend.model, "claude-opus-4-6");
        assert_eq!(backend.provider, Provider::OpenAi);
    }

    #[test]
    fn agent_backend_initializes_empty_sessions() {
        let backend = AgentApiBackend::new_from_env(
            "claude-opus-4-6".to_string(),
            Provider::Anthropic,
            Vec::new(),
        );
        assert!(backend.sessions.lock().unwrap().is_empty());
    }

    fn new_file_tracking() -> FileTracking {
        FileTracking {
            pending: HashMap::new(),
            touched: HashSet::new(),
            last:    None,
        }
    }

    #[test]
    fn track_file_event_records_top_level_write() {
        let mut state = new_file_tracking();

        let mut args = serde_json::Map::new();
        args.insert(
            "file_path".to_string(),
            serde_json::Value::String("/tmp/foo.rs".to_string()),
        );

        track_file_event(
            &AgentEvent::ToolCallStarted {
                tool_name:    "write_file".to_string(),
                tool_call_id: "tc1".to_string(),
                arguments:    serde_json::Value::Object(args),
            },
            &mut state,
        );
        assert_eq!(state.pending.get("tc1").unwrap(), "/tmp/foo.rs");

        track_file_event(
            &AgentEvent::ToolCallCompleted {
                tool_call_id: "tc1".to_string(),
                tool_name:    "write_file".to_string(),
                is_error:     false,
                output:       serde_json::Value::String("ok".to_string()),
            },
            &mut state,
        );
        assert!(state.touched.contains("/tmp/foo.rs"));
        assert_eq!(state.last.as_deref(), Some("/tmp/foo.rs"));
    }

    #[test]
    fn track_file_event_tracks_edit_file() {
        let mut state = new_file_tracking();

        let mut args = serde_json::Map::new();
        args.insert(
            "file_path".to_string(),
            serde_json::Value::String("/src/lib.rs".to_string()),
        );

        track_file_event(
            &AgentEvent::ToolCallStarted {
                tool_name:    "edit_file".to_string(),
                tool_call_id: "tc-sub".to_string(),
                arguments:    serde_json::Value::Object(args),
            },
            &mut state,
        );
        assert_eq!(state.pending.get("tc-sub").unwrap(), "/src/lib.rs");

        track_file_event(
            &AgentEvent::ToolCallCompleted {
                tool_call_id: "tc-sub".to_string(),
                tool_name:    "edit_file".to_string(),
                is_error:     false,
                output:       serde_json::Value::String("ok".to_string()),
            },
            &mut state,
        );
        assert!(state.touched.contains("/src/lib.rs"));
        assert_eq!(state.last.as_deref(), Some("/src/lib.rs"));
    }

    #[test]
    fn track_file_event_error_removes_pending() {
        let mut state = new_file_tracking();

        let mut args = serde_json::Map::new();
        args.insert(
            "file_path".to_string(),
            serde_json::Value::String("/err.rs".to_string()),
        );

        track_file_event(
            &AgentEvent::ToolCallStarted {
                tool_name:    "edit_file".to_string(),
                tool_call_id: "tc-err".to_string(),
                arguments:    serde_json::Value::Object(args),
            },
            &mut state,
        );

        track_file_event(
            &AgentEvent::ToolCallCompleted {
                tool_call_id: "tc-err".to_string(),
                tool_name:    "edit_file".to_string(),
                is_error:     true,
                output:       serde_json::Value::String("failed".to_string()),
            },
            &mut state,
        );
        assert!(state.pending.is_empty());
        assert!(!state.touched.contains("/err.rs"));
    }

    #[test]
    fn build_profile_can_register_subagent_tools() {
        let mut profile = build_profile("claude-opus-4-6", Provider::Anthropic);
        let manager = Arc::new(TokioMutex::new(SubAgentManager::new(1)));
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

    #[tokio::test]
    async fn api_backend_uses_source_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::load(dir.path().join("secrets.json")).unwrap();
        fabro_auth::vault_set_credential(&mut vault, "anthropic", &AuthCredential {
            provider: Provider::Anthropic,
            details:  AuthDetails::ApiKey {
                key: "anthropic-key".to_string(),
            },
        })
        .unwrap();
        let backend = AgentApiBackend::new(
            "claude-opus-4-6".to_string(),
            Provider::Anthropic,
            Vec::new(),
            Arc::new(VaultCredentialSource::with_env_lookup(
                Arc::new(AsyncRwLock::new(vault)),
                |_| None,
            )),
        );

        let client = Client::from_source(backend.source.as_ref()).await.unwrap();

        assert_eq!(client.provider_names(), vec!["anthropic"]);
    }
}
