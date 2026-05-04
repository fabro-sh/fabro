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
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::super::agent::{CodergenBackend, CodergenResult};
use crate::context::keys::Fidelity;
use crate::context::{Context, WorkflowContext};
use crate::error::Error;
use crate::event::{Emitter, Event, StageScope};
use crate::outcome::billed_model_usage_from_llm;

/// Spawn a task that, when the run-level token cancels, sets the agent
/// `Session`'s interrupt reason to `Cancelled` and cancels the session token.
///
/// Factored out of `SessionCancelBridgeGuard::replace` so it can be unit-tested
/// without constructing a real `Session`.
fn spawn_bridge_task(
    run_token: CancellationToken,
    interrupt_reason: Arc<Mutex<Option<fabro_agent::InterruptReason>>>,
    session_token: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        run_token.cancelled().await;
        {
            let mut guard = interrupt_reason
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if guard.is_none() {
                *guard = Some(fabro_agent::InterruptReason::Cancelled);
            }
        }
        session_token.cancel();
    })
}

/// Per-invocation guard that maps a run-level `CancellationToken` to an agent
/// `Session`'s interrupt reason and cancel token.
///
/// Dropping the guard aborts the spawned bridge task so a still-cached session
/// (after success) is not left wired to a stale run token.
struct SessionCancelBridgeGuard {
    handle: Option<JoinHandle<()>>,
}

impl SessionCancelBridgeGuard {
    fn new() -> Self {
        Self { handle: None }
    }

    fn replace(&mut self, run_token: CancellationToken, session: &Session) {
        self.abort();
        self.handle = Some(spawn_bridge_task(
            run_token,
            session.interrupt_reason_handle(),
            session.cancel_token(),
        ));
    }

    fn abort(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

impl Drop for SessionCancelBridgeGuard {
    fn drop(&mut self) {
        self.abort();
    }
}

/// Classification of an `fabro_agent::Error` for the API backend's `run` path.
enum AgentApiErrorDisposition {
    /// Session was interrupted via cancellation; surface as `Error::Cancelled`.
    Cancelled,
    /// Underlying LLM error eligible for provider failover.
    FailoverEligible(fabro_llm::Error),
    /// Terminal error; abort the invocation with this workflow `Error`.
    Terminal(Error),
}

fn classify_agent_error(err: fabro_agent::Error, allow_failover: bool) -> AgentApiErrorDisposition {
    match err {
        fabro_agent::Error::Interrupted(fabro_agent::InterruptReason::Cancelled) => {
            AgentApiErrorDisposition::Cancelled
        }
        fabro_agent::Error::Interrupted(fabro_agent::InterruptReason::WallClockTimeout) => {
            AgentApiErrorDisposition::Terminal(Error::Precondition(
                "Agent session hit its wall-clock timeout".to_string(),
            ))
        }
        fabro_agent::Error::Llm(err) if allow_failover && err.failover_eligible() => {
            AgentApiErrorDisposition::FailoverEligible(err)
        }
        fabro_agent::Error::Llm(err) => AgentApiErrorDisposition::Terminal(Error::Llm(err)),
        other => AgentApiErrorDisposition::Terminal(Error::handler(format!(
            "Agent session failed: {other}"
        ))),
    }
}

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
        let client = Client::from_source(source)
            .await
            .map_err(|e| Error::handler_with_source("Failed to create LLM client", &e))?;

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
        let client = Client::from_source(self.source.as_ref())
            .await
            .map_err(|e| Error::handler_with_source("Failed to create LLM client", &e))?;

        let model = node.model().unwrap_or(&self.model);
        let provider = node
            .provider()
            .map(String::from)
            .or_else(|| Some(self.provider.to_string()));

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

        // Build per-request fallback chain: if the node overrides the provider,
        // no failover is available; otherwise use the backend's.
        let fallback_chain: &[FallbackTarget] = if node.provider().is_some() {
            &[]
        } else {
            &self.fallback_chain
        };

        let result = client.complete(&request).await;

        let default_provider = self.provider.to_string();

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
        cancel_token: CancellationToken,
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

        let mut bridge = SessionCancelBridgeGuard::new();

        // Take a cached session if reusing, otherwise create a new one. Cancel
        // checks bracket `Client::from_source(...)` so cancellation arriving
        // during credential refresh is not lost.
        if cancel_token.is_cancelled() {
            return Err(Error::Cancelled);
        }
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
        if cancel_token.is_cancelled() {
            return Err(Error::Cancelled);
        }
        bridge.replace(cancel_token.clone(), &session);

        tracing::info!(
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

        let allow_failover_primary = !self.fallback_chain.is_empty();
        let init_result = if is_reused {
            Ok(())
        } else {
            match session.initialize().await {
                Ok(()) => Ok(()),
                Err(err) => match classify_agent_error(err, allow_failover_primary) {
                    AgentApiErrorDisposition::Cancelled => {
                        bridge.abort();
                        return Err(Error::Cancelled);
                    }
                    AgentApiErrorDisposition::Terminal(err) => {
                        bridge.abort();
                        return Err(err);
                    }
                    AgentApiErrorDisposition::FailoverEligible(sdk_err) => {
                        Err(fabro_agent::Error::Llm(sdk_err))
                    }
                },
            }
        };

        // If initialize failed with a failover-eligible error, treat as a
        // process_input failover trigger; otherwise run process_input.
        let result = match init_result {
            Ok(()) => session.process_input(prompt).await,
            Err(err) => Err(err),
        };

        // On failover-eligible error, try fallback providers.
        let result: Result<(), Error> = match result {
            Ok(()) => Ok(()),
            Err(err) => match classify_agent_error(err, allow_failover_primary) {
                AgentApiErrorDisposition::Cancelled => {
                    bridge.abort();
                    return Err(Error::Cancelled);
                }
                AgentApiErrorDisposition::Terminal(err) => {
                    bridge.abort();
                    return Err(err);
                }
                AgentApiErrorDisposition::FailoverEligible(sdk_err) => {
                    let error_msg = sdk_err.to_string();
                    let from_provider = self.provider.to_string();
                    let from_model = self.model.clone();

                    let mut last_err = Error::Llm(sdk_err);
                    let mut succeeded = false;

                    for (index, target) in self.fallback_chain.iter().enumerate() {
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

                        // Detach the bridge from the failing session before
                        // refreshing credentials and building a new one.
                        bridge.abort();
                        if cancel_token.is_cancelled() {
                            return Err(Error::Cancelled);
                        }
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
                        if cancel_token.is_cancelled() {
                            return Err(Error::Cancelled);
                        }
                        session = new_session;
                        bridge.replace(cancel_token.clone(), &session);

                        // Re-subscribe to forward events + track files from the new session
                        spawn_event_forwarder(
                            &session,
                            node.id.clone(),
                            stage_scope.clone(),
                            Arc::clone(emitter),
                            Arc::clone(&file_tracking),
                        );

                        let allow_failover_next = index + 1 < self.fallback_chain.len();
                        if let Err(err) = session.initialize().await {
                            match classify_agent_error(err, allow_failover_next) {
                                AgentApiErrorDisposition::Cancelled => {
                                    bridge.abort();
                                    return Err(Error::Cancelled);
                                }
                                AgentApiErrorDisposition::Terminal(err) => {
                                    bridge.abort();
                                    return Err(err);
                                }
                                AgentApiErrorDisposition::FailoverEligible(sdk_err) => {
                                    last_err = Error::Llm(sdk_err);
                                    continue;
                                }
                            }
                        }
                        match session.process_input(prompt).await {
                            Ok(()) => {
                                succeeded = true;
                                break;
                            }
                            Err(err) => match classify_agent_error(err, allow_failover_next) {
                                AgentApiErrorDisposition::Cancelled => {
                                    bridge.abort();
                                    return Err(Error::Cancelled);
                                }
                                AgentApiErrorDisposition::Terminal(err) => {
                                    bridge.abort();
                                    return Err(err);
                                }
                                AgentApiErrorDisposition::FailoverEligible(sdk_err) => {
                                    last_err = Error::Llm(sdk_err);
                                }
                            },
                        }
                    }

                    if succeeded { Ok(()) } else { Err(last_err) }
                }
            },
        };

        // On error, drop the session (don't cache failed state). The bridge's
        // `Drop` will abort the spawned task on early return.
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

        // Cache session back for reuse on success. Detach the bridge first so
        // the cached session is not left wired to this run's cancel token.
        if let Some(key) = reuse_key {
            bridge.abort();
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
    use fabro_llm::{Error as LlmError, ProviderErrorDetail, ProviderErrorKind};
    use fabro_vault::{SecretType, Vault};
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
        vault
            .set(
                "anthropic",
                &serde_json::to_string(&AuthCredential {
                    provider: Provider::Anthropic,
                    details:  AuthDetails::ApiKey {
                        key: "anthropic-key".to_string(),
                    },
                })
                .unwrap(),
                SecretType::Credential,
                None,
            )
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

    // --- Bridge guard tests ---

    fn failover_eligible_llm_error() -> LlmError {
        LlmError::Network {
            message: "boom".into(),
            source:  None,
        }
    }

    fn non_failover_llm_error() -> LlmError {
        LlmError::Provider {
            kind:   ProviderErrorKind::Authentication,
            detail: Box::new(ProviderErrorDetail {
                message:     "bad key".into(),
                provider:    "openai".into(),
                status_code: Some(401),
                error_code:  None,
                retry_after: None,
                raw:         None,
            }),
        }
    }

    #[tokio::test]
    async fn spawn_bridge_task_sets_cancelled_and_cancels_session_token() {
        let run_token = CancellationToken::new();
        let interrupt_reason = Arc::new(Mutex::new(None));
        let session_token = CancellationToken::new();

        let handle = spawn_bridge_task(
            run_token.clone(),
            Arc::clone(&interrupt_reason),
            session_token.clone(),
        );

        assert!(!session_token.is_cancelled());
        assert!(interrupt_reason.lock().unwrap().is_none());

        run_token.cancel();
        handle.await.unwrap();

        assert!(session_token.is_cancelled());
        assert_eq!(
            *interrupt_reason.lock().unwrap(),
            Some(fabro_agent::InterruptReason::Cancelled)
        );
    }

    #[tokio::test]
    async fn spawn_bridge_task_preserves_existing_interrupt_reason() {
        let run_token = CancellationToken::new();
        let interrupt_reason = Arc::new(Mutex::new(Some(
            fabro_agent::InterruptReason::WallClockTimeout,
        )));
        let session_token = CancellationToken::new();

        let handle = spawn_bridge_task(
            run_token.clone(),
            Arc::clone(&interrupt_reason),
            session_token.clone(),
        );
        run_token.cancel();
        handle.await.unwrap();

        // Existing reason wins; the bridge does not overwrite a wall-clock
        // timeout already recorded by the session.
        assert_eq!(
            *interrupt_reason.lock().unwrap(),
            Some(fabro_agent::InterruptReason::WallClockTimeout)
        );
        assert!(session_token.is_cancelled());
    }

    #[tokio::test]
    async fn bridge_guard_drop_aborts_pending_task() {
        let run_token = CancellationToken::new();
        let interrupt_reason = Arc::new(Mutex::new(None));
        let session_token = CancellationToken::new();

        {
            let mut guard = SessionCancelBridgeGuard::new();
            guard.handle = Some(spawn_bridge_task(
                run_token.clone(),
                Arc::clone(&interrupt_reason),
                session_token.clone(),
            ));
            // guard dropped here
        }

        // Trigger the run token after the guard has been dropped. The aborted
        // task must not write to interrupt_reason or cancel session_token.
        run_token.cancel();
        // Yield enough times for any errant task to run.
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }

        assert!(interrupt_reason.lock().unwrap().is_none());
        assert!(!session_token.is_cancelled());
    }

    #[tokio::test]
    async fn bridge_guard_replace_aborts_prior_task() {
        // First (prior) bridge wiring.
        let prior_run_token = CancellationToken::new();
        let prior_interrupt_reason = Arc::new(Mutex::new(None));
        let prior_session_token = CancellationToken::new();

        // Second (replacement) bridge wiring.
        let new_run_token = CancellationToken::new();
        let new_interrupt_reason = Arc::new(Mutex::new(None));
        let new_session_token = CancellationToken::new();

        let mut guard = SessionCancelBridgeGuard::new();
        guard.handle = Some(spawn_bridge_task(
            prior_run_token.clone(),
            Arc::clone(&prior_interrupt_reason),
            prior_session_token.clone(),
        ));

        // Replace with a new task pointing at different handles.
        guard.handle = {
            // Manually mirror `replace` semantics: abort then install.
            if let Some(h) = guard.handle.take() {
                h.abort();
            }
            Some(spawn_bridge_task(
                new_run_token.clone(),
                Arc::clone(&new_interrupt_reason),
                new_session_token.clone(),
            ))
        };

        // Cancelling the prior run token must not affect anything because the
        // prior task was aborted by `replace`.
        prior_run_token.cancel();
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert!(prior_interrupt_reason.lock().unwrap().is_none());
        assert!(!prior_session_token.is_cancelled());

        // The replacement task must still be alive and react to its own token.
        new_run_token.cancel();
        guard.handle.take().unwrap().await.unwrap();
        assert_eq!(
            *new_interrupt_reason.lock().unwrap(),
            Some(fabro_agent::InterruptReason::Cancelled)
        );
        assert!(new_session_token.is_cancelled());
    }

    // --- classify_agent_error tests ---

    #[test]
    fn classify_interrupted_cancelled_is_cancelled() {
        let err = fabro_agent::Error::Interrupted(fabro_agent::InterruptReason::Cancelled);
        assert!(matches!(
            classify_agent_error(err, true),
            AgentApiErrorDisposition::Cancelled
        ));
    }

    #[test]
    fn classify_interrupted_wall_clock_is_terminal_precondition() {
        let err = fabro_agent::Error::Interrupted(fabro_agent::InterruptReason::WallClockTimeout);
        match classify_agent_error(err, true) {
            AgentApiErrorDisposition::Terminal(Error::Precondition(msg)) => {
                assert!(msg.contains("wall-clock"));
            }
            _ => panic!("expected Terminal(Error::Precondition) for WallClockTimeout"),
        }
    }

    #[test]
    fn classify_failover_eligible_llm_returns_failover_when_allowed() {
        let err = fabro_agent::Error::Llm(failover_eligible_llm_error());
        assert!(matches!(
            classify_agent_error(err, true),
            AgentApiErrorDisposition::FailoverEligible(_)
        ));
    }

    #[test]
    fn classify_failover_eligible_llm_returns_terminal_when_not_allowed() {
        let err = fabro_agent::Error::Llm(failover_eligible_llm_error());
        match classify_agent_error(err, false) {
            AgentApiErrorDisposition::Terminal(Error::Llm(_)) => {}
            _ => panic!("expected Terminal(Error::Llm) when failover disallowed"),
        }
    }

    #[test]
    fn classify_non_failover_eligible_llm_is_terminal_llm() {
        let err = fabro_agent::Error::Llm(non_failover_llm_error());
        match classify_agent_error(err, true) {
            AgentApiErrorDisposition::Terminal(Error::Llm(_)) => {}
            _ => panic!("expected Terminal(Error::Llm) for non-failover-eligible LLM error"),
        }
    }

    #[test]
    fn classify_session_closed_is_terminal_handler() {
        let err = fabro_agent::Error::SessionClosed;
        match classify_agent_error(err, true) {
            AgentApiErrorDisposition::Terminal(Error::Handler { message, .. }) => {
                assert!(message.contains("Agent session failed"));
            }
            _ => panic!("expected Terminal(Error::Handler) for SessionClosed"),
        }
    }

    #[test]
    fn classify_invalid_state_is_terminal_handler() {
        let err = fabro_agent::Error::InvalidState("oops".into());
        match classify_agent_error(err, true) {
            AgentApiErrorDisposition::Terminal(Error::Handler { message, .. }) => {
                assert!(message.contains("Agent session failed"));
            }
            _ => panic!("expected Terminal(Error::Handler) for InvalidState"),
        }
    }

    #[test]
    fn classify_tool_execution_is_terminal_handler() {
        let err = fabro_agent::Error::ToolExecution("tool blew up".into());
        match classify_agent_error(err, true) {
            AgentApiErrorDisposition::Terminal(Error::Handler { message, .. }) => {
                assert!(message.contains("Agent session failed"));
            }
            _ => panic!("expected Terminal(Error::Handler) for ToolExecution"),
        }
    }
}
