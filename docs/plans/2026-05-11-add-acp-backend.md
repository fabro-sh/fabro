# ACP Backend Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use trycycle-executing to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `backend="acp"` as a first-class Fabro LLM backend for agent and prompt nodes, backed by the official ACP Rust SDK and isolated in a new `fabro-acp` crate.

**Architecture:** Add a new `fabro-acp` crate that owns ACP command resolution, sandbox-backed stdio transport, protocol execution, response aggregation, and ACP-specific tests. Extend the sandbox abstraction with bidirectional non-PTY stdio so ACP agents run inside the same local/Docker sandbox where they can read and modify the workspace; `fabro-workflow` keeps the workflow-owned `CodergenBackend` adapter, credentials/env preparation, events, and changed-file detection to avoid leaking workflow concerns into the protocol crate. Change the `CodergenBackend::one_shot` contract to receive the active sandbox and cancel token, because prompt nodes must run ACP inside the workflow sandbox just like agent nodes. Add ACP-specific events and router support so `api`, `cli`, and `acp` are explicit backend choices with no silent fallback for misspellings.

**Tech Stack:** Rust, Tokio, `agent-client-protocol = "0.11.1"`, `agent-client-protocol-tokio = "0.11.1"` for command parsing/default ACP agent metadata where useful, `agent_client_protocol::Lines` / `ByteStreams` over sandbox stdio, Fabro sandbox providers, `cargo nextest`.

---

## User-Visible Behavior

- `backend="acp"` on `agent` / `agent_loop` nodes runs an ACP agent turn instead of Fabro's API agent or legacy CLI subprocess parser.
- `backend="acp"` on `prompt` / `one_shot` nodes also runs an ACP prompt turn. ACP cannot universally enforce "no tools" across third-party agents, so the user-visible contract is "ACP prompt turn with text aggregation", not "provider API one-shot with tool use disabled".
- `backend="api"` uses the API backend. A missing `backend` keeps the existing router behavior: API by default, with the current `is_cli_only_model(...)` escape hatch if that list is ever populated.
- `backend="cli"` continues to use the existing CLI backend unchanged except for shared helper extraction. In particular, prompt nodes with `backend="cli"` keep today's API one-shot fallback for compatibility; the router test suite must document that behavior so it is no longer accidental.
- Any other `backend` value fails validation with a clear `unsupported LLM backend` error. Silent fallback to API for misspellings is too risky now that backend choice has behavioral and isolation consequences.
- Default ACP command mapping mirrors current CLI provider mapping:
  - Anthropic: `npx -y @zed-industries/claude-code-acp@latest`
  - OpenAI, Kimi, Zai, Minimax, Inception, OpenAI-compatible: `npx -y @zed-industries/codex-acp@latest`
  - Gemini: `npx -y -- @google/gemini-cli@latest --experimental-acp`
- Before running one of Fabro's default `npx`-based ACP commands, Fabro ensures Node/npm/npx exist in the sandbox using the same Node bootstrap strategy as the legacy CLI backend. Explicit `acp_command` overrides are not implicitly installed beyond this Node bootstrap; if they need other binaries, the workflow/sandbox image must provide them.
- Advanced users and tests can set `acp_command="..."` on an ACP-backed node to override the default command. The override is only honored when `backend="acp"` and is parsed with `agent_client_protocol_tokio::AcpAgent::from_str(...)`; Fabro supports only parsed `McpServer::Stdio` commands in this cutover. JSON stdio server configs are valid, HTTP/SSE configs are rejected with a clear unsupported-command error, and parsed program/args/env are re-rendered for sandbox execution with `shell_quote()` instead of executing the raw attribute string through the shell.
- ACP receives the same provider credentials and workflow tool env currently forwarded to CLI agents. Model selection is recorded in Fabro events/projections, but stable ACP v1 has no portable model-selection request. Users who need model-specific ACP behavior must encode that in their chosen ACP command until ACP model/session config stabilizes.
- ACP stages emit `agent.acp.started`, `agent.acp.completed`, `agent.acp.cancelled`, and `agent.acp.timed_out`; run projections expose `provider_used.mode == "acp"`.
- ACP support is implemented for local and Docker sandboxes in this cutover. Daytona gets an explicit unsupported-provider error for ACP because the current Daytona command API does not expose raw bidirectional stdio; legacy `backend="cli"` remains available there. This is deliberate: running ACP on the host or over a PTY would violate isolation or corrupt JSON-RPC framing.
- Fabro handles ACP `session/request_permission` requests by auto-selecting an allow option, matching the trust model of existing CLI mode flags such as `--full-auto`, `--yolo`, and `--dangerously-skip-permissions`. Prefer an `AllowAlways` option when present, then `AllowOnce`, then the first non-reject option; if no allow option exists, return `RequestPermissionOutcome::Cancelled`.
- Fabro advertises no ACP client filesystem or terminal capabilities in this cutover. ACP agents that need those client-side APIs may fail with method-not-found; the supported path is ACP adapters that operate through their own process in the sandbox, which matches Fabro's existing CLI-agent isolation model.

## Contracts And Invariants

- ACP agent processes must run inside the active Fabro sandbox, not on the host, so file mutations, Git diff detection, secrets forwarding, and cancellation match existing run isolation.
- ACP stdio must be line-preserving, non-PTY JSON-RPC. Do not implement ACP over terminal PTY sessions.
- Do not use `agent_client_protocol_tokio::AcpAgent` to connect to the running agent process; it spawns on the host. It is acceptable only for parsing/validating command strings or using its default command constants. The actual connection must adapt `Sandbox::spawn_stdio_process(...)` into an `agent_client_protocol` transport.
- Do not accept an `acp_command` by validating it with the ACP Tokio helper and then executing the original raw string. Store the parsed stdio server command, args, env, and display string; use the parsed representation for execution so JSON stdio configs and shell-word overrides behave consistently.
- The new `fabro-acp` crate must use `agent-client-protocol` schema/session/message types for initialization, session creation, prompt turns, updates, cancellation, and fake-agent tests. Do not hand-roll ACP request/response structs.
- `fabro-acp` must not depend on `fabro-workflow`; otherwise `fabro-workflow` cannot instantiate it without a dependency cycle. The workflow adapter is intentionally thin and delegates all protocol behavior to `fabro-acp`.
- ACP response text is the concatenation of `SessionUpdate::AgentMessageChunk(ContentChunk { content: ContentBlock::Text(...), .. })` chunks until the prompt response stop reason arrives. Thought chunks, plans, tool call updates, and custom updates are ignored for `CodergenResult::Text` but must keep the stall watchdog alive.
- ACP permission requests must be handled through the official `RequestPermissionRequest` / `RequestPermissionResponse` schema types. Do not ignore them: real coding-agent adapters may request permission before file edits even when filesystem and terminal client capabilities are not advertised.
- Stop reason handling:
  - `EndTurn` and `Refusal`: return text as the stage response.
  - `Cancelled`: emit `agent.acp.cancelled` and return `Error::Cancelled`.
  - `MaxTokens` / `MaxTurnRequests`: emit completion with the partial output and return a handler error containing the stop reason.
- Cancellation must attempt ACP `session/cancel` when a session exists, then terminate the stdio process if the agent does not finish promptly.
- Timeouts use `node.timeout()` like CLI mode. A timeout emits `agent.acp.timed_out`, terminates the ACP process, and returns a handler error.
- Files touched are detected by comparing sandbox Git state before and after the ACP turn, using the same semantics as CLI mode: changed tracked files plus untracked files, sorted and deduplicated, then filtered against pre-existing dirty files.

## File Structure

- Create `lib/crates/fabro-acp/Cargo.toml` and `lib/crates/fabro-acp/src/lib.rs`: crate surface and exports.
- Create `lib/crates/fabro-acp/src/command.rs`: provider-to-ACP-command mapping and command override parsing helpers.
- Create `lib/crates/fabro-acp/src/transport.rs`: adapt `fabro_sandbox::Sandbox::spawn_stdio_process(...)` into an `agent_client_protocol` transport using `Lines` or `ByteStreams`, collect stderr tails, and terminate/wait on process cleanup.
- Create `lib/crates/fabro-acp/src/session.rs`: ACP lifecycle using `agent_client_protocol::Client`, `InitializeRequest`, `NewSessionRequest`, `PromptRequest`, `SessionUpdate`, `CancelNotification`, and stop reason handling.
- Create `lib/crates/fabro-acp/src/error.rs`: ACP-specific error type that converts cleanly into workflow handler errors.
- Create `lib/crates/fabro-acp/src/test_support.rs` behind `#[cfg(any(test, feature = "test-support"))]`: fake ACP agent/transport helpers using `agent-client-protocol` types.
- Modify root `Cargo.toml`: add workspace dependencies for `agent-client-protocol` and `agent-client-protocol-tokio` and include `fabro-acp` through the existing `lib/crates/*` workspace glob.
- Modify `lib/crates/fabro-agent/src/sandbox.rs`, `lib/crates/fabro-agent/src/lib.rs`, and `lib/crates/fabro-sandbox/src/sandbox.rs`: expose the new sandbox stdio process API and public process/handle/stderr-tail types through the existing sandbox API re-export path so callers using either `fabro_sandbox::*` or `fabro_agent::sandbox::*` can compile without reaching into private modules.
- Modify `lib/crates/fabro-sandbox/src/local.rs`: implement bidirectional stdio process spawning.
- Modify `lib/crates/fabro-sandbox/src/docker.rs`: implement bidirectional Docker exec stdio without TTY.
- Modify `lib/crates/fabro-sandbox/src/daytona/mod.rs`: return an explicit unsupported error for bidirectional stdio.
- Modify `lib/crates/fabro-sandbox/src/worktree.rs`, `read_guard.rs`, and sandbox decorators: forward stdio support to wrapped sandboxes and preserve worktree path resolution.
- Create `lib/crates/fabro-workflow/src/handler/llm/acp.rs`: workflow-owned `CodergenBackend` adapter that calls `fabro_acp`.
- Create `lib/crates/fabro-workflow/src/handler/llm/changed_files.rs`: shared Git changed-file detection currently embedded in CLI backend.
- Create `lib/crates/fabro-workflow/src/handler/llm/node_runtime.rs`: shared Node/npm bootstrap helper used by both CLI and ACP default `npx` commands.
- Modify `lib/crates/fabro-workflow/src/handler/llm/cli.rs`: use shared changed-file helpers and move `BackendRouter` to support API/CLI/ACP selection.
- Modify `lib/crates/fabro-workflow/src/handler/llm/mod.rs`: export `AgentAcpBackend` and the router.
- Modify `lib/crates/fabro-workflow/src/handler/agent.rs`, `prompt.rs`, `llm/api.rs`, tests, and any `CodergenBackend` stubs: extend `one_shot` with `sandbox` and `cancel_token` parameters so ACP prompt nodes can run in the active sandbox.
- Modify `lib/crates/fabro-types/src/graph.rs`: add `Node::acp_command()`.
- Modify `lib/crates/fabro-workflow/src/transforms/import.rs`: treat `acp_command` as a semantic default attribute when imported workflow placeholders carry it.
- Create `lib/crates/fabro-validate/src/rules/backend_valid.rs` and modify `lib/crates/fabro-validate/src/rules/mod.rs`: validate node `backend` values are absent or one of `api`, `cli`, `acp`.
- Modify `lib/crates/fabro-workflow/src/pipeline/initialize.rs`: construct `AgentAcpBackend` alongside API and CLI backends.
- Modify event files: `lib/crates/fabro-workflow/src/event/events.rs`, `names.rs`, `convert.rs`, and `stored_fields.rs` for `agent.acp.*`.
- Modify fork replay: `lib/crates/fabro-workflow/src/operations/fork.rs` so ACP provider metadata and terminal status survive fork projection rebuilds.
- Modify run event/projection files: `lib/crates/fabro-types/src/run_event/mod.rs`, `lib/crates/fabro-types/src/run_event/misc.rs`, and `lib/crates/fabro-store/src/run_state.rs`.
- Modify server steerability tracking: `lib/crates/fabro-server/src/server.rs`, `lib/crates/fabro-server/src/server/handler/steer.rs`, and `lib/crates/fabro-server/src/server/tests.rs` so currently running ACP stages are treated as non-steerable like CLI-backed stages instead of allowing buffered API steering.
- Modify docs: `docs/public/reference/dot-language.mdx`, `docs/public/core-concepts/agents.mdx`, and any CLI/backend reference that currently says only `api`/`cli`.
- Add/update tests in `lib/crates/fabro-acp/tests/`, `lib/crates/fabro-sandbox` unit tests, `lib/crates/fabro-workflow/tests/it/integration.rs`, `lib/crates/fabro-store/src/run_state.rs`, `lib/crates/fabro-server/src/server/tests.rs`, and `lib/crates/fabro-cli/tests/it/workflow/`.

## Task 1: Read Strategy Docs And Pin Protocol API

**Files:**
- Read: `docs/internal/events-strategy.md`
- Read: `docs/internal/testing-strategy.md`
- Read: `docs/internal/error-handling-strategy.md`
- Read: `https://docs.rs/agent-client-protocol/latest/agent_client_protocol/`
- Read: `https://docs.rs/agent-client-protocol-tokio/latest/agent_client_protocol_tokio/`

- [ ] **Step 1: Confirm repo strategy constraints**

Read the three internal strategy docs before changing events, tests, or error paths. Capture any additional constraints in short implementation notes inside the task branch, not in committed docs unless the implementation needs them.

- [ ] **Step 2: Confirm ACP SDK API locally**

Run:

```bash
cargo info agent-client-protocol
cargo info agent-client-protocol-tokio
```

Expected: current latest is `0.11.1` for both crates. If newer versions are available, use the latest compatible version and update this plan's exact version references in the implementation commit message.

- [ ] **Step 3: Inspect ACP crate examples/source**

Run:

```bash
rg -n "build_session|send_prompt|read_update|SessionUpdate|AcpAgent|zed_codex|google_gemini" ~/.cargo/registry/src -g '*.rs'
```

Expected: identify the SDK's `Client.builder()`, `InitializeRequest::new(ProtocolVersion::V1)`, `ConnectionTo::build_session`, `ActiveSession::send_prompt`, and `SessionUpdate` types.

- [ ] **Step 4: Commit**

No code changes are expected in this task.

## Task 2: Add `fabro-acp` Crate Skeleton And Command Mapping

**Files:**
- Create: `lib/crates/fabro-acp/Cargo.toml`
- Create: `lib/crates/fabro-acp/src/lib.rs`
- Create: `lib/crates/fabro-acp/src/command.rs`
- Test: `lib/crates/fabro-acp/src/command.rs`
- Modify: `Cargo.toml`

- [ ] **Step 1: Write failing command mapping tests**

Add tests proving provider mapping:

```rust
#[test]
fn default_command_for_anthropic_uses_zed_claude_acp() {
    assert_eq!(
        default_acp_command(Provider::Anthropic).to_string(),
        "npx -y @zed-industries/claude-code-acp@latest"
    );
}

#[test]
fn default_command_for_openai_compatible_family_uses_zed_codex_acp() {
    for provider in [
        Provider::OpenAi,
        Provider::Kimi,
        Provider::Zai,
        Provider::Minimax,
        Provider::Inception,
        Provider::OpenAiCompatible,
    ] {
        assert_eq!(
            default_acp_command(provider).to_string(),
            "npx -y @zed-industries/codex-acp@latest"
        );
    }
}

#[test]
fn default_command_for_gemini_uses_experimental_acp() {
    assert_eq!(
        default_acp_command(Provider::Gemini).to_string(),
        "npx -y -- @google/gemini-cli@latest --experimental-acp"
    );
}
```

Add override parsing tests:

```rust
#[test]
fn explicit_acp_command_overrides_provider_default() {
    let command = resolve_acp_command(Provider::OpenAi, Some("python fake_agent.py")).unwrap();
    assert_eq!(command.to_string(), "python fake_agent.py");
    assert_eq!(command.program(), Path::new("python"));
    assert_eq!(command.args(), &["fake_agent.py".to_string()]);
}

#[test]
fn blank_acp_command_is_rejected() {
    let err = resolve_acp_command(Provider::OpenAi, Some("   ")).unwrap_err();
    assert!(err.to_string().contains("acp_command must not be empty"));
}

#[test]
fn json_stdio_acp_command_is_supported() {
    let raw = r#"{"type":"stdio","name":"fake","command":"python","args":["fake agent.py"],"env":[{"name":"MODE","value":"test"}]}"#;
    let command = resolve_acp_command(Provider::OpenAi, Some(raw)).unwrap();
    assert_eq!(command.program(), Path::new("python"));
    assert_eq!(command.args(), &["fake agent.py".to_string()]);
    assert_eq!(command.env().get("MODE").map(String::as_str), Some("test"));
}

#[test]
fn non_stdio_acp_command_is_rejected() {
    let raw = r#"{"type":"http","name":"remote","url":"https://example.test/acp"}"#;
    let err = resolve_acp_command(Provider::OpenAi, Some(raw)).unwrap_err();
    assert!(err.to_string().contains("only stdio ACP commands are supported"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run:

```bash
ulimit -n 4096 && cargo nextest run -p fabro-acp command
```

Expected: FAIL because `fabro-acp` and `default_acp_command` do not exist.

- [ ] **Step 3: Implement crate skeleton**

Add dependencies:

```toml
[features]
test-support = []

[dependencies]
agent-client-protocol.workspace = true
agent-client-protocol-tokio.workspace = true
fabro-model = { path = "../fabro-model" }
fabro-sandbox = { path = "../fabro-sandbox" }
fabro-types = { path = "../fabro-types" }
fabro-util = { path = "../fabro-util" }
bytes.workspace = true
serde.workspace = true
serde_json.workspace = true
thiserror.workspace = true
tokio.workspace = true
tokio-util = { workspace = true, features = ["compat", "io"] }
futures.workspace = true
uuid.workspace = true
tracing.workspace = true

[dev-dependencies]
tempfile = "3"
```

Add workspace dependencies in root `Cargo.toml`:

```toml
agent-client-protocol = "0.11.1"
agent-client-protocol-tokio = "0.11.1"
```

Implement command resolution around the ACP SDK's parsed stdio server representation, not a raw shell string:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpCommand {
    display: String,
    program: PathBuf,
    args: Vec<String>,
    env: HashMap<String, String>,
}

impl AcpCommand {
    pub fn program(&self) -> &Path { &self.program }
    pub fn args(&self) -> &[String] { &self.args }
    pub fn env(&self) -> &HashMap<String, String> { &self.env }
    pub fn display(&self) -> &str { &self.display }

    pub fn to_shell_command(&self) -> String {
        std::iter::once(self.program.to_string_lossy().into_owned())
            .chain(self.args.iter().cloned())
            .map(|part| fabro_sandbox::shell_quote(&part))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

impl std::fmt::Display for AcpCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.display)
    }
}

pub fn default_acp_command(provider: fabro_model::Provider) -> AcpCommand {
    match provider {
        fabro_model::Provider::Anthropic => {
            parse_acp_command("npx -y @zed-industries/claude-code-acp@latest").unwrap()
        }
        fabro_model::Provider::Gemini => {
            parse_acp_command("npx -y -- @google/gemini-cli@latest --experimental-acp").unwrap()
        }
        fabro_model::Provider::OpenAi
        | fabro_model::Provider::Kimi
        | fabro_model::Provider::Zai
        | fabro_model::Provider::Minimax
        | fabro_model::Provider::Inception
        | fabro_model::Provider::OpenAiCompatible => {
            parse_acp_command("npx -y @zed-industries/codex-acp@latest").unwrap()
        }
    }
}

pub fn resolve_acp_command(
    provider: fabro_model::Provider,
    override_command: Option<&str>,
) -> Result<AcpCommand, AcpCommandError> {
    if let Some(raw) = override_command {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(AcpCommandError::EmptyOverride);
        }
        return parse_acp_command(trimmed);
    }
    Ok(default_acp_command(provider))
}

fn parse_acp_command(raw: &str) -> Result<AcpCommand, AcpCommandError> {
    let agent = agent_client_protocol_tokio::AcpAgent::from_str(raw)?;
    let agent_client_protocol::schema::McpServer::Stdio(stdio) = agent.into_server() else {
        return Err(AcpCommandError::UnsupportedTransport);
    };
    Ok(AcpCommand {
        display: raw.to_string(),
        program: stdio.command,
        args: stdio.args,
        env: stdio.env.into_iter().map(|env| (env.name, env.value)).collect(),
    })
}
```

- [ ] **Step 4: Run test to verify it passes**

Run:

```bash
ulimit -n 4096 && cargo nextest run -p fabro-acp command
```

Expected: PASS.

- [ ] **Step 5: Refactor and verify**

Keep the crate API narrow: export `AcpCommand`, `default_acp_command`, and no workflow types.

Run:

```bash
cargo build -p fabro-acp
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml lib/crates/fabro-acp
git commit -m "feat: add ACP backend crate skeleton"
```

## Task 3: Add Sandbox Bidirectional Stdio Capability

**Files:**
- Modify: `lib/crates/fabro-sandbox/src/sandbox.rs`
- Modify: `lib/crates/fabro-sandbox/src/local.rs`
- Modify: `lib/crates/fabro-sandbox/src/docker.rs`
- Modify: `lib/crates/fabro-sandbox/src/daytona/mod.rs`
- Modify: `lib/crates/fabro-sandbox/src/worktree.rs`
- Modify: `lib/crates/fabro-sandbox/src/read_guard.rs`
- Modify: `lib/crates/fabro-sandbox/src/sandbox.rs` `delegate_sandbox!` macro
- Modify: `lib/crates/fabro-sandbox/src/test_support.rs`
- Modify: `lib/crates/fabro-agent/src/lib.rs`
- Test: provider-local unit tests in `lib/crates/fabro-sandbox/src/local.rs`
- Test: Docker option/unit tests in `lib/crates/fabro-sandbox/src/docker.rs`
- Test: decorator forwarding tests in `lib/crates/fabro-sandbox/src/read_guard.rs` or `test_support.rs`

- [ ] **Step 1: Write failing local stdio test**

Add a local sandbox test that starts a line-oriented process and round-trips stdin/stdout:

```rust
#[tokio::test]
async fn stdio_process_round_trips_lines() {
    let tempdir = tempfile::tempdir().unwrap();
    let sandbox = LocalSandbox::new(tempdir.path().to_path_buf());
    let mut process = sandbox
        .spawn_stdio_process(
            "python3 -u -c 'import sys; [print(line.strip()[::-1], flush=True) for line in sys.stdin]'",
            None,
            None,
            None,
        )
        .await
        .unwrap();

    process.write_line("abc").await.unwrap();
    assert_eq!(process.read_stdout_line().await.unwrap(), Some("cba".to_string()));
    process.terminate().await.unwrap();
}
```

The exact helper names can differ, but the test must prove bidirectional stdio, not just command output streaming.

- [ ] **Step 2: Run test to verify it fails**

Run:

```bash
ulimit -n 4096 && cargo nextest run -p fabro-sandbox stdio_process_round_trips_lines
```

Expected: FAIL because the sandbox stdio API does not exist.

- [ ] **Step 3: Implement API and local provider**

Add an object-safe sandbox method with a default unsupported implementation:

```rust
async fn spawn_stdio_process(
    &self,
    command: &str,
    working_dir: Option<&str>,
    env_vars: Option<&HashMap<String, String>>,
    cancel_token: Option<CancellationToken>,
) -> crate::Result<StdioProcess>;
```

`StdioProcess` should own stdin, stdout, stderr, and child lifecycle. It must expose enough typed IO for `fabro-acp` to build an `agent_client_protocol` transport without provider-specific downcasts:

```rust
pub struct StdioProcess {
    pub stdin: Pin<Box<dyn tokio::io::AsyncWrite + Send>>,
    pub stdout: Pin<Box<dyn tokio::io::AsyncRead + Send>>,
    pub stderr: StderrCollector,
    pub handle: StdioProcessHandle,
}

impl StdioProcessHandle {
    pub async fn terminate(&self) -> crate::Result<()>;
    pub async fn wait(&self) -> crate::Result<CommandTermination>;
}
```

The exact type names can differ, but the public API must support all of these operations:

- build line-oriented JSON-RPC from stdout/stdin in `fabro-acp`
- collect stderr concurrently for protocol/exit errors
- terminate the child/exec on timeout or cancellation
- wait for final termination without leaking provider internals

For local, spawn `/bin/bash -lc <command>` with piped stdin/stdout/stderr, current env filtering consistent with `exec_command_streaming`, and process-group cleanup. The local implementation can expose child stdout directly as `AsyncRead`.

- [ ] **Step 4: Implement Docker provider**

Use Docker exec with non-PTY stdio and explicit cancellation support. Do not rely on Docker having an exec-kill API. Adapt the existing `docker_controlled_shell_command(...)` / stop-file strategy used by `exec_command_streaming` so `StdioProcessHandle::terminate()` can signal the wrapper from a separate exec, wait briefly, and then force-kill the recorded process group when needed.

Create exec with:

```rust
CreateExecOptions {
    attach_stdin: Some(true),
    attach_stdout: Some(true),
    attach_stderr: Some(true),
    tty: Some(false),
    cmd: Some(vec!["/bin/bash".to_string(), "-lc".to_string(), command.to_string()]),
    working_dir: Some(effective_dir),
    env: Some(env_vec),
    ..Default::default()
}
```

Start with `StartExecOptions { detach: false, tty: false, output_capacity: None }` and keep the returned `input` writer. Bollard returns stdout/stderr as a multiplexed `Stream<Item = LogOutput>` rather than separate `AsyncRead`s; convert only `LogOutput::StdOut` bytes into the `stdout` reader used by ACP and feed `LogOutput::StdErr` bytes into the stderr collector. The test must assert both create and start options use `tty == false` because ACP JSON-RPC must not run over PTY.

Add Docker unit tests around the option-builder/control wrapper proving:

- `attach_stdin`, `attach_stdout`, and `attach_stderr` are true
- create/start `tty` is false
- the controlled command writes a pid file and reacts to the stop file
- `terminate()` uses the stop-file path rather than silently dropping the stream

- [ ] **Step 5: Implement provider forwarding and unsupported Daytona**

Forward through `WorktreeSandbox`, read/write decorators, test-support sandboxes, and the `delegate_sandbox!` macro. Re-export the new public stdio process types from both `fabro-sandbox/src/lib.rs` and `fabro-agent/src/sandbox.rs` / `fabro-agent/src/lib.rs` alongside the existing sandbox exports. A decorator must not inherit the default unsupported implementation when its inner sandbox supports stdio. Daytona should return an unsupported error like:

```text
ACP backend requires bidirectional stdio; the Daytona sandbox provider does not support it yet
```

- [ ] **Step 6: Run tests to verify they pass**

Run:

```bash
ulimit -n 4096 && cargo nextest run -p fabro-sandbox stdio
```

Expected: PASS for local/unit stdio tests; no live Docker requirement unless an existing Docker test harness is available.

- [ ] **Step 7: Refactor and verify**

Keep the stdio API provider-neutral. Do not leak Bollard or Tokio child types through public trait signatures.

Run:

```bash
cargo build -p fabro-sandbox
```

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add lib/crates/fabro-sandbox lib/crates/fabro-agent/src/lib.rs lib/crates/fabro-agent/src/sandbox.rs
git commit -m "feat: add sandbox stdio processes"
```

## Task 4: Implement ACP Session Lifecycle In `fabro-acp`

**Files:**
- Create: `lib/crates/fabro-acp/src/session.rs`
- Create: `lib/crates/fabro-acp/src/transport.rs`
- Create: `lib/crates/fabro-acp/src/error.rs`
- Create: `lib/crates/fabro-acp/src/test_support.rs`
- Modify: `lib/crates/fabro-acp/src/lib.rs`
- Test: `lib/crates/fabro-acp/tests/session.rs`

- [ ] **Step 1: Write fake-agent lifecycle test**

Use `agent-client-protocol` request/response/schema types in the fake agent. The test must assert the observed method order:

```text
initialize
session/new
session/prompt
```

It must also assert that `SessionUpdate::AgentMessageChunk(ContentChunk { content: ContentBlock::Text(...), .. })` chunks are concatenated into the returned text.

- [ ] **Step 2: Run test to verify it fails**

Run:

```bash
ulimit -n 4096 && cargo nextest run -p fabro-acp session_lifecycle
```

Expected: FAIL because `run_acp_turn` does not exist.

- [ ] **Step 3: Implement `AcpRunRequest` / `AcpRunResult`**

Use an API that is neutral to `fabro-workflow` and depends directly on `fabro-sandbox` for the sandbox trait:

```rust
pub struct AcpRunRequest {
    pub command: AcpCommand,
    pub prompt: String,
    pub cwd: String,
    pub timeout_ms: Option<u64>,
    pub env: HashMap<String, String>,
    pub sandbox: Arc<dyn fabro_sandbox::Sandbox>,
    pub cancel_token: CancellationToken,
    pub on_activity: Option<Arc<dyn Fn() + Send + Sync>>,
}

pub struct AcpRunResult {
    pub text: String,
    pub stop_reason: agent_client_protocol::schema::StopReason,
    pub stderr: String,
    pub duration_ms: u64,
}
```

Keep usage optional/absent for now because stable ACP v1 does not provide portable token usage without unstable features.

- [ ] **Step 4: Implement sandbox-backed ACP transport**

Implement a `SandboxAcpTransport` in `transport.rs` that implements `agent_client_protocol::ConnectTo<agent_client_protocol::Client>` by:

- building the launch environment from `request.command.env()` first, then overlaying `request.env` from the workflow adapter so Fabro-managed credentials, login results, and tool env win over duplicate keys supplied in an `acp_command` JSON config. This matches the legacy CLI backend's launch-env behavior and prevents a command override from accidentally shadowing refreshed credentials.
- calling `request.sandbox.spawn_stdio_process(request.command.to_shell_command(), ...)`
- adapting process stdout/stdin into `agent_client_protocol::Lines` or `agent_client_protocol::ByteStreams`
- collecting stderr concurrently into a bounded tail for errors
- racing protocol completion against early process exit
- terminating and waiting for the process on timeout/cancellation/error

Use `tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt}` when converting Tokio IO to the `futures` IO traits used by `agent-client-protocol`. Do not use `agent_client_protocol_tokio::AcpAgent` for this connection because it launches the command on the host.

- [ ] **Step 5: Implement ACP lifecycle**

Use the official SDK and register a client-side permission handler before connecting:

```rust
use agent_client_protocol::schema::{
    InitializeRequest, PermissionOptionKind, ProtocolVersion, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, SelectedPermissionOutcome,
};

let permission_cancel_token = request.cancel_token.clone();
Client.builder()
    .name("fabro")
    .on_receive_request(
        async move |request: RequestPermissionRequest, responder, _connection| {
            if permission_cancel_token.is_cancelled() {
                return responder.respond(RequestPermissionResponse::new(
                    RequestPermissionOutcome::Cancelled,
                ));
            }
            let selected = request
                .options
                .iter()
                .find(|option| option.kind == PermissionOptionKind::AllowAlways)
                .or_else(|| {
                    request
                        .options
                        .iter()
                        .find(|option| option.kind == PermissionOptionKind::AllowOnce)
                })
                .or_else(|| {
                    request.options.iter().find(|option| {
                        !matches!(
                            option.kind,
                            PermissionOptionKind::RejectOnce | PermissionOptionKind::RejectAlways
                        )
                    })
                });
            let outcome = selected.map_or(RequestPermissionOutcome::Cancelled, |option| {
                RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                    option.option_id.clone(),
                ))
            });
            responder.respond(RequestPermissionResponse::new(outcome))
        },
        agent_client_protocol::on_receive_request!(),
    )
    .connect_with(SandboxAcpTransport::new(&request), async |cx| {
        cx.send_request(InitializeRequest::new(ProtocolVersion::V1))
            .block_task()
            .await?;
        cx.build_session(&request.cwd)
            .block_task()
            .run_until(async |mut session| {
                session.send_prompt(request.prompt)?;
                read_turn(&mut session).await
            })
            .await
    })
    .await
```

Use lower-level `read_update()` rather than only `read_to_string()` so the implementation can capture stop reasons, call `on_activity` for every update/response, and handle non-text updates deterministically.

When reading updates, handle cancellation with `tokio::select!`:

- if the cancel token fires after a session exists, send `CancelNotification::new(session_id.clone())` with `session.connection().send_notification_to(Agent, ...)`
- keep draining until the agent returns `StopReason::Cancelled`, or terminate the process after a short grace period
- if cancellation fires before a session exists, terminate the process and return `AcpError::Cancelled`

The initialize request should use `InitializeRequest::new(ProtocolVersion::V1)` and the default empty `ClientCapabilities`; do not advertise filesystem or terminal support until handlers for those requests are implemented.

Permission approval is intentionally automatic in this cutover because Fabro's legacy CLI backend already runs coding CLIs with all tool approvals bypassed. If cancellation has already fired when a permission request arrives, respond with `RequestPermissionOutcome::Cancelled`.

- [ ] **Step 6: Add cancellation and timeout tests**

Tests must cover:

- permission requests select an allow option and let the ACP turn continue
- permission requests after cancellation respond with `RequestPermissionOutcome::Cancelled`
- cancellation before prompt completion sends `CancelNotification::new(session_id)` / `session/cancel` when a session exists and returns `AcpError::Cancelled`
- timeout terminates the stdio process and returns `AcpError::TimedOut`
- malformed JSON-RPC from the agent returns a protocol error with stderr tail if present
- early process exit returns an error that includes exit status/stderr

- [ ] **Step 7: Run ACP crate tests**

Run:

```bash
ulimit -n 4096 && cargo nextest run -p fabro-acp
```

Expected: PASS.

- [ ] **Step 8: Refactor and verify**

Ensure all public error messages are suitable for surfacing as handler errors. Avoid exposing test helpers outside `#[cfg(any(test, feature = "test-support"))]`.

Run:

```bash
cargo build -p fabro-acp
```

Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add lib/crates/fabro-acp
git commit -m "feat: implement ACP session client"
```

## Task 5: Add Workflow ACP Backend Adapter And Router Support

**Files:**
- Create: `lib/crates/fabro-workflow/src/handler/llm/acp.rs`
- Create: `lib/crates/fabro-workflow/src/handler/llm/changed_files.rs`
- Create: `lib/crates/fabro-workflow/src/handler/llm/node_runtime.rs`
- Modify: `lib/crates/fabro-workflow/src/handler/agent.rs`
- Modify: `lib/crates/fabro-workflow/src/handler/prompt.rs`
- Modify: `lib/crates/fabro-workflow/src/handler/llm/api.rs`
- Modify: `lib/crates/fabro-workflow/src/handler/llm/cli.rs`
- Modify: `lib/crates/fabro-workflow/src/handler/llm/mod.rs`
- Modify: `lib/crates/fabro-workflow/Cargo.toml`
- Modify: `lib/crates/fabro-types/src/graph.rs`
- Modify: `lib/crates/fabro-workflow/src/transforms/import.rs`
- Create: `lib/crates/fabro-validate/src/rules/backend_valid.rs`
- Modify: `lib/crates/fabro-validate/src/rules/mod.rs`
- Test: `lib/crates/fabro-workflow/src/handler/llm/acp.rs`
- Test: `lib/crates/fabro-workflow/src/handler/llm/cli.rs`
- Test: `lib/crates/fabro-validate/src/rules/backend_valid.rs`

- [ ] **Step 1: Write failing backend adapter tests**

Add tests that prove:

- `AgentAcpBackend::run` sends the node prompt to `fabro-acp` and returns `CodergenResult::Text`
- `AgentAcpBackend::run` honors node `acp_command` only when routing to ACP
- `PromptHandler` passes the active sandbox and run cancel token through `CodergenBackend::one_shot`
- `AgentAcpBackend::one_shot` combines `system_prompt` and `prompt` into a single ACP prompt and runs it through the passed sandbox, not the host
- default ACP commands bootstrap Node/npx before launch when Node is absent
- explicit `acp_command` does not trigger provider CLI installation and is still run inside the sandbox
- stop reason `Cancelled` maps to `Error::Cancelled`
- max-token/max-turn stop reasons map to handler errors
- files touched are computed relative to pre-existing dirty files

- [ ] **Step 2: Write failing router tests**

Update router tests to cover:

```rust
router_uses_api_by_default
router_uses_api_for_backend_api
router_uses_cli_for_backend_cli
router_uses_acp_for_backend_acp
router_rejects_unknown_backend
router_routes_one_shot_to_acp_for_backend_acp
router_routes_one_shot_to_api_by_default
```

- [ ] **Step 3: Write failing backend validation tests**

Add a `backend_valid` rule test proving `backend="api"`, `backend="cli"`, `backend="acp"`, and absent backend are accepted, while `backend="codex"` returns an error diagnostic containing:

```text
unsupported LLM backend "codex"; expected one of: api, cli, acp
```

- [ ] **Step 4: Run tests to verify they fail**

Run:

```bash
ulimit -n 4096 && cargo nextest run -p fabro-workflow -E 'test(router_uses_acp_for_backend_acp) | test(router_routes_one_shot_to_acp_for_backend_acp) | test(acp_backend)'
ulimit -n 4096 && cargo nextest run -p fabro-validate -E 'test(backend_valid)'
```

Expected: FAIL because ACP adapter/router support and backend validation do not exist.

- [ ] **Step 5: Extend `CodergenBackend::one_shot` for sandboxed backends**

Change the trait method signature in `handler/agent.rs` to include the active sandbox and cancel token:

```rust
async fn one_shot(
    &self,
    node: &Node,
    prompt: &str,
    system_prompt: Option<&str>,
    emitter: &Arc<Emitter>,
    stage_scope: &StageScope,
    sandbox: &Arc<dyn Sandbox>,
    cancel_token: CancellationToken,
) -> Result<CodergenResult, Error>
```

Update `PromptHandler::execute` to pass `&services.run.sandbox` and `services.run.cancel_token()`. Update `AgentApiBackend`, `BackendRouter`, and all test stubs to accept the new parameters. API-backed one-shot calls should ignore these new parameters; ACP-backed one-shot calls must use them.

- [ ] **Step 6: Implement shared changed-file helpers**

Move CLI duplicated logic into `changed_files.rs`:

```rust
pub async fn detect_changed_files(sandbox: &Arc<dyn Sandbox>) -> Vec<String>;
pub async fn files_touched_since(
    sandbox: &Arc<dyn Sandbox>,
    files_before: &[String],
) -> (Vec<String>, Option<String>);
```

Use `shell_quote()` for the `ls -t` command. Update CLI backend to call these helpers without behavior changes.

- [ ] **Step 7: Implement ACP env preparation and Node bootstrap**

Mirror CLI credential behavior in the workflow adapter before calling `fabro_acp`:

- If a `CredentialResolver` exists, resolve `CredentialUsage::CliAgent(CliAgentKind::{Claude,Codex,Gemini})`.
- Run any credential `login_command` in the sandbox before starting ACP.
- Forward credential `env_vars`.
- Merge workflow tool env provider values.
- Preserve the GitHub token refresh notice behavior in the workflow adapter, because notices are workflow events.
- Ensure Node/npm/npx exist before running the default `npx` ACP commands. Extract the existing Node installation shell from `ensure_cli` into a shared helper instead of duplicating a second hardcoded tarball command.

- [ ] **Step 8: Implement `AgentAcpBackend`**

The adapter owns model/provider/resolver/tool env configuration like `AgentCliBackend`, builds `AcpRunRequest` with the active sandbox, cancellation token, and an `on_activity` callback that calls `Emitter::touch`, then delegates to `fabro_acp`. Defer ACP-specific started/completed/cancelled/timed-out event emission to Task 6, where the event variants and projection support are added in the same commit.

For `one_shot`, build the ACP prompt as:

```text
System:
{system_prompt}

User:
{prompt}
```

If `system_prompt` is `None` or empty, send only `{prompt}`.

- [ ] **Step 9: Implement three-way `BackendRouter`**

Change router fields to:

```rust
api_backend: Box<dyn CodergenBackend>,
cli_backend: AgentCliBackend,
acp_backend: AgentAcpBackend,
```

Route by parsed backend enum:

```rust
enum SelectedBackend { Api, Cli, Acp }
```

Selection rules:

- `None` -> CLI only if `is_cli_only_model(model)`, otherwise API
- `"api"` -> API
- `"cli"` -> CLI
- `"acp"` -> ACP
- anything else -> validation error

For `one_shot`, route `"acp"` to ACP and all other valid values to API. Keep the existing `backend="cli"` prompt-node API fallback for backward compatibility, but add a test documenting that legacy behavior so it is no longer accidental. Do not silently route `backend="acp"` to API.

- [ ] **Step 10: Implement backend validation**

Add `backend_valid::rule()` to `fabro-validate` and register it in `built_in_rules()`. This complements router runtime errors and makes `fabro validate` catch misspellings before execution.

- [ ] **Step 11: Run tests to verify they pass**

Run:

```bash
ulimit -n 4096 && cargo nextest run -p fabro-workflow -E 'test(router_uses_cli_for_backend_attr) | test(router_uses_api_by_default) | test(router_uses_acp_for_backend_acp) | test(router_routes_one_shot_to_acp_for_backend_acp) | test(agent_cli_backend_run_writes_prompt_and_calls_exec) | test(acp_backend)'
ulimit -n 4096 && cargo nextest run -p fabro-validate -E 'test(backend_valid)'
```

Expected: PASS.

- [ ] **Step 12: Refactor and verify**

Keep ACP-specific protocol code out of `fabro-workflow`; the adapter should translate workflow concepts to `fabro-acp` requests and back.

Run:

```bash
cargo build -p fabro-workflow -p fabro-validate
```

Expected: PASS.

- [ ] **Step 13: Commit**

```bash
git add lib/crates/fabro-workflow lib/crates/fabro-workflow/Cargo.toml lib/crates/fabro-types/src/graph.rs lib/crates/fabro-validate/src/rules
git commit -m "feat: route workflow stages to ACP backend"
```

## Task 6: Add ACP Events And Run Projection Support

**Files:**
- Modify: `lib/crates/fabro-workflow/src/handler/llm/acp.rs`
- Modify: `lib/crates/fabro-workflow/src/event/events.rs`
- Modify: `lib/crates/fabro-workflow/src/event/names.rs`
- Modify: `lib/crates/fabro-workflow/src/event/convert.rs`
- Modify: `lib/crates/fabro-workflow/src/event/stored_fields.rs`
- Modify: `lib/crates/fabro-workflow/src/operations/fork.rs`
- Modify: `lib/crates/fabro-types/src/run_event/mod.rs`
- Modify: `lib/crates/fabro-types/src/run_event/misc.rs`
- Modify: `lib/crates/fabro-store/src/run_state.rs`
- Modify: `lib/crates/fabro-server/src/server.rs`
- Modify: `lib/crates/fabro-server/src/server/handler/steer.rs`
- Test: `lib/crates/fabro-workflow/src/event/convert.rs`
- Test: `lib/crates/fabro-store/src/run_state.rs`
- Test: `lib/crates/fabro-server/src/server/tests.rs`

- [ ] **Step 1: Write failing event conversion tests**

Add tests proving each event maps to stored event names:

```text
agent.acp.started
agent.acp.completed
agent.acp.cancelled
agent.acp.timed_out
```

Also assert converted/stored ACP events carry `node_id`, `stage_id`, and visit-derived stage scope fields just like `agent.cli.*`. This catches missing `stored_fields.rs` wiring, which otherwise makes run projection updates fail to find the active stage.

- [ ] **Step 2: Write failing projection tests**

Add store tests proving:

- `agent.acp.started` sets `stage.provider_used.mode == "acp"`
- provider, model, and command are preserved
- `agent.acp.completed` sets stage output to aggregated text/stderr payload
- cancelled and timed out terminal events set `CommandTermination::{Cancelled,TimedOut}`

- [ ] **Step 3: Write failing fork and steerability tests**

Add a fork replay test proving ACP provider metadata survives replay:

- `agent.acp.started` is included by `replay_event_for_fork_projection`
- `agent.acp.cancelled` and `agent.acp.timed_out` are included for terminal metadata, matching the existing CLI terminal-event behavior

Add server tests proving an active ACP stage is non-steerable:

- after `agent.acp.started`, `/runs/{id}/steer` returns a conflict when no API-mode session is active
- after `agent.acp.completed`, `agent.acp.cancelled`, `agent.acp.timed_out`, `stage.completed`, or `stage.failed`, the non-steerable active-stage marker is cleared

- [ ] **Step 4: Run tests to verify they fail**

Run:

```bash
ulimit -n 4096 && cargo nextest run -p fabro-workflow -E 'test(agent_acp)' && \
ulimit -n 4096 && cargo nextest run -p fabro-store -E 'test(agent_acp)' && \
ulimit -n 4096 && cargo nextest run -p fabro-server -E 'test(acp.*steer|steer.*acp)'
```

Expected: FAIL because event variants do not exist.

- [ ] **Step 5: Implement event variants**

Use fields parallel to CLI events, with ACP-specific additions, and wire `AgentAcpBackend` to emit them around `fabro_acp::run_acp_turn(...)`:

```rust
AgentAcpStarted {
    node_id: String,
    visit: u32,
    mode: String,       // always "acp"
    provider: String,
    model: String,
    command: String,
}

AgentAcpCompleted {
    node_id: String,
    stdout: String,     // aggregated ACP response text
    stderr: String,
    stop_reason: String,
    duration_ms: u64,
}
```

Cancelled/timed-out events mirror CLI terminal payloads.

- [ ] **Step 6: Implement stored fields, projection, and fork replay logic**

Update `stored_fields.rs` so every `AgentAcp*` event gets the same node/stage fields as its `AgentCli*` counterpart. Without this, `stage_at_current_visit` and `stage_at_stored_or_visit` cannot reliably attach ACP event data to the active stage.

Do not reuse `provider_used_from_agent_cli_started`; create `provider_used_from_agent_acp_started` so event names and mode cannot drift.

Update `operations/fork.rs` to replay `AgentAcpStarted`, `AgentAcpCancelled`, and `AgentAcpTimedOut` for fork projections, mirroring the existing CLI started/cancelled/timed-out replay behavior. Do not add `AgentAcpCompleted` unless the implementation also intentionally changes the existing CLI completed replay policy.

- [ ] **Step 7: Implement server steerability tracking**

Treat ACP-backed stages as active non-steerable agent stages while they are running:

- add `EventBody::AgentAcpStarted(_)` to the same active-stage set currently used for CLI-backed stages, or rename the set to a neutral `active_non_steerable_agent_stages` if the surrounding code becomes clearer
- remove the stage on `AgentAcpCompleted`, `AgentAcpCancelled`, `AgentAcpTimedOut`, `StageCompleted`, and `StageFailed`
- update the conflict message/code only if needed to avoid saying "CLI-mode" for an ACP-only active stage; tests should assert the API returns a clear non-steerable-agent conflict

- [ ] **Step 8: Run tests to verify they pass**

Run:

```bash
ulimit -n 4096 && cargo nextest run -p fabro-workflow -E 'test(agent_acp)' && \
ulimit -n 4096 && cargo nextest run -p fabro-store -E 'test(agent_acp)' && \
ulimit -n 4096 && cargo nextest run -p fabro-server -E 'test(acp.*steer|steer.*acp)'
```

Expected: PASS.

- [ ] **Step 9: Refactor and verify**

Check event JSON field names against existing CLI event style. Do not expose raw credentials, environment variables, full JSON-RPC logs, or prompt contents in events.

Run:

```bash
cargo build -p fabro-types -p fabro-workflow -p fabro-store -p fabro-server
```

Expected: PASS.

- [ ] **Step 10: Commit**

```bash
git add lib/crates/fabro-workflow/src/event lib/crates/fabro-workflow/src/operations/fork.rs lib/crates/fabro-types/src/run_event lib/crates/fabro-store/src/run_state.rs lib/crates/fabro-server/src/server.rs lib/crates/fabro-server/src/server/handler/steer.rs lib/crates/fabro-server/src/server/tests.rs
git commit -m "feat: project ACP backend events"
```

## Task 7: Wire ACP Backend Into Pipeline Initialization

**Files:**
- Modify: `lib/crates/fabro-workflow/src/pipeline/initialize.rs`
- Test: `lib/crates/fabro-workflow/src/pipeline/initialize.rs`
- Test: `lib/crates/fabro-workflow/tests/it/integration.rs`

- [ ] **Step 1: Write failing initialization test**

Add a test that builds a graph with `backend="acp"` and verifies the initialized registry can resolve the node and route to an ACP backend when credentials exist. Use a stub backend or fake ACP runner where needed; do not require live provider credentials.

- [ ] **Step 2: Run test to verify it fails**

Run:

```bash
ulimit -n 4096 && cargo nextest run -p fabro-workflow -E 'test(initialize.*acp) | test(backend_router_delegates_to_acp_for_acp_node)'
```

Expected: FAIL because initialization still constructs only API and CLI.

- [ ] **Step 3: Implement initialization wiring**

In `build_registry`, construct:

```rust
let acp = AgentAcpBackend::new(model.clone(), provider, cli_resolver.clone())
    .with_tool_env_provider(tool_env_provider, github_token_refresh_managed);
Some(Box::new(BackendRouter::new(Box::new(api), cli, acp)))
```

If the resolver type is not cloneable, restructure so CLI and ACP each receive their own resolver handle from the same source. Do not make ACP fall back to host env when a vault resolver is available.

- [ ] **Step 4: Run tests to verify they pass**

Run:

```bash
ulimit -n 4096 && cargo nextest run -p fabro-workflow -E 'test(initialize.*acp) | test(backend_router_delegates_to_acp_for_acp_node)'
```

Expected: PASS.

- [ ] **Step 5: Refactor and verify**

Ensure dry-run behavior is unchanged: dry-run builds no real backend and simulates all LLM handlers.

Run:

```bash
ulimit -n 4096 && cargo nextest run -p fabro-workflow -E 'test(dry_run) | test(router_)'
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add lib/crates/fabro-workflow/src/pipeline/initialize.rs lib/crates/fabro-workflow/tests/it/integration.rs
git commit -m "feat: initialize ACP workflow backend"
```

## Task 8: Add Black-Box Workflow Coverage With A Fake ACP Agent

**Files:**
- Create: `lib/crates/fabro-cli/tests/fixtures/acp/fake_acp_agent.rs` or equivalent checked-in script fixture
- Modify: `lib/crates/fabro-cli/tests/it/workflow/real_cli.rs` or create `lib/crates/fabro-cli/tests/it/workflow/acp.rs`
- Test: `lib/crates/fabro-cli/tests/it/workflow/`

- [ ] **Step 1: Write failing black-box test**

Create a temp workflow with an ACP-backed agent node:

```dot
digraph {
  work [type="agent", backend="acp", provider="openai", model="fake-acp", prompt="write hello.txt"]
}
```

Use a fake ACP command fixture that:

- responds to `initialize`
- responds to `session/new`
- on `session/prompt`, writes `hello.txt` in cwd
- emits two `agent_message_chunk` updates
- returns `stopReason: "end_turn"`

The test must assert:

- run succeeds
- response text contains concatenated chunks
- `hello.txt` is included in `files_touched`
- provider projection has `mode == "acp"`

- [ ] **Step 2: Run test to verify it fails**

Run:

```bash
ulimit -n 4096 && cargo nextest run -p fabro-cli -E 'test(acp_backend_workflow)'
```

Expected: FAIL before CLI/workflow wiring is complete.

- [ ] **Step 3: Use the `acp_command` override path**

Set the test node's `acp_command` to the checked-in fake agent command. The override was added in Task 2 and wired through the adapter in Task 5; this black-box test proves it works through real workflow parsing and execution. Do not add global config schema for this cutover.

- [ ] **Step 4: Run test to verify it passes**

Run:

```bash
ulimit -n 4096 && cargo nextest run -p fabro-cli -E 'test(acp_backend_workflow)'
```

Expected: PASS.

- [ ] **Step 5: Refactor and verify**

Keep the fake agent deterministic and dependency-light. Prefer a Rust test helper binary if it avoids shell quoting differences across macOS/Linux.

Run:

```bash
ulimit -n 4096 && cargo nextest run -p fabro-cli -E 'test(acp_backend_workflow) | test(full_pipeline_with_cli_backend_node)'
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add lib/crates/fabro-cli/tests
git commit -m "test: cover ACP backend workflow execution"
```

## Task 9: Update Documentation

**Files:**
- Modify: `docs/public/reference/dot-language.mdx`
- Modify: `docs/public/core-concepts/agents.mdx`
- Modify: any docs found by `rg -n "backend=.*cli|backend: cli|backend.*api|CLI backend|cli mode" docs/public lib/crates -g '*.md' -g '*.mdx'`

- [ ] **Step 1: Find stale backend docs**

Run:

```bash
rg -n "backend=.*cli|backend: cli|backend.*api|CLI backend|cli mode|one_shot" docs/public lib/crates -g '*.md' -g '*.mdx'
```

Expected: list current docs that mention only API/CLI or imply prompt nodes use CLI.

- [ ] **Step 2: Update docs**

Document:

- `backend` values are `api`, `cli`, and `acp`
- default is `api` unless model-specific routing says otherwise
- `cli` is legacy and may be replaced by ACP later
- `acp` runs an ACP agent via stdio inside supported sandboxes
- stable ACP does not portably accept model selection; Fabro records model metadata but command choice controls model behavior for now
- `acp_command` advanced override if implemented in Task 8
- Daytona ACP limitation if still unsupported

- [ ] **Step 3: Verify docs references**

Run:

```bash
rg -n "backend=.*cli|backend: cli|backend.*api|CLI backend|cli mode|ACP" docs/public lib/crates -g '*.md' -g '*.mdx'
```

Expected: no stale claim that prompt nodes support CLI, no missing ACP mention in backend reference.

- [ ] **Step 4: Commit**

```bash
git add docs/public
git commit -m "docs: document ACP workflow backend"
```

## Task 10: Final Verification

**Files:**
- Verify entire changed set

- [ ] **Step 1: Run targeted ACP tests**

Run:

```bash
ulimit -n 4096 && cargo nextest run -p fabro-acp
ulimit -n 4096 && cargo nextest run -p fabro-sandbox -E 'test(stdio)'
ulimit -n 4096 && cargo nextest run -p fabro-workflow -E 'test(router_) | test(acp_backend) | test(agent_acp) | test(initialize.*acp)'
ulimit -n 4096 && cargo nextest run -p fabro-validate -E 'test(backend_valid)'
ulimit -n 4096 && cargo nextest run -p fabro-store -E 'test(agent_acp)'
ulimit -n 4096 && cargo nextest run -p fabro-server -E 'test(acp.*steer|steer.*acp)'
ulimit -n 4096 && cargo nextest run -p fabro-cli -E 'test(acp_backend_workflow)'
```

Expected: all PASS.

- [ ] **Step 2: Run regression tests named in the accepted strategy**

Run:

```bash
ulimit -n 4096 && cargo nextest run -p fabro-workflow -E 'test(router_uses_cli_for_backend_attr) | test(router_uses_api_by_default) | test(backend_router_delegates_to_cli_for_cli_node) | test(backend_router_delegates_to_api_for_normal_node) | test(backend_router_delegates_to_cli_for_backend_attr) | test(full_pipeline_with_cli_backend_node) | test(stylesheet_backend_property_routes_to_cli) | test(cli_backend_run_writes_prompt_and_calls_exec) | test(cli_backend_run_with_codex_provider) | test(parse_real_codex_ndjson)'
ulimit -n 4096 && cargo nextest run -p fabro-mcp -E 'test(stdio_client_initialize_and_list_tools) | test(stdio_client_call_tool_echo) | test(connection_manager_stdio_roundtrip)'
```

Expected: all PASS.

- [ ] **Step 3: Run broader checks**

Run:

```bash
cargo build --workspace
cargo +nightly-2026-04-14 fmt --check --all
cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings
```

Expected: all PASS.

- [ ] **Step 4: Inspect final diff**

Run:

```bash
git status --short
git diff --stat main...HEAD
git diff --name-only main...HEAD
```

Expected: only ACP backend, sandbox stdio, event/projection, tests, and docs changes.

- [ ] **Step 5: Commit final fixes if needed**

If verification required code or docs fixes:

```bash
git add <changed files>
git commit -m "fix: complete ACP backend verification"
```

Expected: clean worktree.

## Regression Risks

- **Sandbox isolation bypass:** using `agent-client-protocol-tokio::AcpAgent` directly would spawn host processes. The implementation must instead run ACP stdio through the sandbox abstraction.
- **Prompt-node host execution:** ACP prompt nodes cannot be implemented against the old `CodergenBackend::one_shot` signature because it lacks sandbox/cancel-token access. The trait and `PromptHandler` call site must change, and API-backed one-shot implementations must ignore the new parameters.
- **SDK transport mismatch:** the ACP SDK expects futures-style line/byte streams; Fabro's sandbox API must expose stdin/stdout in a form `fabro-acp` can adapt to `agent_client_protocol::Lines` or `ByteStreams`. A write-line/read-line-only sandbox API is insufficient if it cannot be converted into a real transport.
- **Command parsing drift:** do not validate `acp_command` with `AcpAgent::from_str` and then execute the raw string. JSON stdio configs would be accepted but fail at runtime, and shell-word parsing would not protect sandbox command construction.
- **Missing Node runtime:** default ACP commands use `npx`. Without the shared Node bootstrap, fresh local/Docker sandboxes that currently work with `backend="cli"` may fail immediately with `npx: command not found`.
- **Crate cycle:** `fabro-acp` cannot implement workflow's `CodergenBackend` directly without making `fabro-workflow` and `fabro-acp` depend on each other. Keep protocol implementation in `fabro-acp` and the trait adapter in workflow.
- **PTY corruption:** ACP JSON-RPC must not use terminal sessions. Docker stdio uses `tty=false`; Daytona remains unsupported until raw stdio exists.
- **Permission deadlock:** ACP agents can send `session/request_permission` before performing edits. Without an automatic permission handler, real adapters may fail with method-not-found or stall even though legacy CLI mode already grants full-auto permissions.
- **Docker orphaned processes:** Bollard does not provide a simple kill-exec primitive. Docker stdio must use a controlled wrapper/stop-file or equivalent process-group cleanup so timeout and cancellation do not leave ACP adapters running inside the container.
- **Decorator capability loss:** `delegate_sandbox!`, `WorktreeSandbox`, read/write guards, and test-support sandboxes must forward `spawn_stdio_process`; otherwise ACP works on the base local sandbox but fails once wrapped by normal workflow setup.
- **Prompt backend ambiguity:** `backend="acp"` on prompt nodes must route to ACP or fail clearly. It must not silently use API.
- **Model drift:** stable ACP does not standardize model selection. Do not pretend node `model` was sent to the ACP agent unless the implementation actually supports it through an explicit command/config mechanism.
- **Event projection drift:** ACP events must set `mode="acp"` independently from CLI projection helpers.
- **Missing stored fields or fork replay:** ACP events must populate stage-scoped stored fields and fork replay rules. Otherwise provider metadata may be emitted correctly but fail to attach to the stage projection or disappear after a fork.
- **Incorrect steerability while ACP is running:** ACP-backed stages are not API-mode steerable in this cutover. The server must treat active ACP stages as non-steerable agent stages so steer requests do not get buffered as if an API session might appear.
- **Credential regression:** ACP must use the same credential resolver path as CLI mode so vault-backed installs do not fall back to missing host env vars.
