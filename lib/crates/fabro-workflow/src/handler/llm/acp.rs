//! Workflow adapter for ACP-backed LLM stages.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fabro_acp::{
    AcpCommandError, AcpControlHandle, AcpError, AcpLiveControl, AcpProcessSpec, AcpRunRequest,
    render_stop_reason,
};
use fabro_agent::{AgentEvent, Sandbox, StaticEnvProvider, SteeringItem, ToolEnvProvider};
use fabro_graphviz::graph::Node;
use fabro_types::{AgentBackend, Principal, SessionCapability, StageId, SteeringMessage};
use fabro_util::time::elapsed_ms;
use tokio_util::sync::CancellationToken;

use super::super::agent::{CodergenBackend, CodergenResult, CodergenRunRequest, OneShotRequest};
use super::activation_lease::{ActivationLease, ActivationLeaseOptions};
use super::changed_files;
use crate::error::Error;
use crate::event::{Emitter, Event, RunNoticeCode, RunNoticeLevel, StageScope};
use crate::handler::NodeTimeoutPolicy;
use crate::steering_hub::{ActiveControlHandle, SteeringHub};

pub struct AgentAcpBackend {
    tool_env:                     Option<Arc<dyn ToolEnvProvider>>,
    github_token_refresh_managed: bool,
    steering_hub:                 Option<Arc<SteeringHub>>,
}

impl AgentAcpBackend {
    #[must_use]
    pub fn new() -> Self {
        Self {
            tool_env:                     None,
            github_token_refresh_managed: false,
            steering_hub:                 None,
        }
    }

    #[must_use]
    pub fn with_env(mut self, env: HashMap<String, String>) -> Self {
        self.tool_env = Some(Arc::new(StaticEnvProvider(env)));
        self
    }

    #[must_use]
    pub fn with_tool_env_provider(
        mut self,
        provider: Arc<dyn ToolEnvProvider>,
        github_token_refresh_managed: bool,
    ) -> Self {
        self.tool_env = Some(provider);
        self.github_token_refresh_managed = github_token_refresh_managed;
        self
    }

    #[must_use]
    pub fn with_steering_hub(mut self, steering_hub: Arc<SteeringHub>) -> Self {
        self.steering_hub = Some(steering_hub);
        self
    }

    async fn run_turn(
        &self,
        node: &Node,
        prompt: String,
        emitter: &Arc<Emitter>,
        stage_scope: &StageScope,
        sandbox: &Arc<dyn Sandbox>,
        cancel_token: CancellationToken,
    ) -> Result<CodergenResult, Error> {
        let process_spec = resolve_acp_process_spec(node)?;
        let config_name = process_spec.name().map(str::to_string);
        let launch_env = self.resolve_launch_env(emitter).await?;
        let on_activity = {
            let emitter = Arc::clone(emitter);
            Arc::new(move || emitter.touch()) as Arc<dyn Fn() + Send + Sync>
        };
        let command_display = process_spec.to_string();
        emitter.emit_scoped(
            &Event::AgentAcpStarted {
                node_id:     node.id.clone(),
                visit:       stage_scope.visit,
                command:     command_display,
                config_name: config_name.clone(),
            },
            stage_scope,
        );

        let control_handle = AcpControlHandle::new();
        let activation_session_id = format!("acp-{}", uuid::Uuid::new_v4());
        let activation_lease = self.activate_control_session(
            &control_handle,
            &activation_session_id,
            node,
            stage_scope,
            emitter,
            config_name.as_deref(),
        )?;
        let lease_for_completion = Arc::new(Mutex::new(activation_lease));
        let on_natural_completion = self.steering_hub.as_ref().map(|_| {
            let lease = Arc::clone(&lease_for_completion);
            let control_handle = control_handle.clone();
            Arc::new(move || {
                let mut lease = lease.lock().expect("ACP activation lease lock poisoned");
                let Some(active_lease) = lease.as_ref() else {
                    return true;
                };
                if active_lease.release_if_no_pending_control_work(&control_handle) {
                    lease.take();
                    true
                } else {
                    false
                }
            }) as Arc<dyn Fn() -> bool + Send + Sync>
        });
        let on_steer_prompt = self.steering_hub.as_ref().map(|_| {
            let emitter = Arc::clone(emitter);
            let stage_scope = stage_scope.clone();
            let node_id = node.id.clone();
            let session_id = activation_session_id.clone();
            Arc::new(move |text: String, actor: Option<Principal>| {
                emitter.emit_scoped(
                    &Event::Agent {
                        stage:             node_id.clone(),
                        visit:             stage_scope.visit,
                        event:             AgentEvent::SteeringInjected { text, actor },
                        session_id:        Some(session_id.clone()),
                        parent_session_id: None,
                        tool_call_id:      None,
                    },
                    &stage_scope,
                );
            }) as Arc<dyn Fn(String, Option<Principal>) + Send + Sync>
        });

        let files_before = changed_files::detect_changed_files(sandbox).await;
        let launch_start = std::time::Instant::now();
        let result = match fabro_acp::run_acp_turn(AcpRunRequest {
            command: process_spec,
            prompt,
            cwd: sandbox.working_directory().to_string(),
            timeout_ms: node.timeout().map(crate::millis_u64),
            env: launch_env,
            sandbox: Arc::clone(sandbox),
            cancel_token: cancel_token.child_token(),
            on_activity: Some(on_activity),
            live_control: Some(AcpLiveControl {
                handle: control_handle.clone(),
                on_natural_completion,
                on_steer_prompt,
            }),
        })
        .await
        {
            Ok(result) => {
                emitter.emit_scoped(
                    &Event::AgentAcpCompleted {
                        node_id:     node.id.clone(),
                        stdout:      result.text.clone(),
                        stderr:      result.stderr.clone(),
                        stop_reason: render_stop_reason(&result.stop_reason),
                        duration_ms: result.duration_ms,
                    },
                    stage_scope,
                );
                result
            }
            Err(AcpError::Cancelled) => {
                emitter.emit_scoped(
                    &Event::AgentAcpCancelled {
                        node_id:     node.id.clone(),
                        stdout:      String::new(),
                        stderr:      String::new(),
                        duration_ms: elapsed_ms(launch_start),
                    },
                    stage_scope,
                );
                return Err(Error::Cancelled);
            }
            Err(AcpError::TimedOut { exec_output_tail }) => {
                let stderr = exec_output_tail
                    .as_ref()
                    .and_then(|tail| tail.stderr.clone())
                    .unwrap_or_default();
                emitter.emit_scoped(
                    &Event::AgentAcpTimedOut {
                        node_id:     node.id.clone(),
                        stdout:      String::new(),
                        stderr:      stderr.clone(),
                        duration_ms: elapsed_ms(launch_start),
                    },
                    stage_scope,
                );
                return Err(acp_error_to_workflow(AcpError::TimedOut {
                    exec_output_tail,
                }));
            }
            Err(AcpError::StopReason { stop_reason, text }) => {
                emitter.emit_scoped(
                    &Event::AgentAcpCompleted {
                        node_id:     node.id.clone(),
                        stdout:      text.clone(),
                        stderr:      String::new(),
                        stop_reason: stop_reason.clone(),
                        duration_ms: elapsed_ms(launch_start),
                    },
                    stage_scope,
                );
                return Err(acp_error_to_workflow(AcpError::StopReason {
                    stop_reason,
                    text,
                }));
            }
            Err(error) => return Err(acp_error_to_workflow(error)),
        };
        if let Some(lease) = lease_for_completion
            .lock()
            .expect("ACP activation lease lock poisoned")
            .take()
        {
            lease.release();
        }

        let (files_touched, last_file_touched) =
            changed_files::files_touched_since(sandbox, &files_before).await;

        Ok(CodergenResult::Text {
            text: result.text,
            usage: None,
            files_touched,
            last_file_touched,
        })
    }

    async fn resolve_launch_env(
        &self,
        emitter: &Arc<Emitter>,
    ) -> Result<HashMap<String, String>, Error> {
        let Some(provider) = &self.tool_env else {
            return Ok(HashMap::new());
        };
        if self.github_token_refresh_managed {
            emitter.notice(
                RunNoticeLevel::Info,
                RunNoticeCode::GithubTokenRefreshLimited,
                "ACP agent stages receive workflow env at process launch; stages running beyond \
                 token expiry may need to be retried.",
            );
        }
        provider
            .resolve()
            .await
            .map_err(|err| Error::handler_with_anyhow("Failed to resolve ACP agent env", err))
    }

    fn activate_control_session(
        &self,
        handle: &AcpControlHandle,
        session_id: &str,
        node: &Node,
        stage_scope: &StageScope,
        emitter: &Arc<Emitter>,
        config_name: Option<&str>,
    ) -> Result<Option<Arc<ActivationLease>>, Error> {
        let Some(steering_hub) = &self.steering_hub else {
            return Ok(None);
        };
        ActivationLease::activate(
            ActivationLeaseOptions {
                stage_id:         StageId::new(node.id.clone(), stage_scope.visit),
                session_id:       session_id.to_string(),
                thread_id:        None,
                provider:         Some(AgentBackend::Acp.to_string()),
                model:            config_name.map(str::to_string),
                reasoning_effort: None,
                speed:            None,
                capabilities:     vec![SessionCapability::Steer],
                hub:              Arc::clone(steering_hub),
                emitter:          Arc::clone(emitter),
            },
            &(Arc::new(handle.clone()) as Arc<dyn ActiveControlHandle>),
        )
        .map(Some)
    }
}

impl ActiveControlHandle for AcpControlHandle {
    fn enqueue_bounded(&self, item: SteeringItem, cap: usize) -> Option<SteeringItem> {
        let item = match item {
            SteeringItem::Steering { text, actor } => SteeringMessage::new(text, actor),
            item => return Some(item),
        };
        Self::enqueue_bounded(self, item, cap).map(SteeringItem::from)
    }

    fn interrupt(&self, actor: Option<Principal>) {
        Self::interrupt(self, actor);
    }

    fn interrupt_then_enqueue_bounded(
        &self,
        item: SteeringItem,
        cap: usize,
    ) -> Option<SteeringItem> {
        let item = match item {
            SteeringItem::Steering { text, actor } => SteeringMessage::new(text, actor),
            item => return Some(item),
        };
        Self::interrupt_then_enqueue_bounded(self, item, cap).map(SteeringItem::from)
    }

    fn has_pending_control_work(&self) -> bool {
        Self::has_pending_control_work(self)
    }
}

impl Default for AgentAcpBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CodergenBackend for AgentAcpBackend {
    async fn run(&self, request: CodergenRunRequest<'_>) -> Result<CodergenResult, Error> {
        let stage_scope = StageScope::for_handler(request.context, &request.node.id);
        self.run_turn(
            request.node,
            request.prompt.to_string(),
            request.emitter,
            &stage_scope,
            request.sandbox,
            request.cancel_token,
        )
        .await
    }

    async fn one_shot(&self, _request: OneShotRequest<'_>) -> Result<CodergenResult, Error> {
        Err(Error::Validation(
            "backend=\"acp\" is only valid on agent nodes; prompt nodes are API-only".to_string(),
        ))
    }

    fn node_timeout_policy(&self, _node: &Node) -> NodeTimeoutPolicy {
        NodeTimeoutPolicy::HandlerManaged
    }
}

fn acp_process_error_to_workflow(error: AcpCommandError) -> Error {
    match error {
        AcpCommandError::LegacyCommandAttribute => {
            Error::handler("acp_command is no longer supported; use acp.command or acp.config")
        }
        AcpCommandError::EmptyOverride => Error::handler("ACP process attribute must not be empty"),
        AcpCommandError::MissingOverride => {
            Error::handler("backend=\"acp\" requires exactly one of acp.command or acp.config")
        }
        AcpCommandError::UnsupportedTransport => {
            Error::handler("only stdio ACP commands are supported")
        }
        AcpCommandError::InvalidCommandString => {
            Error::handler("Failed to parse acp.command as a shell command")
        }
        AcpCommandError::InvalidConfigJson(source) => {
            Error::handler_with_source("Failed to parse acp.config as JSON", source)
        }
        AcpCommandError::InvalidConfigShape(message) => {
            Error::handler(format!("Invalid acp.config shape: {message}"))
        }
    }
}

fn resolve_acp_process_spec(node: &Node) -> Result<AcpProcessSpec, Error> {
    AcpProcessSpec::from_attrs(
        node.legacy_acp_command_attr(),
        node.acp_command_attr(),
        node.acp_config_attr(),
    )
    .map_err(acp_process_error_to_workflow)
}

fn acp_error_to_workflow(error: AcpError) -> Error {
    match error {
        AcpError::Cancelled => Error::Cancelled,
        AcpError::TimedOut { exec_output_tail } => {
            Error::handler_with_exec_output_tail("ACP turn timed out", exec_output_tail)
        }
        AcpError::StopReason { stop_reason, text } => {
            Error::handler(format!("ACP prompt stopped with {stop_reason}: {text}"))
        }
        AcpError::Sandbox(source) => Error::handler_with_source("ACP turn failed", source),
        other => {
            let exec_output_tail = other.exec_output_tail();
            Error::handler_with_source_and_exec_output_tail(
                "ACP turn failed",
                other,
                exec_output_tail,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use fabro_acp::test_support::fake_acp_agent_script;
    use fabro_acp::{AcpError, AcpProcessExit};
    use fabro_agent::{LocalSandbox, Sandbox, shell_quote};
    use fabro_graphviz::graph::{AttrValue, Node};
    use fabro_sandbox::test_support::MockSandbox;
    use fabro_types::{CommandTermination, EventBody, ExecOutputTail};
    use tokio_util::sync::CancellationToken;

    use super::{AgentAcpBackend, acp_error_to_workflow};
    use crate::context::Context;
    use crate::event::Emitter;
    use crate::handler::agent::{CodergenBackend, CodergenResult, CodergenRunRequest};
    use crate::steering_hub::SteeringHub;

    #[tokio::test]
    async fn acp_backend_run_sends_prompt_and_returns_text() {
        let tempdir = tempfile::tempdir().unwrap();
        init_git(tempdir.path());
        let script_path = tempdir.path().join("fake_acp_agent.py");
        tokio::fs::write(&script_path, fake_acp_agent_script())
            .await
            .unwrap();

        let mut node = Node::new("work");
        node.attrs
            .insert("backend".to_string(), AttrValue::String("acp".to_string()));
        node.attrs.insert(
            "acp.command".to_string(),
            AttrValue::String(format!(
                "python3 {}",
                shell_quote(&script_path.to_string_lossy())
            )),
        );

        let backend = AgentAcpBackend::new().with_env(HashMap::from([(
            "ACP_MODE".to_string(),
            "write_file".to_string(),
        )]));
        let sandbox: Arc<dyn Sandbox> = Arc::new(LocalSandbox::new(tempdir.path().to_path_buf()));
        let emitter = Arc::new(Emitter::default());
        let context = Context::new();
        let result = backend
            .run(CodergenRunRequest {
                node:               &node,
                prompt:             "write hello",
                context:            &context,
                thread_id:          None,
                emitter:            &emitter,
                sandbox:            &sandbox,
                tool_hooks:         None,
                cancel_token:       CancellationToken::new(),
                agent_tool_runtime: fabro_agent::AgentToolRuntime::default(),
            })
            .await
            .unwrap();

        let CodergenResult::Text {
            text,
            files_touched,
            ..
        } = result
        else {
            panic!("expected text result");
        };
        assert_eq!(text, "hello from acp");
        assert_eq!(files_touched, vec!["hello.txt"]);
    }

    #[tokio::test]
    async fn acp_backend_accepts_steer_and_incorporates_followup_result() {
        let tempdir = tempfile::tempdir().unwrap();
        init_git(tempdir.path());
        let script_path = tempdir.path().join("fake_acp_agent.py");
        tokio::fs::write(&script_path, fake_acp_agent_script())
            .await
            .unwrap();

        let mut node = Node::new("work");
        node.attrs
            .insert("backend".to_string(), AttrValue::String("acp".to_string()));
        node.attrs.insert(
            "acp.command".to_string(),
            AttrValue::String(format!(
                "python3 {}",
                shell_quote(&script_path.to_string_lossy())
            )),
        );

        let emitter = Arc::new(Emitter::default());
        let steering_hub = Arc::new(SteeringHub::new(Arc::clone(&emitter)));
        let sent = Arc::new(AtomicBool::new(false));
        let sent_for_listener = Arc::clone(&sent);
        let hub_for_listener = Arc::clone(&steering_hub);
        emitter.on_event(move |event| {
            if event.event_name() == "agent.session.activated"
                && !sent_for_listener.swap(true, Ordering::AcqRel)
            {
                hub_for_listener.deliver_steer("please revise".to_string(), None);
            }
        });

        let backend = AgentAcpBackend::new()
            .with_env(HashMap::from([(
                "ACP_MODE".to_string(),
                "steer".to_string(),
            )]))
            .with_steering_hub(steering_hub);
        let sandbox: Arc<dyn Sandbox> = Arc::new(LocalSandbox::new(tempdir.path().to_path_buf()));
        let context = Context::new();
        let result = backend
            .run(CodergenRunRequest {
                node:               &node,
                prompt:             "write hello",
                context:            &context,
                thread_id:          None,
                emitter:            &emitter,
                sandbox:            &sandbox,
                tool_hooks:         None,
                cancel_token:       CancellationToken::new(),
                agent_tool_runtime: fabro_agent::AgentToolRuntime::default(),
            })
            .await
            .unwrap();

        let CodergenResult::Text { text, .. } = result else {
            panic!("expected text result");
        };
        assert_eq!(text, "initial steered:please revise");
    }

    #[tokio::test]
    async fn acp_backend_accepts_acp_command_attribute_without_model_or_provider() {
        let tempdir = tempfile::tempdir().unwrap();
        init_git(tempdir.path());
        let script_path = tempdir.path().join("fake_acp_agent.py");
        tokio::fs::write(&script_path, fake_acp_agent_script())
            .await
            .unwrap();

        let mut node = Node::new("work");
        node.attrs
            .insert("backend".to_string(), AttrValue::String("acp".to_string()));
        node.attrs.insert(
            "acp.command".to_string(),
            AttrValue::String(format!(
                "python3 {}",
                shell_quote(&script_path.to_string_lossy())
            )),
        );

        let backend = AgentAcpBackend::new().with_env(HashMap::from([(
            "ACP_MODE".to_string(),
            "write_file".to_string(),
        )]));
        let sandbox: Arc<dyn Sandbox> = Arc::new(LocalSandbox::new(tempdir.path().to_path_buf()));
        let emitter = Arc::new(Emitter::default());
        let context = Context::new();
        let result = backend
            .run(CodergenRunRequest {
                node:               &node,
                prompt:             "write hello",
                context:            &context,
                thread_id:          None,
                emitter:            &emitter,
                sandbox:            &sandbox,
                tool_hooks:         None,
                cancel_token:       CancellationToken::new(),
                agent_tool_runtime: fabro_agent::AgentToolRuntime::default(),
            })
            .await
            .unwrap();

        let CodergenResult::Text { text, .. } = result else {
            panic!("expected text result");
        };
        assert_eq!(text, "hello from acp");
    }

    #[tokio::test]
    async fn acp_backend_does_not_forward_provider_credentials() {
        let mut sandbox = MockSandbox::linux();
        sandbox.stdio_process_error = Some("stop before ACP handshake".to_string());
        let sandbox = Arc::new(sandbox);
        let sandbox_dyn: Arc<dyn Sandbox> = sandbox.clone();

        let mut node = Node::new("work");
        node.attrs
            .insert("backend".to_string(), AttrValue::String("acp".to_string()));
        node.attrs.insert(
            "acp.command".to_string(),
            AttrValue::String("fake-acp-agent".to_string()),
        );

        let backend = AgentAcpBackend::new();
        let emitter = Arc::new(Emitter::default());
        let context = Context::new();
        let result = backend
            .run(CodergenRunRequest {
                node:               &node,
                prompt:             "write hello",
                context:            &context,
                thread_id:          None,
                emitter:            &emitter,
                sandbox:            &sandbox_dyn,
                tool_hooks:         None,
                cancel_token:       CancellationToken::new(),
                agent_tool_runtime: fabro_agent::AgentToolRuntime::default(),
            })
            .await;
        assert!(result.is_err());

        let captured = sandbox
            .captured_env_vars
            .lock()
            .expect("captured env lock poisoned")
            .clone()
            .unwrap_or_default();
        assert!(!captured.contains_key("OPENAI_API_KEY"));
        assert!(!captured.contains_key("ANTHROPIC_API_KEY"));
        assert!(!captured.contains_key("GEMINI_API_KEY"));
    }

    #[tokio::test]
    async fn acp_backend_cancelled_stop_reason_maps_to_cancelled_error() {
        let tempdir = tempfile::tempdir().unwrap();
        let script_path = tempdir.path().join("fake_acp_agent.py");
        tokio::fs::write(&script_path, fake_acp_agent_script())
            .await
            .unwrap();

        let mut node = Node::new("work");
        node.attrs.insert(
            "acp.command".to_string(),
            AttrValue::String(format!(
                "python3 {}",
                shell_quote(&script_path.to_string_lossy())
            )),
        );

        let backend = AgentAcpBackend::new().with_env(HashMap::from([(
            "ACP_STOP_REASON".to_string(),
            "cancelled".to_string(),
        )]));
        let sandbox: Arc<dyn Sandbox> = Arc::new(LocalSandbox::new(tempdir.path().to_path_buf()));
        let emitter = Arc::new(Emitter::default());
        let context = Context::new();
        let result = backend
            .run(CodergenRunRequest {
                node:               &node,
                prompt:             "cancel",
                context:            &context,
                thread_id:          None,
                emitter:            &emitter,
                sandbox:            &sandbox,
                tool_hooks:         None,
                cancel_token:       CancellationToken::new(),
                agent_tool_runtime: fabro_agent::AgentToolRuntime::default(),
            })
            .await;
        let Err(err) = result else {
            panic!("expected cancellation error");
        };

        assert!(matches!(err, crate::error::Error::Cancelled));
    }

    #[tokio::test]
    async fn acp_started_event_omits_json_command_env_values() {
        let tempdir = tempfile::tempdir().unwrap();
        let script_path = tempdir.path().join("fake_acp_agent.py");
        tokio::fs::write(&script_path, fake_acp_agent_script())
            .await
            .unwrap();

        let raw_command = serde_json::json!({
            "type": "stdio",
            "name": "fake",
            "command": "python3",
            "args": [script_path.to_string_lossy()],
            "env": [
                {"name": "OPENAI_API_KEY", "value": "secret-key"}
            ],
        })
        .to_string();
        let mut node = Node::new("work");
        node.attrs
            .insert("backend".to_string(), AttrValue::String("acp".to_string()));
        node.attrs
            .insert("acp.config".to_string(), AttrValue::String(raw_command));

        let backend = AgentAcpBackend::new();
        let sandbox: Arc<dyn Sandbox> = Arc::new(LocalSandbox::new(tempdir.path().to_path_buf()));
        let emitter = Arc::new(Emitter::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        emitter.on_event({
            let events = Arc::clone(&events);
            move |event| events.lock().unwrap().push(event.clone())
        });

        let context = Context::new();
        backend
            .run(CodergenRunRequest {
                node:               &node,
                prompt:             "write hello",
                context:            &context,
                thread_id:          None,
                emitter:            &emitter,
                sandbox:            &sandbox,
                tool_hooks:         None,
                cancel_token:       CancellationToken::new(),
                agent_tool_runtime: fabro_agent::AgentToolRuntime::default(),
            })
            .await
            .unwrap();

        let events = events.lock().unwrap();
        let command = events
            .iter()
            .find_map(|event| match &event.body {
                EventBody::AgentAcpStarted(props) => Some(props.command.as_str()),
                _ => None,
            })
            .expect("ACP started event should be emitted");
        assert!(command.contains("python3"));
        assert!(command.contains("fake_acp_agent.py"));
        assert!(!command.contains("OPENAI_API_KEY"));
        assert!(!command.contains("secret-key"));
    }

    #[tokio::test]
    async fn acp_backend_requires_explicit_process_attr() {
        let sandbox = MockSandbox::linux();
        let sandbox = Arc::new(sandbox);
        let sandbox_dyn: Arc<dyn Sandbox> = sandbox.clone();

        let mut node = Node::new("work");
        node.attrs
            .insert("backend".to_string(), AttrValue::String("acp".to_string()));

        let backend = AgentAcpBackend::new();
        let emitter = Arc::new(Emitter::default());
        let context = Context::new();
        let result = backend
            .run(CodergenRunRequest {
                node:               &node,
                prompt:             "write hello",
                context:            &context,
                thread_id:          None,
                emitter:            &emitter,
                sandbox:            &sandbox_dyn,
                tool_hooks:         None,
                cancel_token:       CancellationToken::new(),
                agent_tool_runtime: fabro_agent::AgentToolRuntime::default(),
            })
            .await;
        let Err(err) = result else {
            panic!("ACP without process attr should fail");
        };
        assert!(
            err.to_string()
                .contains("requires exactly one of acp.command or acp.config")
        );
        assert!(
            sandbox
                .captured_env_vars
                .lock()
                .expect("captured env lock poisoned")
                .is_none(),
            "ACP process should not launch when process attr is missing"
        );
    }

    #[tokio::test]
    async fn acp_backend_stdio_spawn_failure_preserves_sandbox_cause() {
        const DAYTONA_UNSUPPORTED_ACP: &str = "ACP backend requires bidirectional stdio; the Daytona sandbox provider does not support it yet";

        let mut sandbox = MockSandbox::linux();
        sandbox.stdio_process_error = Some(DAYTONA_UNSUPPORTED_ACP.to_string());
        let sandbox = Arc::new(sandbox);
        let sandbox_dyn: Arc<dyn Sandbox> = sandbox.clone();

        let mut node = Node::new("work");
        node.attrs
            .insert("backend".to_string(), AttrValue::String("acp".to_string()));
        node.attrs.insert(
            "acp.command".to_string(),
            AttrValue::String("fake-acp-agent".to_string()),
        );

        let backend = AgentAcpBackend::new().with_env(HashMap::from([(
            "WORKFLOW_ENV".to_string(),
            "test-value".to_string(),
        )]));
        let emitter = Arc::new(Emitter::default());
        let context = Context::new();
        let result = backend
            .run(CodergenRunRequest {
                node:               &node,
                prompt:             "write hello",
                context:            &context,
                thread_id:          None,
                emitter:            &emitter,
                sandbox:            &sandbox_dyn,
                tool_hooks:         None,
                cancel_token:       CancellationToken::new(),
                agent_tool_runtime: fabro_agent::AgentToolRuntime::default(),
            })
            .await;
        let Err(err) = result else {
            panic!("stdio spawn failure should fail the ACP turn");
        };

        let rendered = err.display_with_causes();
        assert!(
            rendered.contains("ACP turn failed"),
            "rendered error should keep ACP context: {rendered}"
        );
        assert!(
            err.causes()
                .iter()
                .any(|cause| cause == DAYTONA_UNSUPPORTED_ACP),
            "cause chain should include sandbox failure, got: {rendered}"
        );
        assert_eq!(
            err.failure_category(),
            crate::error::FailureCategory::Deterministic
        );
    }

    #[test]
    fn acp_timeout_maps_stderr_to_exec_tail_not_message() {
        let tail = ExecOutputTail {
            stdout:           None,
            stderr:           Some("redacted stderr tail".to_string()),
            stdout_truncated: false,
            stderr_truncated: true,
        };
        let err = acp_error_to_workflow(AcpError::TimedOut {
            exec_output_tail: Some(tail.clone()),
        });

        let detail = err.to_failure_detail();
        assert_eq!(detail.message, "ACP turn timed out");
        assert!(detail.causes.is_empty());
        assert_eq!(detail.exec_output_tail, Some(tail));
    }

    #[test]
    fn acp_process_exit_maps_stderr_to_exec_tail_not_cause_text() {
        let tail = ExecOutputTail {
            stdout:           None,
            stderr:           Some("early boom".to_string()),
            stdout_truncated: false,
            stderr_truncated: false,
        };
        let err = acp_error_to_workflow(AcpError::ProcessExited(AcpProcessExit {
            termination:      CommandTermination::Exited,
            exit_code:        Some(2),
            exec_output_tail: Some(tail.clone()),
        }));

        let detail = err.to_failure_detail();
        assert_eq!(detail.message, "ACP turn failed");
        assert_eq!(detail.exec_output_tail, Some(tail));
        assert!(
            detail
                .causes
                .iter()
                .any(|cause| cause.contains("exit_code=2")),
            "cause chain should retain process exit context: {:?}",
            detail.causes
        );
        assert!(
            !detail
                .causes
                .iter()
                .any(|cause| cause.contains("early boom")),
            "raw stderr belongs in exec_output_tail, not causes: {:?}",
            detail.causes
        );
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "unit test initializes an isolated git repository with the system git binary"
    )]
    fn init_git(path: &std::path::Path) {
        let output = std::process::Command::new("git")
            .arg("init")
            .current_dir(path)
            .output()
            .unwrap();
        assert!(output.status.success());
    }
}
