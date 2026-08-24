//! Tools whose behavior differs from fabro's built-ins, implemented to Kimi
//! Code's contract.
//!
//! Where a Kimi Code tool behaves identically to an existing fabro tool, the
//! Kimi profile reuses that tool and only its exposed name changes (see
//! [`crate::native_tool::ToolVocabulary`]). These three differ in what their
//! parameters *mean*, not just what they are called, so renaming fabro's
//! parameters would advertise behavior fabro does not have:
//!
//! - `Bash` takes `timeout` in **seconds** where fabro takes milliseconds, and
//!   accepts a `cwd`. A rename alone would make every timeout 1000x wrong.
//! - `Read` accepts a **negative** `line_offset`, meaning "read the last N
//!   lines". Fabro's `offset` has no such meaning.
//! - `Write` takes a `mode`, so it can append. Fabro's write always replaces.
//!
//! Everything these tools do reaches the environment through the same
//! [`Sandbox`](crate::sandbox::Sandbox) methods the built-ins use, so sandbox
//! behavior and path policy are unchanged.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::str::FromStr;
use std::sync::Arc;

use fabro_llm::types::ToolDefinition;
use serde_json::Value;
use strum::EnumString;

use crate::native_tool::NativeTool;
use crate::sandbox::{GrepOptions, format_lines_numbered};
use crate::tool_registry::{RegisteredTool, ToolSource};
use crate::tools::{
    DEFAULT_READ_LINES, emit_shell_process_completed, execute_grep, execute_shell_command,
    grep_result_path, make_edit_file_tool, optional_usize_arg, required_str,
};

const DEFAULT_GREP_RESULTS: usize = 250;
const MAX_GREP_RESULTS: usize = 2000;
const MAX_GREP_MATCHES_SCANNED: usize = 20_000;

fn definition(tool: NativeTool, description: &str, parameters: Value) -> ToolDefinition {
    ToolDefinition {
        // Supply the canonical identity; registry insertion rewrites the
        // stored and wire name for the active vocabulary.
        name: tool.canonical_name().to_string(),
        description: description.to_string(),
        parameters,
    }
}

/// `Bash`, taking `timeout` in seconds and an optional `cwd`.
#[must_use]
pub fn make_kimi_bash_tool(default_timeout_ms: u64, max_timeout_ms: u64) -> RegisteredTool {
    let default_timeout_s = default_timeout_ms / 1000;
    let max_timeout_s = max_timeout_ms / 1000;
    let description = format!(
        "Execute a bash command. Use this for shell semantics — pipes, env, processes, git, \
package managers, build and test runners.

Translate these to a dedicated tool instead:
- `cat` / `head` / `tail` on a known path → Read
- `sed` / `awk` for an in-place edit → Edit
- `echo > file` / heredoc → Write
- `find` or recursive `ls` to locate files by name → Glob (plain `ls <dir>` is fine)
- `grep` / `rg` to search file contents → Grep

The dedicated tools cap their output, so they keep large raw dumps out of the conversation.

Output: stdout and stderr are combined and returned as a string. A non-zero exit appends a \
`Command failed with exit code: N` line.

Guidelines:
- Each call runs in a fresh bash process. Environment variables and `cd` do NOT persist between \
calls — pass `cwd`, or use absolute paths.
- `timeout` is in SECONDS. It defaults to {default_timeout_s} and is capped at {max_timeout_s}.
- A long-running command needs a raised `timeout`, not a retry: a command that timed out once \
will time out again.
- Do not run interactive commands, or commands that never exit.
- Chain genuinely dependent steps with `&&`. Issue independent read-only commands as separate \
parallel calls in one response so their output stays separate.
- Quote paths containing spaces.
- Avoid `..` to reach outside the working directory, and do not modify files outside it unless \
explicitly asked. Never run commands requiring superuser privileges unless explicitly asked."
    );

    RegisteredTool {
        definition: definition(
            NativeTool::Shell,
            &description,
            serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "The command to execute."},
                    "cwd": {
                        "type": "string",
                        "description": "Directory to run the command in. Defaults to the \
            working directory."
                    },
                    "timeout": {
                        "type": "integer",
                        "description": format!(
                            "Timeout in seconds (default {default_timeout_s}, max {max_timeout_s})."
                        )
                    },
                    "description": {
                        "type": "string",
                        "description": "Short description of what this command does."
                    }
                },
                "required": ["command"]
            }),
        ),
        executor:   Arc::new(move |args, ctx| {
            Box::pin(async move {
                let command = required_str(&args, "command")?;
                let cwd = args.get("cwd").and_then(Value::as_str);
                // Seconds on the wire, milliseconds in the sandbox.
                let timeout_ms = match args.get("timeout").and_then(Value::as_u64) {
                    Some(seconds) => seconds.saturating_mul(1000).min(max_timeout_ms),
                    None => default_timeout_ms,
                };

                let streaming = execute_shell_command(&ctx, command, timeout_ms, cwd).await?;
                let result = &streaming.result;

                let mut out = String::new();
                if result.is_timed_out() {
                    out.push_str("Command timed out.\n");
                } else if result.is_cancelled() {
                    out.push_str("Command cancelled.\n");
                }
                out.push_str(&result.stdout);
                if !result.stderr.is_empty() {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(&result.stderr);
                }
                if let Some(code) = result.exit_code.filter(|c| *c != 0) {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    let _ = write!(out, "Command failed with exit code: {code}");
                }
                let is_success = result.is_success();
                emit_shell_process_completed(&ctx, streaming).await;
                if is_success { Ok(out) } else { Err(out) }
            })
        }),
        source:     ToolSource::Native,
    }
}

/// `Read`, where a negative `line_offset` reads from the end of the file.
#[must_use]
pub fn make_kimi_read_tool() -> RegisteredTool {
    RegisteredTool {
        definition: definition(
            NativeTool::ReadFile,
            "Read a text file from the workspace.

- If you have a concrete path, call Read directly. Do not Glob or `ls` first to check that it \
exists — a missing path returns an error you can handle.
- When you need several files, emit multiple Read calls in one response rather than one per turn.
- Returns `<line-number> | <content>` per line. Drop the number and separator when taking text for \
an Edit `old_string`.
- `line_offset` is the 1-based first line to read. A NEGATIVE value reads from the end, so -100 \
returns the last 100 lines.
- `n_lines` defaults to 2000 lines.
- Use Bash or an MCP tool for binary formats; this tool reads text.
- After a successful Edit or Write, do not re-read solely to prove the write landed. When the task \
depends on an exact file, API, or output shape, inspect the final result before finishing.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Path to the file to read."},
                    "line_offset": {
                        "type": "integer",
                        "minimum": -2000,
                        "description": "1-based first line to read. Negative reads from the end \
            of the file (-100 reads the last 100 lines); zero is invalid."
                    },
                    "n_lines": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 2000,
                        "description": "Number of lines to read (default 2000)."
                    }
                },
                "required": ["path"]
            }),
        ),
        executor:   Arc::new(|args, ctx| {
            Box::pin(async move {
                let path = required_str(&args, "path")?;
                let n_lines = optional_usize_arg(&args, "n_lines")?.unwrap_or(DEFAULT_READ_LINES);
                if n_lines == 0 || n_lines > DEFAULT_READ_LINES {
                    return Err(format!(
                        "n_lines must be between 1 and {DEFAULT_READ_LINES}"
                    ));
                }
                let line_offset = args.get("line_offset").and_then(Value::as_i64);
                if line_offset == Some(0) {
                    return Err("line_offset must not be zero".to_string());
                }

                let content = match line_offset {
                    // Negative offset: count the file's lines, then start that
                    // many from the end. Kimi Code's semantics.
                    Some(offset) if offset < 0 => {
                        let from_end = usize::try_from(offset.unsigned_abs())
                            .map_err(|_| "line_offset is too large".to_string())?;
                        if from_end > DEFAULT_READ_LINES {
                            return Err(format!(
                                "negative line_offset must be at least -{DEFAULT_READ_LINES}"
                            ));
                        }
                        let raw = ctx
                            .env
                            .read_file_text(path)
                            .await
                            .map_err(|e| e.display_with_causes())?;
                        let total = raw.lines().count();
                        let start = total.saturating_sub(from_end).saturating_add(1);
                        Ok(format_lines_numbered(
                            &raw,
                            Some(start),
                            Some(n_lines.min(from_end)),
                        ))
                    }
                    Some(offset) => {
                        let start = usize::try_from(offset)
                            .map_err(|_| "line_offset must fit in usize".to_string())?;
                        ctx.env.read_file(path, Some(start), Some(n_lines)).await
                    }
                    None => ctx.env.read_file(path, None, Some(n_lines)).await,
                }
                .map_err(|e| e.display_with_causes())?;

                Ok(content)
            })
        }),
        source:     ToolSource::Native,
    }
}

#[derive(Clone, Copy, Default, EnumString)]
#[strum(serialize_all = "snake_case")]
enum KimiWriteMode {
    #[default]
    Overwrite,
    Append,
}

/// `Write`, with Kimi Code's `mode` so it can append.
#[must_use]
pub fn make_kimi_write_tool() -> RegisteredTool {
    RegisteredTool {
        definition: definition(
            NativeTool::WriteFile,
            "Create, append to, or replace a file entirely.

- `mode` defaults to `overwrite`, which replaces the whole file. `append` requires an existing file \
and adds to its end without inserting a newline.
- Write is NOT ALLOWED for incremental changes to existing files, including trivial, one-line, \
quick, or cosmetic edits. Use Edit instead.
- Use Write only when the file does not exist, you intend a complete replacement, or the new \
contents have little continuity with the old contents.
- Read before overwriting an existing file.
- Write ignores the Read/Edit line-number view. NEVER include line prefixes.
- Do not create documentation files that were not asked for.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Path to the file to write."},
                    "content": {"type": "string", "description": "Content to write."},
                    "mode": {
                        "type": "string",
                        "enum": ["overwrite", "append"],
                        "description": "Whether to replace the file or append to it (default \
            overwrite)."
                    }
                },
                "required": ["path", "content"]
            }),
        ),
        executor:   Arc::new(|args, ctx| {
            Box::pin(async move {
                let path = required_str(&args, "path")?;
                let content = required_str(&args, "content")?;
                let mode = args
                    .get("mode")
                    .and_then(Value::as_str)
                    .unwrap_or("overwrite")
                    .parse::<KimiWriteMode>()
                    .map_err(|_| "Invalid mode (expected overwrite|append)".to_string())?;

                match mode {
                    KimiWriteMode::Overwrite => {
                        ctx.env
                            .write_file(path, content)
                            .await
                            .map_err(|e| e.display_with_causes())?;
                    }
                    // The sandbox trait has no append; read-modify-write keeps
                    // every provider working and stays inside path policy.
                    KimiWriteMode::Append => {
                        let mut existing = ctx
                            .env
                            .read_file_text(path)
                            .await
                            .map_err(|e| e.display_with_causes())?;
                        existing.push_str(content);
                        ctx.env
                            .write_file(path, &existing)
                            .await
                            .map_err(|e| e.display_with_causes())?;
                    }
                }
                Ok(format!("Wrote {path}"))
            })
        }),
        source:     ToolSource::Native,
    }
}

/// Kimi Code's `Edit` schema names the target `path`; fabro's shared edit
/// executor calls it `file_path`. Translate only that adapter field and reuse
/// the exact-match implementation.
#[must_use]
pub fn make_kimi_edit_tool(description: &str) -> RegisteredTool {
    let shared = make_edit_file_tool();
    let shared_executor = shared.executor;
    RegisteredTool {
        definition: definition(
            NativeTool::EditFile,
            description,
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Path to the text file to edit."},
                    "old_string": {"type": "string", "description": "Exact content to replace."},
                    "new_string": {"type": "string", "description": "Replacement text."},
                    "replace_all": {
                        "type": "boolean",
                        "description": "Replace every occurrence (default false)."
                    }
                },
                "required": ["path", "old_string", "new_string"]
            }),
        ),
        executor:   Arc::new(move |mut args, ctx| {
            let shared_executor = shared_executor.clone();
            Box::pin(async move {
                let object = args
                    .as_object_mut()
                    .ok_or_else(|| "Edit arguments must be an object".to_string())?;
                let path = object
                    .remove("path")
                    .ok_or_else(|| "Missing required parameter: path".to_string())?;
                object.insert("file_path".to_string(), path);
                shared_executor(args, ctx).await
            })
        }),
        source:     ToolSource::Native,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::sandbox::{ExecResult, Sandbox};
    use crate::test_support::{MockSandbox, MutableMockSandbox};
    use crate::tool_registry::ToolContext;

    fn ctx(env: Arc<dyn Sandbox>) -> ToolContext {
        ToolContext {
            write_locks: None,
            env,
            cancel: CancellationToken::new(),
            tool_env_provider: None,
            session_id: Some("ses".into()),
            root_session_id: Some("ses".into()),
            tool_call_id: None,
            agent_event_emitter: None,
        }
    }

    fn sandbox_with(path: &str, content: &str) -> Arc<MutableMockSandbox> {
        let mut files = HashMap::new();
        files.insert(path.to_string(), content.to_string());
        Arc::new(MutableMockSandbox::new(files))
    }

    /// The reason Read is a separate tool: a negative `line_offset` means
    /// "the last N lines", which fabro's `offset` has no notion of.
    #[tokio::test]
    async fn read_negative_line_offset_reads_from_the_end() {
        let lines: Vec<String> = (1..=20).map(|n| format!("line{n}")).collect();
        let env = sandbox_with("/f.txt", &lines.join("\n"));
        let tool = make_kimi_read_tool();

        let out = (tool.executor)(
            json!({"path": "/f.txt", "line_offset": -3}),
            ctx(env.clone()),
        )
        .await
        .unwrap();

        assert!(out.contains("line18"), "{out}");
        assert!(out.contains("line20"), "{out}");
        assert!(
            !out.contains("line1\n"),
            "should not include the head: {out}"
        );
    }

    #[tokio::test]
    async fn read_positive_line_offset_starts_there() {
        let lines: Vec<String> = (1..=20).map(|n| format!("line{n}")).collect();
        let env = sandbox_with("/f.txt", &lines.join("\n"));
        let tool = make_kimi_read_tool();

        let out = (tool.executor)(
            json!({"path": "/f.txt", "line_offset": 5, "n_lines": 2}),
            ctx(env),
        )
        .await
        .unwrap();
        assert!(out.contains("line5"), "{out}");
        assert!(!out.contains("line8"), "{out}");
    }

    #[tokio::test]
    async fn read_positive_offset_still_applies_the_default_limit() {
        let lines: Vec<String> = (1..=DEFAULT_READ_LINES + 5)
            .map(|n| format!("line{n}"))
            .collect();
        let env = sandbox_with("/f.txt", &lines.join("\n"));
        let tool = make_kimi_read_tool();

        let out = (tool.executor)(json!({"path": "/f.txt", "line_offset": 2}), ctx(env))
            .await
            .unwrap();

        assert!(out.contains("2001 | line2001"), "{out}");
        assert!(!out.contains("2002 | line2002"), "{out}");
    }

    /// The reason Write is a separate tool: it has a mode, so it can append.
    #[tokio::test]
    async fn write_append_mode_preserves_existing_content() {
        let env = sandbox_with("/f.txt", "first");
        let tool = make_kimi_write_tool();

        (tool.executor)(
            json!({"path": "/f.txt", "content": "-second", "mode": "append"}),
            ctx(env.clone()),
        )
        .await
        .unwrap();

        assert_eq!(env.read_file_text("/f.txt").await.unwrap(), "first-second");
    }

    #[tokio::test]
    async fn write_defaults_to_overwrite() {
        let env = sandbox_with("/f.txt", "first");
        let tool = make_kimi_write_tool();
        (tool.executor)(
            json!({"path": "/f.txt", "content": "only"}),
            ctx(env.clone()),
        )
        .await
        .unwrap();
        assert_eq!(env.read_file_text("/f.txt").await.unwrap(), "only");
    }

    #[tokio::test]
    async fn write_rejects_an_unknown_mode() {
        let env = sandbox_with("/f.txt", "x");
        let tool = make_kimi_write_tool();
        let err = (tool.executor)(
            json!({"path": "/f.txt", "content": "y", "mode": "prepend"}),
            ctx(env),
        )
        .await
        .unwrap_err();
        assert!(err.contains("expected overwrite|append"), "{err}");
    }

    #[tokio::test]
    async fn write_append_propagates_a_missing_file_error() {
        let env = Arc::new(MutableMockSandbox::new(HashMap::new()));
        let tool = make_kimi_write_tool();

        let err = (tool.executor)(
            json!({"path": "/missing.txt", "content": "new", "mode": "append"}),
            ctx(env),
        )
        .await
        .unwrap_err();

        assert!(err.contains("missing.txt"), "{err}");
    }

    #[tokio::test]
    async fn edit_translates_kimi_path_to_the_shared_executor() {
        let env = sandbox_with("/f.txt", "before");
        let tool = make_kimi_edit_tool("Edit");

        (tool.executor)(
            json!({"path": "/f.txt", "old_string": "before", "new_string": "after"}),
            ctx(env.clone()),
        )
        .await
        .unwrap();

        assert_eq!(env.read_file_text("/f.txt").await.unwrap(), "after");
        assert!(
            tool.definition.parameters["properties"]
                .get("path")
                .is_some()
        );
        assert!(
            tool.definition.parameters["properties"]
                .get("file_path")
                .is_none()
        );
    }

    /// `files_with_matches` and `count` both need the file path, which the
    /// underlying search only prefixes when scanning a directory.
    #[test]
    fn grep_result_path_handles_both_output_shapes() {
        // Directory scan: `<path>:<line>:<content>`.
        assert_eq!(
            grep_result_path("src/main.rs:42:fn main() {", "src"),
            "src/main.rs"
        );
        // A colon in the content must not be mistaken for the line field.
        assert_eq!(
            grep_result_path("src/a.rs:7:let x: u8 = 1;", "src"),
            "src/a.rs"
        );
        // Single-file scan omits the path, so fall back to what was searched.
        assert_eq!(
            grep_result_path("42:fn main() {", "src/main.rs"),
            "src/main.rs"
        );
    }

    async fn grep_with(args: serde_json::Value, lines: Vec<String>) -> Result<String, String> {
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox {
            grep_results: lines,
            ..MockSandbox::default()
        });
        let tool = make_kimi_grep_tool();
        (tool.executor)(args, ctx(env)).await
    }

    #[tokio::test]
    async fn grep_content_mode_returns_matching_lines() {
        let out = grep_with(json!({"pattern": "x", "output_mode": "content"}), vec![
            "a.rs:1:x".into(),
            "b.rs:2:x".into(),
        ])
        .await
        .unwrap();
        assert_eq!(out, "a.rs:1:x\nb.rs:2:x");
    }

    #[tokio::test]
    async fn grep_defaults_to_files_with_matches() {
        let out = grep_with(json!({"pattern": "x"}), vec![
            "a.rs:1:x".into(),
            "a.rs:2:x".into(),
            "b.rs:2:x".into(),
        ])
        .await
        .unwrap();
        assert_eq!(out, "a.rs\nb.rs");
    }

    #[tokio::test]
    async fn grep_files_with_matches_deduplicates_paths_in_order() {
        let out = grep_with(
            json!({"pattern": "x", "output_mode": "files_with_matches"}),
            vec!["a.rs:1:x".into(), "a.rs:9:x".into(), "b.rs:2:x".into()],
        )
        .await
        .unwrap();
        assert_eq!(out, "a.rs\nb.rs");
    }

    #[tokio::test]
    async fn grep_count_mode_counts_per_file() {
        let out = grep_with(
            json!({"pattern": "x", "output_mode": "count_matches"}),
            vec!["a.rs:1:x".into(), "a.rs:9:x".into(), "b.rs:2:x".into()],
        )
        .await
        .unwrap();
        assert_eq!(out, "a.rs:2\nb.rs:1");
    }

    #[tokio::test]
    async fn grep_offset_and_head_limit_page_results() {
        let lines: Vec<String> = (1..=6).map(|n| format!("f{n}.rs:1:x")).collect();
        let out = grep_with(
            json!({
                "pattern": "x",
                "output_mode": "content",
                "offset": 2,
                "head_limit": 2
            }),
            lines,
        )
        .await
        .unwrap();
        assert_eq!(out, "f3.rs:1:x\nf4.rs:1:x");
    }

    #[tokio::test]
    async fn grep_rejects_an_unknown_output_mode() {
        let err = grep_with(json!({"pattern": "x", "output_mode": "json"}), vec![])
            .await
            .unwrap_err();
        assert!(
            err.contains("expected content|files_with_matches|count_matches"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn grep_reports_no_matches_plainly() {
        let out = grep_with(json!({"pattern": "x"}), vec![]).await.unwrap();
        assert_eq!(out, "No matches found");
    }

    #[test]
    fn grep_schema_uses_kimi_code_modes_and_flags() {
        let parameters = make_kimi_grep_tool().definition.parameters;
        assert_eq!(
            parameters["properties"]["output_mode"]["enum"],
            json!(["content", "files_with_matches", "count_matches"])
        );
        assert!(parameters["properties"].get("-i").is_some());
        assert!(parameters["properties"].get("case_insensitive").is_none());
    }

    /// The reason Bash is a separate tool: `timeout` is seconds, not
    /// milliseconds. A rename would have made every timeout 1000x wrong.
    #[test]
    fn bash_schema_states_seconds_and_quotes_real_limits() {
        let tool = make_kimi_bash_tool(60_000, 600_000);
        let params = &tool.definition.parameters;
        let timeout = params["properties"]["timeout"]["description"]
            .as_str()
            .unwrap();
        assert!(timeout.contains("seconds"), "{timeout}");
        assert!(timeout.contains("60"), "default should be 60s: {timeout}");
        assert!(timeout.contains("600"), "max should be 600s: {timeout}");
        assert!(params["properties"].get("cwd").is_some(), "cwd missing");
        assert!(
            tool.definition
                .description
                .contains("timeout` is in SECONDS")
        );
        // Fabro has no background shell, so none is promised.
        assert!(!tool.definition.description.contains("run_in_background"));
    }

    #[tokio::test]
    async fn bash_reuses_session_env_cwd_and_timeout_rendering() {
        use fabro_types::CommandTermination;

        let tool = make_kimi_bash_tool(60_000, 600_000);
        let env = Arc::new(MockSandbox {
            exec_result: ExecResult {
                stdout:      String::new(),
                stderr:      String::new(),
                exit_code:   None,
                termination: CommandTermination::TimedOut,
                duration_ms: 7_000,
            },
            ..MockSandbox::default()
        });
        let mut tool_ctx = ctx(env.clone());
        let tool_env = HashMap::from([("TOKEN".to_string(), "value".to_string())]);
        tool_ctx.tool_env_provider = Some(Arc::new(crate::StaticEnvProvider(tool_env.clone())));

        let output = (tool.executor)(
            json!({"command": "echo $TOKEN", "cwd": "/repo", "timeout": 7}),
            tool_ctx,
        )
        .await
        .expect_err("a timeout is a failed tool result");

        assert!(output.starts_with("Command timed out.\n"), "{output}");
        assert_eq!(*env.captured_timeout.lock().unwrap(), Some(7_000));
        assert_eq!(env.captured_working_dirs.lock().unwrap().as_slice(), &[
            Some("/repo".to_string())
        ]);
        assert_eq!(*env.captured_env_vars.lock().unwrap(), Some(tool_env));
        assert_eq!(
            env.captured_command.lock().unwrap().as_deref(),
            Some("echo $TOKEN")
        );
    }
}

/// Output shapes Kimi Code's `Grep` supports.
#[derive(Clone, Copy, Default, PartialEq, Eq, EnumString)]
#[strum(serialize_all = "snake_case")]
enum GrepOutputMode {
    Content,
    #[default]
    FilesWithMatches,
    CountMatches,
}

/// `Grep` with Kimi Code's `output_mode`, `head_limit`, and `offset`.
///
/// These are all shapes of the result list the sandbox already returns, so no
/// provider work is needed. Kimi Code's `type`, `multiline`, and
/// `include_ignored` are deliberately absent: they would have to reach ripgrep
/// flags through new `Sandbox` trait methods, and advertising a parameter that
/// is ignored is worse than omitting it.
#[must_use]
pub fn make_kimi_grep_tool() -> RegisteredTool {
    RegisteredTool {
        definition: definition(
            NativeTool::Grep,
            "Search file contents with a regular expression.

Use Grep when looking for unknown content or an unknown location. If you already know the path, \
use Read instead. Prefer this over running `grep` or `rg` through Bash: it caps its output, so it \
will not flood the conversation.

- Backed by ripgrep when available and POSIX `grep` otherwise, so keep patterns portable across \
both rather than relying on ripgrep-only syntax.
- `output_mode` selects what comes back: `files_with_matches` (just the paths, the default), \
`content` (matching lines), or `count_matches` (matches per file).
- `head_limit` caps how many results are returned and `offset` skips that many first, so you can \
page through a large result set.
- `glob` limits which files are searched; `-i` folds case.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Regular expression to search for."},
                    "path": {"type": "string", "description": "Directory or file to search. Defaults to the working directory."},
                    "glob": {"type": "string", "description": "Only search files matching this glob."},
                    "output_mode": {
                        "type": "string",
                        "enum": ["content", "files_with_matches", "count_matches"],
                        "description": "Shape of the results (default files_with_matches)."
                    },
                    "head_limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 2000,
                        "description": "Return at most this many results (default 250)."
                    },
                    "offset": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": 20000,
                        "description": "Skip this many results before returning."
                    },
                    "-i": {"type": "boolean", "description": "Perform a case-insensitive search."}
                },
                "required": ["pattern"]
            }),
        ),
        executor:   Arc::new(|args, ctx| {
            Box::pin(async move {
                let pattern = required_str(&args, "pattern")?;
                // The trait requires a search root; "." is the working directory.
                let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
                let mode = GrepOutputMode::from_str(
                    args.get("output_mode")
                        .and_then(Value::as_str)
                        .unwrap_or("files_with_matches"),
                )
                .map_err(|_| {
                    "Invalid output_mode (expected content|files_with_matches|count_matches)"
                        .to_string()
                })?;
                let head_limit =
                    optional_usize_arg(&args, "head_limit")?.unwrap_or(DEFAULT_GREP_RESULTS);
                if head_limit == 0 || head_limit > MAX_GREP_RESULTS {
                    return Err(format!(
                        "head_limit must be between 1 and {MAX_GREP_RESULTS}"
                    ));
                }
                let offset = optional_usize_arg(&args, "offset")?.unwrap_or(0);
                if offset > MAX_GREP_MATCHES_SCANNED {
                    return Err(format!("offset must be at most {MAX_GREP_MATCHES_SCANNED}"));
                }
                if offset.saturating_add(head_limit) > MAX_GREP_MATCHES_SCANNED {
                    return Err(format!(
                        "offset + head_limit must be at most {MAX_GREP_MATCHES_SCANNED}"
                    ));
                }

                let options = GrepOptions {
                    glob_filter:      args.get("glob").and_then(Value::as_str).map(str::to_string),
                    case_insensitive: args.get("-i").and_then(Value::as_bool).unwrap_or(false),
                    max_results:      match mode {
                        GrepOutputMode::Content => Some(
                            head_limit
                                .saturating_add(offset)
                                .min(MAX_GREP_MATCHES_SCANNED),
                        ),
                        GrepOutputMode::FilesWithMatches | GrepOutputMode::CountMatches => {
                            Some(MAX_GREP_MATCHES_SCANNED)
                        }
                    },
                };

                let lines = execute_grep(&ctx, pattern, path, &options).await?;

                let searched = path;
                let results: Vec<String> = match mode {
                    GrepOutputMode::Content => lines,
                    GrepOutputMode::FilesWithMatches => {
                        let mut seen = HashSet::new();
                        let mut files = Vec::new();
                        for line in lines {
                            let file = grep_result_path(&line, searched).to_string();
                            if seen.insert(file.clone()) {
                                files.push(file);
                            }
                        }
                        files
                    }
                    GrepOutputMode::CountMatches => {
                        let mut counts: HashMap<String, usize> = HashMap::new();
                        let mut order = Vec::new();
                        for line in lines {
                            let file = grep_result_path(&line, searched).to_string();
                            if let Some(count) = counts.get_mut(&file) {
                                *count += 1;
                            } else {
                                counts.insert(file.clone(), 1);
                                order.push(file);
                            }
                        }
                        order
                            .into_iter()
                            .map(|file| {
                                let count = counts[&file];
                                format!("{file}:{count}")
                            })
                            .collect()
                    }
                }
                .into_iter()
                .skip(offset)
                .take(head_limit)
                .collect();

                if results.is_empty() {
                    return Ok("No matches found".to_string());
                }
                Ok(results.join("\n"))
            })
        }),
        source:     ToolSource::Native,
    }
}
