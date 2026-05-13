//! CLI agent stages resolve workflow tool env once when launching the external
//! CLI process. Long-running CLI stages do not observe later GitHub
//! installation token refreshes until a future credential-helper integration
//! moves token lookup inside the child process.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fabro_agent::{Sandbox, StaticEnvProvider, ToolEnvProvider, shell_quote};
use fabro_auth::CredentialResolver;
use fabro_graphviz::graph::Node;
use fabro_llm::types::TokenCounts;
use fabro_model::catalog::LlmCatalogSettings;
use fabro_model::{Catalog, ModelRef, Provider};
use fabro_types::settings::run::RunModelControls;
use fabro_types::{CommandOutputStream, CommandTermination, LlmBackend};
use fabro_util::time::elapsed_ms;
use tokio_util::sync::CancellationToken;

/// Returns up to the last `n` characters of `s`, preserving char boundaries.
fn tail_chars(s: &str, n: usize) -> String {
    let total = s.chars().count();
    if total <= n {
        return s.to_string();
    }
    s.chars().skip(total - n).collect()
}

/// Build a "<stderr-tail>\nstdout: <stdout-tail>" detail string for CLI failure
/// messages, falling back to the original command when both streams are empty.
fn cli_failure_detail(stdout: &str, stderr: &str, command: &str) -> String {
    let stderr_tail = tail_chars(stderr, 500);
    let stdout_tail = tail_chars(stdout, 500);
    match (stderr_tail.is_empty(), stdout_tail.is_empty()) {
        (false, false) => format!("{stderr_tail}\nstdout: {stdout_tail}"),
        (false, true) => stderr_tail,
        (true, false) => format!("stdout: {stdout_tail}"),
        (true, true) => format!("command: {command}"),
    }
}

use super::super::agent::{CodergenBackend, CodergenResult, CodergenRunRequest, OneShotRequest};
use super::acp::AgentAcpBackend;
use super::api::effective_request_controls;
use super::launch_env::{AgentLaunchEnvRequest, resolve_agent_launch_env};
use super::{changed_files, routing};
use crate::error::Error;
use crate::event::{Emitter, Event, StageScope};
use crate::outcome::billed_model_usage_from_llm;

/// Maps a provider to its corresponding CLI tool metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentCli {
    Claude,
    Codex,
    Gemini,
}

impl AgentCli {
    pub fn for_provider(provider: Provider) -> Self {
        match provider {
            Provider::Anthropic => Self::Claude,
            Provider::Gemini => Self::Gemini,
            Provider::OpenAi
            | Provider::Kimi
            | Provider::Zai
            | Provider::Minimax
            | Provider::Inception
            | Provider::OpenAiCompatible => Self::Codex,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
        }
    }
}

/// Verify the provider CLI exists in the sandbox. Fabro does not install agent
/// CLIs at runtime; sandbox images or setup steps own tool installation.
async fn verify_cli_available(
    cli: AgentCli,
    sandbox: &Arc<dyn Sandbox>,
    cancel_token: &CancellationToken,
) -> Result<(), Error> {
    let cli_name = cli.name();

    let availability_check = sandbox
        .exec_command(
            &format!("PATH=\"$HOME/.local/bin:$PATH\" command -v {cli_name}"),
            30_000,
            None,
            None,
            Some(cancel_token.child_token()),
        )
        .await
        .map_err(|e| {
            Error::handler_with_source(format!("Failed to check {cli_name} availability"), e)
        })?;

    if availability_check.is_success() {
        return Ok(());
    }

    Err(Error::handler(format!(
        "CLI backend requires '{cli_name}' to be installed in the sandbox PATH. Install it in the \
         sandbox image or setup steps before running backend=\"cli\"."
    )))
}

/// Models that are only available through CLI tools (not via API).
const CLI_ONLY_MODELS: &[&str] = &[];

/// Returns true if the given model is only available through a CLI tool.
#[must_use]
pub fn is_cli_only_model(model: &str) -> bool {
    CLI_ONLY_MODELS.contains(&model)
}

/// Build the CLI command string for a given provider.
///
/// The `prompt_file` is the path to a file containing the prompt text, which
/// is piped into the command's stdin via `cat`.
#[must_use]
pub fn cli_command_for_provider(provider: Provider, model: &str, prompt_file: &str) -> String {
    let prompt_file = shell_quote(prompt_file);
    let model_flag = if model.is_empty() {
        String::new()
    } else {
        let model = shell_quote(model);
        match provider {
            Provider::OpenAi
            | Provider::Gemini
            | Provider::Kimi
            | Provider::Zai
            | Provider::Minimax
            | Provider::Inception
            | Provider::OpenAiCompatible => {
                format!(" -m {model}")
            }
            Provider::Anthropic => format!(" --model {model}"),
        }
    };
    // Use `cat | command` instead of `command < file` because the background
    // launch wrapper (`setsid sh -c '...' </dev/null`) can clobber stdin
    // redirects in nested shells. A pipe creates an explicit new stdin.
    match provider {
        // --full-auto: sandboxed auto-execution, escalates on request
        Provider::OpenAi
        | Provider::Kimi
        | Provider::Zai
        | Provider::Minimax
        | Provider::Inception
        | Provider::OpenAiCompatible => {
            format!("cat {prompt_file} | codex exec --json --full-auto{model_flag}")
        }
        // --yolo: auto-approve all tool calls
        Provider::Gemini => format!("cat {prompt_file} | gemini -o json --yolo{model_flag}"),
        // --dangerously-skip-permissions: bypass all permission checks (required for
        // non-interactive use). CLAUDECODE= unset to allow running inside a Claude Code
        // session.
        Provider::Anthropic => format!(
            "cat {prompt_file} | CLAUDECODE= claude -p --verbose --output-format stream-json --dangerously-skip-permissions{model_flag}"
        ),
    }
}

/// Parsed response from a CLI tool invocation.
#[derive(Debug)]
pub struct CliResponse {
    pub text:          String,
    pub input_tokens:  i64,
    pub output_tokens: i64,
}

/// Parse NDJSON output from Claude CLI (`--output-format stream-json`).
///
/// Looks for the last `{"type":"result",...}` line, extracts `result` text and
/// `usage`.
fn parse_claude_ndjson(output: &str) -> Option<CliResponse> {
    let mut last_result: Option<serde_json::Value> = None;

    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
            if value.get("type").and_then(|t| t.as_str()) == Some("result") {
                last_result = Some(value);
            }
        }
    }

    let result = last_result?;
    let text = result
        .get("result")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let input_tokens = result
        .pointer("/usage/input_tokens")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    let output_tokens = result
        .pointer("/usage/output_tokens")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);

    Some(CliResponse {
        text,
        input_tokens,
        output_tokens,
    })
}

/// Parse NDJSON output from Codex CLI (`codex exec --json`).
///
/// Codex emits NDJSON lines. Text comes from `item.completed` events where
/// `item.type == "agent_message"`. TokenCounts comes from the `turn.completed`
/// event.
fn parse_codex_ndjson(output: &str) -> Option<CliResponse> {
    let mut last_message_text = String::new();
    let mut input_tokens: i64 = 0;
    let mut output_tokens: i64 = 0;
    let mut found_anything = false;

    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let event_type = value.get("type").and_then(|t| t.as_str()).unwrap_or("");

        match event_type {
            "item.completed" => {
                let item_type = value
                    .pointer("/item/type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("");
                if item_type == "agent_message" {
                    if let Some(text) = value.pointer("/item/text").and_then(|t| t.as_str()) {
                        last_message_text = text.to_string();
                        found_anything = true;
                    }
                }
            }
            "turn.completed" => {
                input_tokens = value
                    .pointer("/usage/input_tokens")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(0);
                output_tokens = value
                    .pointer("/usage/output_tokens")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(0);
                found_anything = true;
            }
            _ => {}
        }
    }

    if !found_anything {
        return None;
    }

    Some(CliResponse {
        text: last_message_text,
        input_tokens,
        output_tokens,
    })
}

/// Parse JSON output from Gemini CLI (`-o json`).
///
/// Gemini outputs a single JSON object with `response` for text and
/// `stats.models.<model>.tokens` for usage.
fn parse_gemini_json(output: &str) -> Option<CliResponse> {
    let value: serde_json::Value = serde_json::from_str(output.trim()).ok()?;
    let text = value
        .get("response")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // Extract tokens from the first model in stats.models
    let (input_tokens, output_tokens) = value
        .pointer("/stats/models")
        .and_then(|m| m.as_object())
        .and_then(|models| models.values().next())
        .map_or((0, 0), |model_stats| {
            let input = model_stats
                .pointer("/tokens/input")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0);
            let output = model_stats
                .pointer("/tokens/candidates")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0);
            (input, output)
        });

    Some(CliResponse {
        text,
        input_tokens,
        output_tokens,
    })
}

/// Parse CLI output, choosing the right parser based on provider.
pub fn parse_cli_response(provider: Provider, output: &str) -> Option<CliResponse> {
    match provider {
        Provider::OpenAi
        | Provider::Kimi
        | Provider::Zai
        | Provider::Minimax
        | Provider::Inception
        | Provider::OpenAiCompatible => parse_codex_ndjson(output),
        Provider::Gemini => parse_gemini_json(output),
        Provider::Anthropic => parse_claude_ndjson(output),
    }
}

/// CLI backend that invokes external CLI tools (claude, codex, gemini) via
/// `exec_command()`.
pub struct AgentCliBackend {
    model: String,
    provider: Provider,
    tool_env: Option<Arc<dyn ToolEnvProvider>>,
    github_token_refresh_managed: bool,
    resolver: Option<CredentialResolver>,
    run_model_controls: RunModelControls,
    catalog: Arc<Catalog>,
}

impl AgentCliBackend {
    #[must_use]
    pub fn new(model: String, provider: Provider, resolver: CredentialResolver) -> Self {
        Self {
            model,
            provider,
            tool_env: None,
            github_token_refresh_managed: false,
            resolver: Some(resolver),
            run_model_controls: RunModelControls::default(),
            catalog: default_catalog(),
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
            run_model_controls: RunModelControls::default(),
            catalog: default_catalog(),
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
    pub fn with_run_model_controls(mut self, controls: RunModelControls) -> Self {
        self.run_model_controls = controls;
        self
    }

    #[must_use]
    pub fn with_catalog(mut self, catalog: Arc<Catalog>) -> Self {
        self.catalog = catalog;
        self
    }
}

fn default_catalog() -> Arc<Catalog> {
    Arc::new(
        Catalog::from_builtin_with_overrides(&LlmCatalogSettings::default())
            .expect("default catalog should build"),
    )
}

#[async_trait]
impl CodergenBackend for AgentCliBackend {
    async fn run(&self, request: CodergenRunRequest<'_>) -> Result<CodergenResult, Error> {
        let node = request.node;
        let prompt = request.prompt;
        let context = request.context;
        let emitter = request.emitter;
        let sandbox = request.sandbox;
        let cancel_token = request.cancel_token;

        // 1. Snapshot git state before the CLI run
        let files_before = changed_files::detect_changed_files(sandbox).await;

        // 2. Generate unique paths for this run
        let run_id = uuid::Uuid::new_v4().to_string();
        let tmp_prefix = format!("/tmp/fabro_cli_{run_id}");
        let prompt_path = format!("{tmp_prefix}_prompt.txt");
        let env_path = format!("{tmp_prefix}_env.sh");

        sandbox
            .write_file(&prompt_path, prompt)
            .await
            .map_err(|e| Error::handler_with_source("Failed to write prompt file", e))?;

        // 3. Build CLI command
        let model = node.model().unwrap_or(&self.model);
        let provider = node
            .provider()
            .and_then(|s| s.parse::<Provider>().ok())
            .unwrap_or(self.provider);
        let controls = effective_request_controls(
            self.catalog.as_ref(),
            &self.run_model_controls,
            model,
            node,
        )?;

        let cli = AgentCli::for_provider(provider);
        verify_cli_available(cli, sandbox, &cancel_token).await?;

        let command = cli_command_for_provider(provider, model, &prompt_path);
        let stage_scope = StageScope::for_handler(context, &node.id);
        emitter.emit_scoped(
            &Event::AgentCliStarted {
                node_id:  node.id.clone(),
                visit:    stage_scope.visit,
                mode:     "cli".to_string(),
                provider: provider.to_string(),
                model:    model.to_string(),
                command:  command.clone(),
            },
            &stage_scope,
        );

        let launch_env = resolve_agent_launch_env(AgentLaunchEnvRequest {
            provider,
            cli,
            resolver: self.resolver.as_ref(),
            tool_env: self.tool_env.as_ref(),
            github_token_refresh_managed: self.github_token_refresh_managed,
            stage_label: "CLI",
            emitter,
            sandbox,
            cancel_token: &cancel_token,
        })
        .await?;

        // Write env file so the inner shell that runs the CLI command picks up
        // PATH and provider env vars; we still pass `launch_env` to
        // `exec_command_streaming` for parity.
        let mut env_lines: Vec<String> = vec!["export PATH=\"$HOME/.local/bin:$PATH\"".to_string()];
        env_lines.extend(
            launch_env
                .iter()
                .map(|(k, v)| format!("export {k}={}", shell_quote(v))),
        );
        sandbox
            .write_file(&env_path, &env_lines.join("\n"))
            .await
            .map_err(|e| Error::handler_with_source("Failed to write env file", e))?;

        // Disable auto-stop so the sandbox stays alive during long CLI runs.
        if let Err(e) = sandbox.set_autostop_interval(0).await {
            tracing::warn!("Failed to disable sandbox auto-stop: {e}");
        }

        // Stream the CLI command directly: the previous detached `setsid &`
        // launcher could not be cancelled mid-flight. By running through
        // `exec_command_streaming` the run-level cancel token (and node
        // timeout, when set) terminate the CLI and its descendants.
        let outer_command = format!(". {} && {command}", shell_quote(&env_path));
        // Use a synchronous Mutex: each callback invocation only does a short
        // `extend_from_slice` with no awaits while the lock is held, so an
        // async Mutex would just add per-chunk scheduling overhead.
        let stdout_buffer: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let stderr_buffer: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let stdout_buf_cb = Arc::clone(&stdout_buffer);
        let stderr_buf_cb = Arc::clone(&stderr_buffer);
        let emitter_for_callback = Arc::clone(emitter);
        let output_callback: fabro_agent::CommandOutputCallback = Arc::new(move |stream, bytes| {
            let stdout_buf = Arc::clone(&stdout_buf_cb);
            let stderr_buf = Arc::clone(&stderr_buf_cb);
            let emitter = Arc::clone(&emitter_for_callback);
            Box::pin(async move {
                // Touch the stall watchdog whenever the CLI emits output
                // so long-running invocations don't trip stall timeout.
                emitter.touch();
                let buf = match stream {
                    CommandOutputStream::Stdout => stdout_buf,
                    CommandOutputStream::Stderr => stderr_buf,
                };
                buf.lock()
                    .expect("CLI output buffer mutex poisoned")
                    .extend_from_slice(&bytes);
                Ok(())
            })
        });
        let launch_env_ref = if launch_env.is_empty() {
            None
        } else {
            Some(&launch_env)
        };
        let timeout_ms = node.timeout().map(crate::millis_u64);
        let invocation_token = cancel_token.child_token();
        let launch_start = std::time::Instant::now();
        let streaming_result = sandbox
            .exec_command_streaming(
                &outer_command,
                timeout_ms,
                None,
                launch_env_ref,
                Some(invocation_token.clone()),
                output_callback,
            )
            .await;

        let cleanup_temp_files = || {
            let sandbox = Arc::clone(sandbox);
            let cleanup_cmd = format!("rm -f {}_*", shell_quote(&tmp_prefix));
            async move {
                let _ = sandbox
                    .exec_command(&cleanup_cmd, 30_000, None, None, None)
                    .await;
            }
        };

        let streaming = match streaming_result {
            Ok(streaming) => streaming,
            Err(err) => {
                cleanup_temp_files().await;
                return Err(Error::handler_with_source("Failed to run CLI command", err));
            }
        };
        let result = streaming.result;
        // Prefer the buffered streaming output (live chunks); fall back to the
        // result struct for sandboxes that bundle output at the end.
        let buffered_stdout = {
            let buf = stdout_buffer
                .lock()
                .expect("CLI stdout buffer mutex poisoned");
            String::from_utf8_lossy(&buf).into_owned()
        };
        let buffered_stderr = {
            let buf = stderr_buffer
                .lock()
                .expect("CLI stderr buffer mutex poisoned");
            String::from_utf8_lossy(&buf).into_owned()
        };
        let stdout = if buffered_stdout.is_empty() {
            result.stdout.clone()
        } else {
            buffered_stdout
        };
        let stderr = if buffered_stderr.is_empty() {
            result.stderr.clone()
        } else {
            buffered_stderr
        };
        let duration_ms = elapsed_ms(launch_start);

        match result.termination {
            CommandTermination::Cancelled => {
                emitter.emit_scoped(
                    &Event::AgentCliCancelled {
                        node_id: node.id.clone(),
                        stdout: stdout.clone(),
                        stderr: stderr.clone(),
                        duration_ms,
                    },
                    &stage_scope,
                );
                cleanup_temp_files().await;
                return Err(Error::Cancelled);
            }
            CommandTermination::TimedOut => {
                emitter.emit_scoped(
                    &Event::AgentCliTimedOut {
                        node_id: node.id.clone(),
                        stdout: stdout.clone(),
                        stderr: stderr.clone(),
                        duration_ms,
                    },
                    &stage_scope,
                );
                cleanup_temp_files().await;
                let detail = cli_failure_detail(&stdout, &stderr, &command);
                return Err(Error::handler(format!(
                    "CLI command timed out after {duration_ms} ms: {detail}"
                )));
            }
            CommandTermination::Exited => {
                emitter.emit_scoped(
                    &Event::AgentCliCompleted {
                        node_id: node.id.clone(),
                        stdout: stdout.clone(),
                        stderr: stderr.clone(),
                        exit_code: result.exit_code.unwrap_or(-1),
                        duration_ms,
                    },
                    &stage_scope,
                );
            }
        }

        // Cleanup temp files (Exited path).
        cleanup_temp_files().await;

        let exited_success =
            result.termination == CommandTermination::Exited && result.exit_code == Some(0);
        if !exited_success {
            let detail = cli_failure_detail(&stdout, &stderr, &command);
            return Err(Error::handler(format!(
                "CLI command exited with code {}: {detail}",
                result
                    .exit_code
                    .map_or_else(|| "<unknown>".to_string(), |c| c.to_string()),
            )));
        }

        // 4. Parse the CLI output
        let parsed = parse_cli_response(provider, &stdout)
            .ok_or_else(|| Error::handler("Failed to parse CLI output".to_string()))?;

        // 5. Detect changed files
        let (files_touched, last_file_touched) =
            changed_files::files_touched_since(sandbox, &files_before).await;

        let stage_usage = billed_model_usage_from_llm(
            self.catalog.as_ref(),
            &ModelRef {
                provider: provider.id(),
                model_id: model.to_string(),
                speed:    controls.speed,
            },
            &TokenCounts {
                input_tokens: parsed.input_tokens,
                output_tokens: parsed.output_tokens,
                ..TokenCounts::default()
            },
        );

        Ok(CodergenResult::Text {
            text: parsed.text,
            usage: Some(stage_usage),
            files_touched,
            last_file_touched,
        })
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "CLI agent fallback credentials intentionally read provider API-key env vars."
)]
pub(crate) fn process_env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// Routes codergen invocations to API, CLI, or ACP backends based on node
/// attributes and model type.
pub struct BackendRouter {
    api: Box<dyn CodergenBackend>,
    cli: AgentCliBackend,
    acp: AgentAcpBackend,
}

impl BackendRouter {
    #[must_use]
    pub fn new(
        api_backend: Box<dyn CodergenBackend>,
        cli_backend: AgentCliBackend,
        acp_backend: AgentAcpBackend,
    ) -> Self {
        Self {
            api: api_backend,
            cli: cli_backend,
            acp: acp_backend,
        }
    }

    fn select_backend(node: &Node) -> Result<LlmBackend, Error> {
        routing::select_run_backend(node)
    }

    fn select_one_shot_backend(node: &Node) -> Result<LlmBackend, Error> {
        routing::select_one_shot_backend(node)
    }

    #[cfg(test)]
    fn should_use_cli(node: &Node) -> bool {
        matches!(Self::select_backend(node), Ok(LlmBackend::Cli))
    }
}

#[async_trait]
impl CodergenBackend for BackendRouter {
    async fn run(&self, request: CodergenRunRequest<'_>) -> Result<CodergenResult, Error> {
        match Self::select_backend(request.node)? {
            LlmBackend::Api => self.api.run(request).await,
            LlmBackend::Cli => self.cli.run(request).await,
            LlmBackend::Acp => self.acp.run(request).await,
        }
    }

    async fn one_shot(&self, request: OneShotRequest<'_>) -> Result<CodergenResult, Error> {
        match Self::select_one_shot_backend(request.node)? {
            LlmBackend::Acp => self.acp.one_shot(request).await,
            LlmBackend::Api | LlmBackend::Cli => self.api.one_shot(request).await,
        }
    }

    async fn shutdown(&self, emitter: &Arc<Emitter>) {
        self.api.shutdown(emitter).await;
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use fabro_agent::LocalSandbox;
    use fabro_agent::sandbox::ExecResult;
    use fabro_graphviz::graph::AttrValue;

    use super::*;
    use crate::context::Context;

    // -- AgentCli --

    #[test]
    fn agent_cli_for_provider() {
        assert_eq!(
            AgentCli::for_provider(Provider::Anthropic),
            AgentCli::Claude
        );
        assert_eq!(AgentCli::for_provider(Provider::OpenAi), AgentCli::Codex);
        assert_eq!(AgentCli::for_provider(Provider::Gemini), AgentCli::Gemini);
        assert_eq!(AgentCli::for_provider(Provider::Kimi), AgentCli::Codex);
        assert_eq!(AgentCli::for_provider(Provider::Zai), AgentCli::Codex);
        assert_eq!(AgentCli::for_provider(Provider::Minimax), AgentCli::Codex);
        assert_eq!(AgentCli::for_provider(Provider::Inception), AgentCli::Codex);
    }

    #[test]
    fn agent_cli_name() {
        assert_eq!(AgentCli::Claude.name(), "claude");
        assert_eq!(AgentCli::Codex.name(), "codex");
        assert_eq!(AgentCli::Gemini.name(), "gemini");
    }

    // -- verify_cli_available --

    use std::collections::VecDeque;
    use std::sync::Mutex;

    use fabro_acp::test_support::fake_acp_agent_script;
    use fabro_agent::sandbox::{DirEntry, GrepOptions};

    /// Mock sandbox that returns pre-configured ExecResults in FIFO order.
    struct CliMockSandbox {
        results:  Mutex<VecDeque<ExecResult>>,
        commands: Arc<Mutex<Vec<String>>>,
    }

    impl CliMockSandbox {
        fn new(results: Vec<ExecResult>, commands: Arc<Mutex<Vec<String>>>) -> Self {
            Self {
                results: Mutex::new(results.into()),
                commands,
            }
        }
    }

    #[async_trait]
    impl Sandbox for CliMockSandbox {
        async fn read_file(
            &self,
            _path: &str,
            _offset: Option<usize>,
            _limit: Option<usize>,
        ) -> fabro_sandbox::Result<String> {
            Ok(String::new())
        }
        async fn write_file(&self, _path: &str, _content: &str) -> fabro_sandbox::Result<()> {
            Ok(())
        }
        async fn delete_file(&self, _path: &str) -> fabro_sandbox::Result<()> {
            Ok(())
        }
        async fn file_exists(&self, _path: &str) -> fabro_sandbox::Result<bool> {
            Ok(false)
        }
        async fn list_directory(
            &self,
            _path: &str,
            _depth: Option<usize>,
        ) -> fabro_sandbox::Result<Vec<DirEntry>> {
            Ok(vec![])
        }
        async fn exec_command(
            &self,
            command: &str,
            _timeout_ms: u64,
            _working_dir: Option<&str>,
            _env_vars: Option<&std::collections::HashMap<String, String>>,
            _cancel_token: Option<tokio_util::sync::CancellationToken>,
        ) -> fabro_sandbox::Result<ExecResult> {
            self.commands.lock().unwrap().push(command.to_string());
            self.results
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| fabro_sandbox::Error::message("no more mock results"))
        }
        async fn grep(
            &self,
            _pattern: &str,
            _path: &str,
            _options: &GrepOptions,
        ) -> fabro_sandbox::Result<Vec<String>> {
            Ok(vec![])
        }
        async fn glob(
            &self,
            _pattern: &str,
            _path: Option<&str>,
        ) -> fabro_sandbox::Result<Vec<String>> {
            Ok(vec![])
        }
        async fn download_file_to_local(
            &self,
            _remote: &str,
            _local: &Path,
        ) -> fabro_sandbox::Result<()> {
            Ok(())
        }
        async fn upload_file_from_local(
            &self,
            _local: &Path,
            _remote: &str,
        ) -> fabro_sandbox::Result<()> {
            Ok(())
        }
        async fn initialize(&self) -> fabro_sandbox::Result<()> {
            Ok(())
        }
        async fn cleanup(&self) -> fabro_sandbox::Result<()> {
            Ok(())
        }
        fn working_directory(&self) -> &str {
            "/workspace"
        }
        fn platform(&self) -> &str {
            "linux"
        }
        fn os_version(&self) -> String {
            "Ubuntu 22.04".to_string()
        }
        async fn set_autostop_interval(&self, _minutes: i32) -> fabro_sandbox::Result<()> {
            Ok(())
        }
    }

    fn ok_result() -> ExecResult {
        ExecResult {
            exit_code:   Some(0),
            termination: CommandTermination::Exited,
            stdout:      String::new(),
            stderr:      String::new(),
            duration_ms: 10,
        }
    }

    fn fail_result(code: i32) -> ExecResult {
        fail_result_with_output(code, "", "error")
    }

    fn fail_result_with_output(code: i32, stdout: &str, stderr: &str) -> ExecResult {
        ExecResult {
            exit_code:   Some(code),
            termination: CommandTermination::Exited,
            stdout:      stdout.to_string(),
            stderr:      stderr.to_string(),
            duration_ms: 10,
        }
    }

    #[tokio::test]
    async fn verify_cli_available_succeeds_when_present() {
        let commands = Arc::new(Mutex::new(Vec::new()));
        let sandbox: Arc<dyn Sandbox> = Arc::new(CliMockSandbox::new(
            vec![ok_result()],
            Arc::clone(&commands),
        ));
        let result =
            verify_cli_available(AgentCli::Claude, &sandbox, &CancellationToken::new()).await;
        assert!(result.is_ok());

        let commands = commands.lock().unwrap();
        assert_eq!(commands.len(), 1);
        assert!(commands[0].contains("command -v claude"));
    }

    #[tokio::test]
    async fn verify_cli_available_fails_when_missing_without_installing() {
        let commands = Arc::new(Mutex::new(Vec::new()));
        let sandbox: Arc<dyn Sandbox> = Arc::new(CliMockSandbox::new(
            vec![fail_result(127)],
            Arc::clone(&commands),
        ));

        let result =
            verify_cli_available(AgentCli::Claude, &sandbox, &CancellationToken::new()).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("CLI backend requires 'claude' to be installed")
        );

        let commands = commands.lock().unwrap();
        assert_eq!(commands.len(), 1);
        assert!(commands[0].contains("command -v claude"));
        assert!(
            !commands
                .iter()
                .any(|command| command.contains("npm install"))
        );
    }

    // -- Cycle 1: cli_command_for_provider --

    #[test]
    fn cli_command_for_codex() {
        let cmd = cli_command_for_provider(Provider::OpenAi, "gpt-5.3-codex", "/tmp/prompt.txt");
        assert!(cmd.starts_with("cat /tmp/prompt.txt | codex exec --json --full-auto"));
        assert!(cmd.contains("-m gpt-5.3-codex"));
    }

    #[test]
    fn cli_command_for_claude() {
        let cmd =
            cli_command_for_provider(Provider::Anthropic, "claude-opus-4-6", "/tmp/prompt.txt");
        assert!(cmd.starts_with("cat /tmp/prompt.txt |"));
        assert!(cmd.contains("claude -p"));
        assert!(cmd.contains("--dangerously-skip-permissions"));
        assert!(cmd.contains("--output-format stream-json"));
        assert!(cmd.contains("--model claude-opus-4-6"));
    }

    #[test]
    fn cli_command_for_gemini() {
        let cmd = cli_command_for_provider(Provider::Gemini, "gemini-3.1-pro", "/tmp/prompt.txt");
        assert!(cmd.starts_with("cat /tmp/prompt.txt | gemini -o json --yolo"));
        assert!(cmd.contains("-m gemini-3.1-pro"));
    }

    #[test]
    fn cli_command_omits_model_when_empty() {
        let cmd = cli_command_for_provider(Provider::OpenAi, "", "/tmp/prompt.txt");
        assert!(cmd.contains("codex exec --json --full-auto"));
        assert!(!cmd.contains("-m "));
        let cmd = cli_command_for_provider(Provider::Anthropic, "", "/tmp/prompt.txt");
        assert!(cmd.contains("--dangerously-skip-permissions"));
        assert!(!cmd.contains("--model "));
        let cmd = cli_command_for_provider(Provider::Gemini, "", "/tmp/prompt.txt");
        assert!(cmd.contains("--yolo"));
        assert!(!cmd.contains("-m "));
    }

    // -- Cycle 2: is_cli_only_model --

    #[test]
    fn no_models_are_currently_cli_only() {
        assert!(!is_cli_only_model("gpt-5.3-codex"));
        assert!(!is_cli_only_model("claude-opus-4-6"));
        assert!(!is_cli_only_model("gemini-3.1-pro-preview"));
    }

    // -- Cycle 3: parse_cli_response — Claude/Gemini NDJSON --

    #[test]
    fn parse_claude_ndjson_extracts_text_and_usage() {
        let output = r#"{"type":"system","message":"Claude CLI v1.0"}
{"type":"assistant","message":{"content":"thinking..."}}
{"type":"result","result":"Here is the implementation.","usage":{"input_tokens":100,"output_tokens":50}}"#;
        let response = parse_cli_response(Provider::Anthropic, output).unwrap();
        assert_eq!(response.text, "Here is the implementation.");
        assert_eq!(response.input_tokens, 100);
        assert_eq!(response.output_tokens, 50);
    }

    #[test]
    fn parse_claude_ndjson_uses_last_result() {
        let output = r#"{"type":"result","result":"first","usage":{"input_tokens":10,"output_tokens":5}}
{"type":"result","result":"second","usage":{"input_tokens":20,"output_tokens":10}}"#;
        let response = parse_cli_response(Provider::Anthropic, output).unwrap();
        assert_eq!(response.text, "second");
        assert_eq!(response.input_tokens, 20);
    }

    #[test]
    fn parse_claude_ndjson_returns_none_for_no_result() {
        let output = r#"{"type":"system","message":"hello"}
{"type":"assistant","message":{"content":"no result line"}}"#;
        assert!(parse_cli_response(Provider::Anthropic, output).is_none());
    }

    #[test]
    fn parse_gemini_json_extracts_text_and_usage() {
        let output = r#"{"session_id":"abc","response":"Gemini says hello","stats":{"models":{"gemini-2.5-flash":{"tokens":{"input":200,"candidates":80,"total":280}}}}}"#;
        let response = parse_cli_response(Provider::Gemini, output).unwrap();
        assert_eq!(response.text, "Gemini says hello");
        assert_eq!(response.input_tokens, 200);
        assert_eq!(response.output_tokens, 80);
    }

    #[test]
    fn parse_gemini_json_handles_missing_stats() {
        let output = r#"{"response":"hello"}"#;
        let response = parse_cli_response(Provider::Gemini, output).unwrap();
        assert_eq!(response.text, "hello");
        assert_eq!(response.input_tokens, 0);
        assert_eq!(response.output_tokens, 0);
    }

    #[test]
    fn parse_gemini_json_returns_none_for_invalid_json() {
        assert!(parse_cli_response(Provider::Gemini, "not json").is_none());
    }

    // -- Cycle 4: parse_cli_response — Codex NDJSON --

    #[test]
    fn parse_codex_ndjson_extracts_text_and_usage() {
        let output = r#"{"type":"thread.started","thread_id":"abc"}
{"type":"turn.started"}
{"type":"item.completed","item":{"id":"item_0","type":"reasoning","text":"thinking..."}}
{"type":"item.completed","item":{"id":"item_1","type":"agent_message","text":"Fixed the bug."}}
{"type":"turn.completed","usage":{"input_tokens":300,"output_tokens":150}}"#;
        let response = parse_cli_response(Provider::OpenAi, output).unwrap();
        assert_eq!(response.text, "Fixed the bug.");
        assert_eq!(response.input_tokens, 300);
        assert_eq!(response.output_tokens, 150);
    }

    #[test]
    fn parse_codex_ndjson_handles_no_message() {
        let output = r#"{"type":"turn.completed","usage":{"input_tokens":10,"output_tokens":5}}"#;
        let response = parse_cli_response(Provider::OpenAi, output).unwrap();
        assert_eq!(response.text, "");
        assert_eq!(response.input_tokens, 10);
    }

    #[test]
    fn parse_codex_ndjson_returns_none_for_no_events() {
        assert!(parse_cli_response(Provider::OpenAi, "not json at all").is_none());
    }

    // -- Cycle 5: Node::backend() accessor (tested here since the accessor is
    // simple) --

    #[test]
    fn node_backend_returns_none_by_default() {
        let node = Node::new("test");
        assert_eq!(node.backend(), None);
    }

    #[test]
    fn node_backend_returns_cli_when_set() {
        let mut node = Node::new("test");
        node.attrs
            .insert("backend".to_string(), AttrValue::String("cli".to_string()));
        assert_eq!(node.backend(), Some("cli"));
    }

    // -- Cycle 6: backend in stylesheet (tested in stylesheet.rs) --

    // -- Cycle 7: BackendRouter routing logic --

    #[test]
    fn router_uses_cli_for_backend_attr() {
        let mut node = Node::new("test");
        node.attrs
            .insert("backend".to_string(), AttrValue::String("cli".to_string()));

        assert!(BackendRouter::should_use_cli(&node));
    }

    #[test]
    fn router_uses_api_by_default() {
        let node = Node::new("test");

        assert!(!BackendRouter::should_use_cli(&node));
    }

    #[test]
    fn router_uses_api_for_non_cli_model() {
        let mut node = Node::new("test");
        node.attrs.insert(
            "model".to_string(),
            AttrValue::String("claude-opus-4-6".to_string()),
        );

        assert!(!BackendRouter::should_use_cli(&node));
    }

    #[test]
    fn router_uses_api_for_backend_api() {
        let mut node = Node::new("test");
        node.attrs
            .insert("backend".to_string(), AttrValue::String("api".to_string()));

        assert_eq!(
            BackendRouter::select_backend(&node).unwrap(),
            LlmBackend::Api
        );
    }

    #[test]
    fn router_uses_cli_for_backend_cli() {
        let mut node = Node::new("test");
        node.attrs
            .insert("backend".to_string(), AttrValue::String("cli".to_string()));

        assert_eq!(
            BackendRouter::select_backend(&node).unwrap(),
            LlmBackend::Cli
        );
    }

    #[test]
    fn router_uses_acp_for_backend_acp() {
        let mut node = Node::new("test");
        node.attrs
            .insert("backend".to_string(), AttrValue::String("acp".to_string()));

        assert_eq!(
            BackendRouter::select_backend(&node).unwrap(),
            LlmBackend::Acp
        );
    }

    #[test]
    fn router_rejects_unknown_backend() {
        let mut node = Node::new("test");
        node.attrs.insert(
            "backend".to_string(),
            AttrValue::String("codex".to_string()),
        );

        let err = BackendRouter::select_backend(&node).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Validation error: unsupported LLM backend \"codex\"; expected one of: api, cli, acp"
        );
    }

    #[tokio::test]
    async fn router_routes_one_shot_to_acp_for_backend_acp() {
        let tempdir = tempfile::tempdir().unwrap();
        let script_path = tempdir.path().join("fake_acp_agent.py");
        tokio::fs::write(&script_path, fake_acp_agent_script())
            .await
            .unwrap();
        let sandbox: Arc<dyn Sandbox> = Arc::new(LocalSandbox::new(tempdir.path().to_path_buf()));
        let mut node = Node::new("test");
        node.attrs
            .insert("backend".to_string(), AttrValue::String("acp".to_string()));
        node.attrs.insert(
            "acp_command".to_string(),
            AttrValue::String(format!(
                "python3 {}",
                shell_quote(&script_path.to_string_lossy())
            )),
        );

        let context = Context::new();
        let router = test_router();
        let emitter = Arc::new(Emitter::default());
        let stage_scope = StageScope::for_handler(&context, "test");
        let result = router
            .one_shot(OneShotRequest {
                node:          &node,
                prompt:        "prompt",
                system_prompt: None,
                emitter:       &emitter,
                stage_scope:   &stage_scope,
                sandbox:       &sandbox,
                cancel_token:  CancellationToken::new(),
            })
            .await
            .unwrap();

        let CodergenResult::Text { text, .. } = result else {
            panic!("expected text result");
        };
        assert_eq!(text, "hello from acp");
    }

    #[tokio::test]
    async fn router_routes_one_shot_to_api_by_default() {
        let node = Node::new("test");
        let sandbox: Arc<dyn Sandbox> = Arc::new(LocalSandbox::new(
            tempfile::tempdir().unwrap().path().to_path_buf(),
        ));
        let context = Context::new();
        let router = test_router();
        let emitter = Arc::new(Emitter::default());
        let stage_scope = StageScope::for_handler(&context, "test");

        let result = router
            .one_shot(OneShotRequest {
                node:          &node,
                prompt:        "prompt",
                system_prompt: None,
                emitter:       &emitter,
                stage_scope:   &stage_scope,
                sandbox:       &sandbox,
                cancel_token:  CancellationToken::new(),
            })
            .await
            .unwrap();

        let CodergenResult::Text { text, .. } = result else {
            panic!("expected text result");
        };
        assert_eq!(text, "api one-shot");
    }

    #[tokio::test]
    async fn router_routes_one_shot_to_api_for_legacy_cli_backend() {
        let mut node = Node::new("test");
        node.attrs
            .insert("backend".to_string(), AttrValue::String("cli".to_string()));
        let sandbox: Arc<dyn Sandbox> = Arc::new(LocalSandbox::new(
            tempfile::tempdir().unwrap().path().to_path_buf(),
        ));
        let context = Context::new();
        let router = test_router();
        let emitter = Arc::new(Emitter::default());
        let stage_scope = StageScope::for_handler(&context, "test");

        let result = router
            .one_shot(OneShotRequest {
                node:          &node,
                prompt:        "prompt",
                system_prompt: None,
                emitter:       &emitter,
                stage_scope:   &stage_scope,
                sandbox:       &sandbox,
                cancel_token:  CancellationToken::new(),
            })
            .await
            .unwrap();

        let CodergenResult::Text { text, .. } = result else {
            panic!("expected text result");
        };
        assert_eq!(text, "api one-shot");
    }

    fn test_router() -> BackendRouter {
        let cli_backend = AgentCliBackend::new_from_env("model".into(), Provider::Anthropic);
        let acp_backend = AgentAcpBackend::new_from_env("model".into(), Provider::Anthropic);
        BackendRouter::new(Box::new(StubBackend), cli_backend, acp_backend)
    }

    /// Minimal stub backend for testing routing logic.
    struct StubBackend;

    #[async_trait]
    impl CodergenBackend for StubBackend {
        async fn run(&self, _request: CodergenRunRequest<'_>) -> Result<CodergenResult, Error> {
            Ok(CodergenResult::Text {
                text:              "stub".to_string(),
                usage:             None,
                files_touched:     Vec::new(),
                last_file_touched: None,
            })
        }

        async fn one_shot(&self, _request: OneShotRequest<'_>) -> Result<CodergenResult, Error> {
            Ok(CodergenResult::Text {
                text:              "api one-shot".to_string(),
                usage:             None,
                files_touched:     Vec::new(),
                last_file_touched: None,
            })
        }
    }

    /// Sandbox stub whose `exec_command_streaming` returns a configurable
    /// `CommandTermination` so we can exercise the cancel/timeout paths in
    /// `AgentCliBackend::run` without spawning real processes.
    struct StreamingCliMock {
        commands:    Arc<Mutex<Vec<String>>>,
        termination: CommandTermination,
        exit_code:   Option<i32>,
    }

    #[async_trait]
    impl Sandbox for StreamingCliMock {
        async fn read_file(
            &self,
            _path: &str,
            _offset: Option<usize>,
            _limit: Option<usize>,
        ) -> fabro_sandbox::Result<String> {
            Ok(String::new())
        }
        async fn write_file(&self, _path: &str, _content: &str) -> fabro_sandbox::Result<()> {
            Ok(())
        }
        async fn delete_file(&self, _path: &str) -> fabro_sandbox::Result<()> {
            Ok(())
        }
        async fn file_exists(&self, _path: &str) -> fabro_sandbox::Result<bool> {
            Ok(false)
        }
        async fn list_directory(
            &self,
            _path: &str,
            _depth: Option<usize>,
        ) -> fabro_sandbox::Result<Vec<fabro_agent::sandbox::DirEntry>> {
            Ok(vec![])
        }
        async fn exec_command(
            &self,
            command: &str,
            _timeout_ms: u64,
            _working_dir: Option<&str>,
            _env_vars: Option<&std::collections::HashMap<String, String>>,
            _cancel_token: Option<CancellationToken>,
        ) -> fabro_sandbox::Result<ExecResult> {
            self.commands.lock().unwrap().push(command.to_string());
            // Default: success for CLI availability checks and lightweight setup.
            if command.contains("command -v ") {
                return Ok(ok_result());
            }
            Ok(ExecResult {
                stdout:      String::new(),
                stderr:      String::new(),
                exit_code:   Some(0),
                termination: CommandTermination::Exited,
                duration_ms: 1,
            })
        }
        async fn exec_command_streaming(
            &self,
            command: &str,
            _timeout_ms: Option<u64>,
            _working_dir: Option<&str>,
            _env_vars: Option<&std::collections::HashMap<String, String>>,
            _cancel_token: Option<CancellationToken>,
            _output_callback: fabro_agent::CommandOutputCallback,
        ) -> fabro_sandbox::Result<fabro_sandbox::ExecStreamingResult> {
            self.commands.lock().unwrap().push(command.to_string());
            Ok(fabro_sandbox::ExecStreamingResult {
                result:            ExecResult {
                    stdout:      String::new(),
                    stderr:      String::new(),
                    exit_code:   self.exit_code,
                    termination: self.termination,
                    duration_ms: 5,
                },
                streams_separated: true,
                live_streaming:    true,
            })
        }
        async fn grep(
            &self,
            _pattern: &str,
            _path: &str,
            _options: &fabro_agent::sandbox::GrepOptions,
        ) -> fabro_sandbox::Result<Vec<String>> {
            Ok(vec![])
        }
        async fn glob(
            &self,
            _pattern: &str,
            _path: Option<&str>,
        ) -> fabro_sandbox::Result<Vec<String>> {
            Ok(vec![])
        }
        async fn download_file_to_local(&self, _: &str, _: &Path) -> fabro_sandbox::Result<()> {
            Ok(())
        }
        async fn upload_file_from_local(&self, _: &Path, _: &str) -> fabro_sandbox::Result<()> {
            Ok(())
        }
        async fn initialize(&self) -> fabro_sandbox::Result<()> {
            Ok(())
        }
        async fn cleanup(&self) -> fabro_sandbox::Result<()> {
            Ok(())
        }
        fn working_directory(&self) -> &str {
            "/workspace"
        }
        fn platform(&self) -> &str {
            "linux"
        }
        fn os_version(&self) -> String {
            "Ubuntu 22.04".into()
        }
        async fn set_autostop_interval(&self, _minutes: i32) -> fabro_sandbox::Result<()> {
            Ok(())
        }
    }

    fn collect_events(emitter: &Arc<Emitter>) -> Arc<Mutex<Vec<fabro_types::RunEvent>>> {
        let events = Arc::new(Mutex::new(Vec::new()));
        let events_clone = Arc::clone(&events);
        emitter.on_event(move |event| events_clone.lock().unwrap().push(event.clone()));
        events
    }

    #[tokio::test]
    async fn agent_cli_backend_run_emits_cancelled_event_and_returns_cancelled() {
        let commands = Arc::new(Mutex::new(Vec::new()));
        let sandbox: Arc<dyn Sandbox> = Arc::new(StreamingCliMock {
            commands:    Arc::clone(&commands),
            termination: CommandTermination::Cancelled,
            exit_code:   None,
        });
        let backend = AgentCliBackend::new_from_env("claude-opus-4-6".into(), Provider::Anthropic);
        let node = Node::new("step");
        let context = Context::new();
        let emitter = Arc::new(Emitter::default());
        let events = collect_events(&emitter);

        let result = backend
            .run(CodergenRunRequest {
                node:         &node,
                prompt:       "Do something",
                context:      &context,
                thread_id:    None,
                emitter:      &emitter,
                sandbox:      &sandbox,
                tool_hooks:   None,
                cancel_token: CancellationToken::new(),
            })
            .await;

        let Err(err) = result else {
            panic!("cancelled streaming should bubble Error::Cancelled");
        };
        assert!(matches!(err, Error::Cancelled));

        let events = events.lock().unwrap();
        let names: Vec<String> = events
            .iter()
            .map(|e| e.body.event_name().to_string())
            .collect();
        assert!(
            names.iter().any(|n| n == "agent.cli.cancelled"),
            "expected agent.cli.cancelled, got events: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n == "agent.cli.completed"),
            "should not emit agent.cli.completed on cancellation"
        );
        // Cleanup `rm -f` ran.
        let cmds = commands.lock().unwrap();
        assert!(
            cmds.iter().any(|c| c.starts_with("rm -f /tmp/fabro_cli_")),
            "expected temp cleanup, got commands: {cmds:?}"
        );
    }

    #[tokio::test]
    async fn agent_cli_backend_run_emits_timed_out_event_and_returns_handler_error() {
        let commands = Arc::new(Mutex::new(Vec::new()));
        let sandbox: Arc<dyn Sandbox> = Arc::new(StreamingCliMock {
            commands:    Arc::clone(&commands),
            termination: CommandTermination::TimedOut,
            exit_code:   None,
        });
        let backend = AgentCliBackend::new_from_env("claude-opus-4-6".into(), Provider::Anthropic);
        let node = Node::new("step");
        let context = Context::new();
        let emitter = Arc::new(Emitter::default());
        let events = collect_events(&emitter);

        let result = backend
            .run(CodergenRunRequest {
                node:         &node,
                prompt:       "Do something slow",
                context:      &context,
                thread_id:    None,
                emitter:      &emitter,
                sandbox:      &sandbox,
                tool_hooks:   None,
                cancel_token: CancellationToken::new(),
            })
            .await;

        let Err(err) = result else {
            panic!("timeout streaming should produce a handler error");
        };
        assert!(
            matches!(err, Error::Handler { .. }),
            "expected handler error on timeout, got {err:?}"
        );

        let events = events.lock().unwrap();
        let names: Vec<String> = events
            .iter()
            .map(|e| e.body.event_name().to_string())
            .collect();
        assert!(
            names.iter().any(|n| n == "agent.cli.timed_out"),
            "expected agent.cli.timed_out, got events: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n == "agent.cli.completed"),
            "should not emit agent.cli.completed on timeout"
        );
    }
}
