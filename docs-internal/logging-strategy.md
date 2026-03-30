# Fabro Logging Strategy

Fabro uses the `tracing` crate for structured, file-based logging. Logs write to `~/.fabro/logs/YYYY-MM-DD.log`, controlled by the `FABRO_LOG` env var (default: `info`). Logs are for **developers debugging issues after the fact** — they are not user-facing output.

Production runs at INFO level. INFO should be low-volume and high-signal — the summary of what happened. When something goes wrong, developers enable `FABRO_LOG=debug` to get the full picture. DEBUG can be as verbose as needed since it's only turned on temporarily.

## When to Log

**Log at INFO (always on in production):**

- Lifecycle boundaries of top-level operations — session started/completed, pipeline started/completed, server ready
- Failures and warnings — every error/warn path, with enough context to diagnose the cause
- Keep it sparse: a typical agent session should produce ~5-10 INFO lines

**Log at DEBUG (enabled on-demand for investigation):**

- Individual steps within an operation — each LLM request, each tool call, each pipeline node
- External interactions with detail — request parameters, response metadata, token counts
- Decision points — why a code path was taken (retry triggered, fallback used, config value resolved)
- State changes and intermediate results — config resolution, parsing outcomes

**Do not log:**

- Hot loops or per-token streaming events (use DEBUG only if truly needed for diagnosis)
- Data that belongs in user-facing output (`eprintln!` for interactive CLI feedback, not tracing)
- Detached user-visible warnings or errors that need to survive `attach`/`logs` (`detach.log` is debug-only; emit a `WorkflowRunEvent` into `progress.jsonl` instead)
- Redundant information already captured by a parent event (if you logged "starting X", you don't need to log every sub-step at the same level)
- Events that are already traced via `EventEnum::trace()` — the event enums (`AgentEvent`, `PipelineEvent`, `ExecutionEnvEvent`) each have a `trace()` method called automatically at their emit site; do not add manual `info!`/`debug!` calls that duplicate what `trace()` already emits
- Wrapper/forwarding variants that re-emit an inner event — `PipelineEvent::Agent`, `PipelineEvent::ExecutionEnv`, and `AgentEvent::SubAgentEvent` are no-ops in `trace()` because the inner event is already traced at its origin
- Secrets, API keys, or auth tokens — even at DEBUG level

## Log Levels

### ERROR — Something failed and the operation cannot continue

The current operation is aborting. A human reviewing logs should investigate every ERROR.

```rust
error!(server = %name, error = %err, "MCP server failed to start");
error!(provider = %provider, status = %status, "LLM request failed after all retries");
```

### WARN — Something unexpected happened but execution continues

Degraded behavior, fallback paths, or conditions that might indicate a problem.

```rust
warn!(server = %name, "MCP server disconnected, removing tools");
warn!(attempt = attempt, max = max_retries, error = %err, "LLM request failed, retrying");
```

### INFO — The production log level

INFO is always on. It should tell you **what** happened at a high level: which operations started, which completed, and key outcomes. Think of INFO as the audit trail — enough to answer "what did the system do?" but not so much that it creates noise. A typical agent session should produce a handful of INFO lines, not hundreds.

```rust
info!(model = %model, "Starting agent session");
info!(server = %name, tools = tool_count, "MCP server ready");
info!(pipeline = %name, "Pipeline complete");
info!(turns = turn_count, tool_calls = tool_call_count, "Agent session complete");
```

### DEBUG — Turn this on when something goes wrong

DEBUG is off in production by default. Enable it with `FABRO_LOG=debug` to investigate a specific issue. DEBUG events provide the **how** and **why**: request/response details, intermediate state, config resolution, individual steps within a larger operation. DEBUG can be verbose — that's fine, since it's only enabled temporarily.

```rust
debug!(model = %model, messages = msg_count, tools = tool_count, "Sending LLM request");
debug!(provider = %provider, input_tokens = input, output_tokens = output, "LLM response received");
debug!(tool = %name, duration_ms = elapsed, "Tool call complete");
debug!(path = %path.display(), "Loading workflow file");
debug!(env_var = "ANTHROPIC_API_KEY", "API key resolved from environment");
```

## How to Write a Log Event

### Message: describe what happened

The message string is a short, human-readable description. Use sentence fragments starting with a verb or noun. No variable interpolation in the message — put variable data in structured fields.

```rust
// Good — message is a fixed string, data is in fields
info!(server = %name, tools = tool_count, "MCP server ready");

// Bad — variable data interpolated into message string
info!("MCP server '{}' ready with {} tools", name, tool_count);
```

Fixed message strings make logs grepable and let tooling aggregate events by message.

### Fields: attach structured context

Fields are key-value pairs that make events queryable. Include enough context that the event is useful on its own without reading surrounding log lines.

**Field naming:**
- Use `snake_case` for field names
- Use consistent names across the codebase (see table below)
- Keep names short but unambiguous

**Common field names:**

| Field | Used for |
|-------|----------|
| `model` | LLM model identifier |
| `provider` | LLM provider name (anthropic, openai, gemini) |
| `server` | MCP server name |
| `tool` | Tool name being called |
| `turn` | Agent turn number |
| `attempt` | Retry attempt number |
| `error` | Error value on failure |
| `path` | File system path |
| `duration_ms` | Elapsed time in milliseconds |
| `input_tokens` | Token count for LLM input |
| `output_tokens` | Token count for LLM output |

**Field format specifiers:**
- `%` (Display) for user-readable values: `server = %name`, `error = %err`, `path = %path.display()`
- `?` (Debug) for internal/enum values: `level = ?params.level`, `status = ?response.status`
- No specifier for primitives: `tools = tool_count`, `attempt = 3`

### Examples by crate

**fabro-agent:**
```rust
info!(model = %model, "Starting agent session");
info!(turns = turn_count, tool_calls = total_calls, "Agent session complete");
debug!(turn = turn_number, "Starting agent turn");
debug!(tool = %name, "Executing tool call");
debug!(tool = %name, duration_ms = elapsed, "Tool call complete");
warn!(tool = %name, error = %err, "Tool execution failed");
```

**fabro-llm:**
```rust
debug!(provider = %provider, model = %model, messages = count, "Sending LLM request");
debug!(provider = %provider, model = %model, input_tokens = input, output_tokens = output, "LLM response received");
warn!(provider = %provider, attempt = n, error = %err, "Request failed, retrying");
error!(provider = %provider, error = %err, "Request failed after all retries");
```

**fabro-workflow:**
```rust
info!(pipeline = %name, "Starting pipeline execution");
info!(pipeline = %name, nodes = count, "Pipeline complete");
debug!(node = %id, handler = %handler_type, "Executing pipeline node");
debug!(node = %id, duration_ms = elapsed, "Pipeline node complete");
```

**fabro-mcp:**
```rust
info!(server = %name, tools = tool_count, "MCP server ready");
debug!(server = %name, transport = %transport_type, "Connecting to MCP server");
error!(server = %name, error = %err, "MCP server failed to start");
```

## Cross-Package Guidelines

Every crate that does meaningful work should emit tracing events. The `tracing` dependency is workspace-level — add it to any crate's `Cargo.toml` with:

```toml
tracing.workspace = true
```

The subscriber is initialized once in `fabro-cli`. Library crates (`fabro-agent`, `fabro-llm`, etc.) only emit events — they never configure the subscriber. This means:

- Library crates import `tracing::{info, debug, warn, error}` and call the macros
- The events go nowhere in unit tests (this is fine — tests verify behavior, not log output)
- The events are captured by whatever subscriber the binary sets up

When adding tracing to a new crate, start with the boundaries: INFO for the start/end of top-level operations, DEBUG for the individual steps within them. When in doubt about the level, use DEBUG — it's easy to promote something to INFO later, but hard to demote a noisy INFO event without breaking someone's log monitoring.

## Event Enum Tracing

The domain event enums (`AgentEvent`, `PipelineEvent`, `ExecutionEnvEvent`) each implement a `pub fn trace(&self)` method (or `trace(&self, session_id: &str)` for `AgentEvent`) that emits a structured tracing log line per variant. This method is called automatically from each enum's emit site, so every emitted event produces a log line without any additional code at the call site.

**Rules for event tracing:**

- **Add tracing for new variants** by adding a match arm in the enum's `trace()` method. Choose the level based on the guidelines above (INFO for lifecycle boundaries, DEBUG for individual steps, WARN/ERROR for failures).
- **Do not add manual log calls at emit sites.** The `trace()` call in the emitter handles it. Adding `info!` or `debug!` next to an `emit()` call will double-log.
- **Detached UX belongs in events, not stderr.** If an attached user needs to see the message later via `fabro attach` or `fabro logs`, emit a workflow event (for example `RunNotice`) and let tracing capture the developer-oriented copy separately.
- **Wrapper variants are no-ops.** When one event enum wraps another (`PipelineEvent::Agent` wraps `AgentEvent`, `AgentEvent::SubAgentEvent` wraps a child `AgentEvent`), the wrapper's `trace()` arm is `{}` because the inner event was already traced at its origin. This prevents double-logging.
- **Streaming noise variants are no-ops.** `TextDelta` and `ToolCallOutputDelta` produce no log output — per-token events would flood the logs even at DEBUG level.
