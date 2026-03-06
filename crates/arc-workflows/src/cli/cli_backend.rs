use std::path::Path;
use std::sync::Arc;

use arc_agent::Sandbox;
use arc_llm::provider::Provider;
use async_trait::async_trait;

use crate::context::Context;
use crate::error::ArcError;
use crate::event::EventEmitter;
use crate::graph::Node;
use crate::handler::agent::{CodergenBackend, CodergenResult};
use crate::outcome::StageUsage;

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
/// will be shell-redirected into the command's stdin.
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
            | Provider::Inception => {
                format!(" -m {model}")
            }
            Provider::Anthropic => format!(" --model {model}"),
        }
    };
    match provider {
        // --full-auto: sandboxed auto-execution, escalates on request
        Provider::OpenAi | Provider::Kimi | Provider::Zai | Provider::Minimax | Provider::Inception => {
            format!("codex exec --json --full-auto{model_flag} < {prompt_file}")
        }
        // --yolo: auto-approve all tool calls
        Provider::Gemini => format!("gemini -o json --yolo{model_flag} < {prompt_file}"),
        // --dangerously-skip-permissions: bypass all permission checks (required for non-interactive use).
        // CLAUDECODE= unset to allow running inside a Claude Code session.
        Provider::Anthropic => format!("CLAUDECODE= claude -p --verbose --output-format stream-json --dangerously-skip-permissions{model_flag} < {prompt_file}"),
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
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let output_tokens = result
        .pointer("/usage/output_tokens")
        .and_then(|v| v.as_i64())
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
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                output_tokens = value
                    .pointer("/usage/output_tokens")
                    .and_then(|v| v.as_i64())
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
        .map(|model_stats| {
            let input = model_stats
                .pointer("/tokens/input")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let output = model_stats
                .pointer("/tokens/candidates")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            (input, output)
        })
        .unwrap_or((0, 0));

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
        | Provider::Inception => parse_codex_ndjson(output),
        Provider::Gemini => parse_gemini_json(output),
        Provider::Anthropic => parse_claude_ndjson(output),
    }
}

/// CLI backend that invokes external CLI tools (claude, codex, gemini) via `exec_command()`.
pub struct AgentCliBackend {
    model: String,
    provider: Provider,
}

impl AgentCliBackend {
    #[must_use]
    pub fn new(model: String, provider: Provider) -> Self {
        Self { model, provider }
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
        _emitter: &Arc<EventEmitter>,
        stage_dir: &Path,
        sandbox: &Arc<dyn Sandbox>,
    ) -> Result<CodergenResult, ArcError> {
        // 1. Snapshot git state before the CLI run
        let files_before = self.detect_changed_files(sandbox).await;

        // 2. Write prompt to temp file
        let prompt_path = "/tmp/arc_cli_prompt.txt";
        sandbox
            .write_file(prompt_path, prompt)
            .await
            .map_err(|e| ArcError::handler(format!("Failed to write prompt file: {e}")))?;

        // 3. Build and execute CLI command
        let model = node.llm_model().unwrap_or(&self.model);
        let provider = node
            .llm_provider()
            .and_then(|s| s.parse::<Provider>().ok())
            .unwrap_or(self.provider);
        let command = cli_command_for_provider(provider, model, prompt_path);

        let _ = tokio::fs::create_dir_all(stage_dir).await;
        let provider_used = serde_json::json!({
            "mode": "cli",
            "provider": provider.as_str(),
            "model": model,
            "command": &command,
        });
        if let Ok(json) = serde_json::to_string_pretty(&provider_used) {
            let _ = tokio::fs::write(stage_dir.join("provider_used.json"), json).await;
        }

        // Forward provider API key so the CLI tool can authenticate.
        // Written to a temp file and sourced, since Daytona's exec API doesn't
        // support env vars and inline export would leak keys in logs.
        let env_file = "/tmp/arc_cli_env.sh";
        let env_lines: Vec<String> = provider
            .api_key_env_vars()
            .iter()
            .filter_map(|name| {
                std::env::var(name)
                    .ok()
                    .map(|val| format!("export {name}='{val}'"))
            })
            .collect();
        if !env_lines.is_empty() {
            sandbox
                .write_file(env_file, &env_lines.join("\n"))
                .await
                .map_err(|e| ArcError::handler(format!("Failed to write env file: {e}")))?;
        }
        let full_command = if env_lines.is_empty() {
            command.clone()
        } else {
            format!(". {env_file} && {command}")
        };

        let result = sandbox
            .exec_command(&full_command, 600_000, None, None, None)
            .await
            .map_err(|e| ArcError::handler(format!("CLI command failed: {e}")))?;

        if let Ok(json) = serde_json::to_string_pretty(&serde_json::json!({
            "exit_code": result.exit_code,
            "stdout_len": result.stdout.len(),
            "stderr_len": result.stderr.len(),
            "duration_ms": result.duration_ms,
        })) {
            let _ = tokio::fs::write(stage_dir.join("cli_result_meta.json"), json).await;
        }

        if result.exit_code != 0 {
            let _ = tokio::fs::write(stage_dir.join("cli_stdout.log"), &result.stdout).await;
            let _ = tokio::fs::write(stage_dir.join("cli_stderr.log"), &result.stderr).await;

            let stderr: String = result.stderr.chars().rev().take(500).collect::<Vec<_>>().into_iter().rev().collect();
            let detail = if !stderr.is_empty() {
                stderr
            } else {
                let stdout: String = result.stdout.chars().rev().take(500).collect::<Vec<_>>().into_iter().rev().collect();
                if !stdout.is_empty() {
                    format!("stdout: {stdout}")
                } else {
                    format!("command: {command}")
                }
            };
            return Err(ArcError::handler(format!(
                "CLI command exited with code {}: {detail}",
                result.exit_code,
            )));
        }

        // 4. Parse the CLI output
        let parsed = parse_cli_response(provider, &result.stdout)
            .ok_or_else(|| ArcError::handler("Failed to parse CLI output".to_string()))?;

        // 5. Detect changed files
        let files_after = self.detect_changed_files(sandbox).await;
        let files_touched: Vec<String> = files_after
            .into_iter()
            .filter(|f| !files_before.contains(f))
            .collect();

        let mut stage_usage = StageUsage {
            model: model.to_string(),
            input_tokens: parsed.input_tokens,
            output_tokens: parsed.output_tokens,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            cost: None,
        };
        stage_usage.cost = super::compute_stage_cost(&stage_usage);

        Ok(CodergenResult::Text {
            text: parsed.text,
            usage: Some(stage_usage),
            files_touched,
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

    fn should_use_cli(&self, node: &Node) -> bool {
        // Explicit backend="cli" attribute on the node
        if node.backend() == Some("cli") {
            return true;
        }

        // CLI-only model on the node
        if let Some(model) = node.llm_model() {
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
    ) -> Result<CodergenResult, ArcError> {
        if self.should_use_cli(node) {
            self.cli_backend
                .run(
                    node, prompt, context, thread_id, emitter, stage_dir, sandbox,
                )
                .await
        } else {
            self.api_backend
                .run(
                    node, prompt, context, thread_id, emitter, stage_dir, sandbox,
                )
                .await
        }
    }

    async fn one_shot(
        &self,
        node: &Node,
        prompt: &str,
        stage_dir: &Path,
    ) -> Result<CodergenResult, ArcError> {
        // CLI backend doesn't support one_shot, always route to API
        self.api_backend.one_shot(node, prompt, stage_dir).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::AttrValue;

    // -- Cycle 1: cli_command_for_provider --

    #[test]
    fn cli_command_for_codex() {
        let cmd = cli_command_for_provider(Provider::OpenAi, "gpt-5.3-codex", "/tmp/prompt.txt");
        assert!(cmd.starts_with("codex exec --json --full-auto"));
        assert!(cmd.contains("-m gpt-5.3-codex"));
        assert!(cmd.ends_with("< /tmp/prompt.txt"));
    }

    #[test]
    fn cli_command_for_claude() {
        let cmd =
            cli_command_for_provider(Provider::Anthropic, "claude-opus-4-6", "/tmp/prompt.txt");
        assert!(cmd.contains("claude -p"));
        assert!(cmd.contains("--dangerously-skip-permissions"));
        assert!(cmd.contains("--output-format stream-json"));
        assert!(cmd.contains("--model claude-opus-4-6"));
    }

    #[test]
    fn cli_command_for_gemini() {
        let cmd = cli_command_for_provider(Provider::Gemini, "gemini-3.1-pro", "/tmp/prompt.txt");
        assert!(cmd.starts_with("gemini -o json --yolo"));
        assert!(cmd.contains("-m gemini-3.1-pro"));
    }

    #[test]
    fn cli_command_omits_model_when_empty() {
        let cmd = cli_command_for_provider(Provider::OpenAi, "", "/tmp/prompt.txt");
        assert!(cmd.starts_with("codex exec --json --full-auto"));
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
            "llm_model".to_string(),
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
        ) -> Result<CodergenResult, ArcError> {
            Ok(CodergenResult::Text {
                text: "stub".to_string(),
                usage: None,
                files_touched: Vec::new(),
            })
        }
    }
}
