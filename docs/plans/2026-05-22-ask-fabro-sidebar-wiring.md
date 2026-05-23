# Ask Fabro Sidebar Wiring — Implementation Plan

> **For agentic workers:** Use superpowers:subagent-driven-development or superpowers:executing-plans. Steps use `- [ ]` checkboxes.

**Goal:** Ship the Ask Fabro sidebar on run pages — wired to real session APIs, with the agent able to inspect its owning run.

**Architecture:** Two phases. Phase 1 (Rust): give Ask Fabro agent sessions the read-only `fabro_run_get` + `fabro_run_events` tools, scoped to the owning run, via the existing run-tool service path. Reuse `register_named_fabro_run_tools`; do not add a second subset registration helper. Phase 2 (web): replace the scripted sidebar adapter with real session calls; drop `?ask=1`, gate on `run.ask_fabro.available`.

**Tech Stack:** Rust (fabro-server, fabro-workflow, fabro-tool), React 19 + assistant-ui (fabro-web), generated API clients.

**Decisions locked:**
- Reuse `fabro-tool` — no new tool.
- Subset = `fabro_run_get` + `fabro_run_events`, read-only run inspection.
- Backend = existing `FabroRunToolServices` registration path. Prefer in-process server/store access when adding new same-process backends; avoid new loopback URL plumbing.
- Scoped to owning run — enforced by a same-run worker token; the API 403s cross-run calls.
- File/shell tools stay read-only in Ask Fabro sessions.
- Gate the sidebar on `run.ask_fabro.available`; drop `?ask=1`.

---

## Background (current state)

- API endpoints exist (`296fbddec`): `SessionDetail`, `/sessions/{id}/events`, `/sessions/{id}/attach`, turn submission w/ `x-fabro-turn-id`, `Run.ask_fabro` readiness. Web helpers exist: `session-stream.ts`, `sessionsApi`.
- Sidebar (`ask-fabro-sidebar.tsx`) is a prototype: scripted adapter (`chats-runtime.ts`/`chats-script.ts`), no API calls, gated behind `?ask=1` (`run-detail.tsx:363`).
- Ask Fabro sessions built by `build_agent_session` (`fabro-server/.../handler/sessions.rs`): profile + run sandbox + a read-only gate.
- `fabro-tool`: tools built on the `FabroToolBackend` trait. `FabroRunToolServices`, `register_fabro_run_tools`, `register_named_fabro_run_tools`, and `execute_fabro_run_tool` live in `fabro-workflow` (`handler/llm/api.rs`, `services.rs`).
- Worker tokens: `worker_token.rs` — `issue_worker_token(keys, &run_id)` mints a base same-run token; `AppState::worker_token_keys()` exposes the keys. `server.rs` already mints tokens this way for dispatched workers.
- `register_fabro_run_tools` is `pub(crate)`; `fabro-server` already depends on `fabro-workflow`.

---

## File structure

**Phase 1 — Rust**
- Modify: `lib/crates/fabro-server/src/server/handler/sessions.rs` — register the named read-only run tools and allowlist them in the gate.

**Phase 2 — Web**
- Create: `apps/fabro-web/app/lib/ask-fabro-runtime.ts` — real session adapter.
- Modify: `apps/fabro-web/app/components/chats/ask-fabro-sidebar.tsx` — use real adapter, take `runId`.
- Modify: `apps/fabro-web/app/routes/run-detail.tsx` — drop `?ask=1`, gate on `run.ask_fabro`.
- Delete (verify orphaned first): `apps/fabro-web/app/lib/chats-script.ts` + scripted paths in `chats-runtime.ts`.

---

## Phase 1: Run tools for Ask Fabro sessions

### Task 1 — Reuse named run-tool registration

- [ ] Use `register_named_fabro_run_tools` from `fabro-workflow/src/handler/llm/api.rs`.
- [ ] Register only `fabro_tool::FABRO_RUN_EVENTS_TOOL_NAME` and `fabro_tool::FABRO_RUN_GET_TOOL_NAME`.
- [ ] Do not add another subset helper or duplicate the tool-catalog filtering loop.
- [ ] `cargo build --workspace`; `cargo nextest run -p fabro-workflow agent_run`.

### Task 2 — Wire scoped run tools into `build_agent_session`

The session's run-inspection backend is scoped to the owning run. The same-run token remains the authorization backstop for HTTP-backed calls, and any future in-process backend must enforce the same run-id check before executing a tool.

**Files:** `fabro-server/src/server/handler/sessions.rs`

- [ ] Change `build_profile` to return `Box<dyn AgentProfile>` (currently `Arc`); the caller registers tools on `&mut` then `Arc::from`s.
- [ ] In `build_agent_session`, after `build_profile`, before `Session::from_record`:
  ```rust
  let worker_token = issue_worker_token(state.worker_token_keys(), &run_id)
      .map_err(|err| AskFabroBuildError::Agent(anyhow::Error::new(err)))?;
  // fabro_client::Client = generated reqwest client from the `fabro-client` crate.
  let api_client = fabro_client::Client::new_with_client(
      state.self_server_target()?,
      reqwest_client_with_bearer(&worker_token),
  );
  let backend = fabro_tool::fabro_client::FabroClient::new(Arc::new(api_client));
  let services = FabroRunToolServices {
      backend:            Arc::new(backend),
      current_run_id:     run_id,
      base_cwd:           PathBuf::new(),     // unused by events/get
      user_settings_path: PathBuf::new(),    // unused by events/get
  };
  register_named_fabro_run_tools(
      profile.tool_registry_mut(),
      &services,
      &[fabro_tool::FABRO_RUN_EVENTS_TOOL_NAME, fabro_tool::FABRO_RUN_GET_TOOL_NAME],
  );
  ```
  Reference impls: the worker-token mint in `server.rs`; `FabroRunToolServices` construction in `fabro-cli/src/commands/run/runner.rs`.
- [ ] Ensure the session prompt/context names the owning run id so the agent passes the correct `run_id` to the tools. (Backstop: wrong id → API 403, agent self-corrects.)
- [ ] `cargo build --workspace`.
- [ ] Commit: `feat(fabro-server): give Ask Fabro sessions run-control tools`.

### Task 3 — Allowlist the two run tools in the session gate

`build_ask_fabro_tool_approval` (`sessions.rs`) currently denies everything not `ReadOnly`-approved. The two read-only run tools should be allowed; file/shell stay read-only.

- [ ] Update the closure:
  ```rust
  Arc::new(move |tool_name: &str, _args: &Value| {
      if matches!(tool_name, "fabro_run_get" | "fabro_run_events") {
          return Ok(()); // read-only run-inspection tools, scoped by run id
      }
      if is_tool_auto_approved(PermissionLevel::ReadOnly, tool_name) {
          Ok(())
      } else {
          Err(format!("{tool_name} tool denied by Ask Fabro tool policy"))
      }
  })
  ```
- [ ] Tests: `fabro_run_get` and `fabro_run_events` approved; `write_file` and shell denied; `read_file` approved.
- [ ] `cargo +nightly-2026-04-14 fmt --all && cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings`.
- [ ] `cargo nextest run -p fabro-server --features test-support api::sessions`.
- [ ] Commit: `feat(fabro-server): allow run tools through the Ask Fabro session gate`.

### Task 4 — E2E coverage

- [ ] E2E test (twin), mirroring `tests/it/api/sessions.rs`: create a run with a sandbox, open an Ask Fabro session, submit a turn asking about run stages — assert the agent calls a read-only run tool and the turn completes.
- [ ] Cross-run guard test: a tool call with a different `run_id` is rejected (worker-token scope).
- [ ] `cargo nextest run -p fabro-server --features test-support --test it api::sessions`.
- [ ] Commit: `test(fabro-server): Ask Fabro run-tool E2E coverage`.

---

## Phase 2: Wire the sidebar

### Task 5 — Real session adapter

**Files:** Create `apps/fabro-web/app/lib/ask-fabro-runtime.ts`

- [ ] assistant-ui adapter parameterized by `runId`:
  - First turn: `sessionsApi.createRunSession(runId, { model })` (model from `run.ask_fabro.default_model`); persist session id in `sessionStorage` keyed by `runId` so reopen resumes.
  - Open with existing session id: `sessionsApi.getSession(id)` → render `SessionDetail.messages`, then `attachSessionEvents(id, { sinceSeq: last_seq })`.
  - Send: `streamSessionTurn(id, { input })`; map streamed `EventEnvelope`s (incl. `run.session.*` tool-call events) to assistant-ui messages.
- [ ] Route tool-call events through the existing `tool-fallback.tsx` renderer.
- [ ] `bun test app/lib/ask-fabro-runtime.test.ts` (mock SSE as `session-stream.test.ts` does).
- [ ] Commit: `feat(web): real session adapter for Ask Fabro sidebar`.

### Task 6 — Sidebar uses the adapter

- [ ] `ask-fabro-sidebar.tsx`: accept a `runId` prop; replace `createScriptedAdapter` with `ask-fabro-runtime`; remove `EMPTY_CHAT`/`scriptIndexRef`.
- [ ] `rg createScriptedAdapter` — if `chats-script.ts`/scripted paths are orphaned, delete them.
- [ ] `bun run typecheck`.
- [ ] Commit: `feat(web): drive Ask Fabro sidebar from session API`.

### Task 7 — Drop `?ask=1`, gate on readiness

- [ ] `run-detail.tsx`: remove `askEnabled`/`searchParams.get("ask")` (lines ~363-368, 635-648, 724-730).
- [ ] Render the Ask Fabro button always; `disabled={!run.ask_fabro.available}`. Disabled tooltip from `unavailable_reason`: `no_sandbox`/`sandbox_not_ready` → "Run sandbox isn't ready"; `llm_unconfigured` → "No LLM configured".
- [ ] Pass `runId={params.id}` to `<AskFabroSidebar>`.
- [ ] `bun run typecheck && bun test`.
- [ ] Commit: `feat(web): enable Ask Fabro sidebar on run pages`.

---

## Tests to run before each PR

- Rust: `cargo +nightly-2026-04-14 fmt --check --all` · `cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings` · `cargo build --workspace` · `cargo nextest run -p fabro-server -p fabro-workflow`
- Web: `cd apps/fabro-web && bun run typecheck && bun test`

## Unresolved questions

1. **`fabro_run_get` payload size** — `get` may return a large `RunProjection` (all stage data). If it blows agent context, consider a trimmed projection. Verify before Task 4.
2. **Session reuse** — one Ask Fabro session per run reused across sidebar opens (plan assumes this via `sessionStorage`), or fresh each open?
3. **Capability scope** — Ask Fabro is read-only in this plan. Mutating run-control tools should be a separate product decision.
4. **Phase split** — Phase 1 + 2 as two PRs (Phase 2 works without 1; agent just lacks run tools), or ship together so the feature only appears once useful?
