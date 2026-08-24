use std::borrow::Cow;
use std::fmt::Write;
use std::sync::Arc;

use fabro_llm::client::Client;
use fabro_llm::types::{Message, Request, ToolDefinition};
use fabro_model::ModelHandle;
#[cfg(test)]
use fabro_static::EnvVars;
use futures::{StreamExt, stream};
use tokio::task;

use crate::config::NativeToolOptions;
use crate::sandbox::{ExecStreamingResult, GrepOptions};
use crate::tool_registry::{RegisteredTool, ToolContext, ToolRegistry, ToolSource};
use crate::types::AgentEvent;
use crate::web_search::{SearchBackend, make_web_search_tool};
use crate::write_locks;

const MAX_WEB_FETCH_BYTES: usize = 100 * 1024;
const MAX_READ_MANY_FILES_CONCURRENCY: usize = 8;
pub(crate) const DEFAULT_READ_LINES: usize = 2000;

/// Configuration for the optional LLM-based summarizer used by `web_fetch`.
#[derive(Clone)]
pub struct WebFetchSummarizer {
    pub client:   Client,
    pub model_id: ModelHandle,
}

/// Returns true if the input looks like it contains HTML markup.
fn looks_like_html(text: &str) -> bool {
    let trimmed = text.trim_start();
    trimmed.starts_with("<!")
        || trimmed.starts_with("<html")
        || trimmed.starts_with("<HTML")
        || trimmed.contains("</div>")
        || trimmed.contains("</p>")
        || trimmed.contains("</body>")
}

/// Converts HTML to Markdown, stripping script/style tags.
/// Non-HTML content (JSON, plain text) passes through unchanged.
fn html_to_markdown(text: &str) -> String {
    if !looks_like_html(text) {
        return text.to_string();
    }
    let converter = htmd::HtmlToMarkdown::builder()
        .skip_tags(vec!["script", "style"])
        .build();
    converter.convert(text).unwrap_or_else(|_| text.to_string())
}

/// Name of the credential-backed web search tool. Profiles look this up in
/// their own registry to decide whether to advertise web search in the system
/// prompt, so availability and prompt guidance cannot drift apart.
pub const WEB_SEARCH_TOOL_NAME: &str = "web_search";

/// Registers the core tools shared by all provider profiles: `read_file`,
/// `write_file`, `shell`, `grep`, `glob`, and `web_fetch`. `web_search` is
/// included when a Brave or Venice Search API key is configured.
///
/// The shell tool captures its default and max timeouts from `options`.
pub fn register_core_tools(
    registry: &mut ToolRegistry,
    options: &NativeToolOptions,
    summarizer: Option<WebFetchSummarizer>,
) {
    registry.register(make_read_file_tool());
    registry.register(make_write_file_tool());
    registry.register(make_shell_tool_with_options(options));
    registry.register(make_grep_tool());
    register_discovery_and_web_tools(registry, options, summarizer);
}

/// Register the core tools whose Kimi Code contracts match fabro's own.
pub(crate) fn register_discovery_and_web_tools(
    registry: &mut ToolRegistry,
    options: &NativeToolOptions,
    summarizer: Option<WebFetchSummarizer>,
) {
    registry.register(make_glob_tool());
    register_web_search_tool(registry, options);
    registry.register(make_web_fetch_tool(summarizer));
}

/// Register `web_search` when a search provider credential is configured.
///
/// Separate from [`register_discovery_and_web_tools`] for profiles that offer
/// search without fabro's discovery tools.
pub(crate) fn register_web_search_tool(registry: &mut ToolRegistry, options: &NativeToolOptions) {
    if let Some(backend) = SearchBackend::from_secrets(&options.secrets) {
        registry.register(make_web_search_tool(backend));
    }
}

pub(crate) fn required_str<'a>(args: &'a serde_json::Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("Missing required parameter: {key}"))
}

pub(crate) fn optional_usize_arg(
    args: &serde_json::Value,
    key: &str,
) -> Result<Option<usize>, String> {
    args.get(key)
        .and_then(serde_json::Value::as_u64)
        .map(|value| {
            usize::try_from(value).map_err(|_| format!("Parameter {key} is too large: {value}"))
        })
        .transpose()
}

#[must_use]
pub fn make_read_file_tool() -> RegisteredTool {
    RegisteredTool {
        definition: ToolDefinition {
            name:        "read_file".into(),
            description: "Read files before editing them. Returns line-numbered text and supports offset/limit for large files. Use this instead of shell cat, head, tail, or sed when inspecting repository files.".into(),
            parameters:  serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": {"type": "string", "description": "Absolute path to the file"},
                    "offset": {"type": "integer", "description": "1-based line number to start reading from"},
                    "limit": {"type": "integer", "description": "Number of lines to read (default 2000)"}
                },
                "required": ["file_path"]
            }),
        },
        executor:   Arc::new(|args, ctx| {
            Box::pin(async move {
                let file_path = required_str(&args, "file_path")?;
                let offset_usize = optional_usize_arg(&args, "offset")?;
                let limit_usize =
                    optional_usize_arg(&args, "limit")?.or(Some(DEFAULT_READ_LINES));

                let content = ctx
                    .env
                    .read_file(file_path, offset_usize, limit_usize)
                    .await
                    .map_err(|e| e.display_with_causes())?;
                Ok(content)
            })
        }),
        source:     ToolSource::Native,
    }
}

#[must_use]
pub fn make_write_file_tool() -> RegisteredTool {
    RegisteredTool {
        definition: ToolDefinition {
            name:        "write_file".into(),
            description: "Create new files, or overwrite an existing file only when replacement is explicitly intended. Prefer edit_file for targeted changes to existing files because write_file overwrites the full file content.".into(),
            parameters:  serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": {"type": "string", "description": "Absolute path to the file"},
                    "content": {"type": "string", "description": "Content to write to the file"}
                },
                "required": ["file_path", "content"]
            }),
        },
        executor:   Arc::new(|args, ctx| {
            Box::pin(async move {
                let file_path = required_str(&args, "file_path")?;
                let content = required_str(&args, "content")?;

                // Serialize same-file writes within one parallel batch so a
                // write_file cannot interleave with an edit_file's
                // read-modify-write span on the same path.
                let _write_guard = match ctx.write_locks.as_ref() {
                    Some(locks) => Some(write_locks::lock_write_path(locks, file_path).await),
                    None => None,
                };

                ctx.env
                    .write_file(file_path, content)
                    .await
                    .map_err(|e| e.display_with_causes())?;
                Ok(format!("Successfully wrote to {file_path}"))
            })
        }),
        source:     ToolSource::Native,
    }
}

#[must_use]
pub fn make_edit_file_tool() -> RegisteredTool {
    RegisteredTool {
        definition: ToolDefinition {
            name:        "edit_file".into(),
            description: "Edit a file by replacing an exact string. The old_string must be an exact match and unique unless replace_all is true; include surrounding context when needed. Read the file first and preserve existing indentation.".into(),
            parameters:  serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": {"type": "string", "description": "Absolute path to the file"},
                    "old_string": {"type": "string", "description": "The string to find and replace"},
                    "new_string": {"type": "string", "description": "The replacement string"},
                    "replace_all": {"type": "boolean", "description": "Replace all occurrences (default false)"}
                },
                "required": ["file_path", "old_string", "new_string"]
            }),
        },
        executor:   Arc::new(|args, ctx| {
            Box::pin(async move {
                let file_path = required_str(&args, "file_path")?;
                let old_string = required_str(&args, "old_string")?;
                let new_string = required_str(&args, "new_string")?;
                let replace_all = args
                    .get("replace_all")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);

                // Hold the per-path write lock across the whole
                // read-modify-write span: parallel edit_file calls to the
                // same file otherwise read identical base content and the
                // last write silently discards the other call's edit.
                let _write_guard = match ctx.write_locks.as_ref() {
                    Some(locks) => Some(write_locks::lock_write_path(locks, file_path).await),
                    None => None,
                };

                let raw_content = ctx
                    .env
                    .read_file_text(file_path)
                    .await
                    .map_err(|e| e.display_with_causes())?;

                let count = raw_content.matches(old_string).count();
                if count == 0 {
                    return Err("old_string not found in file".to_string());
                }
                if count > 1 && !replace_all {
                    return Err(format!(
                        "old_string is not unique in file (found {count} occurrences). Use replace_all or provide more context"
                    ));
                }

                let new_content = if replace_all {
                    raw_content.replace(old_string, new_string)
                } else {
                    raw_content.replacen(old_string, new_string, 1)
                };

                ctx.env
                    .write_existing_file(file_path, &new_content)
                    .await
                    .map_err(|e| e.display_with_causes())?;
                Ok(format!("Successfully edited {file_path}"))
            })
        }),
        source:     ToolSource::Native,
    }
}

#[must_use]
pub fn make_shell_tool() -> RegisteredTool {
    make_shell_tool_with_options(&NativeToolOptions::default())
}

#[must_use]
pub fn make_shell_tool_with_options(options: &NativeToolOptions) -> RegisteredTool {
    let default_timeout = options.default_command_timeout_ms;
    let max_timeout = options.max_command_timeout_ms;
    RegisteredTool {
        definition: ToolDefinition {
            name:        "shell".into(),
            description: "Execute Bash commands for terminal operations, package managers, tests and builds. Use dedicated tools for file reads, file edits, filename searches, and content searches. Provide timeout_ms for long-running commands.".into(),
            parameters:  serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "Bash source to evaluate, run by a non-login Bash shell"},
                    "timeout_ms": {"type": "integer", "description": "Timeout in milliseconds"},
                    "description": {"type": "string", "description": "Description of what this command does"}
                },
                "required": ["command"]
            }),
        },
        executor:   Arc::new(move |args, ctx| {
            Box::pin(async move {
                let command = required_str(&args, "command")?;
                let timeout_ms = args
                    .get("timeout_ms")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(default_timeout)
                    .min(max_timeout);

                run_shell_command(&ctx, command, timeout_ms, None).await
            })
        }),
        source:     ToolSource::Native,
    }
}

/// Prefix for shell failures that never produced an `ExecResult`, so the model
/// can distinguish missing process diagnostics from a reported process failure.
const SHELL_NO_PROCESS_RESULT: &str = "Shell command produced no process result";

/// Execute a shell command with the session's environment and cancellation
/// plumbing. Provider profiles can vary their wire schema and result
/// rendering without accidentally bypassing those shared semantics.
pub(crate) async fn execute_shell_command(
    ctx: &ToolContext,
    command: &str,
    timeout_ms: u64,
    cwd: Option<&str>,
) -> Result<ExecStreamingResult, String> {
    let tool_env = ctx
        .resolve_tool_env()
        .await
        .map_err(|e| format!("{SHELL_NO_PROCESS_RESULT}: {e:#}"))?;
    tracing::debug!(
        env_var_count = tool_env.as_ref().map_or(0, std::collections::HashMap::len),
        "Injecting sandbox env vars into tool execution"
    );
    ctx.env
        .exec_command_streaming(crate::ExecStreamingRequest {
            timeout_ms: Some(timeout_ms),
            working_dir: cwd,
            env_vars: tool_env.as_ref(),
            cancel_token: Some(ctx.cancel.clone()),
            ..crate::ExecStreamingRequest::new(command)
        })
        .await
        .map_err(|e| format!("{SHELL_NO_PROCESS_RESULT}: {}", e.display_with_causes()))
}

/// Execute a shell command, render its standard model-facing output, and emit
/// the subordinate process result.
pub(crate) async fn run_shell_command(
    ctx: &ToolContext,
    command: &str,
    timeout_ms: u64,
    cwd: Option<&str>,
) -> Result<String, String> {
    let streaming = execute_shell_command(ctx, command, timeout_ms, cwd).await?;
    let text = render_shell_result(&streaming);
    let is_success = streaming.result.is_success();
    emit_shell_process_completed(ctx, streaming).await;

    if is_success { Ok(text) } else { Err(text) }
}

/// Emit the subordinate process outcome after model-facing output has been
/// rendered. Consumes the raw result so redaction does not require cloning
/// potentially large process output.
pub(crate) async fn emit_shell_process_completed(
    ctx: &ToolContext,
    streaming: ExecStreamingResult,
) {
    if ctx.agent_event_emitter.is_none() {
        return;
    }

    let exit_code = streaming.result.exit_code;
    let termination = streaming.result.termination;
    let duration_ms = streaming.result.duration_ms;
    let streams_separated = streaming.streams_separated;
    let result = streaming.result;
    let exec_output_tail =
        match task::spawn_blocking(move || result.default_redacted_output_tail()).await {
            Ok(exec_output_tail) => exec_output_tail,
            Err(err) => {
                tracing::warn!(
                    error = ?err,
                    "Failed to redact shell process output tail"
                );
                None
            }
        };
    ctx.emit_agent_event(AgentEvent::ToolProcessCompleted {
        exit_code,
        termination,
        duration_ms,
        streams_separated,
        exec_output_tail,
    });
}

/// Renders the model-facing shell result: termination, exit code, duration,
/// and provider-honest output sections. Metadata stays at the head and
/// `stderr` at the tail so head/tail truncation preserves both.
fn render_shell_result(streaming: &ExecStreamingResult) -> String {
    let result = &streaming.result;
    let mut output = format!(
        "Termination: {}\nExit code: {}\nDuration: {}ms\n",
        result.termination.as_str(),
        result
            .exit_code
            .map_or_else(|| "none".to_string(), |code| code.to_string()),
        result.duration_ms,
    );
    if streaming.streams_separated {
        if !result.stdout.is_empty() {
            let _ = write!(output, "stdout:\n{}\n", result.stdout);
        }
        if !result.stderr.is_empty() {
            let _ = write!(output, "stderr:\n{}\n", result.stderr);
        }
    } else if !result.stdout.is_empty() {
        let _ = write!(output, "output (combined):\n{}\n", result.stdout);
    }
    output
}

#[must_use]
pub fn make_grep_tool() -> RegisteredTool {
    RegisteredTool {
        definition: ToolDefinition {
            name:        "grep".into(),
            description: "Search file contents with a regex pattern. Use path to choose the search root, glob_filter to limit matching files, case_insensitive for case folding, and max_results to cap output.".into(),
            parameters:  serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Regex pattern to search for"},
                    "path": {"type": "string", "description": "Path to search in (default \".\")"},
                    "glob_filter": {"type": "string", "description": "Glob pattern to filter files"},
                    "case_insensitive": {"type": "boolean", "description": "Case insensitive search"},
                    "max_results": {"type": "integer", "description": "Maximum number of results"}
                },
                "required": ["pattern"]
            }),
        },
        executor:   Arc::new(|args, ctx| {
            Box::pin(async move {
                let pattern = required_str(&args, "pattern")?;
                let path = args
                    .get("path")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(".");

                let max_results = args.get("max_results").and_then(serde_json::Value::as_u64);
                let max_results = max_results
                    .map(|value| {
                        usize::try_from(value)
                            .map_err(|_| format!("Parameter max_results is too large: {value}"))
                    })
                    .transpose()?;
                let options = GrepOptions {
                    glob_filter: args
                        .get("glob_filter")
                        .and_then(serde_json::Value::as_str)
                        .map(String::from),
                    case_insensitive: args
                        .get("case_insensitive")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                    max_results,
                };

                let results = execute_grep(&ctx, pattern, path, &options).await?;
                Ok(results.join("\n"))
            })
        }),
        source:     ToolSource::Native,
    }
}

/// Run a content search, rendering sandbox failures as tool-result strings.
///
/// Shared by the canonical `grep` tool and the Kimi profile's `Grep`, which
/// group the same result lines differently.
pub(crate) async fn execute_grep(
    ctx: &ToolContext,
    pattern: &str,
    path: &str,
    options: &GrepOptions,
) -> Result<Vec<String>, String> {
    ctx.env
        .grep(pattern, path, options)
        .await
        .map_err(|e| e.display_with_causes())
}

/// Extract the file path from `<path>:<line>:<content>` grep output.
///
/// A search of one concrete file may omit `<path>`, in which case the searched
/// path itself is returned. Candidate separators are walked so paths that
/// contain colons (including Windows drive prefixes) still parse correctly.
pub(crate) fn grep_result_path<'a>(line: &'a str, searched: &'a str) -> &'a str {
    let mut rest = line;
    let mut consumed = 0usize;
    while let Some(index) = rest.find(':') {
        let after = &rest[index + 1..];
        let digit_count = after.chars().take_while(char::is_ascii_digit).count();
        if digit_count > 0 && after[digit_count..].starts_with(':') {
            return &line[..consumed + index];
        }
        consumed += index + 1;
        rest = after;
    }
    searched
}

#[must_use]
pub fn make_glob_tool() -> RegisteredTool {
    RegisteredTool {
        definition: ToolDefinition {
            name:        "glob".into(),
            description: "Find files by search-root-relative path using a glob pattern. Use path to choose the search root. `*` stays within one path segment and `**` searches recursively. Prefer this over shell find or ls when locating repository files.".into(),
            parameters:  serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Glob pattern relative to the search root"},
                    "path": {"type": "string", "description": "Directory to search in (default: working directory)"}
                },
                "required": ["pattern"]
            }),
        },
        executor:   Arc::new(|args, ctx| {
            Box::pin(async move {
                let pattern = required_str(&args, "pattern")?;
                let path = args.get("path").and_then(serde_json::Value::as_str);

                let results = ctx
                    .env
                    .glob(pattern, path)
                    .await
                    .map_err(|e| e.display_with_causes())?;
                Ok(results.join("\n"))
            })
        }),
        source:     ToolSource::Native,
    }
}

#[must_use]
pub(crate) fn make_read_many_files_tool() -> RegisteredTool {
    RegisteredTool {
        definition: ToolDefinition {
            name:        "read_many_files".into(),
            description: "Read multiple files at once".into(),
            parameters:  serde_json::json!({
                "type": "object",
                "properties": {
                    "paths": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Array of absolute file paths to read"
                    }
                },
                "required": ["paths"]
            }),
        },
        executor:   Arc::new(|args, ctx| {
            Box::pin(async move {
                let paths: Vec<String> = args["paths"]
                    .as_array()
                    .ok_or_else(|| "paths must be an array".to_string())?
                    .iter()
                    .map(|p| {
                        p.as_str()
                            .ok_or_else(|| "each path must be a string".to_string())
                            .map(str::to_string)
                    })
                    .collect::<Result<_, _>>()?;

                let results = stream::iter(paths)
                    .map(|path| {
                        let env = Arc::clone(&ctx.env);
                        async move {
                            let result = env.read_file(&path, None, None).await;
                            (path, result)
                        }
                    })
                    .buffered(MAX_READ_MANY_FILES_CONCURRENCY)
                    .collect::<Vec<_>>()
                    .await;

                let mut output = String::new();
                for (path, result) in results {
                    match result {
                        Ok(content) => {
                            let _ = write!(output, "=== {path} ===\n{content}\n\n");
                        }
                        Err(err) => {
                            let _ = write!(output, "=== {path} ===\nError: {err}\n\n");
                        }
                    }
                }
                Ok(output)
            })
        }),
        source:     ToolSource::Native,
    }
}

#[must_use]
pub(crate) fn make_list_dir_tool() -> RegisteredTool {
    RegisteredTool {
        definition: ToolDefinition {
            name:        "list_dir".into(),
            description: "List directory contents with depth control".into(),
            parameters:  serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Directory path to list"},
                    "depth": {"type": "integer", "description": "Depth of listing (default 1)"}
                },
                "required": ["path"]
            }),
        },
        executor:   Arc::new(|args, ctx| {
            Box::pin(async move {
                let path = required_str(&args, "path")?;
                let depth = optional_usize_arg(&args, "depth")?;

                let entries = ctx
                    .env
                    .list_directory(path, depth)
                    .await
                    .map_err(|e| e.display_with_causes())?;
                let lines: Vec<String> = entries
                    .iter()
                    .map(|e| {
                        if e.is_dir {
                            format!("{}/", e.name)
                        } else {
                            e.name.clone()
                        }
                    })
                    .collect();
                Ok(lines.join("\n"))
            })
        }),
        source:     ToolSource::Native,
    }
}

#[must_use]
pub(crate) fn make_web_fetch_tool(summarizer: Option<WebFetchSummarizer>) -> RegisteredTool {
    RegisteredTool {
        definition: ToolDefinition {
            name: "web_fetch".into(),
            description: "Fetch content from a URL that starts with http:// or https://. Pass a prompt to extract specific information or summarize the page; omit prompt to return the page content.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {"type": "string", "description": "URL to fetch (must be http:// or https://)"},
                    "prompt": {"type": "string", "description": "A question or instruction about the page content. When provided, returns a concise answer instead of the full page."},
                    "timeout_ms": {"type": "integer", "description": "Timeout in milliseconds (default 30000, max 60000)"}
                },
                "required": ["url"]
            }),
        },
        executor: Arc::new(move |args, ctx| {
            let summarizer = summarizer.clone();
            Box::pin(async move {
                let url = required_str(&args, "url")?;
                let prompt = args.get("prompt").and_then(serde_json::Value::as_str);
                let timeout_ms = args
                    .get("timeout_ms")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(30_000)
                    .min(60_000);

                if !url.starts_with("http://") && !url.starts_with("https://") {
                    return Err("URL must start with http:// or https://".to_string());
                }

                let timeout_secs = timeout_ms.div_ceil(1000);
                let escaped_url = shell_escape::escape(Cow::Borrowed(url));
                let command = format!(
                    "curl -sL --max-time {timeout_secs} -H 'User-Agent: fabro-agent/0.1' {escaped_url}"
                );

                let tool_env = ctx.resolve_tool_env().await.map_err(|e| format!("{e:#}"))?;
                let result = ctx
                    .env
                    .exec_command(
                        &command,
                        timeout_ms,
                        None,
                        tool_env.as_ref(),
                        Some(ctx.cancel),
                    )
                    .await
                    .map_err(|e| e.display_with_causes())?;

                if !result.is_success() {
                    return Err(format!(
                        "curl failed (exit code {}): {}",
                        result.display_exit_code(),
                        result.stderr.trim()
                    ));
                }

                let mut content = html_to_markdown(&result.stdout);
                if content.len() > MAX_WEB_FETCH_BYTES {
                    content.truncate(MAX_WEB_FETCH_BYTES);
                    content.push_str("\n\n[Output truncated at 100KB]");
                }

                match (prompt, &summarizer) {
                    (Some(user_prompt), Some(s)) => {
                        let summarization_prompt = format!(
                            "Content from {url}:\n---\n{content}\n---\n\n{user_prompt}\n\nRespond concisely based only on the content above."
                        );
                        let request = Request {
                            model: s.model_id.model_id().to_string(),
                            messages: vec![Message::user(summarization_prompt)],
                            provider: Some(s.model_id.provider().to_string()),
                            tools: None,
                            tool_choice: None,
                            response_format: None,
                            temperature: None,
                            top_p: None,
                            max_tokens: None,
                            stop_sequences: None,
                            reasoning_effort: None,
                            speed: None,
                            metadata: None,
                            provider_options: None,
                        };
                        let response = s.client.complete(&request).await.map_err(|e| {
                            format!("web_fetch summarization (model={}) failed: {e}", s.model_id.model_id())
                        })?;
                        Ok(response.text())
                    }
                    (Some(_), None) => {
                        // Graceful degradation: return content with a note
                        Ok(format!("[Note: prompt summarization unavailable, returning full content]\n\n{content}"))
                    }
                    (None, _) => Ok(content),
                }
            })
        }),
        source: ToolSource::Native,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use fabro_llm::provider::ProviderAdapter;
    use fabro_model::ProviderId;
    use fabro_types::CommandTermination;
    use tokio::sync::broadcast;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::config::{NativeToolOptions, SessionOptions, ToolSecrets};
    use crate::event::{Emitter, SessionBoundEmitter};
    use crate::local_sandbox::LocalSandbox;
    use crate::sandbox::*;
    use crate::test_support::{MockSandbox, SlowWriteMockSandbox};
    use crate::tool_registry::ToolContext;
    use crate::truncation;
    use crate::types::SessionEvent;
    use crate::web_search::make_web_search_tool_with_api_key;
    use crate::write_locks::{self, BatchWriteLocks};

    #[test]
    fn core_tool_descriptions_include_actionable_guidance() {
        let options = NativeToolOptions::default();
        let tools = [
            make_read_file_tool(),
            make_write_file_tool(),
            make_edit_file_tool(),
            make_shell_tool_with_options(&options),
            make_grep_tool(),
            make_glob_tool(),
            make_web_fetch_tool(None),
        ];
        let description = |name: &str| {
            tools
                .iter()
                .find(|tool| tool.definition.name == name)
                .unwrap_or_else(|| panic!("missing tool {name}"))
                .definition
                .description
                .as_str()
        };

        assert!(description("read_file").contains("Read files before editing"));
        assert!(description("read_file").contains("offset"));
        assert!(description("write_file").contains("new files"));
        assert!(description("write_file").contains("overwrites"));
        assert!(description("edit_file").contains("exact match"));
        assert!(description("edit_file").contains("unique"));
        assert!(description("shell").contains("tests and builds"));
        assert!(description("shell").contains("timeout_ms"));
        assert!(description("grep").contains("regex"));
        assert!(description("grep").contains("glob_filter"));
        assert!(description("glob").contains("search-root-relative"));
        assert!(description("glob").contains("`**` searches recursively"));
        assert!(description("web_fetch").contains("http:// or https://"));
        assert!(description("web_fetch").contains("prompt"));

        for tool in tools {
            let text = &tool.definition.description;
            assert!(
                !text.contains("addComment"),
                "unsupported comment API in {text}"
            );
            assert!(
                !text.contains("background Bash"),
                "unsupported background Bash guidance in {text}"
            );
            assert!(!text.contains("PDF"), "unsupported PDF reads in {text}");
            assert!(!text.contains("image"), "unsupported image reads in {text}");
        }
    }

    /// The `shell` tool's wire shape is deliberately unchanged by the Bash
    /// contract: only its prose became explicit about the interpreter. A model
    /// that learned `shell({command, timeout_ms, description})` must keep
    /// seeing exactly that.
    #[test]
    fn shell_tool_schema_is_unchanged_and_names_bash() {
        let tool = make_shell_tool();

        assert_eq!(tool.definition.name, "shell");
        assert_eq!(
            tool.definition.parameters,
            serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "Bash source to evaluate, run by a non-login Bash shell"},
                    "timeout_ms": {"type": "integer", "description": "Timeout in milliseconds"},
                    "description": {"type": "string", "description": "Description of what this command does"}
                },
                "required": ["command"]
            })
        );
        assert!(
            tool.definition.description.contains("Bash"),
            "the shell tool should identify its interpreter: {}",
            tool.definition.description
        );
    }

    #[tokio::test]
    async fn read_file_returns_content() {
        let tool = make_read_file_tool();
        let mut files = HashMap::new();
        files.insert("/test.txt".into(), "hello\nworld".into());
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox {
            files,
            ..Default::default()
        });
        let result = (tool.executor)(serde_json::json!({"file_path": "/test.txt"}), ToolContext {
            write_locks: None,
            env,
            cancel: CancellationToken::new(),
            tool_env_provider: None,
            session_id: None,
            root_session_id: None,
            tool_call_id: None,
            agent_event_emitter: None,
        })
        .await;
        assert_eq!(result.unwrap(), "1 | hello\n2 | world\n");
    }

    #[tokio::test]
    async fn read_file_applies_the_documented_default_limit() {
        let tool = make_read_file_tool();
        let content = (1..=DEFAULT_READ_LINES + 1)
            .map(|line| format!("line{line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox {
            files: HashMap::from([("/test.txt".to_string(), content)]),
            ..Default::default()
        });

        let result = (tool.executor)(serde_json::json!({"file_path": "/test.txt"}), ToolContext {
            write_locks: None,
            env,
            cancel: CancellationToken::new(),
            tool_env_provider: None,
            session_id: None,
            root_session_id: None,
            tool_call_id: None,
            agent_event_emitter: None,
        })
        .await
        .unwrap();

        assert!(result.contains("2000 | line2000"), "{result}");
        assert!(!result.contains("2001 | line2001"), "{result}");
    }

    #[tokio::test]
    async fn read_file_with_offset_and_limit() {
        let tool = make_read_file_tool();
        let mut files = HashMap::new();
        files.insert("/test.txt".into(), "line1\nline2\nline3\nline4".into());
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox {
            files,
            ..Default::default()
        });
        let result = (tool.executor)(
            serde_json::json!({"file_path": "/test.txt", "offset": 2, "limit": 2}),
            ToolContext {
                write_locks: None,
                env,
                cancel: CancellationToken::new(),
                tool_env_provider: None,
                session_id: None,
                root_session_id: None,
                tool_call_id: None,
                agent_event_emitter: None,
            },
        )
        .await;
        assert_eq!(result.unwrap(), "2 | line2\n3 | line3\n");
    }

    fn locked_ctx(env: Arc<dyn Sandbox>, locks: &BatchWriteLocks) -> ToolContext {
        ToolContext {
            env,
            cancel: CancellationToken::new(),
            tool_env_provider: None,
            write_locks: Some(locks.clone()),
            session_id: None,
            root_session_id: None,
            tool_call_id: None,
            agent_event_emitter: None,
        }
    }

    #[tokio::test]
    async fn parallel_edit_file_calls_to_same_file_apply_both_edits() {
        // Reproduces run 01M0NJZGX7BMJAJC3CZJZ0RNT0: two edit_file calls to
        // one file in a single parallel batch read identical base content and
        // the last write silently discarded the other edit. The per-path
        // write lock serializes the read-modify-write span so both land.
        // The slow writes force the interleaving instant mocks would hide.
        let tool = make_edit_file_tool();
        let env = Arc::new(SlowWriteMockSandbox::new(
            HashMap::from([("/main.go".to_string(), "alpha beta".to_string())]),
            25,
        ));
        let locks = write_locks::new_batch_write_locks();

        let first = (tool.executor)(
            serde_json::json!({
                "file_path": "/main.go",
                "old_string": "alpha",
                "new_string": "ALPHA"
            }),
            locked_ctx(env.clone() as Arc<dyn Sandbox>, &locks),
        );
        let second = (tool.executor)(
            serde_json::json!({
                "file_path": "/main.go",
                "old_string": "beta",
                "new_string": "BETA"
            }),
            locked_ctx(env.clone() as Arc<dyn Sandbox>, &locks),
        );
        let (first, second) = tokio::join!(first, second);

        assert!(first.is_ok(), "first edit failed: {:?}", first.err());
        assert!(second.is_ok(), "second edit failed: {:?}", second.err());
        let content = env.inner.read_file_text("/main.go").await.unwrap();
        assert_eq!(content, "ALPHA BETA");
    }

    #[tokio::test]
    async fn parallel_edit_file_calls_to_different_files_still_apply() {
        let tool = make_edit_file_tool();
        let env = Arc::new(SlowWriteMockSandbox::new(
            HashMap::from([
                ("/a.go".to_string(), "alpha".to_string()),
                ("/b.go".to_string(), "beta".to_string()),
            ]),
            25,
        ));
        let locks = write_locks::new_batch_write_locks();

        let first = (tool.executor)(
            serde_json::json!({
                "file_path": "/a.go",
                "old_string": "alpha",
                "new_string": "ALPHA"
            }),
            locked_ctx(env.clone() as Arc<dyn Sandbox>, &locks),
        );
        let second = (tool.executor)(
            serde_json::json!({
                "file_path": "/b.go",
                "old_string": "beta",
                "new_string": "BETA"
            }),
            locked_ctx(env.clone() as Arc<dyn Sandbox>, &locks),
        );
        let (first, second) = tokio::join!(first, second);

        assert!(first.is_ok(), "first edit failed: {:?}", first.err());
        assert!(second.is_ok(), "second edit failed: {:?}", second.err());
        assert_eq!(env.inner.read_file_text("/a.go").await.unwrap(), "ALPHA");
        assert_eq!(env.inner.read_file_text("/b.go").await.unwrap(), "BETA");
    }

    #[tokio::test]
    async fn write_file_calls_env() {
        let tool = make_write_file_tool();
        let env = Arc::new(MockSandbox::default());
        let env_clone: Arc<dyn Sandbox> = env.clone();
        let result = (tool.executor)(
            serde_json::json!({"file_path": "/out.txt", "content": "hello"}),
            ToolContext {
                write_locks:         None,
                env:                 env_clone,
                cancel:              CancellationToken::new(),
                tool_env_provider:   None,
                session_id:          None,
                root_session_id:     None,
                tool_call_id:        None,
                agent_event_emitter: None,
            },
        )
        .await;
        assert_eq!(result.unwrap(), "Successfully wrote to /out.txt");
        assert_eq!(env.existing_file_write_count(), 0);
        let written = env.written_files.lock().unwrap();
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].0, "/out.txt");
        assert_eq!(written[0].1, "hello");
    }

    #[tokio::test]
    async fn edit_file_replaces_match() {
        let tool = make_edit_file_tool();
        let mut files = HashMap::new();
        files.insert("/f.txt".into(), "hello world".into());
        let env = Arc::new(MockSandbox {
            files,
            ..Default::default()
        });
        let env_clone: Arc<dyn Sandbox> = env.clone();
        let result = (tool.executor)(
            serde_json::json!({
                "file_path": "/f.txt",
                "old_string": "hello",
                "new_string": "goodbye"
            }),
            ToolContext {
                write_locks:         None,
                env:                 env_clone,
                cancel:              CancellationToken::new(),
                tool_env_provider:   None,
                session_id:          None,
                root_session_id:     None,
                tool_call_id:        None,
                agent_event_emitter: None,
            },
        )
        .await;
        assert_eq!(result.unwrap(), "Successfully edited /f.txt");
        assert_eq!(env.existing_file_write_count(), 1);
        let written = env.written_files.lock().unwrap();
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].1, "goodbye world");
    }

    #[tokio::test]
    async fn edit_file_not_found_error() {
        let tool = make_edit_file_tool();
        let mut files = HashMap::new();
        files.insert("/f.txt".into(), "hello world".into());
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox {
            files,
            ..Default::default()
        });
        let result = (tool.executor)(
            serde_json::json!({
                "file_path": "/f.txt",
                "old_string": "missing",
                "new_string": "replacement"
            }),
            ToolContext {
                write_locks: None,
                env,
                cancel: CancellationToken::new(),
                tool_env_provider: None,
                session_id: None,
                root_session_id: None,
                tool_call_id: None,
                agent_event_emitter: None,
            },
        )
        .await;
        assert_eq!(result.unwrap_err(), "old_string not found in file");
    }

    #[tokio::test]
    async fn edit_file_not_unique_error() {
        let tool = make_edit_file_tool();
        let mut files = HashMap::new();
        files.insert("/f.txt".into(), "aa bb aa".into());
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox {
            files,
            ..Default::default()
        });
        let result = (tool.executor)(
            serde_json::json!({
                "file_path": "/f.txt",
                "old_string": "aa",
                "new_string": "cc"
            }),
            ToolContext {
                write_locks: None,
                env,
                cancel: CancellationToken::new(),
                tool_env_provider: None,
                session_id: None,
                root_session_id: None,
                tool_call_id: None,
                agent_event_emitter: None,
            },
        )
        .await;
        let err = result.unwrap_err();
        assert!(err.contains("not unique"));
        assert!(err.contains("2 occurrences"));
    }

    #[tokio::test]
    async fn edit_file_replace_all() {
        let tool = make_edit_file_tool();
        let mut files = HashMap::new();
        files.insert("/f.txt".into(), "aa bb aa".into());
        let env = Arc::new(MockSandbox {
            files,
            ..Default::default()
        });
        let env_clone: Arc<dyn Sandbox> = env.clone();
        let result = (tool.executor)(
            serde_json::json!({
                "file_path": "/f.txt",
                "old_string": "aa",
                "new_string": "cc",
                "replace_all": true
            }),
            ToolContext {
                write_locks:         None,
                env:                 env_clone,
                cancel:              CancellationToken::new(),
                tool_env_provider:   None,
                session_id:          None,
                root_session_id:     None,
                tool_call_id:        None,
                agent_event_emitter: None,
            },
        )
        .await;
        assert_eq!(result.unwrap(), "Successfully edited /f.txt");
        let written = env.written_files.lock().unwrap();
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].1, "cc bb cc");
    }

    #[tokio::test]
    async fn edit_file_preserves_literal_line_number_prefixes() {
        let tool = make_edit_file_tool();
        let mut files = HashMap::new();
        files.insert("/f.txt".into(), "1 | keep this literal\nhello".into());
        let env = Arc::new(MockSandbox {
            files,
            ..Default::default()
        });
        let env_clone: Arc<dyn Sandbox> = env.clone();
        let result = (tool.executor)(
            serde_json::json!({
                "file_path": "/f.txt",
                "old_string": "hello",
                "new_string": "goodbye"
            }),
            ToolContext {
                write_locks:         None,
                env:                 env_clone,
                cancel:              CancellationToken::new(),
                tool_env_provider:   None,
                session_id:          None,
                root_session_id:     None,
                tool_call_id:        None,
                agent_event_emitter: None,
            },
        )
        .await;
        assert_eq!(result.unwrap(), "Successfully edited /f.txt");
        let written = env.written_files.lock().unwrap();
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].1, "1 | keep this literal\ngoodbye");
    }

    fn shell_context(env: Arc<dyn Sandbox>) -> ToolContext {
        ToolContext {
            write_locks: None,
            env,
            cancel: CancellationToken::new(),
            tool_env_provider: None,
            session_id: None,
            root_session_id: None,
            tool_call_id: None,
            agent_event_emitter: None,
        }
    }

    fn shell_context_with_emitter(env: Arc<dyn Sandbox>, emitter: &Emitter) -> ToolContext {
        ToolContext {
            write_locks: None,
            session_id: Some("test-session".to_string()),
            root_session_id: Some("test-session".to_string()),
            tool_call_id: Some("call_1".to_string()),
            agent_event_emitter: Some(Arc::new(SessionBoundEmitter {
                emitter:      emitter.clone(),
                session_id:   "test-session".to_string(),
                tool_call_id: Some("call_1".to_string()),
            })),
            ..shell_context(env)
        }
    }

    fn only_process_event(receiver: &mut broadcast::Receiver<SessionEvent>) -> AgentEvent {
        let event = receiver.try_recv().expect("one process event");
        assert_eq!(event.session_id, "test-session");
        assert_eq!(event.tool_call_id.as_deref(), Some("call_1"));
        assert!(matches!(
            receiver.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
        event.event
    }

    fn mock_sandbox_with(result: ExecResult) -> Arc<MockSandbox> {
        Arc::new(MockSandbox {
            exec_result: result,
            ..Default::default()
        })
    }

    #[tokio::test]
    async fn shell_success_returns_ok_with_metadata_and_separate_streams() {
        let tool = make_shell_tool();
        let env: Arc<dyn Sandbox> = mock_sandbox_with(ExecResult {
            stdout:      "hello".into(),
            stderr:      "a warning".into(),
            exit_code:   Some(0),
            termination: CommandTermination::Exited,
            duration_ms: 10,
        });
        let output = (tool.executor)(
            serde_json::json!({"command": "echo hello"}),
            shell_context(env),
        )
        .await
        .expect("exit 0 is a successful tool result");

        assert_eq!(
            output,
            "Termination: exited\nExit code: 0\nDuration: 10ms\nstdout:\nhello\nstderr:\na \
             warning\n"
        );
    }

    #[tokio::test]
    async fn shell_forwards_command_without_stream_redirection_wrapper() {
        let tool = make_shell_tool();
        let env = mock_sandbox_with(ExecResult {
            stdout:      String::new(),
            stderr:      String::new(),
            exit_code:   Some(0),
            termination: CommandTermination::Exited,
            duration_ms: 1,
        });
        let _ = (tool.executor)(
            serde_json::json!({"command": "make test"}),
            shell_context(env.clone()),
        )
        .await;

        let captured = env
            .captured_command
            .lock()
            .expect("captured_command lock poisoned")
            .clone();
        assert_eq!(captured.as_deref(), Some("make test"));
    }

    #[tokio::test]
    async fn shell_with_timeout() {
        let tool = make_shell_tool();
        let env = Arc::new(MockSandbox::default());
        let env_clone: Arc<dyn Sandbox> = env.clone();
        let _result = (tool.executor)(
            serde_json::json!({"command": "sleep 1", "timeout_ms": 5000}),
            ToolContext {
                write_locks:         None,
                env:                 env_clone,
                cancel:              CancellationToken::new(),
                tool_env_provider:   None,
                session_id:          None,
                root_session_id:     None,
                tool_call_id:        None,
                agent_event_emitter: None,
            },
        )
        .await;
        assert_eq!(*env.captured_timeout.lock().unwrap(), Some(5000));
    }

    #[tokio::test]
    async fn shell_nonzero_exit_code() {
        let tool = make_shell_tool();
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox {
            exec_result: ExecResult {
                stdout:      "error".into(),
                stderr:      String::new(),
                exit_code:   Some(1),
                termination: CommandTermination::Exited,
                duration_ms: 10,
            },
            ..Default::default()
        });
        let output = (tool.executor)(serde_json::json!({"command": "false"}), shell_context(env))
            .await
            .expect_err("a nonzero exit is a failed tool result");
        assert!(output.contains("Termination: exited"), "got: {output}");
        assert!(output.contains("Exit code: 1"), "got: {output}");
        assert!(output.contains("stdout:\nerror"), "got: {output}");
        assert!(!output.contains("stderr:"), "got: {output}");
    }

    #[tokio::test]
    async fn shell_timeout_returns_error_with_partial_output() {
        let tool = make_shell_tool();
        let env: Arc<dyn Sandbox> = mock_sandbox_with(ExecResult {
            stdout:      "partial".into(),
            stderr:      String::new(),
            exit_code:   None,
            termination: CommandTermination::TimedOut,
            duration_ms: 10000,
        });
        let output = (tool.executor)(
            serde_json::json!({"command": "sleep 100"}),
            shell_context(env),
        )
        .await
        .expect_err("a timeout is a failed tool result");

        assert!(output.contains("Termination: timed_out"), "got: {output}");
        assert!(output.contains("Exit code: none"), "got: {output}");
        assert!(output.contains("stdout:\npartial"), "got: {output}");
    }

    #[tokio::test]
    async fn shell_cancellation_returns_error_with_partial_output() {
        let tool = make_shell_tool();
        let env: Arc<dyn Sandbox> = mock_sandbox_with(ExecResult {
            stdout:      "partial".into(),
            stderr:      String::new(),
            exit_code:   None,
            termination: CommandTermination::Cancelled,
            duration_ms: 42,
        });
        let output = (tool.executor)(
            serde_json::json!({"command": "sleep 100"}),
            shell_context(env),
        )
        .await
        .expect_err("a cancellation is a failed tool result");

        assert!(output.contains("Termination: cancelled"), "got: {output}");
        assert!(output.contains("Exit code: none"), "got: {output}");
        assert!(output.contains("stdout:\npartial"), "got: {output}");
    }

    #[tokio::test]
    async fn shell_sandbox_failure_returns_error_without_a_process_outcome() {
        let tool = make_shell_tool();
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox {
            exec_error: Some("sandbox transport is down".into()),
            ..Default::default()
        });
        let emitter = Emitter::new();
        let mut receiver = emitter.subscribe();

        let output = (tool.executor)(
            serde_json::json!({"command": "make test"}),
            shell_context_with_emitter(env, &emitter),
        )
        .await
        .expect_err("a sandbox transport failure is a failed tool result");

        assert!(
            output.contains("Shell command produced no process result"),
            "got: {output}"
        );
        assert!(
            output.contains("sandbox transport is down"),
            "got: {output}"
        );
        assert!(!output.contains("Exit code"), "got: {output}");
        assert!(matches!(
            receiver.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn shell_emits_process_event_with_typed_outcome_and_redacted_tails() {
        let tool = make_shell_tool();
        let env: Arc<dyn Sandbox> = mock_sandbox_with(ExecResult {
            stdout:      "out".into(),
            stderr:      "boom key=AKIAYRWQG5EJLPZLBYNP".into(),
            exit_code:   Some(7),
            termination: CommandTermination::Exited,
            duration_ms: 12,
        });
        let emitter = Emitter::new();
        let mut receiver = emitter.subscribe();

        let _ = (tool.executor)(
            serde_json::json!({"command": "printf out; printf err >&2; exit 7"}),
            shell_context_with_emitter(env, &emitter),
        )
        .await;

        match only_process_event(&mut receiver) {
            AgentEvent::ToolProcessCompleted {
                exit_code,
                termination,
                duration_ms,
                streams_separated,
                exec_output_tail,
            } => {
                assert_eq!(exit_code, Some(7));
                assert_eq!(termination, CommandTermination::Exited);
                assert_eq!(duration_ms, 12);
                assert!(streams_separated);
                let tail = exec_output_tail.expect("output tail");
                assert_eq!(tail.stdout.as_deref(), Some("out"));
                let stderr = tail.stderr.expect("stderr tail");
                assert!(stderr.contains("boom"), "got: {stderr}");
                assert!(!stderr.contains("AKIAYRWQG5EJLPZLBYNP"), "got: {stderr}");
            }
            other => panic!("expected a process event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn shell_renders_combined_output_when_streams_are_not_separated() {
        let tool = make_shell_tool();
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox {
            exec_result: ExecResult {
                stdout:      "interleaved".into(),
                stderr:      String::new(),
                exit_code:   Some(0),
                termination: CommandTermination::Exited,
                duration_ms: 5,
            },
            streams_separated: false,
            ..Default::default()
        });
        let emitter = Emitter::new();
        let mut receiver = emitter.subscribe();

        let output = (tool.executor)(
            serde_json::json!({"command": "echo interleaved"}),
            shell_context_with_emitter(env, &emitter),
        )
        .await
        .expect("exit 0 is a successful tool result");

        assert!(
            output.contains("output (combined):\ninterleaved"),
            "got: {output}"
        );
        assert!(!output.contains("stderr:"), "got: {output}");
        match only_process_event(&mut receiver) {
            AgentEvent::ToolProcessCompleted {
                streams_separated, ..
            } => assert!(!streams_separated),
            other => panic!("expected a process event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn shell_truncation_preserves_exit_metadata_and_stderr_tail() {
        let tool = make_shell_tool();
        let stdout = (0..400)
            .map(|line| format!("{line}: {}", "x".repeat(100)))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(stdout.len() > 30_000);
        let env: Arc<dyn Sandbox> = mock_sandbox_with(ExecResult {
            stdout,
            stderr: "the build failed".into(),
            exit_code: Some(2),
            termination: CommandTermination::Exited,
            duration_ms: 900,
        });

        let output = (tool.executor)(
            serde_json::json!({"command": "make build"}),
            shell_context(env),
        )
        .await
        .expect_err("a nonzero exit is a failed tool result");
        let truncated =
            truncation::truncate_tool_output(&output, "shell", &SessionOptions::default());

        assert!(truncated.len() < output.len());
        assert!(truncated.starts_with("Termination: exited\nExit code: 2\n"));
        assert!(
            truncated.contains("stderr:\nthe build failed"),
            "stderr tail did not survive truncation"
        );
    }

    /// End-to-end against a real process: the local provider separates the
    /// streams and reports the real exit code, and none of it is laundered
    /// into a successful tool result.
    #[tokio::test]
    async fn shell_reports_real_local_process_outcome() {
        let tool = make_shell_tool();
        let env: Arc<dyn Sandbox> = Arc::new(LocalSandbox::new(
            std::env::current_dir().expect("current dir"),
        ));
        let emitter = Emitter::new();
        let mut receiver = emitter.subscribe();

        let output = (tool.executor)(
            serde_json::json!({"command": "printf 'out'; printf 'err' >&2; exit 7"}),
            shell_context_with_emitter(env, &emitter),
        )
        .await
        .expect_err("exit 7 is a failed tool result");

        assert!(output.contains("Termination: exited"), "got: {output}");
        assert!(output.contains("Exit code: 7"), "got: {output}");
        assert!(output.contains("stdout:\nout"), "got: {output}");
        assert!(output.contains("stderr:\nerr"), "got: {output}");

        match only_process_event(&mut receiver) {
            AgentEvent::ToolProcessCompleted {
                exit_code,
                termination,
                streams_separated,
                exec_output_tail,
                ..
            } => {
                assert_eq!(exit_code, Some(7));
                assert_eq!(termination, CommandTermination::Exited);
                assert!(streams_separated);
                let tail = exec_output_tail.expect("output tail");
                assert_eq!(tail.stdout.as_deref(), Some("out"));
                assert_eq!(tail.stderr.as_deref(), Some("err"));
            }
            other => panic!("expected a process event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn shell_passes_tool_env_to_exec_command() {
        let tool = make_shell_tool();
        let env = Arc::new(MockSandbox::default());
        let env_clone: Arc<dyn Sandbox> = env.clone();
        let mut tool_env = HashMap::new();
        tool_env.insert("MY_KEY".into(), "my_value".into());
        let _result = (tool.executor)(
            serde_json::json!({"command": "echo $MY_KEY"}),
            ToolContext {
                write_locks:         None,
                env:                 env_clone,
                cancel:              CancellationToken::new(),
                tool_env_provider:   Some(Arc::new(crate::StaticEnvProvider(tool_env.clone()))),
                session_id:          None,
                root_session_id:     None,
                tool_call_id:        None,
                agent_event_emitter: None,
            },
        )
        .await;
        let captured = env.captured_env_vars.lock().unwrap().clone();
        assert_eq!(captured, Some(tool_env));
    }

    struct SequenceToolEnvProvider {
        values: std::sync::Mutex<Vec<HashMap<String, String>>>,
    }

    #[async_trait::async_trait]
    impl crate::ToolEnvProvider for SequenceToolEnvProvider {
        async fn resolve(&self) -> anyhow::Result<HashMap<String, String>> {
            Ok(self.values.lock().unwrap().remove(0))
        }
    }

    struct FailingToolEnvProvider;

    #[async_trait::async_trait]
    impl crate::ToolEnvProvider for FailingToolEnvProvider {
        async fn resolve(&self) -> anyhow::Result<HashMap<String, String>> {
            Err(anyhow::anyhow!("GITHUB_TOKEN refresh failed"))
        }
    }

    #[tokio::test]
    async fn shell_resolves_tool_env_for_each_call() {
        let tool = make_shell_tool();
        let env = Arc::new(MockSandbox::default());
        let provider = Arc::new(SequenceToolEnvProvider {
            values: std::sync::Mutex::new(vec![
                HashMap::from([("GITHUB_TOKEN".to_string(), "t1".to_string())]),
                HashMap::from([("GITHUB_TOKEN".to_string(), "t2".to_string())]),
            ]),
        });

        let _result = (tool.executor)(
            serde_json::json!({"command": "echo $GITHUB_TOKEN"}),
            ToolContext {
                write_locks:         None,
                env:                 env.clone(),
                cancel:              CancellationToken::new(),
                tool_env_provider:   Some(provider.clone()),
                session_id:          None,
                root_session_id:     None,
                tool_call_id:        None,
                agent_event_emitter: None,
            },
        )
        .await;
        assert_eq!(
            env.captured_env_vars.lock().unwrap().clone(),
            Some(HashMap::from([(
                "GITHUB_TOKEN".to_string(),
                "t1".to_string()
            )]))
        );

        let _result = (tool.executor)(
            serde_json::json!({"command": "echo $GITHUB_TOKEN"}),
            ToolContext {
                write_locks:         None,
                env:                 env.clone(),
                cancel:              CancellationToken::new(),
                tool_env_provider:   Some(provider),
                session_id:          None,
                root_session_id:     None,
                tool_call_id:        None,
                agent_event_emitter: None,
            },
        )
        .await;
        assert_eq!(
            env.captured_env_vars.lock().unwrap().clone(),
            Some(HashMap::from([(
                "GITHUB_TOKEN".to_string(),
                "t2".to_string()
            )]))
        );
    }

    #[tokio::test]
    async fn shell_returns_provider_error_for_env_resolution_failure() {
        let tool = make_shell_tool();
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox::default());

        let result = (tool.executor)(
            serde_json::json!({"command": "echo $GITHUB_TOKEN"}),
            ToolContext {
                write_locks: None,
                env,
                cancel: CancellationToken::new(),
                tool_env_provider: Some(Arc::new(FailingToolEnvProvider)),
                session_id: None,
                root_session_id: None,
                tool_call_id: None,
                agent_event_emitter: None,
            },
        )
        .await
        .unwrap_err();

        assert!(
            result.contains("GITHUB_TOKEN refresh failed"),
            "got: {result}"
        );
    }

    #[tokio::test]
    async fn read_file_does_not_resolve_failing_tool_env_provider() {
        let tool = make_read_file_tool();
        let mut files = HashMap::new();
        files.insert("/test.txt".into(), "hello".into());
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox {
            files,
            ..Default::default()
        });

        let result = (tool.executor)(serde_json::json!({"file_path": "/test.txt"}), ToolContext {
            write_locks: None,
            env,
            cancel: CancellationToken::new(),
            tool_env_provider: Some(Arc::new(FailingToolEnvProvider)),
            session_id: None,
            root_session_id: None,
            tool_call_id: None,
            agent_event_emitter: None,
        })
        .await;

        assert_eq!(result.unwrap(), "1 | hello\n");
    }

    #[tokio::test]
    async fn shell_passes_none_env_when_tool_env_is_none() {
        let tool = make_shell_tool();
        let env = Arc::new(MockSandbox::default());
        let env_clone: Arc<dyn Sandbox> = env.clone();
        let _result = (tool.executor)(serde_json::json!({"command": "echo hello"}), ToolContext {
            write_locks:         None,
            env:                 env_clone,
            cancel:              CancellationToken::new(),
            tool_env_provider:   None,
            session_id:          None,
            root_session_id:     None,
            tool_call_id:        None,
            agent_event_emitter: None,
        })
        .await;
        let captured = env.captured_env_vars.lock().unwrap().clone();
        assert_eq!(captured, None);
    }

    #[tokio::test]
    async fn web_fetch_passes_tool_env_to_exec_command() {
        let tool = make_web_fetch_tool(None);
        let env = Arc::new(MockSandbox {
            exec_result: ExecResult {
                stdout:      "fetched content".into(),
                stderr:      String::new(),
                exit_code:   Some(0),
                termination: CommandTermination::Exited,
                duration_ms: 100,
            },
            ..Default::default()
        });
        let env_clone: Arc<dyn Sandbox> = env.clone();
        let mut tool_env = HashMap::new();
        tool_env.insert("API_KEY".into(), "secret".into());
        let _result = (tool.executor)(
            serde_json::json!({"url": "https://example.com"}),
            ToolContext {
                write_locks:         None,
                env:                 env_clone,
                cancel:              CancellationToken::new(),
                tool_env_provider:   Some(Arc::new(crate::StaticEnvProvider(tool_env.clone()))),
                session_id:          None,
                root_session_id:     None,
                tool_call_id:        None,
                agent_event_emitter: None,
            },
        )
        .await;
        let captured = env.captured_env_vars.lock().unwrap().clone();
        assert_eq!(captured, Some(tool_env));
    }

    #[tokio::test]
    async fn grep_basic() {
        let tool = make_grep_tool();
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox {
            grep_results: vec![
                "src/main.rs:10:fn main()".into(),
                "src/lib.rs:5:pub fn".into(),
            ],
            ..Default::default()
        });
        let result = (tool.executor)(serde_json::json!({"pattern": "fn"}), ToolContext {
            write_locks: None,
            env,
            cancel: CancellationToken::new(),
            tool_env_provider: None,
            session_id: None,
            root_session_id: None,
            tool_call_id: None,
            agent_event_emitter: None,
        })
        .await;
        let output = result.unwrap();
        assert!(output.contains("src/main.rs:10:fn main()"));
        assert!(output.contains("src/lib.rs:5:pub fn"));
    }

    #[tokio::test]
    async fn glob_basic() {
        let tool = make_glob_tool();
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox {
            glob_results: vec!["src/main.rs".into(), "src/lib.rs".into()],
            ..Default::default()
        });
        let result = (tool.executor)(serde_json::json!({"pattern": "src/**/*.rs"}), ToolContext {
            write_locks: None,
            env,
            cancel: CancellationToken::new(),
            tool_env_provider: None,
            session_id: None,
            root_session_id: None,
            tool_call_id: None,
            agent_event_emitter: None,
        })
        .await;
        let output = result.unwrap();
        assert!(output.contains("src/main.rs"));
        assert!(output.contains("src/lib.rs"));
    }

    #[test]
    fn register_core_tools_omits_web_search_without_api_key() {
        let mut registry = ToolRegistry::new();

        register_core_tools(&mut registry, &NativeToolOptions::default(), None);

        assert!(registry.get("web_search").is_none());
    }

    #[tokio::test]
    async fn web_search_missing_query_returns_error() {
        let tool = make_web_search_tool_with_api_key("fake-key".into());
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox::default());
        let result = (tool.executor)(serde_json::json!({}), ToolContext {
            write_locks: None,
            env,
            cancel: CancellationToken::new(),
            tool_env_provider: None,
            session_id: None,
            root_session_id: None,
            tool_call_id: None,
            agent_event_emitter: None,
        })
        .await;
        let err = result.unwrap_err();
        assert!(
            err.contains("query"),
            "error should mention missing query, got: {err}"
        );
    }

    #[tokio::test]
    async fn register_core_tools_passes_configured_brave_search_key() {
        let mut registry = ToolRegistry::new();
        let options = NativeToolOptions {
            secrets: ToolSecrets {
                brave_search_api_key: Some("fake-key".to_string()),
                ..ToolSecrets::default()
            },
            ..NativeToolOptions::default()
        };

        register_core_tools(&mut registry, &options, None);

        let tool = registry
            .get("web_search")
            .expect("web_search should be registered");
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox::default());
        let result = (tool.executor)(serde_json::json!({}), ToolContext {
            write_locks: None,
            env,
            cancel: CancellationToken::new(),
            tool_env_provider: None,
            session_id: None,
            root_session_id: None,
            tool_call_id: None,
            agent_event_emitter: None,
        })
        .await;

        let err = result.unwrap_err();
        assert!(
            err.contains("query"),
            "configured key should allow validation to reach query parsing, got: {err}"
        );
    }

    #[tokio::test]
    async fn web_fetch_builds_curl_command() {
        let tool = make_web_fetch_tool(None);
        let env = Arc::new(MockSandbox {
            exec_result: ExecResult {
                stdout:      "<html><body><h1>hello</h1></body></html>".into(),
                stderr:      String::new(),
                exit_code:   Some(0),
                termination: CommandTermination::Exited,
                duration_ms: 100,
            },
            ..Default::default()
        });
        let env_clone: Arc<dyn Sandbox> = env.clone();
        let result = (tool.executor)(
            serde_json::json!({"url": "https://example.com"}),
            ToolContext {
                write_locks:         None,
                env:                 env_clone,
                cancel:              CancellationToken::new(),
                tool_env_provider:   None,
                session_id:          None,
                root_session_id:     None,
                tool_call_id:        None,
                agent_event_emitter: None,
            },
        )
        .await;
        let output = result.unwrap();
        assert!(
            output.contains("# hello"),
            "HTML should be converted to markdown, got: {output}"
        );
        assert!(
            !output.contains("<html>"),
            "raw HTML tags should be removed, got: {output}"
        );
        let cmd = env.captured_command.lock().unwrap().clone().unwrap();
        assert!(
            cmd.starts_with("curl -sL --max-time 30 "),
            "command should start with curl flags, got: {cmd}"
        );
        assert!(
            cmd.contains("https://example.com"),
            "command should contain the URL"
        );
        assert!(
            cmd.contains("User-Agent: fabro-agent/0.1"),
            "command should set user agent"
        );
    }

    #[tokio::test]
    async fn web_fetch_rejects_non_http_url() {
        let tool = make_web_fetch_tool(None);
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox::default());
        let result = (tool.executor)(
            serde_json::json!({"url": "ftp://example.com/file"}),
            ToolContext {
                write_locks: None,
                env,
                cancel: CancellationToken::new(),
                tool_env_provider: None,
                session_id: None,
                root_session_id: None,
                tool_call_id: None,
                agent_event_emitter: None,
            },
        )
        .await;
        let err = result.unwrap_err();
        assert!(
            err.contains("http://") || err.contains("https://"),
            "error should mention valid schemes, got: {err}"
        );
    }

    #[tokio::test]
    async fn web_fetch_timeout_flows_through() {
        let tool = make_web_fetch_tool(None);
        let env = Arc::new(MockSandbox::default());
        let env_clone: Arc<dyn Sandbox> = env.clone();
        let _result = (tool.executor)(
            serde_json::json!({"url": "https://example.com", "timeout_ms": 15000}),
            ToolContext {
                write_locks:         None,
                env:                 env_clone,
                cancel:              CancellationToken::new(),
                tool_env_provider:   None,
                session_id:          None,
                root_session_id:     None,
                tool_call_id:        None,
                agent_event_emitter: None,
            },
        )
        .await;
        assert_eq!(*env.captured_timeout.lock().unwrap(), Some(15000));
        let cmd = env.captured_command.lock().unwrap().clone().unwrap();
        assert!(
            cmd.contains("--max-time 15"),
            "curl timeout should be 15 seconds, got: {cmd}"
        );
    }

    #[tokio::test]
    async fn web_fetch_timeout_capped_at_60s() {
        let tool = make_web_fetch_tool(None);
        let env = Arc::new(MockSandbox::default());
        let env_clone: Arc<dyn Sandbox> = env.clone();
        let _result = (tool.executor)(
            serde_json::json!({"url": "https://example.com", "timeout_ms": 120_000}),
            ToolContext {
                write_locks:         None,
                env:                 env_clone,
                cancel:              CancellationToken::new(),
                tool_env_provider:   None,
                session_id:          None,
                root_session_id:     None,
                tool_call_id:        None,
                agent_event_emitter: None,
            },
        )
        .await;
        assert_eq!(*env.captured_timeout.lock().unwrap(), Some(60000));
        let cmd = env.captured_command.lock().unwrap().clone().unwrap();
        assert!(
            cmd.contains("--max-time 60"),
            "curl timeout should be capped at 60 seconds, got: {cmd}"
        );
    }

    #[tokio::test]
    async fn web_fetch_truncates_large_output() {
        let large_content = "x".repeat(150 * 1024);
        let tool = make_web_fetch_tool(None);
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox {
            exec_result: ExecResult {
                stdout:      large_content,
                stderr:      String::new(),
                exit_code:   Some(0),
                termination: CommandTermination::Exited,
                duration_ms: 100,
            },
            ..Default::default()
        });
        let result = (tool.executor)(
            serde_json::json!({"url": "https://example.com"}),
            ToolContext {
                write_locks: None,
                env,
                cancel: CancellationToken::new(),
                tool_env_provider: None,
                session_id: None,
                root_session_id: None,
                tool_call_id: None,
                agent_event_emitter: None,
            },
        )
        .await;
        let output = result.unwrap();
        assert!(output.len() < 110 * 1024, "output should be truncated");
        assert!(output.ends_with("[Output truncated at 100KB]"));
    }

    #[tokio::test]
    async fn web_fetch_returns_error_on_nonzero_exit() {
        let tool = make_web_fetch_tool(None);
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox {
            exec_result: ExecResult {
                stdout:      String::new(),
                stderr:      "curl: (6) Could not resolve host".into(),
                exit_code:   Some(6),
                termination: CommandTermination::Exited,
                duration_ms: 100,
            },
            ..Default::default()
        });
        let result = (tool.executor)(
            serde_json::json!({"url": "https://nonexistent.example.com"}),
            ToolContext {
                write_locks: None,
                env,
                cancel: CancellationToken::new(),
                tool_env_provider: None,
                session_id: None,
                root_session_id: None,
                tool_call_id: None,
                agent_event_emitter: None,
            },
        )
        .await;
        let err = result.unwrap_err();
        assert!(
            err.contains("exit code 6"),
            "error should contain exit code, got: {err}"
        );
        assert!(
            err.contains("Could not resolve host"),
            "error should contain stderr, got: {err}"
        );
    }

    #[tokio::test]
    async fn web_fetch_prompt_with_summarizer_returns_llm_answer() {
        use crate::test_support::{MockLlmProvider, make_client, text_response};

        let provider = Arc::new(MockLlmProvider::new(vec![text_response(
            "Rust is a systems programming language focused on safety and performance.",
        )]));
        let client = make_client(provider).await;
        let summarizer = WebFetchSummarizer {
            client,
            model_id: ModelHandle::ByName {
                provider: ProviderId::anthropic(),
                model:    "mock-model".to_string(),
            },
        };

        let tool = make_web_fetch_tool(Some(summarizer));
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox {
            exec_result: ExecResult {
                stdout:      "<html><body><p>Lots of content about Rust...</p></body></html>"
                    .into(),
                stderr:      String::new(),
                exit_code:   Some(0),
                termination: CommandTermination::Exited,
                duration_ms: 100,
            },
            ..Default::default()
        });
        let result = (tool.executor)(
            serde_json::json!({"url": "https://example.com", "prompt": "What is Rust?"}),
            ToolContext {
                write_locks: None,
                env,
                cancel: CancellationToken::new(),
                tool_env_provider: None,
                session_id: None,
                root_session_id: None,
                tool_call_id: None,
                agent_event_emitter: None,
            },
        )
        .await;
        let output = result.unwrap();
        assert_eq!(
            output,
            "Rust is a systems programming language focused on safety and performance."
        );
    }

    #[tokio::test]
    async fn web_fetch_prompt_without_summarizer_returns_content_with_note() {
        let tool = make_web_fetch_tool(None);
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox {
            exec_result: ExecResult {
                stdout:
                    "<html><body><p>Rust is a systems programming language.</p></body></html>"
                        .into(),
                stderr:      String::new(),
                exit_code:   Some(0),
                termination: CommandTermination::Exited,
                duration_ms: 100,
            },
            ..Default::default()
        });
        let result = (tool.executor)(
            serde_json::json!({"url": "https://example.com", "prompt": "What is Rust?"}),
            ToolContext {
                write_locks: None,
                env,
                cancel: CancellationToken::new(),
                tool_env_provider: None,
                session_id: None,
                root_session_id: None,
                tool_call_id: None,
                agent_event_emitter: None,
            },
        )
        .await;
        let output = result.unwrap();
        assert!(
            output.contains("summarization unavailable"),
            "should note unavailability, got: {output}"
        );
        assert!(
            output.contains("Rust is a systems programming language"),
            "should contain page content, got: {output}"
        );
    }

    #[tokio::test]
    async fn web_fetch_summarizer_routes_to_specified_provider() {
        use fabro_llm::Error as LlmError;
        use fabro_llm::error::{ProviderErrorDetail, ProviderErrorKind};

        use crate::test_support::{MockErrorProvider, MockLlmProvider, text_response};

        // "other_provider" is the default — it rejects all requests.
        let default_provider: Arc<dyn ProviderAdapter> = Arc::new(MockErrorProvider {
            error: LlmError::Provider {
                kind:   ProviderErrorKind::NotFound,
                detail: Box::new(ProviderErrorDetail::new(
                    "model not found",
                    "other_provider",
                )),
            },
        });
        // "anthropic" provider has the model we actually want.
        let target_provider: Arc<dyn ProviderAdapter> =
            Arc::new(MockLlmProvider::new(vec![text_response(
                "summarized content",
            )]));

        let mut providers = HashMap::new();
        providers.insert("other_provider".to_string(), default_provider);
        // Register under "anthropic" so ModelRef { provider: "anthropic", .. } routes
        // here
        providers.insert("anthropic".to_string(), target_provider);
        let client = Client::new(providers, Some("other_provider".into()), vec![]);

        let summarizer = WebFetchSummarizer {
            client,
            model_id: ModelHandle::ByName {
                provider: ProviderId::anthropic(),
                model:    "target-model".to_string(),
            },
        };

        let tool = make_web_fetch_tool(Some(summarizer));
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox {
            exec_result: ExecResult {
                stdout:      "<html><body><p>Page content</p></body></html>".into(),
                stderr:      String::new(),
                exit_code:   Some(0),
                termination: CommandTermination::Exited,
                duration_ms: 100,
            },
            ..Default::default()
        });
        let result = (tool.executor)(
            serde_json::json!({"url": "https://example.com", "prompt": "Summarize this"}),
            ToolContext {
                write_locks: None,
                env,
                cancel: CancellationToken::new(),
                tool_env_provider: None,
                session_id: None,
                root_session_id: None,
                tool_call_id: None,
                agent_event_emitter: None,
            },
        )
        .await;
        let output =
            result.expect("summarization should succeed when provider is correctly routed");
        assert_eq!(output, "summarized content");
    }

    #[test]
    fn html_to_markdown_converts_basic_html() {
        let result = html_to_markdown("<h1>Hello</h1><p>World</p>");
        assert_eq!(result, "# Hello\n\nWorld");
    }

    #[test]
    fn html_to_markdown_strips_script_and_style() {
        let html = "<html><head><style>body{color:red}</style></head><body><script>alert(1)</script><p>Content</p></body></html>";
        let result = html_to_markdown(html);
        assert!(
            !result.contains("alert"),
            "script content should be stripped"
        );
        assert!(
            !result.contains("color:red"),
            "style content should be stripped"
        );
        assert!(result.contains("Content"), "paragraph text should remain");
    }

    #[test]
    fn html_to_markdown_passes_through_non_html() {
        let json = r#"{"key": "value", "items": [1, 2, 3]}"#;
        assert_eq!(html_to_markdown(json), json);

        let plain = "Just some plain text\nwith newlines";
        assert_eq!(html_to_markdown(plain), plain);
    }

    #[fabro_macros::e2e_test(live("BRAVE_SEARCH_API_KEY"))]
    #[expect(
        clippy::disallowed_methods,
        reason = "Live web-search integration test reads its required API key from process env."
    )]
    async fn web_search_returns_results() {
        let api_key = std::env::var(EnvVars::BRAVE_SEARCH_API_KEY)
            .expect("BRAVE_SEARCH_API_KEY must be set to run this test");
        let tool = make_web_search_tool_with_api_key(api_key);
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox::default());
        let result = (tool.executor)(
            serde_json::json!({"query": "rust programming language"}),
            ToolContext {
                write_locks: None,
                env,
                cancel: CancellationToken::new(),
                tool_env_provider: None,
                session_id: None,
                root_session_id: None,
                tool_call_id: None,
                agent_event_emitter: None,
            },
        )
        .await;
        let output = result.expect("web search should succeed with valid API key");
        assert!(
            output.to_lowercase().contains("rust"),
            "results should mention rust, got: {output}"
        );
    }
}
