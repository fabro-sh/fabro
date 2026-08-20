//! Workflow adapter for ACP-backed LLM stages.

use std::collections::HashMap;
use std::env;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use fabro_acp::{
    AcpCommandError, AcpControlHandle, AcpError, AcpLiveControl, AcpProcessSpec, AcpRunRequest,
    render_stop_reason,
};
use fabro_agent::{
    AgentEvent, RefreshOutcome, Sandbox, StaticEnvProvider, SteeringItem, ToolEnvProvider,
};
use fabro_graphviz::graph::Node;
use fabro_static::EnvVars;
use fabro_types::{
    AgentBackend, Principal, SessionCapability, StageId, StageTiming, SteeringMessage,
};
use fabro_util::time::elapsed_ms;
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;

use super::super::agent::{CodergenBackend, CodergenResult, CodergenRunRequest, OneShotRequest};
use super::activation_lease::{ActivationLease, ActivationLeaseOptions};
use super::changed_files;
use crate::error::Error;
use crate::event::{Emitter, Event, RunNoticeCode, RunNoticeLevel, StageScope};
use crate::handler::NodeTimeoutPolicy;
use crate::steering_hub::{ActiveControlHandle, SteeringHub};

/// Default refresh-ahead interval — comfortably under the ~60-min GitHub App
/// installation-token TTL.
const REFRESH_INTERVAL_DEFAULT: Duration = Duration::from_mins(45);
/// Upper bound on a single push-credential refresh (token mint + `git remote
/// set-url` exec). The turn-entry refresh runs before the ACP process spawns
/// and the ACP node uses `NodeTimeoutPolicy::HandlerManaged`, so without this
/// bound a stalled GitHub API call would hang node entry indefinitely.
const REFRESH_MINT_TIMEOUT: Duration = Duration::from_secs(30);

/// Aborts the wrapped task when dropped, bounding the refresh-ahead loop to the
/// lifetime of a single ACP turn.
struct AbortOnDrop(JoinHandle<()>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Process-env lookup facade for the `FABRO_PUSH_CRED_REFRESH_*` tunables,
/// isolated so the single disallowed-methods exception is documented in one
/// place. Variable names come from [`EnvVars`].
#[expect(
    clippy::disallowed_methods,
    reason = "Documented process-env facade for the FABRO_PUSH_CRED_REFRESH_* tunables; names come from fabro_static::EnvVars."
)]
fn refresh_env(name: &str) -> Option<String> {
    env::var(name).ok()
}

/// Whether the push-credential refresh feature is enabled. Default ON; disabled
/// by a falsy value (empty / `0` / `false` / `off` / `no`, case-insensitive),
/// matching the repo's env-flag convention.
fn parse_refresh_enabled(raw: Option<&str>) -> bool {
    !matches!(
        raw.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Some("" | "0" | "false" | "off" | "no")
    )
}

/// Parse the refresh-ahead loop interval. `None` disables the loop (explicit
/// `0`, mirroring the codebase's `set_autostop_interval` "0 to disable"
/// convention). Unset/empty or an unparsable value falls back to the default.
fn parse_refresh_interval(raw: Option<&str>) -> Option<Duration> {
    match raw.map(str::trim) {
        None | Some("") => Some(REFRESH_INTERVAL_DEFAULT),
        Some(s) => match s.parse::<u64>() {
            Ok(0) => None,
            Ok(secs) => Some(Duration::from_secs(secs)),
            Err(_) => {
                tracing::warn!(
                    value = %s,
                    "invalid FABRO_PUSH_CRED_REFRESH_INTERVAL_SECONDS; using default"
                );
                Some(REFRESH_INTERVAL_DEFAULT)
            }
        },
    }
}

fn push_cred_refresh_enabled() -> bool {
    parse_refresh_enabled(refresh_env(EnvVars::FABRO_PUSH_CRED_REFRESH_AHEAD).as_deref())
}

fn push_cred_refresh_interval() -> Option<Duration> {
    parse_refresh_interval(
        refresh_env(EnvVars::FABRO_PUSH_CRED_REFRESH_INTERVAL_SECONDS).as_deref(),
    )
}

/// Background loop that re-mints the sandbox's push credentials every
/// `interval` for the duration of one ACP turn, so a single turn that outlives
/// the installation-token TTL still pushes with a fresh token. Bounded by
/// `cancel` (the drop-guard cancels it at turn end). A failed or timed-out tick
/// retries after a shorter delay so a transient error does not leave a
/// longer-than-interval window with an expired token.
async fn refresh_ahead_loop(
    sandbox: Arc<dyn Sandbox>,
    cancel: CancellationToken,
    interval: Duration,
) {
    let retry_delay = interval.min(Duration::from_mins(1));
    let mut delay = interval;
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            () = sleep(delay) => {
                match timeout(REFRESH_MINT_TIMEOUT, sandbox.refresh_push_credentials())
                    .await
                {
                    Ok(Ok(RefreshOutcome::Refreshed)) => {
                        tracing::info!(
                            interval_secs = interval.as_secs(),
                            "refresh-ahead re-minted push credentials mid-turn"
                        );
                        delay = interval;
                    }
                    Ok(Ok(RefreshOutcome::Skipped)) => {
                        tracing::debug!(
                            interval_secs = interval.as_secs(),
                            "refresh-ahead tick: no managed push credentials to refresh"
                        );
                        delay = interval;
                    }
                    Ok(Err(e)) => {
                        tracing::warn!(
                            error = %fabro_sandbox::display_for_log(&e),
                            "refresh-ahead mid-turn refresh failed; retrying sooner"
                        );
                        delay = retry_delay;
                    }
                    Err(_elapsed) => {
                        tracing::warn!(
                            timeout_secs = REFRESH_MINT_TIMEOUT.as_secs(),
                            "refresh-ahead mid-turn refresh timed out; retrying sooner"
                        );
                        delay = retry_delay;
                    }
                }
            }
        }
    }
}

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

        // Keep the sandbox's push credentials fresh for the duration of this ACP
        // turn so the agent's own `git push` uses a live token instead of the one
        // baked into the clone at run start.
        //
        // Part 2 (turn-entry): re-mint + rewrite the origin URL before the ACP
        // process spawns, covering a push early in the turn. Non-fatal and
        // timeout-bounded — a stalled mint must neither fail nor hang node entry.
        // Part 3 (loop): a background task re-mints every ~45 min so a single turn
        // that itself outlives the ~60-min installation-token TTL still pushes
        // with a fresh token; a normal sub-interval turn never ticks (the
        // drop-guard aborts the task at turn end before the first tick).
        //
        // FABRO_PUSH_CRED_REFRESH_AHEAD=0 (or false/off/no/empty, case-
        // insensitive) disables the WHOLE feature — turn-entry re-mint AND loop —
        // so an operator who manages `origin` themselves can opt out of all
        // fabro-side origin rewriting. FABRO_PUSH_CRED_REFRESH_INTERVAL_SECONDS
        // overrides the loop interval; 0 disables just the loop.
        //
        // Known limitations tracked as follow-ups (not addressed here): (a)
        // resumed/parked runs reconnect the sandbox with no GitHub App creds, so
        // refresh no-ops until those creds are threaded through the reconnect
        // path; (b) the turn-entry re-mint has no freshness check, so it mints
        // once per node entry even when the current token is still fresh; (c) the
        // background `git remote set-url` can contend with the agent's own git on
        // `.git/config.lock`; (d) parallel ACP branches each run their own loop;
        // (e) this refresh lives in the ACP handler only, though the stale-origin
        // problem is stage-type-agnostic (native/command stages that push are not
        // covered); (f) refresh failures are logged via tracing but not surfaced
        // as a RunNotice event on the run stream.
        let refresh_enabled = push_cred_refresh_enabled();
        if refresh_enabled {
            match timeout(REFRESH_MINT_TIMEOUT, sandbox.refresh_push_credentials()).await {
                Ok(Ok(RefreshOutcome::Refreshed)) => {
                    tracing::debug!("refreshed sandbox push credentials at ACP turn entry");
                }
                Ok(Ok(RefreshOutcome::Skipped)) => {}
                Ok(Err(e)) => {
                    tracing::warn!(
                        error = %fabro_sandbox::display_for_log(&e),
                        "node-entry push-credential refresh failed (non-fatal)"
                    );
                }
                Err(_elapsed) => {
                    tracing::warn!(
                        timeout_secs = REFRESH_MINT_TIMEOUT.as_secs(),
                        "node-entry push-credential refresh timed out (non-fatal)"
                    );
                }
            }
        }
        let _refresh_ahead_guard: Option<AbortOnDrop> = refresh_enabled
            .then(push_cred_refresh_interval)
            .flatten()
            .map(|interval| {
                AbortOnDrop(tokio::spawn(refresh_ahead_loop(
                    Arc::clone(sandbox),
                    cancel_token.child_token(),
                    interval,
                )))
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
            timing: StageTiming::active_only(result.duration_ms, 0),
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
                permission_level: None,
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
        if request.node.output_schema().is_some() {
            return Err(Error::Validation(
                "output_schema is not supported with backend=\"acp\" in this release".to_string(),
            ));
        }
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
    use std::time::Duration;

    use fabro_acp::test_support::fake_acp_agent_script;
    use fabro_acp::{AcpError, AcpProcessExit};
    use fabro_agent::{LocalSandbox, RefreshOutcome, Sandbox, shell_quote};
    use fabro_graphviz::graph::{AttrValue, Node};
    use fabro_sandbox::test_support::MockSandbox;
    use fabro_types::{CommandTermination, EventBody, ExecOutputTail};
    use tokio_util::sync::CancellationToken;

    use super::{
        AgentAcpBackend, acp_error_to_workflow, parse_refresh_enabled, parse_refresh_interval,
    };
    use crate::context::Context;
    use crate::event::Emitter;
    use crate::handler::agent::{CodergenBackend, CodergenResult, CodergenRunRequest};
    use crate::steering_hub::SteeringHub;

    #[test]
    fn refresh_enabled_defaults_on_and_honors_falsy_values() {
        // Default ON when unset.
        assert!(parse_refresh_enabled(None));
        // Truthy / non-falsy values stay enabled.
        for v in ["1", "true", "on", "yes", "anything"] {
            assert!(parse_refresh_enabled(Some(v)), "{v} should be enabled");
        }
        // Falsy values disable — case-insensitive, and empty/whitespace counts.
        for v in [
            "0", "false", "off", "no", "FALSE", "Off", "No", "OFF", "", "  ",
        ] {
            assert!(!parse_refresh_enabled(Some(v)), "{v} should be disabled");
        }
    }

    #[test]
    fn refresh_interval_parses_default_disable_and_override() {
        // Unset or empty → default.
        assert_eq!(parse_refresh_interval(None), Some(Duration::from_mins(45)));
        assert_eq!(
            parse_refresh_interval(Some("  ")),
            Some(Duration::from_mins(45))
        );
        // Explicit 0 disables the loop.
        assert_eq!(parse_refresh_interval(Some("0")), None);
        // A positive value overrides.
        assert_eq!(
            parse_refresh_interval(Some("1800")),
            Some(Duration::from_mins(30))
        );
        assert_eq!(
            parse_refresh_interval(Some(" 900 ")),
            Some(Duration::from_mins(15))
        );
        // Unparsable → default (never panics).
        for v in ["15m", "-1", "abc", "9999999999999999999999"] {
            assert_eq!(
                parse_refresh_interval(Some(v)),
                Some(Duration::from_mins(45)),
                "{v} should fall back to default"
            );
        }
    }

    #[tokio::test]
    async fn refresh_reports_skipped_without_managed_credentials() {
        // MockSandbox uses the trait default (no GitHub App creds), so refresh is
        // a no-op that must report Skipped — the signal the refresh-ahead loop
        // relies on to log at debug rather than falsely claim a re-mint.
        let sandbox = MockSandbox::linux();
        assert_eq!(
            sandbox.refresh_push_credentials().await.unwrap(),
            RefreshOutcome::Skipped
        );
    }

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
    async fn acp_backend_rejects_output_schema_without_launching_process() {
        let tempdir = tempfile::tempdir().unwrap();
        let launched_path = tempdir.path().join("launched");

        let mut node = Node::new("work");
        node.attrs
            .insert("backend".to_string(), AttrValue::String("acp".to_string()));
        node.attrs.insert(
            "acp.command".to_string(),
            AttrValue::String("sh -c 'touch launched'".to_string()),
        );
        node.attrs.insert(
            "output_schema".to_string(),
            AttrValue::String("routing".to_string()),
        );

        let backend = AgentAcpBackend::new();
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
            .await;

        let Err(error) = result else {
            panic!("expected output_schema guardrail error");
        };
        assert!(
            error
                .to_string()
                .contains("output_schema is not supported with backend=\"acp\" in this release"),
            "unexpected error: {error}",
        );
        assert!(
            !launched_path.exists(),
            "ACP process should not launch when output_schema is present",
        );
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
