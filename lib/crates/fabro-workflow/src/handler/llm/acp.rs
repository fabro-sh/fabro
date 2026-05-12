//! Workflow adapter for ACP-backed LLM stages.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use fabro_acp::{
    AcpCommandError, AcpError, AcpRunRequest, render_stop_reason, resolve_acp_command,
};
use fabro_agent::{Sandbox, StaticEnvProvider, ToolEnvProvider};
use fabro_auth::CredentialResolver;
use fabro_graphviz::graph::Node;
use fabro_model::Provider;
use fabro_util::time::elapsed_ms;
use tokio_util::sync::CancellationToken;

use super::super::agent::{CodergenBackend, CodergenResult, CodergenRunRequest, OneShotRequest};
use super::changed_files;
use super::cli::AgentCli;
use super::launch_env::{AgentLaunchEnvRequest, resolve_agent_launch_env};
use crate::error::Error;
use crate::event::{Emitter, Event, StageScope};

pub struct AgentAcpBackend {
    model: String,
    provider: Provider,
    tool_env: Option<Arc<dyn ToolEnvProvider>>,
    github_token_refresh_managed: bool,
    resolver: Option<CredentialResolver>,
}

impl AgentAcpBackend {
    #[must_use]
    pub fn new(model: String, provider: Provider, resolver: CredentialResolver) -> Self {
        Self {
            model,
            provider,
            tool_env: None,
            github_token_refresh_managed: false,
            resolver: Some(resolver),
        }
    }

    #[must_use]
    pub fn new_from_env(model: String, provider: Provider) -> Self {
        Self {
            model,
            provider,
            tool_env: None,
            github_token_refresh_managed: false,
            resolver: None,
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

    async fn run_turn(
        &self,
        node: &Node,
        prompt: String,
        emitter: &Arc<Emitter>,
        stage_scope: &StageScope,
        sandbox: &Arc<dyn Sandbox>,
        cancel_token: CancellationToken,
    ) -> Result<CodergenResult, Error> {
        let files_before = changed_files::detect_changed_files(sandbox).await;
        let model = node.model().unwrap_or(&self.model);
        let provider = node
            .provider()
            .and_then(|value| value.parse::<Provider>().ok())
            .unwrap_or(self.provider);
        let command =
            resolve_acp_command(node.acp_command()).map_err(acp_command_error_to_workflow)?;

        let launch_env = resolve_agent_launch_env(AgentLaunchEnvRequest {
            provider,
            cli: AgentCli::for_provider(provider),
            resolver: self.resolver.as_ref(),
            tool_env: self.tool_env.as_ref(),
            github_token_refresh_managed: self.github_token_refresh_managed,
            stage_label: "ACP",
            emitter,
            sandbox,
            cancel_token: &cancel_token,
        })
        .await?;
        let on_activity = {
            let emitter = Arc::clone(emitter);
            Arc::new(move || emitter.touch()) as Arc<dyn Fn() + Send + Sync>
        };

        let command_display = command.to_string();
        emitter.emit_scoped(
            &Event::AgentAcpStarted {
                node_id:  node.id.clone(),
                visit:    stage_scope.visit,
                mode:     "acp".to_string(),
                provider: provider.to_string(),
                model:    model.to_string(),
                command:  command_display,
            },
            stage_scope,
        );

        let launch_start = std::time::Instant::now();
        let result = match fabro_acp::run_acp_turn(AcpRunRequest {
            command,
            prompt,
            cwd: sandbox.working_directory().to_string(),
            timeout_ms: node.timeout().map(crate::millis_u64),
            env: launch_env,
            sandbox: Arc::clone(sandbox),
            cancel_token: cancel_token.child_token(),
            on_activity: Some(on_activity),
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
            Err(AcpError::TimedOut { stderr }) => {
                emitter.emit_scoped(
                    &Event::AgentAcpTimedOut {
                        node_id:     node.id.clone(),
                        stdout:      String::new(),
                        stderr:      stderr.clone(),
                        duration_ms: elapsed_ms(launch_start),
                    },
                    stage_scope,
                );
                return Err(acp_error_to_workflow(AcpError::TimedOut { stderr }));
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

        let (files_touched, last_file_touched) =
            changed_files::files_touched_since(sandbox, &files_before).await;

        Ok(CodergenResult::Text {
            text: result.text,
            usage: None,
            files_touched,
            last_file_touched,
        })
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

    async fn one_shot(&self, request: OneShotRequest<'_>) -> Result<CodergenResult, Error> {
        let prompt = match request.system_prompt.filter(|prompt| !prompt.is_empty()) {
            Some(system_prompt) => format!("System:\n{system_prompt}\n\nUser:\n{}", request.prompt),
            None => request.prompt.to_string(),
        };
        self.run_turn(
            request.node,
            prompt,
            request.emitter,
            request.stage_scope,
            request.sandbox,
            request.cancel_token,
        )
        .await
    }
}

fn acp_command_error_to_workflow(error: AcpCommandError) -> Error {
    match error {
        AcpCommandError::EmptyOverride => Error::handler("acp_command must not be empty"),
        AcpCommandError::MissingOverride => Error::handler(
            "acp_command is required for backend=\"acp\" because Fabro does not install ACP agents",
        ),
        AcpCommandError::UnsupportedTransport => {
            Error::handler("only stdio ACP commands are supported")
        }
        AcpCommandError::Parse(source) => {
            Error::handler_with_source("Failed to resolve ACP command", &source)
        }
    }
}

fn acp_error_to_workflow(error: AcpError) -> Error {
    match error {
        AcpError::Cancelled => Error::Cancelled,
        AcpError::TimedOut { stderr } => {
            if stderr.is_empty() {
                Error::handler("ACP turn timed out")
            } else {
                Error::handler(format!("ACP turn timed out: {stderr}"))
            }
        }
        AcpError::StopReason { stop_reason, text } => {
            Error::handler(format!("ACP prompt stopped with {stop_reason}: {text}"))
        }
        AcpError::Sandbox(source) => Error::handler_with_source("ACP turn failed", &source),
        other => Error::handler_with_source("ACP turn failed", &other),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use fabro_acp::test_support::fake_acp_agent_script;
    use fabro_agent::{LocalSandbox, Sandbox, shell_quote};
    use fabro_graphviz::graph::{AttrValue, Node};
    use fabro_model::Provider;
    use fabro_sandbox::test_support::MockSandbox;
    use fabro_types::EventBody;
    use tokio_util::sync::CancellationToken;

    use super::AgentAcpBackend;
    use crate::context::Context;
    use crate::event::{Emitter, StageScope};
    use crate::handler::agent::{
        CodergenBackend, CodergenResult, CodergenRunRequest, OneShotRequest,
    };

    #[tokio::test]
    async fn acp_backend_run_sends_prompt_and_returns_text() {
        let tempdir = tempfile::tempdir().unwrap();
        init_git(tempdir.path());
        let script_path = tempdir.path().join("fake_acp_agent.py");
        tokio::fs::write(&script_path, fake_acp_agent_script())
            .await
            .unwrap();

        let mut node = Node::new("work");
        node.attrs.insert(
            "provider".to_string(),
            AttrValue::String("openai".to_string()),
        );
        node.attrs.insert(
            "model".to_string(),
            AttrValue::String("fake-acp".to_string()),
        );
        node.attrs
            .insert("backend".to_string(), AttrValue::String("acp".to_string()));
        node.attrs.insert(
            "acp_command".to_string(),
            AttrValue::String(format!(
                "python3 {}",
                shell_quote(&script_path.to_string_lossy())
            )),
        );

        let backend =
            AgentAcpBackend::new_from_env("fake-acp".to_string(), Provider::OpenAi).with_env(
                HashMap::from([("ACP_MODE".to_string(), "write_file".to_string())]),
            );
        let sandbox: Arc<dyn Sandbox> = Arc::new(LocalSandbox::new(tempdir.path().to_path_buf()));
        let emitter = Arc::new(Emitter::default());
        let context = Context::new();
        let result = backend
            .run(CodergenRunRequest {
                node:         &node,
                prompt:       "write hello",
                context:      &context,
                thread_id:    None,
                emitter:      &emitter,
                sandbox:      &sandbox,
                tool_hooks:   None,
                cancel_token: CancellationToken::new(),
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
    async fn acp_backend_one_shot_combines_system_prompt_and_uses_passed_sandbox() {
        let tempdir = tempfile::tempdir().unwrap();
        let script_path = tempdir.path().join("fake_acp_agent.py");
        let prompt_record_path = tempdir.path().join("prompt.json");
        tokio::fs::write(&script_path, fake_acp_agent_script())
            .await
            .unwrap();

        let mut node = Node::new("prompt");
        node.attrs.insert(
            "provider".to_string(),
            AttrValue::String("openai".to_string()),
        );
        node.attrs
            .insert("backend".to_string(), AttrValue::String("acp".to_string()));
        node.attrs.insert(
            "acp_command".to_string(),
            AttrValue::String(format!(
                "python3 {}",
                shell_quote(&script_path.to_string_lossy())
            )),
        );

        let backend = AgentAcpBackend::new_from_env("fake-acp".to_string(), Provider::OpenAi)
            .with_env(HashMap::from([
                (
                    "ACP_PROMPT_RECORD".to_string(),
                    prompt_record_path.to_string_lossy().into_owned(),
                ),
                ("ACP_MODE".to_string(), "write_file".to_string()),
            ]));
        let sandbox: Arc<dyn Sandbox> = Arc::new(LocalSandbox::new(tempdir.path().to_path_buf()));
        let emitter = Arc::new(Emitter::default());
        let context = Context::new();
        let stage_scope = StageScope::for_handler(&context, "prompt");
        let result = backend
            .one_shot(OneShotRequest {
                node:          &node,
                prompt:        "User prompt",
                system_prompt: Some("System prompt"),
                emitter:       &emitter,
                stage_scope:   &stage_scope,
                sandbox:       &sandbox,
                cancel_token:  CancellationToken::new(),
            })
            .await
            .unwrap();

        assert!(matches!(result, CodergenResult::Text { .. }));
        let recorded = tokio::fs::read_to_string(prompt_record_path).await.unwrap();
        assert!(recorded.contains("System:\\nSystem prompt\\n\\nUser:\\nUser prompt"));
        assert_eq!(
            tokio::fs::read_to_string(tempdir.path().join("hello.txt"))
                .await
                .unwrap(),
            "hello from sandbox\n"
        );
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
            "provider".to_string(),
            AttrValue::String("openai".to_string()),
        );
        node.attrs.insert(
            "acp_command".to_string(),
            AttrValue::String(format!(
                "python3 {}",
                shell_quote(&script_path.to_string_lossy())
            )),
        );

        let backend =
            AgentAcpBackend::new_from_env("fake-acp".to_string(), Provider::OpenAi).with_env(
                HashMap::from([("ACP_STOP_REASON".to_string(), "cancelled".to_string())]),
            );
        let sandbox: Arc<dyn Sandbox> = Arc::new(LocalSandbox::new(tempdir.path().to_path_buf()));
        let emitter = Arc::new(Emitter::default());
        let context = Context::new();
        let result = backend
            .run(CodergenRunRequest {
                node:         &node,
                prompt:       "cancel",
                context:      &context,
                thread_id:    None,
                emitter:      &emitter,
                sandbox:      &sandbox,
                tool_hooks:   None,
                cancel_token: CancellationToken::new(),
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
        node.attrs.insert(
            "provider".to_string(),
            AttrValue::String("openai".to_string()),
        );
        node.attrs
            .insert("backend".to_string(), AttrValue::String("acp".to_string()));
        node.attrs
            .insert("acp_command".to_string(), AttrValue::String(raw_command));

        let backend = AgentAcpBackend::new_from_env("fake-acp".to_string(), Provider::OpenAi);
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
                node:         &node,
                prompt:       "write hello",
                context:      &context,
                thread_id:    None,
                emitter:      &emitter,
                sandbox:      &sandbox,
                tool_hooks:   None,
                cancel_token: CancellationToken::new(),
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
    async fn acp_backend_requires_explicit_acp_command() {
        let sandbox = MockSandbox::linux();
        let sandbox = Arc::new(sandbox);
        let sandbox_dyn: Arc<dyn Sandbox> = sandbox.clone();

        let mut node = Node::new("work");
        node.attrs.insert(
            "provider".to_string(),
            AttrValue::String("openai".to_string()),
        );
        node.attrs
            .insert("backend".to_string(), AttrValue::String("acp".to_string()));

        let backend = AgentAcpBackend::new_from_env("fake-acp".to_string(), Provider::OpenAi);
        let emitter = Arc::new(Emitter::default());
        let context = Context::new();
        let result = backend
            .run(CodergenRunRequest {
                node:         &node,
                prompt:       "write hello",
                context:      &context,
                thread_id:    None,
                emitter:      &emitter,
                sandbox:      &sandbox_dyn,
                tool_hooks:   None,
                cancel_token: CancellationToken::new(),
            })
            .await;
        let Err(err) = result else {
            panic!("ACP without acp_command should fail");
        };
        assert!(
            err.to_string()
                .contains("acp_command is required for backend=\"acp\"")
        );
        assert!(
            sandbox
                .captured_env_vars
                .lock()
                .expect("captured env lock poisoned")
                .is_none(),
            "ACP process should not launch when acp_command is missing"
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
        node.attrs.insert(
            "provider".to_string(),
            AttrValue::String("openai".to_string()),
        );
        node.attrs
            .insert("backend".to_string(), AttrValue::String("acp".to_string()));
        node.attrs.insert(
            "acp_command".to_string(),
            AttrValue::String("fake-acp-agent".to_string()),
        );

        let backend =
            AgentAcpBackend::new_from_env("fake-acp".to_string(), Provider::OpenAi).with_env(
                HashMap::from([("OPENAI_API_KEY".to_string(), "test-key".to_string())]),
            );
        let emitter = Arc::new(Emitter::default());
        let context = Context::new();
        let result = backend
            .run(CodergenRunRequest {
                node:         &node,
                prompt:       "write hello",
                context:      &context,
                thread_id:    None,
                emitter:      &emitter,
                sandbox:      &sandbox_dyn,
                tool_hooks:   None,
                cancel_token: CancellationToken::new(),
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
