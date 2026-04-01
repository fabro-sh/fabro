use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use fabro_agent::Sandbox;
use fabro_agent::sandbox::ExecResult;
use fabro_model::Provider;
use tokio::fs;
use tokio::time::sleep;

use super::super::agent::{CodergenBackend, CodergenResult};
use crate::context::Context;
use crate::error::FabroError;
use crate::event::{EventEmitter, WorkflowRunEvent};
use crate::outcome::StageUsage;
use crate::outcome::compute_stage_cost;
use fabro_graphviz::graph::Node;

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

    pub fn npm_package(self) -> &'static str {
        match self {
            Self::Claude => "@anthropic-ai/claude-code",
            Self::Codex => "@openai/codex",
            Self::Gemini => "@anthropic-ai/gemini-cli",
        }
    }
}

/// Ensure the CLI tool for the given provider is installed in the sandbox.
///
/// Checks if the CLI binary exists; if not, installs Node.js (if missing) and
/// the CLI via npm. Emits `CliEnsure*` events for observability.
async fn ensure_cli(
    cli: AgentCli,
    provider: Provider,
    sandbox: &Arc<dyn Sandbox>,
    emitter: &Arc<EventEmitter>,
) -> Result<(), FabroError> {
    let start = std::time::Instant::now();
    let cli_name = cli.name();
    let provider_str = provider.as_str();

    emitter.emit(&WorkflowRunEvent::CliEnsureStarted {
        cli_name: cli_name.to_string(),
        provider: provider_str.to_string(),
    });

    // Check if the CLI is already installed (include ~/.local/bin for npm-installed CLIs)
    let version_check = sandbox
        .exec_command(
            &format!("PATH=\"$HOME/.local/bin:$PATH\" {cli_name} --version"),
            30_000,
            None,
            None,
            None,
        )
        .await
        .map_err(|e| FabroError::handler(format!("Failed to check {cli_name} version: {e}")))?;

    if version_check.exit_code == 0 {
        let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        emitter.emit(&WorkflowRunEvent::CliEnsureCompleted {
            cli_name: cli_name.to_string(),
            provider: provider_str.to_string(),
            already_installed: true,
            node_installed: false,
            duration_ms,
        });
        return Ok(());
    }

    // Install Node.js (if needed) and the CLI in a single shell so PATH persists
    let install_cmd = format!(
        "export PATH=\"$HOME/.local/bin:$PATH\" && \
         (node --version >/dev/null 2>&1 || \
          (mkdir -p ~/.local && curl -fsSL https://nodejs.org/dist/v22.14.0/node-v22.14.0-linux-x64.tar.gz | tar -xz --strip-components=1 -C ~/.local)) && \
         npm install -g {}",
        cli.npm_package()
    );
    let install_result = sandbox
        .exec_command(&install_cmd, 180_000, None, None, None)
        .await
        .map_err(|e| FabroError::handler(format!("Failed to install {cli_name}: {e}")))?;

    let node_installed = true;
    if install_result.exit_code != 0 {
        let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        let output = if install_result.stderr.is_empty() {
            &install_result.stdout
        } else {
            &install_result.stderr
        };
        let detail: String = output
            .chars()
            .rev()
            .take(500)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let error_msg = format!(
            "{cli_name} install exited with code {}: {detail}",
            install_result.exit_code
        );
        emitter.emit(&WorkflowRunEvent::CliEnsureFailed {
            cli_name: cli_name.to_string(),
            provider: provider_str.to_string(),
            error: error_msg.clone(),
            duration_ms,
        });
        return Err(FabroError::handler(error_msg));
    }

    let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
    emitter.emit(&WorkflowRunEvent::CliEnsureCompleted {
        cli_name: cli_name.to_string(),
        provider: provider_str.to_string(),
        already_installed: false,
        node_installed,
        duration_ms,
    });

    Ok(())
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
    let model_flag = if model.is_empty() {
        String::new()
    } else {
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
        // --dangerously-skip-permissions: bypass all permission checks (required for non-interactive use).
        // CLAUDECODE= unset to allow running inside a Claude Code session.
        Provider::Anthropic => format!(
            "cat {prompt_file} | CLAUDECODE= claude -p --verbose --output-format stream-json --dangerously-skip-permissions{model_flag}"
        ),
    }
}

/// Parsed response from a CLI tool invocation.
#[derive(Debug)]
pub struct CliResponse {
    pub text: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
}

/// Parse NDJSON output from Claude CLI (`--output-format stream-json`).
///
/// Looks for the last `{"type":"result",...}` line, extracts `result` text and `usage`.
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
/// `item.type == "agent_message"`. Usage comes from the `turn.completed` event.
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

/// Escape a value for safe embedding inside single quotes in a shell command.
fn shell_escape(val: &str) -> String {
    val.replace('\'', "'\\''")
}

/// CLI backend that invokes external CLI tools (claude, codex, gemini) via `exec_command()`.
pub struct AgentCliBackend {
    model: String,
    provider: Provider,
    env: HashMap<String, String>,
    poll_interval: std::time::Duration,
}

impl AgentCliBackend {
    #[must_use]
    pub fn new(model: String, provider: Provider) -> Self {
        Self {
            model,
            provider,
            env: HashMap::new(),
            poll_interval: std::time::Duration::from_secs(5),
        }
    }

    #[must_use]
    pub fn with_env(mut self, env: HashMap<String, String>) -> Self {
        self.env = env;
        self
    }

    #[must_use]
    pub fn with_poll_interval(mut self, interval: std::time::Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    /// Detect changed files by comparing git state before and after the CLI run.
    async fn detect_changed_files(&self, sandbox: &Arc<dyn Sandbox>) -> Vec<String> {
        // Get unstaged changes
        let diff_result = sandbox
            .exec_command("git diff --name-only", 30_000, None, None, None)
            .await;

        // Get untracked files
        let untracked_result = sandbox
            .exec_command(
                "git ls-files --others --exclude-standard",
                30_000,
                None,
                None,
                None,
            )
            .await;

        let mut files: Vec<String> = Vec::new();

        if let Ok(result) = diff_result {
            if result.exit_code == 0 {
                files.extend(
                    result
                        .stdout
                        .lines()
                        .filter(|l| !l.trim().is_empty())
                        .map(String::from),
                );
            }
        }

        if let Ok(result) = untracked_result {
            if result.exit_code == 0 {
                files.extend(
                    result
                        .stdout
                        .lines()
                        .filter(|l| !l.trim().is_empty())
                        .map(String::from),
                );
            }
        }

        files.sort();
        files.dedup();
        files
    }
}

#[async_trait]
impl CodergenBackend for AgentCliBackend {
    async fn run(
        &self,
        node: &Node,
        prompt: &str,
        _context: &Context,
        _thread_id: Option<&str>,
        emitter: &Arc<EventEmitter>,
        stage_dir: &Path,
        sandbox: &Arc<dyn Sandbox>,
        _tool_hooks: Option<Arc<dyn fabro_agent::ToolHookCallback>>,
    ) -> Result<CodergenResult, FabroError> {
        // 1. Snapshot git state before the CLI run
        let files_before = self.detect_changed_files(sandbox).await;

        // 2. Generate unique paths for this run
        let run_id = uuid::Uuid::new_v4().to_string();
        let tmp_prefix = format!("/tmp/fabro_cli_{run_id}");
        let prompt_path = format!("{tmp_prefix}_prompt.txt");
        let stdout_path = format!("{tmp_prefix}_stdout.log");
        let stderr_path = format!("{tmp_prefix}_stderr.log");
        let exit_code_path = format!("{tmp_prefix}_exit_code");
        let env_path = format!("{tmp_prefix}_env.sh");

        sandbox
            .write_file(&prompt_path, prompt)
            .await
            .map_err(|e| FabroError::handler(format!("Failed to write prompt file: {e}")))?;

        // 3. Build CLI command
        let model = node.model().unwrap_or(&self.model);
        let provider = node
            .provider()
            .and_then(|s| s.parse::<Provider>().ok())
            .unwrap_or(self.provider);

        // Ensure the CLI tool is installed in the sandbox
        let cli = AgentCli::for_provider(provider);
        ensure_cli(cli, provider, sandbox, emitter).await?;

        let command = cli_command_for_provider(provider, model, &prompt_path);
        emitter.emit(&WorkflowRunEvent::AgentCliStarted {
            node_id: node.id.clone(),
            mode: "cli".to_string(),
            provider: provider.as_str().to_string(),
            model: model.to_string(),
            command: command.clone(),
        });

        let _ = fs::create_dir_all(stage_dir).await;
        let provider_used = serde_json::json!({
            "mode": "cli",
            "provider": provider.as_str(),
            "model": model,
            "command": &command,
        });
        if let Ok(json) = serde_json::to_string_pretty(&provider_used) {
            let _ = fs::write(stage_dir.join("provider_used.json"), json).await;
        }

        // Forward provider API key and custom env vars so the CLI tool can authenticate.
        // Build a HashMap to pass via exec_command's env_vars parameter — this
        // prepends `export` statements directly into the base64-encoded command,
        // avoiding filesystem-to-process race conditions that can occur when
        // writing an env file via the fs API and sourcing it via the process API.
        let mut launch_env: HashMap<String, String> = HashMap::new();
        for name in provider.api_key_env_vars() {
            if let Ok(val) = std::env::var(name) {
                launch_env.insert((*name).to_string(), val);
            }
        }
        for (name, val) in &self.env {
            launch_env.insert(name.clone(), val.clone());
        }

        // Codex CLI requires `codex login --with-api-key` to store credentials;
        // it does not read OPENAI_API_KEY from the environment at runtime.
        if cli == AgentCli::Codex {
            if let Some(api_key) = launch_env.get("OPENAI_API_KEY") {
                let login_cmd = format!(
                    "PATH=\"$HOME/.local/bin:$PATH\" echo '{}' | codex login --with-api-key",
                    shell_escape(api_key)
                );
                let login_result = sandbox
                    .exec_command(&login_cmd, 30_000, None, None, None)
                    .await
                    .map_err(|e| FabroError::handler(format!("codex login failed: {e}")))?;
                if login_result.exit_code != 0 {
                    tracing::warn!(
                        exit_code = login_result.exit_code,
                        "codex login --with-api-key failed: {}",
                        login_result.stderr
                    );
                }
            }
        }

        // Also write env file as fallback for commands that source it (e.g. ensure_cli PATH)
        let mut env_lines: Vec<String> = vec!["export PATH=\"$HOME/.local/bin:$PATH\"".to_string()];
        env_lines.extend(
            launch_env
                .iter()
                .map(|(k, v)| format!("export {k}='{}'", shell_escape(v))),
        );
        {
            sandbox
                .write_file(&env_path, &env_lines.join("\n"))
                .await
                .map_err(|e| FabroError::handler(format!("Failed to write env file: {e}")))?;
        }

        // 3a. Disable auto-stop so the sandbox stays alive during long CLI runs
        if let Err(e) = sandbox.set_autostop_interval(0).await {
            tracing::warn!("Failed to disable sandbox auto-stop: {e}");
        }

        // 3b. Launch CLI command in background (env file is always written)
        let inner_command = format!(". {env_path} && {command}");
        // Use setsid (if available) to create a new session so the child process is
        // fully detached from the shell. Without this, Daytona's POST /process/execute
        // blocks until ALL descendant processes exit, causing a 60s HTTP timeout.
        // $SID is empty on macOS (where setsid doesn't exist but isn't needed since
        // the local exec implementation doesn't wait for grandchildren).
        let bg_command = format!(
            "SID=$(command -v setsid || true)\n$SID sh -c '{inner_command} > {stdout_path} 2>{stderr_path}; echo $? > {exit_code_path}' </dev/null >/dev/null 2>&1 &\necho $!"
        );
        let launch_start = std::time::Instant::now();
        let launch_env_ref = if launch_env.is_empty() {
            None
        } else {
            Some(&launch_env)
        };
        let launch_result = sandbox
            .exec_command(&bg_command, 30_000, None, launch_env_ref, None)
            .await
            .map_err(|e| FabroError::handler(format!("Failed to launch CLI command: {e}")))?;
        let pid = launch_result.stdout.trim();
        tracing::info!(pid, "CLI process launched in background");

        // 3c. Poll for completion
        let poll_command =
            format!("[ -f {exit_code_path} ] && cat {exit_code_path} || echo running");
        let poll_interval = self.poll_interval;
        let exit_code: i32 = loop {
            sleep(poll_interval).await;
            emitter.touch(); // keep the stall watchdog alive while polling
            let poll_result = sandbox
                .exec_command(&poll_command, 30_000, None, None, None)
                .await
                .map_err(|e| FabroError::handler(format!("Failed to poll CLI command: {e}")))?;
            let status = poll_result.stdout.trim();

            // Sync CLI output to stage_dir for visibility (best-effort)
            for (remote, local) in [
                (&stdout_path, "cli_stdout.log"),
                (&stderr_path, "cli_stderr.log"),
            ] {
                if let Ok(r) = sandbox
                    .exec_command(&format!("cat {remote}"), 30_000, None, None, None)
                    .await
                {
                    if !r.stdout.is_empty() {
                        let _ = fs::write(stage_dir.join(local), &r.stdout).await;
                    }
                }
            }

            if status != "running" {
                break status.parse::<i32>().unwrap_or(-1);
            }
        };

        // 3d. Read results
        let duration_ms = u64::try_from(launch_start.elapsed().as_millis()).unwrap_or(u64::MAX);
        let stdout_result = sandbox
            .exec_command(&format!("cat {stdout_path}"), 60_000, None, None, None)
            .await
            .map_err(|e| FabroError::handler(format!("Failed to read stdout: {e}")))?;
        let stderr_result = sandbox
            .exec_command(&format!("cat {stderr_path}"), 60_000, None, None, None)
            .await
            .map_err(|e| FabroError::handler(format!("Failed to read stderr: {e}")))?;

        let result = ExecResult {
            stdout: stdout_result.stdout,
            stderr: stderr_result.stdout,
            exit_code,
            timed_out: false,
            duration_ms,
        };
        emitter.emit(&WorkflowRunEvent::AgentCliCompleted {
            node_id: node.id.clone(),
            stdout: result.stdout.clone(),
            stderr: result.stderr.clone(),
            exit_code: result.exit_code,
            duration_ms: result.duration_ms,
        });

        // 3e. Cleanup temp files
        let _ = sandbox
            .exec_command(&format!("rm -f {tmp_prefix}_*"), 30_000, None, None, None)
            .await;

        if let Ok(json) = serde_json::to_string_pretty(&serde_json::json!({
            "exit_code": result.exit_code,
            "stdout_len": result.stdout.len(),
            "stderr_len": result.stderr.len(),
            "duration_ms": result.duration_ms,
        })) {
            let _ = fs::write(stage_dir.join("cli_result_meta.json"), json).await;
        }

        if result.exit_code != 0 {
            let _ = fs::write(stage_dir.join("cli_stdout.log"), &result.stdout).await;
            let _ = fs::write(stage_dir.join("cli_stderr.log"), &result.stderr).await;

            let tail = |s: &str, n: usize| -> String {
                s.chars()
                    .rev()
                    .take(n)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect()
            };
            let stderr_tail = tail(&result.stderr, 500);
            let stdout_tail = tail(&result.stdout, 500);
            let detail = match (stderr_tail.is_empty(), stdout_tail.is_empty()) {
                (false, false) => format!("{stderr_tail}\nstdout: {stdout_tail}"),
                (false, true) => stderr_tail,
                (true, false) => format!("stdout: {stdout_tail}"),
                (true, true) => format!("command: {command}"),
            };
            return Err(FabroError::handler(format!(
                "CLI command exited with code {}: {detail}",
                result.exit_code,
            )));
        }

        // 4. Parse the CLI output
        let parsed = parse_cli_response(provider, &result.stdout)
            .ok_or_else(|| FabroError::handler("Failed to parse CLI output".to_string()))?;

        // 5. Detect changed files
        let files_after = self.detect_changed_files(sandbox).await;
        let files_touched: Vec<String> = files_after
            .into_iter()
            .filter(|f| !files_before.contains(f))
            .collect();

        // Find the most recently modified file by mtime
        let last_file_touched = if files_touched.is_empty() {
            None
        } else {
            let quoted_files: Vec<String> = files_touched
                .iter()
                .filter_map(|f| shlex::try_quote(f).ok().map(std::borrow::Cow::into_owned))
                .collect();
            let cmd = format!("ls -t {} | head -1", quoted_files.join(" "));
            if let Ok(result) = sandbox.exec_command(&cmd, 5_000, None, None, None).await {
                let trimmed = result.stdout.trim().to_string();
                if result.exit_code == 0 && !trimmed.is_empty() {
                    Some(trimmed)
                } else {
                    None
                }
            } else {
                None
            }
        };

        let mut stage_usage = StageUsage {
            model: model.to_string(),
            input_tokens: parsed.input_tokens,
            output_tokens: parsed.output_tokens,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            speed: None,
            cost: None,
        };
        stage_usage.cost = compute_stage_cost(&stage_usage);

        Ok(CodergenResult::Text {
            text: parsed.text,
            usage: Some(stage_usage),
            files_touched,
            last_file_touched,
        })
    }
}

/// Routes codergen invocations to either the API backend or CLI backend
/// based on node attributes and model type.
pub struct BackendRouter {
    api_backend: Box<dyn CodergenBackend>,
    cli_backend: AgentCliBackend,
}

impl BackendRouter {
    #[must_use]
    pub fn new(api_backend: Box<dyn CodergenBackend>, cli_backend: AgentCliBackend) -> Self {
        Self {
            api_backend,
            cli_backend,
        }
    }

    #[allow(clippy::unused_self)]
    fn should_use_cli(&self, node: &Node) -> bool {
        // Explicit backend="cli" attribute on the node
        if node.backend() == Some("cli") {
            return true;
        }

        // CLI-only model on the node
        if let Some(model) = node.model() {
            if is_cli_only_model(model) {
                return true;
            }
        }

        false
    }
}

#[async_trait]
impl CodergenBackend for BackendRouter {
    async fn run(
        &self,
        node: &Node,
        prompt: &str,
        context: &Context,
        thread_id: Option<&str>,
        emitter: &Arc<EventEmitter>,
        stage_dir: &Path,
        sandbox: &Arc<dyn Sandbox>,
        tool_hooks: Option<Arc<dyn fabro_agent::ToolHookCallback>>,
    ) -> Result<CodergenResult, FabroError> {
        if self.should_use_cli(node) {
            self.cli_backend
                .run(
                    node, prompt, context, thread_id, emitter, stage_dir, sandbox, tool_hooks,
                )
                .await
        } else {
            self.api_backend
                .run(
                    node, prompt, context, thread_id, emitter, stage_dir, sandbox, tool_hooks,
                )
                .await
        }
    }

    async fn one_shot(
        &self,
        node: &Node,
        prompt: &str,
        system_prompt: Option<&str>,
        stage_dir: &Path,
    ) -> Result<CodergenResult, FabroError> {
        // CLI backend doesn't support one_shot, always route to API
        self.api_backend
            .one_shot(node, prompt, system_prompt, stage_dir)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fabro_graphviz::graph::AttrValue;

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

    #[test]
    fn agent_cli_npm_package() {
        assert_eq!(AgentCli::Claude.npm_package(), "@anthropic-ai/claude-code");
        assert_eq!(AgentCli::Codex.npm_package(), "@openai/codex");
        assert_eq!(AgentCli::Gemini.npm_package(), "@anthropic-ai/gemini-cli");
    }

    // -- ensure_cli --

    use fabro_agent::sandbox::{DirEntry, GrepOptions};
    use std::collections::VecDeque;
    use std::sync::Mutex;

    /// Mock sandbox that returns pre-configured ExecResults in FIFO order.
    struct CliMockSandbox {
        results: Mutex<VecDeque<ExecResult>>,
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
        ) -> Result<String, String> {
            Ok(String::new())
        }
        async fn write_file(&self, _path: &str, _content: &str) -> Result<(), String> {
            Ok(())
        }
        async fn delete_file(&self, _path: &str) -> Result<(), String> {
            Ok(())
        }
        async fn file_exists(&self, _path: &str) -> Result<bool, String> {
            Ok(false)
        }
        async fn list_directory(
            &self,
            _path: &str,
            _depth: Option<usize>,
        ) -> Result<Vec<DirEntry>, String> {
            Ok(vec![])
        }
        async fn exec_command(
            &self,
            command: &str,
            _timeout_ms: u64,
            _working_dir: Option<&str>,
            _env_vars: Option<&std::collections::HashMap<String, String>>,
            _cancel_token: Option<tokio_util::sync::CancellationToken>,
        ) -> Result<ExecResult, String> {
            self.commands.lock().unwrap().push(command.to_string());
            self.results
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| "no more mock results".to_string())
        }
        async fn grep(
            &self,
            _pattern: &str,
            _path: &str,
            _options: &GrepOptions,
        ) -> Result<Vec<String>, String> {
            Ok(vec![])
        }
        async fn glob(&self, _pattern: &str, _path: Option<&str>) -> Result<Vec<String>, String> {
            Ok(vec![])
        }
        async fn download_file_to_local(&self, _remote: &str, _local: &Path) -> Result<(), String> {
            Ok(())
        }
        async fn upload_file_from_local(&self, _local: &Path, _remote: &str) -> Result<(), String> {
            Ok(())
        }
        async fn initialize(&self) -> Result<(), String> {
            Ok(())
        }
        async fn cleanup(&self) -> Result<(), String> {
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
        async fn set_autostop_interval(&self, _minutes: i32) -> Result<(), String> {
            Ok(())
        }
    }

    fn ok_result() -> ExecResult {
        ExecResult {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            timed_out: false,
            duration_ms: 10,
        }
    }

    fn fail_result(code: i32) -> ExecResult {
        ExecResult {
            exit_code: code,
            stdout: String::new(),
            stderr: "error".to_string(),
            timed_out: false,
            duration_ms: 10,
        }
    }

    #[tokio::test]
    async fn ensure_cli_skips_install_when_present() {
        let commands = Arc::new(Mutex::new(Vec::new()));
        let sandbox: Arc<dyn Sandbox> = Arc::new(CliMockSandbox::new(
            vec![ok_result()],
            Arc::clone(&commands),
        ));
        let emitter = Arc::new(EventEmitter::default());

        let result = ensure_cli(AgentCli::Claude, Provider::Anthropic, &sandbox, &emitter).await;
        assert!(result.is_ok());

        let commands = commands.lock().unwrap();
        assert_eq!(commands.len(), 1);
        assert!(commands[0].contains("claude --version"));
    }

    #[tokio::test]
    async fn ensure_cli_installs_when_missing() {
        let commands = Arc::new(Mutex::new(Vec::new()));
        // version check fails, combined install succeeds
        let sandbox: Arc<dyn Sandbox> = Arc::new(CliMockSandbox::new(
            vec![
                fail_result(127), // claude --version
                ok_result(),      // combined node + npm install
            ],
            Arc::clone(&commands),
        ));
        let emitter = Arc::new(EventEmitter::default());

        let result = ensure_cli(AgentCli::Claude, Provider::Anthropic, &sandbox, &emitter).await;
        assert!(result.is_ok());

        let commands = commands.lock().unwrap();
        assert_eq!(commands.len(), 2);
        assert!(commands[1].contains("npm install -g @anthropic-ai/claude-code"));
    }

    #[tokio::test]
    async fn ensure_cli_fails_on_install_failure() {
        let commands = Arc::new(Mutex::new(Vec::new()));
        let sandbox: Arc<dyn Sandbox> = Arc::new(CliMockSandbox::new(
            vec![
                fail_result(127), // claude --version
                fail_result(1),   // combined install fails
            ],
            Arc::clone(&commands),
        ));
        let emitter = Arc::new(EventEmitter::default());

        let result = ensure_cli(AgentCli::Claude, Provider::Anthropic, &sandbox, &emitter).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("install exited with code")
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

    // -- Cycle 5: Node::backend() accessor (tested here since the accessor is simple) --

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

        let cli_backend = AgentCliBackend::new("model".into(), Provider::Anthropic);
        let router = BackendRouter::new(Box::new(StubBackend), cli_backend);
        assert!(router.should_use_cli(&node));
    }

    #[test]
    fn router_uses_api_by_default() {
        let node = Node::new("test");

        let cli_backend = AgentCliBackend::new("model".into(), Provider::Anthropic);
        let router = BackendRouter::new(Box::new(StubBackend), cli_backend);
        assert!(!router.should_use_cli(&node));
    }

    #[test]
    fn router_uses_api_for_non_cli_model() {
        let mut node = Node::new("test");
        node.attrs.insert(
            "model".to_string(),
            AttrValue::String("claude-opus-4-6".to_string()),
        );

        let cli_backend = AgentCliBackend::new("model".into(), Provider::Anthropic);
        let router = BackendRouter::new(Box::new(StubBackend), cli_backend);
        assert!(!router.should_use_cli(&node));
    }

    /// Minimal stub backend for testing routing logic.
    struct StubBackend;

    #[async_trait]
    impl CodergenBackend for StubBackend {
        async fn run(
            &self,
            _node: &Node,
            _prompt: &str,
            _context: &Context,
            _thread_id: Option<&str>,
            _emitter: &Arc<EventEmitter>,
            _stage_dir: &Path,
            _sandbox: &Arc<dyn Sandbox>,
            _tool_hooks: Option<Arc<dyn fabro_agent::ToolHookCallback>>,
        ) -> Result<CodergenResult, FabroError> {
            Ok(CodergenResult::Text {
                text: "stub".to_string(),
                usage: None,
                files_touched: Vec::new(),
                last_file_touched: None,
            })
        }
    }
}
