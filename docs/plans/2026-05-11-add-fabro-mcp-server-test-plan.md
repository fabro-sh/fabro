# Fabro MCP Server Test Plan

## Harness Requirements

The agreed testing strategy still holds after reading the implementation plan. The plan narrows the tool contract to five Devin-shaped run tools and requires the implementation to live in a new `fabro-mcp-server` crate, but it does not add paid APIs, live LLM calls, external infrastructure, or browser/UI behavior. The highest-value evidence remains a real `fabro mcp start` subprocess driven over stdio and backed by Fabro's real local test server/auth harness.

1. **Deterministic MCP stdio fixture**
   - **Does:** constructs the exact command, environment, and cwd used to spawn `env!("CARGO_BIN_EXE_fabro") mcp start`.
   - **Exposes:** `command: Vec<String>`, `env: HashMap<String, String>`, and `current_dir: PathBuf` usable by both `fabro_mcp::client::McpClient` and raw `std::process::Command` tests.
   - **Complexity:** low. Add a narrow helper in `lib/crates/fabro-cli/tests/it/cmd/mcp.rs`; if needed, add `fabro_test::isolated_env(home_dir)` to mirror `apply_test_isolation`.
   - **Tests depending on it:** 5, 6, 7, 8, 9, 10, 15, 16, 17.

2. **MCP tool-call assertion helpers**
   - **Does:** calls a named MCP tool, asserts tool success or tool error, extracts `structured_content`, and verifies fallback text is concise rather than a JSON dump.
   - **Exposes:** `call_tool_json(...)`, `call_tool_error_text(...)`, and normalization helpers for run IDs, timestamps, paths, event IDs, cursors, durations, and elapsed times.
   - **Complexity:** low to medium. Keep it local to `cmd/mcp.rs` unless more than one test file needs it.
   - **Tests depending on it:** 8, 9, 10, 11, 12, 13, 15, 16, 17.

3. **Real authenticated Fabro server fixture**
   - **Does:** starts `RealAuthHarness::start_with_dev_token(...)`, seeds CLI dev-token auth into the test home, creates dry-run workflows through public CLI/MCP/API surfaces, and shuts down the server.
   - **Exposes:** API target URL, persisted auth entry, HTTP client/server-visible state checks, and workflow fixture paths.
   - **Complexity:** medium, mostly reuse existing `lib/crates/fabro-cli/tests/it/support/auth_harness.rs`.
   - **Tests depending on it:** 8, 10, 11, 12, 13, 14, 17.

## Test Plan

1. **`fabro mcp` help exposes the MCP namespace**
   - **Type:** integration
   - **Disposition:** new
   - **Harness:** output capture harness through existing `fabro_snapshot!`
   - **Preconditions:** isolated `TestContext`; no auth or server required.
   - **Actions:** run `fabro mcp --help`.
   - **Expected outcome:** stdout snapshots a `Model Context Protocol server` namespace with `start`, `config`, and `init` subcommands; stderr is empty; exit status is 0. Source of truth: user request for `fabro mcp start`, `fabro mcp config`, `fabro mcp init <agent>`, and implementation plan CLI contract.
   - **Interactions:** clap command tree, global CLI flags, snapshot filters.

2. **`fabro mcp start --help` documents stdio startup options**
   - **Type:** integration
   - **Disposition:** new
   - **Harness:** output capture harness through `fabro_snapshot!`
   - **Preconditions:** isolated `TestContext`; no auth or server required.
   - **Actions:** run `fabro mcp start --help`.
   - **Expected outcome:** stdout snapshots usage `fabro mcp start [OPTIONS]` with `--server <SERVER>` and `--storage-dir <DIR>`; stderr is empty; exit status is 0. Source of truth: implementation plan CLI contract.
   - **Interactions:** clap flattening for `ServerConnectionArgs`.

3. **`fabro mcp config --help` documents config rendering options**
   - **Type:** integration
   - **Disposition:** new
   - **Harness:** output capture harness through `fabro_snapshot!`
   - **Preconditions:** isolated `TestContext`; no auth or server required.
   - **Actions:** run `fabro mcp config --help`.
   - **Expected outcome:** stdout snapshots usage and the same connection override flags as `start`; stderr is empty; exit status is 0. Source of truth: implementation plan CLI contract.
   - **Interactions:** clap command help and global CLI flags.

4. **`fabro mcp init --help` documents supported agent selection**
   - **Type:** integration
   - **Disposition:** new
   - **Harness:** output capture harness through `fabro_snapshot!`
   - **Preconditions:** isolated `TestContext`; no auth or server required.
   - **Actions:** run `fabro mcp init --help`.
   - **Expected outcome:** stdout snapshots required `<AGENT>` with supported values `claude`, `cursor`, and `windsurf`; exit status is 0. Source of truth: user request and implementation plan supported-agent contract.
   - **Interactions:** clap value enum rendering.

5. **`fabro mcp config` prints generic MCP client JSON**
   - **Type:** integration
   - **Disposition:** new
   - **Harness:** output capture harness plus structured JSON parsing
   - **Preconditions:** isolated `TestContext`; no auth or server required.
   - **Actions:** run `fabro mcp config`; parse stdout as JSON.
   - **Expected outcome:** stdout is valid JSON with `mcpServers.fabro.command == "fabro"` and `args == ["mcp", "start"]`; stderr is empty; exit status is 0. Source of truth: Daytona-shaped user request and implementation plan config JSON contract.
   - **Interactions:** config rendering, stdout contract for a non-stdio command.

6. **`fabro mcp config` preserves connection flags in generated startup args**
   - **Type:** integration
   - **Disposition:** new
   - **Harness:** output capture harness plus structured JSON parsing
   - **Preconditions:** isolated `TestContext`; no auth or server required.
   - **Actions:** run `fabro mcp config --server https://example.test/api/v1 --storage-dir /tmp/fabro-mcp-storage`; parse stdout as JSON.
   - **Expected outcome:** JSON contains `args == ["mcp", "start", "--server", "https://example.test/api/v1", "--storage-dir", "/tmp/fabro-mcp-storage"]`; stderr is empty; exit status is 0. Source of truth: implementation plan examples for flag preservation.
   - **Interactions:** CLI argument forwarding into MCP client config.

7. **`fabro mcp init <agent>` writes idempotent config without clobbering unrelated keys**
   - **Type:** integration
   - **Disposition:** new
   - **Harness:** direct filesystem artifact assertion in isolated home
   - **Preconditions:** isolated `TestContext`; pre-existing Cursor config with `mcpServers.other` and unrelated top-level key.
   - **Actions:** run `fabro mcp init cursor --server https://example.test/api/v1` twice; read `~/.cursor/mcp.json`.
   - **Expected outcome:** parsed JSON preserves unrelated keys and existing `mcpServers.other`, contains exactly one `mcpServers.fabro` entry with command `fabro` and expected args, and the second run does not duplicate or reorder into an invalid shape. Source of truth: implementation plan idempotent config merge contract.
   - **Interactions:** filesystem directory creation, JSON merge/write, test home isolation.

8. **`fabro mcp init` writes each supported agent path**
   - **Type:** integration
   - **Disposition:** new
   - **Harness:** direct filesystem artifact assertion in isolated home
   - **Preconditions:** isolated `TestContext`; no existing Claude, Cursor, or Windsurf config.
   - **Actions:** run `fabro mcp init claude`, `fabro mcp init cursor`, and `fabro mcp init windsurf` in separate contexts; read the platform-specific config file for each.
   - **Expected outcome:** each config file exists at the path named by the implementation plan and contains `mcpServers.fabro` with `command: "fabro"` and `args: ["mcp", "start"]`. Source of truth: implementation plan agent path contract.
   - **Interactions:** platform-specific path selection, filesystem writes.

9. **`fabro mcp init` rejects invalid existing config without overwrite**
   - **Type:** boundary
   - **Disposition:** new
   - **Harness:** output capture and filesystem artifact assertion
   - **Preconditions:** isolated `TestContext`; Cursor config file contains invalid JSON bytes.
   - **Actions:** run `fabro mcp init cursor`; read the same file after failure.
   - **Expected outcome:** command exits non-zero with a clear error that includes the config path; the file content is byte-for-byte unchanged. Source of truth: implementation plan invalid JSON failure contract and error-handling strategy.
   - **Interactions:** JSON parsing, write avoidance on error, CLI error rendering.

10. **`fabro mcp start` initializes over stdio and lists the five run tools**
   - **Type:** scenario
   - **Disposition:** new
   - **Harness:** interaction harness using deterministic MCP stdio fixture and `fabro_mcp::client::McpClient`
   - **Preconditions:** isolated `TestContext`; no auth; no live Fabro server.
   - **Actions:** spawn `fabro mcp start`; perform MCP `initialize`; call `tools/list`.
   - **Expected outcome:** initialize succeeds without auth/server connectivity; `tools/list` returns exactly `fabro_run_create`, `fabro_run_search`, `fabro_run_interact`, `fabro_run_gather`, and `fabro_run_events`, each with an input schema. Source of truth: MCP lifecycle/tools spec as captured in the agreed strategy and implementation plan exact tool list.
   - **Interactions:** `rmcp` stdio transport, existing `fabro-mcp` client crate, child process lifecycle.

11. **`fabro mcp start` reserves stdout for JSON-RPC only**
   - **Type:** regression
   - **Disposition:** new
   - **Harness:** raw subprocess stdio harness
   - **Preconditions:** isolated `TestContext`; no auth; no live Fabro server.
   - **Actions:** spawn `fabro mcp start`; write a JSON-RPC `initialize` request to stdin; read the first stdout line.
   - **Expected outcome:** first stdout line parses as JSON and has `jsonrpc: "2.0"`; no leading human log/help text appears on stdout; stderr may contain logs. Source of truth: MCP stdio transport contract and implementation plan stdout invariant.
   - **Interactions:** CLI logging initialization, raw process pipes, JSON-RPC framing.

12. **MCP startup and tool discovery are fast without auth or server**
   - **Type:** invariant
   - **Disposition:** new
   - **Harness:** interaction harness plus timing assertion
   - **Preconditions:** isolated `TestContext`; no auth; no live Fabro server.
   - **Actions:** measure elapsed time for spawning `fabro mcp start`, initializing, and calling `tools/list`.
   - **Expected outcome:** operation completes under a generous smoke threshold, initially 2 seconds unless CI evidence requires a documented adjustment; all five tools are listed. Source of truth: agreed testing strategy performance smoke and implementation plan lazy API connection invariant.
   - **Interactions:** process startup, `rmcp` initialization, tool schema generation.

13. **`fabro_run_create` creates and starts a real dry-run using persisted CLI auth**
   - **Type:** scenario
   - **Disposition:** new
   - **Harness:** interaction harness plus real authenticated Fabro server fixture
   - **Preconditions:** `RealAuthHarness::start_with_dev_token(...)`; dev-token auth seeded into isolated home for the harness target; checked-in `simple.fabro` fixture installed.
   - **Actions:** spawn `fabro mcp start --server <target>`; call `fabro_run_create` with one run using `workflow`, `dry_run: true`, `auto_approve: true`, and label `source=mcp-test`.
   - **Expected outcome:** tool result is not an MCP error; `structured_content.runs[0]` includes a run id, workflow, `started: true`, and status; fallback text exists and does not start with `{` or `[`; server-visible state contains the created run. Source of truth: user request for run-management MCP tools, implementation plan create semantics, OpenAPI `POST /api/v1/runs`, and `POST /api/v1/runs/{id}/start`.
   - **Interactions:** persisted CLI auth store, Fabro API client, manifest builder/validation, run engine dry-run path.

14. **`fabro_run_search` filters, paginates, and includes archived runs by default**
   - **Type:** integration
   - **Disposition:** new
   - **Harness:** interaction harness plus real authenticated Fabro server fixture
   - **Preconditions:** authenticated MCP server; at least two MCP-created dry-run runs with distinct labels; one terminal run archived through API or MCP.
   - **Actions:** call `fabro_run_search` with `run_ids`, `workflow`, `labels`, `status`, `archived`, `first`, and `after` combinations.
   - **Expected outcome:** results are normalized run summaries; filters include only matching runs; `first` limits page size and returns an opaque cursor when more results exist; archived runs appear unless `archived: false` is supplied. Source of truth: implementation plan search semantics and OpenAPI list-runs include-archived behavior adapted by the plan.
   - **Interactions:** server run listing, status string normalization, timestamp/date parsing, cursor handling.

15. **`fabro_run_interact get/start/message/cancel` uses selector resolution and server APIs**
   - **Type:** integration
   - **Disposition:** new
   - **Harness:** interaction harness plus mocked HTTP server for precise API call assertions
   - **Preconditions:** isolated `TestContext`; HTTP mock server with `/api/v1/runs/resolve`, `/runs/{id}`, `/state`, `/start`, `/steer`, and `/cancel` endpoints; CLI auth seeded if the mock requires auth.
   - **Actions:** call `fabro_run_interact` with actions `get`, `start`, `message` with `interrupt: true`, and `cancel`, using a workflow-name selector rather than the exact run id.
   - **Expected outcome:** each action first resolves the selector through `/runs/resolve`; calls the matching endpoint; returns a structured object with `run_id`, `action`, and action-specific `result`; tool errors are not produced for mocked successful API responses. Source of truth: implementation plan interact semantics and OpenAPI operation descriptions for retrieve, state, start, steer, and cancel.
   - **Interactions:** run selector semantics, API error conversion, structured content projection.

16. **`fabro_run_interact archive/unarchive` changes real server-visible archived state**
   - **Type:** scenario
   - **Disposition:** new
   - **Harness:** interaction harness plus real authenticated Fabro server fixture
   - **Preconditions:** authenticated MCP server; completed dry-run created through MCP or public CLI.
   - **Actions:** call `fabro_run_interact` with `archive`; call `fabro_run_search` with `archived: true`; call `fabro_run_interact` with `unarchive`; call `fabro_run_search` with `archived: false`.
   - **Expected outcome:** archive action succeeds for the terminal run; archived search shows the run; unarchive action succeeds; unarchived search shows the run as terminal and not archived. Source of truth: implementation plan interact actions and OpenAPI archive/unarchive contracts.
   - **Interactions:** archive state transitions, list/search visibility, server-side idempotence.

17. **`fabro_run_interact get_questions/answer` maps answer JSON to the API contract**
   - **Type:** integration
   - **Disposition:** new
   - **Harness:** interaction harness plus mocked HTTP server for endpoint/body assertions
   - **Preconditions:** isolated `TestContext`; HTTP mock server returns pending questions and accepts answer submissions.
   - **Actions:** call `fabro_run_interact` with `get_questions`; call `answer` using representative payloads: `true`, `false`, string text, `{ "option": "a" }`, `{ "options": ["a", "b"] }`, and `{ "text": "hello" }`.
   - **Expected outcome:** `get_questions` returns the API question list projection; `answer` sends `SubmitAnswerRequest` wire shapes with `kind: yes`, `no`, `text`, `selected`, and `multi_selected`, and returns a successful structured action result. Source of truth: implementation plan answer mapping and `lib/crates/fabro-api/tests/submit_answer_request_round_trip.rs`.
   - **Interactions:** generated API type shape, JSON body serialization, human-in-the-loop endpoints.

18. **`fabro_run_gather` waits for terminal runs and returns current state on timeout**
   - **Type:** scenario
   - **Disposition:** new
   - **Harness:** interaction harness plus real authenticated Fabro server fixture
   - **Preconditions:** authenticated MCP server; one completed dry-run and one submitted/non-terminal run available.
   - **Actions:** call `fabro_run_gather` on the completed run; call it on the non-terminal run with `timeout_seconds: 1` and `poll_interval_seconds: 5`.
   - **Expected outcome:** completed run result has `timed_out: false` and terminal status; timeout case returns a successful structured result with `timed_out: true`, current run summary, and bounded elapsed wall time rather than an MCP/process error. Source of truth: implementation plan gather semantics and agreed performance/timeout strategy.
   - **Interactions:** selector resolution, polling loop, server retrieve endpoint, terminal status classification.

19. **`fabro_run_events` lists, details, searches, filters, paginates, and truncates events**
   - **Type:** integration
   - **Disposition:** new
   - **Harness:** interaction harness plus real authenticated Fabro server fixture
   - **Preconditions:** authenticated MCP server; completed dry-run with stored events.
   - **Actions:** call `fabro_run_events` with `action: "list"` and `first`; call `details` with returned event ids; call `search` with a known event-name substring; call filters for `event_types`, `categories`, `direction: "desc"`, `after`, `offset`, `limit`, and a small `max_content_length`.
   - **Expected outcome:** returned events belong to the run; list ordering and pagination match requested parameters; details returns only requested event ids; search returns serialized events containing the query; category filtering uses event-name prefix; oversized serialized payloads are truncated with `truncated: true`; `next_cursor` is derived from the last returned sequence. Source of truth: implementation plan events semantics and OpenAPI `GET /api/v1/runs/{id}/events`.
   - **Interactions:** event store pagination, event-name/category derivation, JSON serialization/truncation.

20. **Local validation errors happen before auth or network lookup and do not stop the server**
   - **Type:** boundary
   - **Disposition:** new
   - **Harness:** interaction harness with `--server http://127.0.0.1:9` and no auth
   - **Preconditions:** isolated `TestContext`; no auth entry; unreachable server URL.
   - **Actions:** call `fabro_run_gather` with 51 run ids; call `tools/list`; call `fabro_run_interact` action `message` without `message`; call `tools/list` again.
   - **Expected outcome:** each invalid tool call returns an MCP tool error mentioning the invalid field (`run_ids` or `message`); no auth guidance or connection error masks the local validation failure; subsequent `tools/list` succeeds. Source of truth: implementation plan validate-before-client invariant and MCP tool-error contract.
   - **Interactions:** parameter validation, lazy client initialization, MCP service liveness after errors.

21. **Auth failures use existing Fabro login guidance and remain tool errors**
   - **Type:** boundary
   - **Disposition:** new
   - **Harness:** interaction harness with protected real or mocked API target
   - **Preconditions:** isolated `TestContext`; no saved auth for the target; server requires auth.
   - **Actions:** spawn `fabro mcp start --server <protected-target>`; call a valid read tool such as `fabro_run_search`.
   - **Expected outcome:** call returns an MCP tool error, not process exit; error text includes `Run \`fabro auth login\` to authenticate.`; subsequent `tools/list` still succeeds. Source of truth: user request for no separate MCP auth and implementation plan auth invariant.
   - **Interactions:** auth store lookup, client connection, error classification/rendering.

22. **Invalid create inputs are rejected with field-specific tool errors**
   - **Type:** boundary
   - **Disposition:** new
   - **Harness:** interaction harness with no auth and unreachable server
   - **Preconditions:** isolated `TestContext`; no auth entry.
   - **Actions:** call `fabro_run_create` with empty `runs`, with 51 runs, and with `inputs` containing a null value.
   - **Expected outcome:** each call returns an MCP tool error naming the invalid field/key before any auth/server error; server remains alive for a subsequent `tools/list`. Source of truth: implementation plan create validation and JSON-to-TOML null rejection.
   - **Interactions:** schema/validation layer, JSON-to-TOML conversion.

23. **Run tool successes always include structured content and concise text**
   - **Type:** invariant
   - **Disposition:** new
   - **Harness:** MCP tool-call assertion helpers reused by scenario tests
   - **Preconditions:** any successful calls from tests 13, 14, 16, 18, and 19.
   - **Actions:** for each successful call, inspect `CallToolResult`.
   - **Expected outcome:** `structured_content` is present; at least one text content item is present; text content is short and does not begin with `{` or `[`; `is_error` is absent or false. Source of truth: implementation plan successful tool-result invariant.
   - **Interactions:** `rmcp::model::CallToolResult` construction and MCP client display fallback.

24. **Pure conversion helpers cover JSON-to-TOML and answer-request mapping**
   - **Type:** unit
   - **Disposition:** new
   - **Harness:** `cargo nextest run -p fabro-mcp-server run_tools`
   - **Preconditions:** none beyond crate compilation.
   - **Actions:** call conversion helpers directly for strings, bools, integers, floats, arrays, objects, null input, and every supported answer payload shape.
   - **Expected outcome:** JSON-compatible input values map to equivalent `toml::Value`; null returns an error naming the key; answer payloads serialize to `SubmitAnswerRequest` wire JSON with documented `kind` values; unsupported answer objects return a tool error. Source of truth: implementation plan conversion requirements and `fabro-api` submit-answer round-trip tests.
   - **Interactions:** serde, generated API types, conversion error text.

25. **Existing MCP client crate behavior is not regressed**
   - **Type:** regression
   - **Disposition:** existing
   - **Harness:** existing `fabro-mcp` crate tests
   - **Preconditions:** repository builds with the new `fabro-mcp-server` crate added.
   - **Actions:** run `cargo nextest run -p fabro-mcp`.
   - **Expected outcome:** existing stdio client initialize/list/call tests pass. Source of truth: existing automated evidence and implementation plan decision to keep `fabro-mcp` as the external MCP client crate.
   - **Interactions:** workspace dependency feature unification for `rmcp`, existing client transport behavior.

26. **Relevant existing CLI run/auth regressions still pass**
   - **Type:** regression
   - **Disposition:** existing
   - **Harness:** existing `fabro-cli` integration tests
   - **Preconditions:** implementation complete.
   - **Actions:** run the existing tests matching `scenario::auth::auth_login_refresh_logout_flow`, `scenario::lifecycle::dry_run_create_start_attach_works_with_default_run_lookup`, and `cmd::ps::ps_explicit_local_tcp_target_uses_auth_store`; if names drift, list tests and run the corresponding auth/lifecycle/local-target checks.
   - **Expected outcome:** all selected tests pass unchanged. Source of truth: agreed strategy existing automated evidence and user requirement that MCP reuse CLI auth/config behavior.
   - **Interactions:** auth refresh/logout, local server run lifecycle, server target resolution.

27. **Final MCP command contract and workspace checks pass**
   - **Type:** regression
   - **Disposition:** extend
   - **Harness:** repository command checks
   - **Preconditions:** all feature implementation and snapshots complete.
   - **Actions:** run `cargo nextest run -p fabro-cli --test it cmd::mcp`, `cargo +nightly-2026-04-14 fmt --check --all`, `cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings`, `ulimit -n 4096 && cargo nextest run --workspace`, and `cargo insta pending-snapshots`.
   - **Expected outcome:** MCP command tests pass; formatting and clippy pass; workspace tests pass; no pending snapshots remain unless explicitly inspected and accepted for this feature. Source of truth: repository `AGENTS.md` build/test commands and snapshot policy.
   - **Interactions:** entire workspace, rustfmt/clippy pinned nightly, nextest parallelism and file descriptor limit.

## Coverage Summary

Covered action space:

- CLI executable commands: `fabro mcp --help`, `fabro mcp start --help`, `fabro mcp config --help`, `fabro mcp init --help`, `fabro mcp config`, `fabro mcp config --server --storage-dir`, and `fabro mcp init claude|cursor|windsurf`.
- MCP protocol actions: stdio process startup, `initialize`, `tools/list`, and `tools/call`.
- MCP tool actions: `fabro_run_create`; `fabro_run_search`; `fabro_run_interact` actions `get`, `start`, `message`, `cancel`, `archive`, `unarchive`, `get_questions`, `answer`; `fabro_run_gather`; `fabro_run_events` actions `list`, `details`, and `search`.
- Error and boundary behavior: invalid local parameters, too many run ids, null input conversion, missing action fields, unsupported answer shapes, invalid agent config JSON, missing auth, unreachable server after local validation, timeout expiry, and service liveness after tool errors.
- Integration boundaries: CLI auth store reuse, Fabro API client, real local Fabro server, run manifest construction/validation, event store, generated API answer types, and existing `fabro-mcp` client crate.
- Performance smoke: initialize plus `tools/list` without auth/server.

Explicitly excluded per the agreed strategy:

- Live LLM/provider tests. Dry-run workflows and local/mocked servers cover run-management behavior without external credentials or spend.
- Manual QA of agent apps. `init` tests assert Fabro's written config path and JSON merge contract, not whether Claude/Cursor/Windsurf accept the file in a live app.
- Browser/UI tests. This feature adds CLI and MCP stdio surfaces only.
- Differential tests against Daytona or Devin. Their docs inspired shape, but no runnable reference implementation is available or required.

Residual risks:

- Agent config formats may evolve externally; tests protect Fabro's chosen file/path contract only.
- MCP SDK behavior can change with `rmcp` upgrades; protocol tests and existing `fabro-mcp` tests should catch startup/list/call regressions.
- Full workspace tests may be slower and subject to local FD limits; use the documented `ulimit -n 4096` command before `cargo nextest run --workspace`.
