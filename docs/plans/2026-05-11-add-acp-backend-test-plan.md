# ACP Backend Test Plan

The accepted testing strategy still holds, with scoped additions from the implementation plan: ACP prompt nodes are explicitly supported, backend validation becomes strict, sandbox stdio is part of the public contract, and ACP events affect run projection, fork replay, and server steerability. These additions do not require paid services or materially change the agreed scope because all high-value ACP checks can run against deterministic fake agents and local/unit harnesses.

## Harness Requirements

1. **Fake ACP agent harness**
   - What it does: runs a deterministic ACP agent over stdio, records observed JSON-RPC method order, emits configurable `session/update` messages, writes optional files in cwd, responds to permission requests, and simulates cancellation, malformed JSON, early exit, timeout, and stop reasons.
   - Exposes: a checked-in fixture binary or script plus crate-local helpers in `fabro-acp::test_support` using `agent-client-protocol` schema types where practical.
   - Complexity: medium. It is the main substitute for paid/live ACP agents.
   - Tests depending on it: 7, 8, 9, 10, 11, 12, 13, 26, 27.

2. **Sandbox stdio process harness**
   - What it does: exercises `Sandbox::spawn_stdio_process` with a line-oriented subprocess, captures stdout/stderr separately, terminates the process, and validates Docker exec option construction without requiring live Docker.
   - Exposes: local sandbox round-trip tests, Docker option-builder/control-wrapper tests, Daytona unsupported-provider assertion, and decorator/test-support forwarding assertions.
   - Complexity: medium because Docker stdio is multiplexed and cancellation needs an explicit control path.
   - Tests depending on it: 5, 6, 8, 12, 26.

3. **Workflow ACP runner harness**
   - What it does: runs a real Fabro workflow with `backend="acp"` and node-level `acp_command` pointing at the fake ACP agent, then inspects persisted run events/projection through existing CLI workflow helpers.
   - Exposes: user-visible `fabro run` result, run events, stage response, `files_touched`, and `provider_used`.
   - Complexity: low once the fake ACP agent exists.
   - Tests depending on it: 26, 27.

4. **Server event-state harness extension**
   - What it does: uses existing server test fixtures to insert a running run, apply ACP events, and call `POST /runs/{id}/steer`.
   - Exposes: HTTP status and JSON error codes through the Axum test router.
   - Complexity: low.
   - Tests depending on it: 23, 24.

## Test Plan

1. **Existing CLI backend behavior remains intact**
   - Type: regression
   - Disposition: existing
   - Harness: existing `fabro-workflow` router/CLI tests from the accepted strategy.
   - Preconditions: current repository before ACP changes; no ACP-specific code required.
   - Actions: run `ulimit -n 4096 && cargo nextest run -p fabro-workflow -E 'test(router_uses_cli_for_backend_attr) | test(router_uses_api_by_default) | test(backend_router_delegates_to_cli_for_cli_node) | test(backend_router_delegates_to_api_for_normal_node) | test(backend_router_delegates_to_cli_for_backend_attr) | test(full_pipeline_with_cli_backend_node) | test(stylesheet_backend_property_routes_to_cli) | test(cli_backend_run_writes_prompt_and_calls_exec) | test(cli_backend_run_with_codex_provider) | test(parse_real_codex_ndjson)'`.
   - Expected outcome: all tests pass; `backend="cli"` still routes agent nodes to CLI, default routing remains API, stylesheet `backend: cli` still works, CLI output parsing remains unchanged. Source of truth: user request to keep `api`, `cli`, and `acp` as three backends for now; implementation plan User-Visible Behavior for legacy CLI compatibility.
   - Interactions: router, CLI backend, stylesheet import, sandbox command execution, CLI event emission.

2. **Existing stdio JSON-RPC precedent remains intact**
   - Type: regression
   - Disposition: existing
   - Harness: existing `fabro-mcp` stdio integration tests.
   - Preconditions: Python is available; no live MCP service required.
   - Actions: run `ulimit -n 4096 && cargo nextest run -p fabro-mcp -E 'test(stdio_client_initialize_and_list_tools) | test(stdio_client_call_tool_echo) | test(connection_manager_stdio_roundtrip)'`.
   - Expected outcome: all tests pass; Fabro can still spawn a stdio JSON-RPC collaborator, initialize it, list capabilities, and call it. Source of truth: accepted strategy listed these as relevant stdio precedent.
   - Interactions: child process stdio, JSON-RPC framing, local subprocess lifecycle.

3. **ACP default command mapping matches provider families**
   - Type: unit
   - Disposition: new
   - Harness: `fabro-acp` command mapping tests.
   - Preconditions: `fabro-acp` crate exists with `agent-client-protocol-tokio = 0.11.1`.
   - Actions: call `default_acp_command` for Anthropic, OpenAI, Kimi, Zai, Minimax, Inception, OpenAI-compatible, and Gemini.
   - Expected outcome: Anthropic maps to `npx -y @zed-industries/claude-code-acp@latest`; OpenAI-compatible family maps to `npx -y @zed-industries/codex-acp@latest`; Gemini maps to `npx -y -- @google/gemini-cli@latest --experimental-acp`. Source of truth: implementation plan User-Visible Behavior default ACP command mapping.
   - Interactions: provider enum coverage and ACP Tokio parser defaults.

4. **ACP command overrides are parsed as stdio commands, not raw shell**
   - Type: boundary
   - Disposition: new
   - Harness: `fabro-acp` command parsing tests using `agent_client_protocol_tokio::AcpAgent::from_str`.
   - Preconditions: no sandbox required.
   - Actions: resolve `acp_command` values for a shell-word command, a blank string, a JSON stdio config with args/env, and a non-stdio JSON config.
   - Expected outcome: shell-word and JSON stdio commands expose parsed program/args/env; blank overrides fail with `acp_command must not be empty`; HTTP/SSE configs fail with `only stdio ACP commands are supported`; rendered sandbox command uses parsed parts with shell quoting. Source of truth: implementation plan command override contract and shell quoting invariant in `AGENTS.md`.
   - Interactions: ACP Tokio parser, command rendering, env merge inputs.

5. **Local sandbox stdio round-trips without a PTY**
   - Type: integration
   - Disposition: new
   - Harness: `fabro-sandbox` local stdio process harness.
   - Preconditions: temp local sandbox workspace; Python or a POSIX shell command available.
   - Actions: spawn a line-oriented process with `spawn_stdio_process`, write `abc\n` to stdin, read one stdout line, then terminate and wait.
   - Expected outcome: stdout returns the transformed line, stderr remains separately collectible, and `terminate()` completes without leaking the process. Source of truth: implementation plan Contracts And Invariants requiring sandbox-backed, bidirectional, non-PTY stdio.
   - Interactions: local process groups, env filtering, async IO, cancellation cleanup.

6. **Sandbox providers preserve or reject ACP stdio capability correctly**
   - Type: invariant
   - Disposition: new
   - Harness: `fabro-sandbox` provider/decorator tests.
   - Preconditions: local sandbox, read/write guard, worktree/decorator wrappers, test-support sandbox, and Daytona provider stub are available.
   - Actions: call `spawn_stdio_process` through each wrapper around a supporting sandbox; call it on Daytona; construct Docker exec stdio options.
   - Expected outcome: wrappers forward to the inner sandbox; Daytona returns `ACP backend requires bidirectional stdio; the Daytona sandbox provider does not support it yet`; Docker create/start options attach stdin/stdout/stderr and set `tty=false`; Docker termination uses the stop-file/control path. Source of truth: implementation plan provider support and PTY corruption risk.
   - Interactions: decorator macro, worktree path resolution, Docker option builder, Daytona provider boundary.

7. **ACP lifecycle initializes, creates a session, sends a prompt, and aggregates text**
   - Type: integration
   - Disposition: new
   - Harness: `fabro-acp` fake ACP agent harness.
   - Preconditions: fake ACP agent configured to emit two text `agent_message_chunk` updates and return `stopReason: "end_turn"`.
   - Actions: call `run_acp_turn` with a prompt and cwd.
   - Expected outcome: fake agent observes `initialize`, `session/new`, `session/prompt` in order; result text is the concatenation of text chunks; stop reason is `EndTurn`. Source of truth: ACP initialization/session/prompt docs and docs.rs quick-start lifecycle.
   - Interactions: official ACP SDK client, sandbox stdio transport, JSON-RPC ordering.

8. **ACP runs inside the active sandbox and sees the workflow cwd**
   - Type: integration
   - Disposition: new
   - Harness: `fabro-acp` fake agent plus local sandbox stdio.
   - Preconditions: temp sandbox workspace; fake agent writes `hello.txt` in its cwd during `session/prompt`.
   - Actions: call `run_acp_turn`, then inspect the sandbox workspace for `hello.txt`.
   - Expected outcome: file exists inside the sandbox workspace, not the host process cwd; `session/new` cwd matches `sandbox.working_directory()`. Source of truth: implementation plan Contracts And Invariants requiring ACP processes to run inside the active Fabro sandbox.
   - Interactions: sandbox cwd resolution, command launch, file mutation visibility.

9. **ACP permission requests auto-select an allow option**
   - Type: integration
   - Disposition: new
   - Harness: `fabro-acp` fake ACP agent harness.
   - Preconditions: fake agent sends `session/request_permission` with `AllowAlways`, `AllowOnce`, and reject options before completing the prompt.
   - Actions: call `run_acp_turn` and record the client response.
   - Expected outcome: client responds with the `AllowAlways` option id when present, then the turn continues and returns text. Source of truth: implementation plan permission handling contract; ACP supports agent-to-client permission requests.
   - Interactions: ACP client request handler, cancellation token state, prompt turn progress.

10. **ACP cancellation sends session cancel and returns cancellation**
    - Type: boundary
    - Disposition: new
    - Harness: `fabro-acp` fake ACP agent harness.
    - Preconditions: fake agent has created a session and is holding `session/prompt` open.
    - Actions: start `run_acp_turn`, cancel the token before completion, and let the fake agent record incoming notifications.
    - Expected outcome: client sends `session/cancel` for the active session, terminates if the agent does not finish within grace, and returns `AcpError::Cancelled`. If a permission request arrives after cancellation, the response is `RequestPermissionOutcome::Cancelled`. Source of truth: implementation plan cancellation contract and ACP prompt lifecycle stop reasons.
    - Interactions: cancel token, JSON-RPC notification, process termination.

11. **ACP timeout terminates the process and reports timeout**
    - Type: boundary
    - Disposition: new
    - Harness: `fabro-acp` fake ACP agent harness.
    - Preconditions: fake agent never responds to `session/prompt`; request timeout is short.
    - Actions: call `run_acp_turn`.
    - Expected outcome: process is terminated, stderr tail is available if emitted, and error is `AcpError::TimedOut`. Source of truth: implementation plan timeout contract using node timeout like CLI mode.
    - Interactions: watchdog activity, process handle termination, stderr collector.

12. **ACP protocol failures include diagnostic stderr without losing typed errors**
    - Type: boundary
    - Disposition: new
    - Harness: `fabro-acp` fake ACP agent harness.
    - Preconditions: fake agents for malformed JSON-RPC and early nonzero exit.
    - Actions: call `run_acp_turn` for each failure mode.
    - Expected outcome: malformed JSON returns a protocol error; early exit includes exit status and stderr tail; error source chains remain inspectable where applicable. Source of truth: implementation plan malformed/early-exit behavior and error-handling strategy.
    - Interactions: ACP SDK error propagation, stderr tail collection, process wait.

13. **ACP stop reasons map to Fabro backend outcomes**
    - Type: boundary
    - Disposition: new
    - Harness: `fabro-acp` fake agent plus workflow `AgentAcpBackend` adapter tests.
    - Preconditions: fake agent can return `EndTurn`, `Refusal`, `Cancelled`, `MaxTokens`, and `MaxTurnRequests`.
    - Actions: run an ACP backend turn for each stop reason.
    - Expected outcome: `EndTurn` and `Refusal` return text; `Cancelled` maps to `Error::Cancelled`; `MaxTokens` and `MaxTurnRequests` return handler errors containing the stop reason and partial output. Source of truth: implementation plan Stop reason handling.
    - Interactions: protocol result mapping, workflow error conversion, event terminal paths.

14. **ACP backend adapter prepares credentials, env, Node runtime, and changed files**
    - Type: integration
    - Disposition: new
    - Harness: `fabro-workflow` ACP adapter tests with fake credential resolver and fake sandbox.
    - Preconditions: node uses `backend="acp"`; fake resolver can provide env vars and login command; sandbox records commands and git status before/after.
    - Actions: call `AgentAcpBackend::run`.
    - Expected outcome: login command runs before ACP; tool env overlays command env; default `npx` commands trigger Node/npm/npx bootstrap; explicit `acp_command` does not install provider CLIs; `files_touched` excludes pre-existing dirty files and includes new changed/untracked files. Source of truth: implementation plan env preparation, Node bootstrap, and changed-file semantics.
    - Interactions: credential resolver, workflow tool env, sandbox exec, Git diff helper.

15. **ACP one-shot prompt nodes use sandboxed ACP and combine system prompt correctly**
    - Type: integration
    - Disposition: new
    - Harness: `fabro-workflow` `PromptHandler` and `AgentAcpBackend::one_shot` tests.
    - Preconditions: prompt node has `backend="acp"`; project memory can produce a system prompt; fake backend captures sandbox pointer and cancellation token.
    - Actions: execute the prompt handler.
    - Expected outcome: `PromptHandler` passes the active sandbox and run cancel token into `CodergenBackend::one_shot`; ACP one-shot sends `System:\n{system_prompt}\n\nUser:\n{prompt}` when system prompt exists and only the prompt when absent; no host process is used. Source of truth: implementation plan User-Visible Behavior for prompt/one_shot ACP support.
    - Interactions: prompt handler, memory discovery, backend trait signature, run services.

16. **Backend router selects api, cli, and acp explicitly**
    - Type: integration
    - Disposition: extend
    - Harness: `fabro-workflow` router tests.
    - Preconditions: router has API, CLI, and ACP test backends with distinguishable responses.
    - Actions: run agent nodes with absent backend, `backend="api"`, `backend="cli"`, `backend="acp"`, and `backend="codex"`.
    - Expected outcome: absent and `api` use API; `cli` uses CLI; `acp` uses ACP; unknown backend fails with `unsupported LLM backend "codex"; expected one of: api, cli, acp`. Source of truth: implementation plan three-way router selection and strict validation requirement.
    - Interactions: node attributes, model fallback, handler errors.

17. **Prompt router keeps legacy cli one-shot fallback but routes acp to ACP**
    - Type: regression
    - Disposition: extend
    - Harness: `fabro-workflow` router one-shot tests.
    - Preconditions: router has API and ACP one-shot test backends.
    - Actions: call `one_shot` for prompt nodes with absent backend, `backend="api"`, `backend="cli"`, and `backend="acp"`.
    - Expected outcome: absent, `api`, and legacy `cli` prompt nodes use API; `acp` uses ACP. Source of truth: implementation plan compatibility note for prompt nodes with `backend="cli"` and explicit ACP prompt support.
    - Interactions: backend routing, prompt handler behavior, backward compatibility.

18. **Workflow validation accepts only supported backend values**
    - Type: boundary
    - Disposition: new
    - Harness: `fabro-validate` `backend_valid` rule tests and CLI validate coverage if practical.
    - Preconditions: graphs with absent backend and with `api`, `cli`, `acp`, and `codex`.
    - Actions: run `fabro_validate::validate` against each graph; optionally run `fabro validate` against an invalid fixture.
    - Expected outcome: absent, `api`, `cli`, and `acp` have no backend diagnostic; `codex` returns an error diagnostic containing `unsupported LLM backend "codex"; expected one of: api, cli, acp`. Source of truth: implementation plan User-Visible Behavior for unknown backend values.
    - Interactions: validation registry, parser, CLI diagnostic rendering.

19. **Imported workflow placeholders propagate acp_command**
    - Type: regression
    - Disposition: extend
    - Harness: `fabro-workflow` import transform tests.
    - Preconditions: host workflow has an import placeholder with `backend="acp"` and `acp_command="python fake_agent.py"`; imported workflow has LLM nodes.
    - Actions: run `ImportTransform` and inspect imported node attrs.
    - Expected outcome: imported LLM nodes receive `backend="acp"` and the placeholder `acp_command`; unsupported placeholder attributes still poison the placeholder. Source of truth: implementation plan file list and import transform requirement.
    - Interactions: graph transform, default attribute propagation, import validation.

20. **ACP events serialize with stage-scoped metadata**
    - Type: integration
    - Disposition: new
    - Harness: `fabro-workflow` event conversion tests.
    - Preconditions: construct `Event::AgentAcpStarted`, `AgentAcpCompleted`, `AgentAcpCancelled`, and `AgentAcpTimedOut` with a `StageScope`.
    - Actions: convert each event through `to_run_event`.
    - Expected outcome: event names are `agent.acp.started`, `agent.acp.completed`, `agent.acp.cancelled`, and `agent.acp.timed_out`; envelope includes `node_id`, stage id/visit-derived fields, and no prompt/env/credential contents. Source of truth: events strategy and implementation plan ACP event contract.
    - Interactions: event naming, stored fields, `fabro-types` event body serde.

21. **Run projection records ACP provider metadata and terminal output**
    - Type: integration
    - Disposition: new
    - Harness: `fabro-store` run projection tests.
    - Preconditions: event sequence has stage start, `agent.acp.started`, terminal ACP event, and stage completion/failure.
    - Actions: apply events to `RunProjection`.
    - Expected outcome: `stage.provider_used.mode == "acp"` with provider, model, and command; completed output contains aggregated text/stderr payload; cancelled and timed-out terminal events set `CommandTermination::Cancelled` and `CommandTermination::TimedOut`. Source of truth: implementation plan run projection support.
    - Interactions: stored event fields, stage lookup by visit, projection terminal data.

22. **Fork replay preserves ACP stage metadata**
    - Type: regression
    - Disposition: new
    - Harness: `fabro-workflow` fork replay tests.
    - Preconditions: source run history includes ACP started/cancelled/timed-out events before a checkpoint.
    - Actions: call fork replay filtering or run a lower-level fork projection test.
    - Expected outcome: `AgentAcpStarted`, `AgentAcpCancelled`, and `AgentAcpTimedOut` are replayed into the fork projection; `AgentAcpCompleted` follows the existing CLI completed replay policy. Source of truth: implementation plan fork replay requirement.
    - Interactions: historical event filtering, forked run projection.

23. **ACP running stages are not steerable through the server API**
    - Type: scenario
    - Disposition: new
    - Harness: server event-state harness extension.
    - Preconditions: a running managed run with worker control channel; no active API-mode agent session; active stage marker has been set by `agent.acp.started`.
    - Actions: call `POST /runs/{id}/steer` with a plain steer request and with interrupt+steer.
    - Expected outcome: response is `409 CONFLICT` with a clear non-steerable-agent error code/message; no worker control message is enqueued as if an API session might appear. Source of truth: implementation plan server steerability tracking.
    - Interactions: run manager event reducer, HTTP handler, worker control queue.

24. **ACP non-steerable marker clears on all terminal paths**
    - Type: invariant
    - Disposition: new
    - Harness: server event-state harness extension.
    - Preconditions: a running managed run with active ACP stage and no active API-mode stage.
    - Actions: apply each clearing event independently: `agent.acp.completed`, `agent.acp.cancelled`, `agent.acp.timed_out`, `stage.completed`, and `stage.failed`; then call `POST /runs/{id}/steer` with a plain steer request.
    - Expected outcome: plain steer is accepted/buffered after each terminal event because no non-steerable active agent remains. Source of truth: implementation plan server steerability clearing rules.
    - Interactions: event reducer backstops, HTTP handler, stage lifecycle.

25. **Pipeline initialization wires ACP into real workflow handlers**
    - Type: integration
    - Disposition: new
    - Harness: `fabro-workflow` pipeline initialization tests.
    - Preconditions: graph contains `backend="acp"` LLM node; credentials are supplied through a stub/env source; dry-run and non-dry-run cases are both available.
    - Actions: call `initialize`/`build_registry` and execute or resolve the node through the initialized registry using a fake ACP runner.
    - Expected outcome: non-dry-run registry constructs a router with ACP; dry-run still builds no real backend and simulates LLM handlers; ACP does not fall back to host env when a resolver exists. Source of truth: implementation plan pipeline initialization task.
    - Interactions: credential source, handler registry, dry-run path.

26. **Black-box `fabro run` executes an ACP-backed agent workflow**
    - Type: scenario
    - Disposition: new
    - Harness: workflow ACP runner harness in `fabro-cli/tests/it/workflow`.
    - Preconditions: temp workflow has an agent node with `backend="acp"`, `provider="openai"`, `model="fake-acp"`, and `acp_command` pointing to the checked-in fake ACP agent; local sandbox is used.
    - Actions: run the workflow through the CLI test command, then read run state/events through existing workflow helpers.
    - Expected outcome: run succeeds; stage response contains concatenated chunks; `hello.txt` is included in `files_touched`; run projection has `provider_used.mode == "acp"`; `agent.acp.started` and `agent.acp.completed` events are present. Source of truth: user request for first-class `backend="acp"` and implementation plan black-box workflow coverage.
    - Interactions: CLI command, parser, validation, pipeline initialization, sandbox stdio, ACP protocol, run store.

27. **Black-box ACP prompt workflow uses ACP instead of API**
    - Type: scenario
    - Disposition: new
    - Harness: workflow ACP runner harness.
    - Preconditions: temp workflow has a prompt/one_shot node with `backend="acp"` and fake ACP command.
    - Actions: run the workflow through the CLI test command and inspect stage response/events.
    - Expected outcome: prompt node succeeds through ACP, response is fake ACP text, and `agent.acp.*` provider metadata appears; no API-mode `agent.session.activated` event is needed for the prompt. Source of truth: implementation plan User-Visible Behavior for `backend="acp"` on prompt/one_shot nodes.
    - Interactions: prompt handler, one-shot routing, pipeline initialization, run projection.

28. **Documentation examples and backend references include ACP without stale CLI prompt claims**
    - Type: regression
    - Disposition: extend
    - Harness: documentation grep plus existing docs build if normally run in CI.
    - Preconditions: docs have been updated.
    - Actions: run `rg -n "backend=.*cli|backend: cli|backend.*api|CLI backend|cli mode|ACP" docs/public lib/crates -g '*.md' -g '*.mdx'` and `cd apps/marketing && bun run build` only if the touched docs are built by that package.
    - Expected outcome: docs mention valid backend values `api`, `cli`, `acp`; `cli` is described as legacy; ACP sandbox and Daytona limitations are documented; no stale claim remains that prompt nodes use CLI mode. Source of truth: implementation plan documentation task.
    - Interactions: public docs, marketing/docs build pipeline.

29. **Final targeted ACP verification passes**
    - Type: invariant
    - Disposition: new
    - Harness: repository test suites named by the implementation plan.
    - Preconditions: all implementation tasks complete.
   - Actions: run:
     `ulimit -n 4096 && cargo nextest run -p fabro-acp --run-ignored all --no-fail-fast`;
     `ulimit -n 4096 && cargo nextest run -p fabro-sandbox --run-ignored all --no-fail-fast`;
     `ulimit -n 4096 && cargo nextest run -p fabro-workflow --run-ignored all --no-fail-fast`;
     `ulimit -n 4096 && cargo nextest run -p fabro-validate --run-ignored all --no-fail-fast`;
     `ulimit -n 4096 && cargo nextest run -p fabro-store --run-ignored all --no-fail-fast`;
     `ulimit -n 4096 && cargo nextest run -p fabro-server --run-ignored all --no-fail-fast`;
     `ulimit -n 4096 && cargo nextest run -p fabro-cli --run-ignored all --no-fail-fast`.
    - Expected outcome: every suite passes without skipped tests or live provider credentials. Source of truth: accepted strategy final verification and implementation plan Task 10.
    - Interactions: all changed crates and user-visible workflow/server surfaces.

30. **Workspace-wide build, formatting, and lint gates pass**
    - Type: invariant
    - Disposition: existing
    - Harness: repository-wide Cargo/rustfmt/clippy commands.
    - Preconditions: targeted tests pass.
   - Actions: run `cargo build --workspace`, `ulimit -n 4096 && cargo nextest run --workspace --run-ignored all --no-fail-fast`, `cargo +nightly-2026-04-14 fmt --check --all`, and `cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings`.
    - Expected outcome: build, workspace tests, formatting, and clippy all pass with zero skipped tests. Source of truth: repository `AGENTS.md` build/test commands and the no-skipped-tests final-run requirement.
    - Interactions: full workspace dependency graph, feature flags, generated code boundaries.

## Coverage Summary

Covered action space:

- Workflow authoring: `backend` absent, `api`, `cli`, `acp`, invalid values, stylesheet/import propagation, `acp_command` shell-word and JSON stdio overrides.
- Execution surfaces: agent nodes, prompt/one_shot nodes, local sandbox ACP execution, default command selection, explicit override execution, credentials/env, Node bootstrap, changed-file reporting, cancellation, timeout, and stop reason handling.
- Protocol behavior: `initialize`, `session/new`, `session/prompt`, `session/update` text aggregation, permission requests, `session/cancel`, malformed JSON-RPC, and early process exit.
- Provider/sandbox boundaries: local stdio, Docker non-PTY stdio option/control behavior, Daytona unsupported error, decorator forwarding.
- Product-visible state: ACP events, run projection `provider_used.mode == "acp"`, terminal output/termination, fork replay, CLI workflow run state, and server steerability API behavior.
- Regression protection: existing CLI routing/CLI parsing tests, existing MCP stdio tests, dry-run initialization, repository build/fmt/clippy.

Explicit exclusions:

- Live Anthropic/OpenAI/Gemini ACP adapter calls are excluded; fake ACP agents provide deterministic coverage without paid credentials. Risk: vendor-specific adapter quirks may escape until optional/live tests are added.
- Full live Docker ACP workflow execution is not required unless the existing test environment already provides Docker. Unit-level Docker exec option/control tests cover the non-PTY and termination contract. Risk: daemon-specific stream behavior could still differ from Bollard option construction.
- Remote ACP transports are excluded because the implementation plan supports only stdio in this cutover. Risk: none for the agreed scope.
- ACP client filesystem and terminal capabilities are excluded because Fabro intentionally advertises none in this cutover. Risk: agents requiring those client APIs will fail as documented rather than silently using unsafe host capabilities.
