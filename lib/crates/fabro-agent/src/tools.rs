use crate::config::SessionConfig;
use crate::sandbox::GrepOptions;
use crate::tool_registry::RegisteredTool;
use fabro_llm::client::Client;
use fabro_llm::types::{Message, Request, ToolDefinition};
use fabro_model::ModelId;
use std::borrow::Cow;
use std::fmt::Write;
use std::sync::Arc;

const MAX_WEB_FETCH_BYTES: usize = 100 * 1024;

/// Configuration for the optional LLM-based summarizer used by `web_fetch`.
#[derive(Clone)]
pub struct WebFetchSummarizer {
    pub client: Client,
    pub model_id: ModelId,
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

/// Registers the core tools shared by all provider profiles: `read_file`, `write_file`,
/// `shell`, `grep`, `glob`, `web_search`, and `web_fetch`.
///
/// The shell tool uses `config` to set its default and max timeouts. Pass a custom
/// `SessionConfig` (e.g. with a longer `default_command_timeout_ms`) for providers
/// that need non-default shell behavior.
pub fn register_core_tools(
    registry: &mut crate::tool_registry::ToolRegistry,
    config: &SessionConfig,
    summarizer: Option<WebFetchSummarizer>,
) {
    registry.register(make_read_file_tool());
    registry.register(make_write_file_tool());
    registry.register(make_shell_tool_with_config(config));
    registry.register(make_grep_tool());
    registry.register(make_glob_tool());
    registry.register(make_web_search_tool());
    registry.register(make_web_fetch_tool(summarizer));
}

pub(crate) fn required_str<'a>(args: &'a serde_json::Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("Missing required parameter: {key}"))
}

#[must_use]
pub fn make_read_file_tool() -> RegisteredTool {
    RegisteredTool {
        definition: ToolDefinition {
            name: "read_file".into(),
            description: "Read the contents of a file".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": {"type": "string", "description": "Absolute path to the file"},
                    "offset": {"type": "integer", "description": "1-based line number to start reading from"},
                    "limit": {"type": "integer", "description": "Number of lines to read (default 2000)"}
                },
                "required": ["file_path"]
            }),
        },
        executor: Arc::new(|args, ctx| {
            Box::pin(async move {
                let file_path = required_str(&args, "file_path")?;
                let offset = args.get("offset").and_then(serde_json::Value::as_u64);
                let limit = args.get("limit").and_then(serde_json::Value::as_u64);

                let offset_usize = offset.map(|v| v as usize);
                let limit_usize = limit.map(|v| v as usize);

                let content = ctx
                    .env
                    .read_file(file_path, offset_usize, limit_usize)
                    .await?;
                ctx.env.mark_agent_read(file_path);
                Ok(content)
            })
        }),
    }
}

#[must_use]
pub fn make_write_file_tool() -> RegisteredTool {
    RegisteredTool {
        definition: ToolDefinition {
            name: "write_file".into(),
            description: "Write content to a file".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": {"type": "string", "description": "Absolute path to the file"},
                    "content": {"type": "string", "description": "Content to write to the file"}
                },
                "required": ["file_path", "content"]
            }),
        },
        executor: Arc::new(|args, ctx| {
            Box::pin(async move {
                let file_path = required_str(&args, "file_path")?;
                let content = required_str(&args, "content")?;

                ctx.env.write_file(file_path, content).await?;
                Ok(format!("Successfully wrote to {file_path}"))
            })
        }),
    }
}

#[must_use]
pub fn make_edit_file_tool() -> RegisteredTool {
    RegisteredTool {
        definition: ToolDefinition {
            name: "edit_file".into(),
            description: "Edit a file by replacing a string".into(),
            parameters: serde_json::json!({
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
        executor: Arc::new(|args, ctx| {
            Box::pin(async move {
                let file_path = required_str(&args, "file_path")?;
                let old_string = required_str(&args, "old_string")?;
                let new_string = required_str(&args, "new_string")?;
                let replace_all = args
                    .get("replace_all")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);

                let numbered_content = ctx.env.read_file(file_path, None, None).await?;

                // Strip line numbers: each line looks like "  1 | content" or " 10 | content"
                let raw_lines: Vec<&str> = numbered_content
                    .lines()
                    .map(|line| line.find(" | ").map_or(line, |idx| &line[idx + 3..]))
                    .collect();
                let raw_content = raw_lines.join("\n");

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

                ctx.env.write_file(file_path, &new_content).await?;
                Ok(format!("Successfully edited {file_path}"))
            })
        }),
    }
}

#[must_use]
pub fn make_shell_tool() -> RegisteredTool {
    make_shell_tool_with_config(&SessionConfig::default())
}

#[must_use]
pub fn make_shell_tool_with_config(config: &SessionConfig) -> RegisteredTool {
    let default_timeout = config.default_command_timeout_ms;
    let max_timeout = config.max_command_timeout_ms;
    RegisteredTool {
        definition: ToolDefinition {
            name: "shell".into(),
            description: "Execute a shell command".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "The shell command to execute"},
                    "timeout_ms": {"type": "integer", "description": "Timeout in milliseconds"},
                    "description": {"type": "string", "description": "Description of what this command does"}
                },
                "required": ["command"]
            }),
        },
        executor: Arc::new(move |args, ctx| {
            Box::pin(async move {
                let command = required_str(&args, "command")?;
                let timeout_ms = args
                    .get("timeout_ms")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(default_timeout)
                    .min(max_timeout);

                tracing::debug!(
                    env_var_count = ctx.tool_env.as_ref().map_or(0, |e| e.len()),
                    "Injecting sandbox env vars into tool execution"
                );
                let result = ctx
                    .env
                    .exec_command(
                        command,
                        timeout_ms,
                        None,
                        ctx.tool_env.as_ref(),
                        Some(ctx.cancel),
                    )
                    .await?;

                let mut output = String::new();
                if result.timed_out {
                    output.push_str("Command timed out.\n");
                }
                let _ = write!(
                    output,
                    "Exit code: {}\nstdout:\n{}\nstderr:\n{}",
                    result.exit_code, result.stdout, result.stderr
                );
                Ok(output)
            })
        }),
    }
}

#[must_use]
pub fn make_grep_tool() -> RegisteredTool {
    RegisteredTool {
        definition: ToolDefinition {
            name: "grep".into(),
            description: "Search file contents with a regex pattern".into(),
            parameters: serde_json::json!({
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
        executor: Arc::new(|args, ctx| {
            Box::pin(async move {
                let pattern = required_str(&args, "pattern")?;
                let path = args
                    .get("path")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(".");

                let options = GrepOptions {
                    glob_filter: args
                        .get("glob_filter")
                        .and_then(serde_json::Value::as_str)
                        .map(String::from),
                    case_insensitive: args
                        .get("case_insensitive")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                    max_results: args
                        .get("max_results")
                        .and_then(serde_json::Value::as_u64)
                        .map(|v| v as usize),
                };

                let results = ctx.env.grep(pattern, path, &options).await?;
                let mut seen_files = std::collections::HashSet::new();
                for line in &results {
                    if let Some(file_path) = line.split(':').next() {
                        if !file_path.is_empty() && seen_files.insert(file_path) {
                            ctx.env.mark_agent_read(file_path);
                        }
                    }
                }
                Ok(results.join("\n"))
            })
        }),
    }
}

#[must_use]
pub fn make_glob_tool() -> RegisteredTool {
    RegisteredTool {
        definition: ToolDefinition {
            name: "glob".into(),
            description: "Find files matching a glob pattern".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Glob pattern to match files"},
                    "path": {"type": "string", "description": "Directory to search in (default: working directory)"}
                },
                "required": ["pattern"]
            }),
        },
        executor: Arc::new(|args, ctx| {
            Box::pin(async move {
                let pattern = required_str(&args, "pattern")?;
                let path = args.get("path").and_then(serde_json::Value::as_str);

                let results = ctx.env.glob(pattern, path).await?;
                Ok(results.join("\n"))
            })
        }),
    }
}

#[must_use]
pub(crate) fn make_read_many_files_tool() -> RegisteredTool {
    RegisteredTool {
        definition: ToolDefinition {
            name: "read_many_files".into(),
            description: "Read multiple files at once".into(),
            parameters: serde_json::json!({
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
        executor: Arc::new(|args, ctx| {
            Box::pin(async move {
                let paths = args["paths"]
                    .as_array()
                    .ok_or_else(|| "paths must be an array".to_string())?;

                let mut output = String::new();
                for path_val in paths {
                    let path = path_val
                        .as_str()
                        .ok_or_else(|| "each path must be a string".to_string())?;
                    match ctx.env.read_file(path, None, None).await {
                        Ok(content) => {
                            ctx.env.mark_agent_read(path);
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
    }
}

#[must_use]
pub(crate) fn make_list_dir_tool() -> RegisteredTool {
    RegisteredTool {
        definition: ToolDefinition {
            name: "list_dir".into(),
            description: "List directory contents with depth control".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Directory path to list"},
                    "depth": {"type": "integer", "description": "Depth of listing (default 1)"}
                },
                "required": ["path"]
            }),
        },
        executor: Arc::new(|args, ctx| {
            Box::pin(async move {
                let path = required_str(&args, "path")?;
                let depth = args
                    .get("depth")
                    .and_then(serde_json::Value::as_u64)
                    .map(|v| v as usize);

                let entries = ctx.env.list_directory(path, depth).await?;
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
    }
}

fn format_brave_results(body: &serde_json::Value) -> String {
    let results = body
        .get("web")
        .and_then(|w| w.get("results"))
        .and_then(serde_json::Value::as_array);

    let Some(results) = results else {
        return "No results found.".to_string();
    };

    let mut output = String::new();
    for (i, result) in results.iter().enumerate() {
        let title = result
            .get("title")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("(no title)");
        let url = result
            .get("url")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("(no url)");
        let description = result
            .get("description")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let _ = write!(
            output,
            "{}. {}\n   {}\n   {}\n\n",
            i + 1,
            title,
            url,
            description
        );
    }
    output
}

#[must_use]
pub(crate) fn make_web_search_tool() -> RegisteredTool {
    make_web_search_tool_with_api_key(std::env::var("BRAVE_SEARCH_API_KEY").ok())
}

fn make_web_search_tool_with_api_key(api_key: Option<String>) -> RegisteredTool {
    let client = reqwest::Client::new();
    RegisteredTool {
        definition: ToolDefinition {
            name: "web_search".into(),
            description: "Search the web using Brave Search".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Search query"},
                    "max_results": {"type": "integer", "description": "Maximum number of results (default 5, max 20)"}
                },
                "required": ["query"]
            }),
        },
        executor: Arc::new(move |args, _ctx| {
            let client = client.clone();
            let api_key = api_key.clone();
            Box::pin(async move {
                let api_key = api_key.ok_or_else(|| {
                    "BRAVE_SEARCH_API_KEY environment variable is not set".to_string()
                })?;

                let query = required_str(&args, "query")?;
                let count = args
                    .get("max_results")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(5)
                    .min(20);

                let resp = client
                    .get("https://api.search.brave.com/res/v1/web/search")
                    .header("X-Subscription-Token", &api_key)
                    .header("Accept", "application/json")
                    .query(&[("q", query), ("count", &count.to_string())])
                    .send()
                    .await
                    .map_err(|e| format!("HTTP request failed: {e}"))?;

                if !resp.status().is_success() {
                    return Err(format!(
                        "Brave Search API returned status {}",
                        resp.status()
                    ));
                }

                let body: serde_json::Value = resp
                    .json()
                    .await
                    .map_err(|e| format!("Failed to parse response: {e}"))?;

                Ok(format_brave_results(&body))
            })
        }),
    }
}

#[must_use]
pub(crate) fn make_web_fetch_tool(summarizer: Option<WebFetchSummarizer>) -> RegisteredTool {
    RegisteredTool {
        definition: ToolDefinition {
            name: "web_fetch".into(),
            description: "Fetch content from a URL and optionally summarize it. Pass a prompt to extract specific information instead of returning the full page.".into(),
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

                let result = ctx.env
                    .exec_command(&command, timeout_ms, None, ctx.tool_env.as_ref(), Some(ctx.cancel))
                    .await?;

                if result.exit_code != 0 {
                    return Err(format!(
                        "curl failed (exit code {}): {}",
                        result.exit_code,
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
                            model: s.model_id.model.clone(),
                            messages: vec![Message::user(summarization_prompt)],
                            provider: Some(s.model_id.provider.as_str().to_string()),
                            tools: None,
                            tool_choice: None,
                            response_format: None,
                            temperature: None,
                            top_p: None,
                            max_tokens: None,
                            stop_sequences: None,
                            reasoning_effort: None,
                            metadata: None,
                            provider_options: None,
                        };
                        let response = s.client.complete(&request).await.map_err(|e| {
                            format!("web_fetch summarization (model={}) failed: {e}", s.model_id.model)
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::*;
    use crate::test_support::MockSandbox;
    use crate::tool_registry::ToolContext;
    use std::collections::HashMap;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn read_file_returns_content() {
        let tool = make_read_file_tool();
        let mut files = HashMap::new();
        files.insert("/test.txt".into(), "  1 | hello\n  2 | world".into());
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox {
            files,
            apply_read_offset_limit: true,
            ..Default::default()
        });
        let result = (tool.executor)(
            serde_json::json!({"file_path": "/test.txt"}),
            ToolContext {
                env,
                cancel: CancellationToken::new(),
                tool_env: None,
            },
        )
        .await;
        assert_eq!(result.unwrap(), "  1 | hello\n  2 | world");
    }

    #[tokio::test]
    async fn read_file_with_offset_and_limit() {
        let tool = make_read_file_tool();
        let mut files = HashMap::new();
        files.insert(
            "/test.txt".into(),
            "  1 | line1\n  2 | line2\n  3 | line3\n  4 | line4".into(),
        );
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox {
            files,
            apply_read_offset_limit: true,
            ..Default::default()
        });
        let result = (tool.executor)(
            serde_json::json!({"file_path": "/test.txt", "offset": 2, "limit": 2}),
            ToolContext {
                env,
                cancel: CancellationToken::new(),
                tool_env: None,
            },
        )
        .await;
        assert_eq!(result.unwrap(), "  2 | line2\n  3 | line3");
    }

    #[tokio::test]
    async fn write_file_calls_env() {
        let tool = make_write_file_tool();
        let env = Arc::new(MockSandbox::default());
        let env_clone: Arc<dyn Sandbox> = env.clone();
        let result = (tool.executor)(
            serde_json::json!({"file_path": "/out.txt", "content": "hello"}),
            ToolContext {
                env: env_clone,
                cancel: CancellationToken::new(),
                tool_env: None,
            },
        )
        .await;
        assert_eq!(result.unwrap(), "Successfully wrote to /out.txt");
        let written = env.written_files.lock().unwrap();
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].0, "/out.txt");
        assert_eq!(written[0].1, "hello");
    }

    #[tokio::test]
    async fn edit_file_replaces_match() {
        let tool = make_edit_file_tool();
        let mut files = HashMap::new();
        files.insert("/f.txt".into(), "  1 | hello world".into());
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
                env: env_clone,
                cancel: CancellationToken::new(),
                tool_env: None,
            },
        )
        .await;
        assert_eq!(result.unwrap(), "Successfully edited /f.txt");
        let written = env.written_files.lock().unwrap();
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].1, "goodbye world");
    }

    #[tokio::test]
    async fn edit_file_not_found_error() {
        let tool = make_edit_file_tool();
        let mut files = HashMap::new();
        files.insert("/f.txt".into(), "  1 | hello world".into());
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
                env,
                cancel: CancellationToken::new(),
                tool_env: None,
            },
        )
        .await;
        assert_eq!(result.unwrap_err(), "old_string not found in file");
    }

    #[tokio::test]
    async fn edit_file_not_unique_error() {
        let tool = make_edit_file_tool();
        let mut files = HashMap::new();
        files.insert("/f.txt".into(), "  1 | aa bb aa".into());
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
                env,
                cancel: CancellationToken::new(),
                tool_env: None,
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
        files.insert("/f.txt".into(), "  1 | aa bb aa".into());
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
                env: env_clone,
                cancel: CancellationToken::new(),
                tool_env: None,
            },
        )
        .await;
        assert_eq!(result.unwrap(), "Successfully edited /f.txt");
        let written = env.written_files.lock().unwrap();
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].1, "cc bb cc");
    }

    #[tokio::test]
    async fn shell_basic_command() {
        let tool = make_shell_tool();
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox {
            exec_result: ExecResult {
                stdout: "hello".into(),
                stderr: String::new(),
                exit_code: 0,
                timed_out: false,
                duration_ms: 10,
            },
            ..Default::default()
        });
        let result = (tool.executor)(
            serde_json::json!({"command": "echo hello"}),
            ToolContext {
                env,
                cancel: CancellationToken::new(),
                tool_env: None,
            },
        )
        .await;
        let output = result.unwrap();
        assert!(output.contains("Exit code: 0"));
        assert!(output.contains("hello"));
    }

    #[tokio::test]
    async fn shell_with_timeout() {
        let tool = make_shell_tool();
        let env = Arc::new(MockSandbox::default());
        let env_clone: Arc<dyn Sandbox> = env.clone();
        let _result = (tool.executor)(
            serde_json::json!({"command": "sleep 1", "timeout_ms": 5000}),
            ToolContext {
                env: env_clone,
                cancel: CancellationToken::new(),
                tool_env: None,
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
                stdout: String::new(),
                stderr: "error".into(),
                exit_code: 1,
                timed_out: false,
                duration_ms: 10,
            },
            ..Default::default()
        });
        let result = (tool.executor)(
            serde_json::json!({"command": "false"}),
            ToolContext {
                env,
                cancel: CancellationToken::new(),
                tool_env: None,
            },
        )
        .await;
        let output = result.unwrap();
        assert!(output.contains("Exit code: 1"));
        assert!(output.contains("error"));
    }

    #[tokio::test]
    async fn shell_timeout_output() {
        let tool = make_shell_tool();
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox {
            exec_result: ExecResult {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: -1,
                timed_out: true,
                duration_ms: 10000,
            },
            ..Default::default()
        });
        let result = (tool.executor)(
            serde_json::json!({"command": "sleep 100"}),
            ToolContext {
                env,
                cancel: CancellationToken::new(),
                tool_env: None,
            },
        )
        .await;
        let output = result.unwrap();
        assert!(output.starts_with("Command timed out.\n"));
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
                env: env_clone,
                cancel: CancellationToken::new(),
                tool_env: Some(tool_env.clone()),
            },
        )
        .await;
        let captured = env.captured_env_vars.lock().unwrap().clone();
        assert_eq!(captured, Some(tool_env));
    }

    #[tokio::test]
    async fn shell_passes_none_env_when_tool_env_is_none() {
        let tool = make_shell_tool();
        let env = Arc::new(MockSandbox::default());
        let env_clone: Arc<dyn Sandbox> = env.clone();
        let _result = (tool.executor)(
            serde_json::json!({"command": "echo hello"}),
            ToolContext {
                env: env_clone,
                cancel: CancellationToken::new(),
                tool_env: None,
            },
        )
        .await;
        let captured = env.captured_env_vars.lock().unwrap().clone();
        assert_eq!(captured, None);
    }

    #[tokio::test]
    async fn web_fetch_passes_tool_env_to_exec_command() {
        let tool = make_web_fetch_tool(None);
        let env = Arc::new(MockSandbox {
            exec_result: ExecResult {
                stdout: "fetched content".into(),
                stderr: String::new(),
                exit_code: 0,
                timed_out: false,
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
                env: env_clone,
                cancel: CancellationToken::new(),
                tool_env: Some(tool_env.clone()),
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
        let result = (tool.executor)(
            serde_json::json!({"pattern": "fn"}),
            ToolContext {
                env,
                cancel: CancellationToken::new(),
                tool_env: None,
            },
        )
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
        let result = (tool.executor)(
            serde_json::json!({"pattern": "src/**/*.rs"}),
            ToolContext {
                env,
                cancel: CancellationToken::new(),
                tool_env: None,
            },
        )
        .await;
        let output = result.unwrap();
        assert!(output.contains("src/main.rs"));
        assert!(output.contains("src/lib.rs"));
    }

    #[tokio::test]
    async fn web_search_missing_api_key_returns_error() {
        let tool = make_web_search_tool_with_api_key(None);
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox::default());
        let result = (tool.executor)(
            serde_json::json!({"query": "test"}),
            ToolContext {
                env,
                cancel: CancellationToken::new(),
                tool_env: None,
            },
        )
        .await;
        let err = result.unwrap_err();
        assert!(
            err.contains("BRAVE_SEARCH_API_KEY"),
            "error should mention BRAVE_SEARCH_API_KEY, got: {err}"
        );
    }

    #[tokio::test]
    async fn web_search_missing_query_returns_error() {
        let tool = make_web_search_tool_with_api_key(Some("fake-key".into()));
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox::default());
        let result = (tool.executor)(
            serde_json::json!({}),
            ToolContext {
                env,
                cancel: CancellationToken::new(),
                tool_env: None,
            },
        )
        .await;
        let err = result.unwrap_err();
        assert!(
            err.contains("query"),
            "error should mention missing query, got: {err}"
        );
    }

    #[test]
    fn format_brave_results_formats_results() {
        let body = serde_json::json!({
            "web": {
                "results": [
                    {"title": "Rust Lang", "url": "https://rust-lang.org", "description": "A systems language"},
                    {"title": "Rust Book", "url": "https://doc.rust-lang.org/book", "description": "The Rust book"}
                ]
            }
        });
        let output = format_brave_results(&body);
        assert!(output.contains("1. Rust Lang"));
        assert!(output.contains("https://rust-lang.org"));
        assert!(output.contains("A systems language"));
        assert!(output.contains("2. Rust Book"));
    }

    #[test]
    fn format_brave_results_no_results() {
        let body = serde_json::json!({"web": {}});
        assert_eq!(format_brave_results(&body), "No results found.");
    }

    #[tokio::test]
    async fn web_fetch_builds_curl_command() {
        let tool = make_web_fetch_tool(None);
        let env = Arc::new(MockSandbox {
            exec_result: ExecResult {
                stdout: "<html><body><h1>hello</h1></body></html>".into(),
                stderr: String::new(),
                exit_code: 0,
                timed_out: false,
                duration_ms: 100,
            },
            ..Default::default()
        });
        let env_clone: Arc<dyn Sandbox> = env.clone();
        let result = (tool.executor)(
            serde_json::json!({"url": "https://example.com"}),
            ToolContext {
                env: env_clone,
                cancel: CancellationToken::new(),
                tool_env: None,
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
                env,
                cancel: CancellationToken::new(),
                tool_env: None,
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
                env: env_clone,
                cancel: CancellationToken::new(),
                tool_env: None,
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
            serde_json::json!({"url": "https://example.com", "timeout_ms": 120000}),
            ToolContext {
                env: env_clone,
                cancel: CancellationToken::new(),
                tool_env: None,
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
                stdout: large_content,
                stderr: String::new(),
                exit_code: 0,
                timed_out: false,
                duration_ms: 100,
            },
            ..Default::default()
        });
        let result = (tool.executor)(
            serde_json::json!({"url": "https://example.com"}),
            ToolContext {
                env,
                cancel: CancellationToken::new(),
                tool_env: None,
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
                stdout: String::new(),
                stderr: "curl: (6) Could not resolve host".into(),
                exit_code: 6,
                timed_out: false,
                duration_ms: 100,
            },
            ..Default::default()
        });
        let result = (tool.executor)(
            serde_json::json!({"url": "https://nonexistent.example.com"}),
            ToolContext {
                env,
                cancel: CancellationToken::new(),
                tool_env: None,
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
        use crate::test_support::{make_client, text_response, MockLlmProvider};

        let provider = Arc::new(MockLlmProvider::new(vec![text_response(
            "Rust is a systems programming language focused on safety and performance.",
        )]));
        let client = make_client(provider).await;
        let summarizer = WebFetchSummarizer {
            client,
            model_id: ModelId::new(fabro_model::Provider::Anthropic, "mock-model"),
        };

        let tool = make_web_fetch_tool(Some(summarizer));
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox {
            exec_result: ExecResult {
                stdout: "<html><body><p>Lots of content about Rust...</p></body></html>".into(),
                stderr: String::new(),
                exit_code: 0,
                timed_out: false,
                duration_ms: 100,
            },
            ..Default::default()
        });
        let result = (tool.executor)(
            serde_json::json!({"url": "https://example.com", "prompt": "What is Rust?"}),
            ToolContext {
                env,
                cancel: CancellationToken::new(),
                tool_env: None,
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
                stdout: "<html><body><p>Rust is a systems programming language.</p></body></html>"
                    .into(),
                stderr: String::new(),
                exit_code: 0,
                timed_out: false,
                duration_ms: 100,
            },
            ..Default::default()
        });
        let result = (tool.executor)(
            serde_json::json!({"url": "https://example.com", "prompt": "What is Rust?"}),
            ToolContext {
                env,
                cancel: CancellationToken::new(),
                tool_env: None,
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
        use crate::test_support::{text_response, MockErrorProvider, MockLlmProvider};
        use fabro_llm::error::{ProviderErrorDetail, ProviderErrorKind, SdkError};

        // "other_provider" is the default — it rejects all requests.
        let default_provider: Arc<dyn fabro_llm::provider::ProviderAdapter> =
            Arc::new(MockErrorProvider {
                error: SdkError::Provider {
                    kind: ProviderErrorKind::NotFound,
                    detail: Box::new(ProviderErrorDetail::new(
                        "model not found",
                        "other_provider",
                    )),
                },
            });
        // "anthropic" provider has the model we actually want.
        let target_provider: Arc<dyn fabro_llm::provider::ProviderAdapter> = Arc::new(
            MockLlmProvider::new(vec![text_response("summarized content")]),
        );

        let mut providers = HashMap::new();
        providers.insert("other_provider".to_string(), default_provider);
        // Register under "anthropic" so ModelId { provider: Anthropic, .. } routes here
        providers.insert("anthropic".to_string(), target_provider);
        let client = Client::new(providers, Some("other_provider".into()), vec![]);

        let summarizer = WebFetchSummarizer {
            client,
            model_id: ModelId::new(fabro_model::Provider::Anthropic, "target-model"),
        };

        let tool = make_web_fetch_tool(Some(summarizer));
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox {
            exec_result: ExecResult {
                stdout: "<html><body><p>Page content</p></body></html>".into(),
                stderr: String::new(),
                exit_code: 0,
                timed_out: false,
                duration_ms: 100,
            },
            ..Default::default()
        });
        let result = (tool.executor)(
            serde_json::json!({"url": "https://example.com", "prompt": "Summarize this"}),
            ToolContext {
                env,
                cancel: CancellationToken::new(),
                tool_env: None,
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

    #[tokio::test]
    #[ignore] // Requires BRAVE_SEARCH_API_KEY env var
    async fn web_search_returns_results() {
        let api_key = std::env::var("BRAVE_SEARCH_API_KEY")
            .expect("BRAVE_SEARCH_API_KEY must be set to run this test");
        let tool = make_web_search_tool_with_api_key(Some(api_key));
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox::default());
        let result = (tool.executor)(
            serde_json::json!({"query": "rust programming language"}),
            ToolContext {
                env,
                cancel: CancellationToken::new(),
                tool_env: None,
            },
        )
        .await;
        let output = result.expect("web search should succeed with valid API key");
        assert!(
            output.to_lowercase().contains("rust"),
            "results should mention rust, got: {output}"
        );
    }

    #[tokio::test]
    async fn read_file_tool_marks_agent_read() {
        use crate::read_before_write_sandbox::ReadBeforeWriteSandbox;

        let mock = MockSandbox {
            files: HashMap::from([("a.ts".into(), "content".into())]),
            ..Default::default()
        };
        let env: Arc<dyn Sandbox> = Arc::new(ReadBeforeWriteSandbox::new(Arc::new(mock)));

        // read_file tool should mark the file as agent-read
        let tool = make_read_file_tool();
        (tool.executor)(
            serde_json::json!({"file_path": "a.ts"}),
            ToolContext {
                env: Arc::clone(&env),
                cancel: CancellationToken::new(),
                tool_env: None,
            },
        )
        .await
        .unwrap();

        // write_file should succeed because read_file tool marked it
        let result = env.write_file("a.ts", "new content").await;
        assert!(
            result.is_ok(),
            "write should succeed after read_file tool marks agent-read"
        );
    }

    #[tokio::test]
    async fn grep_tool_marks_agent_read() {
        use crate::read_before_write_sandbox::ReadBeforeWriteSandbox;

        let mock = MockSandbox {
            files: HashMap::from([("b.ts".into(), "content".into())]),
            grep_results: vec!["b.ts:1:content".into()],
            ..Default::default()
        };
        let env: Arc<dyn Sandbox> = Arc::new(ReadBeforeWriteSandbox::new(Arc::new(mock)));

        // grep tool should mark matched files as agent-read
        let tool = make_grep_tool();
        (tool.executor)(
            serde_json::json!({"pattern": "content"}),
            ToolContext {
                env: Arc::clone(&env),
                cancel: CancellationToken::new(),
                tool_env: None,
            },
        )
        .await
        .unwrap();

        // write_file should succeed because grep tool marked it
        let result = env.write_file("b.ts", "new content").await;
        assert!(
            result.is_ok(),
            "write should succeed after grep tool marks agent-read"
        );
    }
}
