---
title: "feat: auto-start server and route CLI store access over HTTP"
type: feat
status: active
date: 2026-04-02
origin: docs/ideation/2026-04-02-slatedb-consolidation-ideation.md
deepened: 2026-04-04
---

# feat: auto-start server and route CLI store access over HTTP

## Overview

Phase 1 replaces direct CLI access to SlateDB with a Unix-socket HTTP client that talks to `fabro server`, and auto-starts that daemon whenever CLI store access is needed. Use the generated `fabro-api` Rust client as the transport surface where the existing API already matches the needed operations.

Phase 2 is an explicit follow-on decision: if we require strict single-owner semantics for all workflow execution writes, detached/start/resume execution must also move under server ownership rather than remaining in CLI-spawned engine processes.

## Problem Frame

The old plan assumed most of the HTTP surface and daemon infrastructure still needed to be built. That is no longer true.

Current state in the repo:

- Server daemon management is already implemented in `fabro-cli`:
  - `lib/crates/fabro-cli/src/commands/server/start.rs`
  - `lib/crates/fabro-cli/src/commands/server/record.rs`
- Unix socket bind support is already implemented in `fabro-server`:
  - `lib/crates/fabro-server/src/serve.rs`
  - `lib/crates/fabro-server/src/bind.rs`
- The server already owns a single `SlateStore` instance and already exposes store-backed endpoints for:
  - run state
  - events
  - live event attach over SSE
  - blobs
  - checkpoint
  - retro
  - stage artifacts
- The generated `fabro-api` client is reqwest-based and can be constructed with a custom `reqwest::Client`.
- Reqwest 0.13 in this repo supports Unix sockets via `ClientBuilder::unix_socket(...)`.

The real remaining work is narrower:

1. CLI store access still opens `SlateStore` directly from local disk.
2. CLI does not auto-start the server when store access is needed.
3. Some API gaps remain, especially durable run listing and durable run deletion.
4. Several CLI command paths still assume local engine processes own workflow execution and store writes.

## Requirements Trace

- R1. CLI commands that need store access auto-start `fabro server` when no active daemon is available.
- R2. CLI store reads and writes stop opening SlateDB directly and instead use the server over HTTP via Unix socket.
- R3. `fabro server` becomes the only process that opens SlateDB for migrated CLI store-access flows in phase 1, with full execution-path consolidation tracked separately in Unit 6 if strict single-owner semantics remain required.
- R4. The generated `fabro-api` Rust client is used for server communication where its current API surface applies.
- R5. Existing `InMemoryStore`-based tests and server-internal `SlateStore` usage remain unaffected.
- R6. Event streaming remains live and efficient for attach/log-follow workflows.
- R7. Binary transfer for blobs and artifacts remains raw bytes over HTTP.
- R8. Error messages for server startup and server-unreachable cases are explicit and actionable.
- R9. The plan must reflect the current repo state rather than the assumptions in the original 2026-04-02 draft.

## Scope Boundaries

In scope:

- CLI auto-start of the local server daemon
- HTTP-backed client access for CLI store consumers
- Server API additions needed for store parity
- Migration of CLI run discovery, logs/attach, blob/artifact access, and delete flows to server-backed access

Out of scope for the first pass:

- TypeScript/web client changes
- Replacing server-internal `SlateStore`
- Removing local run directories or runtime files
- Re-architecting workflow execution to be fully server-owned in the same change set
- Full trait refactors across `fabro-workflow` unless they are required to unblock the CLI migration

Follow-on scope, likely separate plan or addressed by Unit 6's decision fork:

- Consolidating detached/resume/start execution so workflow engine writes are also server-owned end-to-end

## Current-State Audit

### Already Implemented

- Daemon lifecycle:
  - `lib/crates/fabro-cli/src/commands/server/start.rs`
  - `lib/crates/fabro-cli/src/commands/server/status.rs`
  - `lib/crates/fabro-cli/src/commands/server/stop.rs`
  - `lib/crates/fabro-cli/src/commands/server/record.rs`
- Unix socket server binding:
  - `lib/crates/fabro-server/src/serve.rs`
  - `lib/crates/fabro-server/src/bind.rs`
- Existing server routes relevant to store access:
  - `GET /api/v1/runs/{id}/state`
  - `GET /api/v1/runs/{id}/events`
  - `GET /api/v1/runs/{id}/attach`
  - `POST /api/v1/runs/{id}/events`
  - `POST /api/v1/runs/{id}/blobs`
  - `GET /api/v1/runs/{id}/blobs/{blobId}`
  - `GET|POST /api/v1/runs/{id}/stages/{stageId}/artifacts`
  - `GET /api/v1/runs/{id}/stages/{stageId}/artifacts/download`
  - `GET /api/v1/runs/{id}/checkpoint`
  - `GET /api/v1/runs/{id}/retro`
- `RunProjection` already contains much more than the old plan assumed:
  - checkpoint and checkpoint history
  - retro and retro prompt/response
  - sandbox
  - final patch
  - pull request

### Still Missing or Mismatched

- `lib/crates/fabro-cli/src/store.rs` still builds a local `SlateStore`
- CLI commands are typed against concrete `SlateStore` / `SlateRunStore`
- Server `GET /api/v1/runs` is not durable store-backed; it serves in-memory managed runs only
- No durable delete endpoint exists for runs
- Artifact listing CLI still reads local artifact directories directly rather than using the server
- Detached CLI execution still opens the store directly and runs workflow engine code locally

## Key Technical Decisions

- **Use `fabro-api::Client` over Unix socket, not a custom hyper-only client.**
  - The previous plan's "reqwest cannot do Unix sockets" assumption is stale.
  - The generated client already covers `state`, `events`, `attach`, `blobs`, and artifact endpoints.
  - Build a thin `fabro-cli` client wrapper around:
    - `fabro_api::Client`
    - `reqwest::ClientBuilder::unix_socket(socket_path)`
  - For any operation not yet present in the OpenAPI spec, add the endpoint to the spec and regenerate `fabro-api`.

- **Do not introduce a new transport crate until there is a clear reuse case.**
  - The immediate consumers are in `fabro-cli`.
  - A small `fabro-cli::server_client` module is enough for the first migration.
  - If a second Rust crate later needs the same client, extract then.

- **Auto-start belongs in CLI store/bootstrap code, not in the generated client.**
  - The generated client should stay transport-only.
  - Server lifecycle discovery/start remains in `fabro-cli`.

- **Treat `RunProjection` as the primary read snapshot.**
  - The existing `/runs/{id}/state` endpoint already returns the coarse-grained shape most CLI reads need.
  - This should replace many fine-grained local store reads without widening the API.

- **Split the migration into two layers.**
  - Layer 1: CLI read/write operations that are naturally expressible against the current server API
  - Layer 2: execution-path consolidation for detached/resume/start flows if we want the server, not CLI subprocesses, to be the sole write owner

- **Preserve local run-directory access where it is orthogonal to SlateDB.**
  - Run discovery still needs local run-dir paths for UI output and fallback/orphan detection.
  - Runtime interview files and launcher metadata remain file-based unless separately redesigned.

## Open Questions

### Resolved for This Plan

- **Should we use the generated `fabro-api` client?**
  - Yes. It matches the repo direction and now works with Unix socket transport via reqwest.

- **Do we need a brand-new HTTP store crate first?**
  - No. Start with a thin CLI-side server client and only extract if a second consumer appears.

- **Does the server already expose enough snapshot data?**
  - Mostly yes. `RunProjection` already covers much more than the earlier plan assumed.

### Deferred to Implementation

- Whether to model the new CLI-side access layer as:
  - a direct "server client" API, or
  - a local wrapper that mimics `SlateStore` / `SlateRunStore`
- Whether artifact-list CLI should remain filesystem-based for local-only debugging or migrate fully to server-backed listing in phase 1
- Whether detached engine execution should be migrated in the same branch or explicitly deferred behind a feature boundary

## Relevant Code and Patterns

### Daemon and Unix Socket Patterns

- `lib/crates/fabro-cli/src/commands/server/start.rs`
- `lib/crates/fabro-cli/src/commands/server/record.rs`
- `lib/crates/fabro-server/src/serve.rs`
- `lib/crates/fabro-server/src/bind.rs`

### Existing Generated Client

- `lib/crates/fabro-api/src/lib.rs`
- `lib/crates/fabro-api/build.rs`
- `docs/api-reference/fabro-api.yaml`

### Store-Backed Server Routes

- `lib/crates/fabro-server/src/server.rs`

### CLI Entry Points That Must Migrate

- `lib/crates/fabro-cli/src/store.rs`
- `lib/crates/fabro-cli/src/commands/runs/list.rs`
- `lib/crates/fabro-cli/src/commands/run/logs.rs`
- `lib/crates/fabro-cli/src/commands/run/attach.rs`
- `lib/crates/fabro-cli/src/commands/run/diff.rs`
- `lib/crates/fabro-cli/src/commands/run/ssh.rs`
- `lib/crates/fabro-cli/src/commands/run/output.rs`
- `lib/crates/fabro-cli/src/commands/store/dump.rs`
- `lib/crates/fabro-cli/src/commands/pr/create.rs`
- `lib/crates/fabro-cli/src/commands/pr/list.rs`
- `lib/crates/fabro-cli/src/commands/pr/view.rs`
- `lib/crates/fabro-cli/src/commands/runs/rm.rs`
- `lib/crates/fabro-cli/src/commands/system/df.rs`
- `lib/crates/fabro-cli/src/commands/artifact/list.rs`
- `lib/crates/fabro-cli/src/commands/artifact/cp.rs`
- `lib/crates/fabro-cli/src/commands/run/wait.rs`
- `lib/crates/fabro-cli/src/commands/run/preview.rs`
- `lib/crates/fabro-cli/src/commands/run/rewind.rs`

### Run Discovery Coupling

- `lib/crates/fabro-workflow/src/run_lookup.rs`

### Execution-Path Coupling

- `lib/crates/fabro-cli/src/commands/run/create.rs`
- `lib/crates/fabro-cli/src/commands/run/start.rs`
- `lib/crates/fabro-cli/src/commands/run/detached.rs`
- `lib/crates/fabro-cli/src/commands/run/resume.rs`

## High-Level Design

### Layer 1: CLI server-backed store access

`fabro-cli` gains a small client/bootstrap layer:

1. Resolve active server from `server.json`
2. If absent or stale, auto-start daemon using existing lock/spawn/readiness logic
3. Construct `reqwest::Client` bound to the Unix socket
4. Construct `fabro_api::Client` with base URL like `http://fabro`
5. Expose helper methods for:
   - run state
   - run events list
   - run events attach SSE stream
   - blob read/write
   - stage artifact list/read/write
   - durable run list
   - durable run delete

CLI command handlers stop calling `build_store()` and instead call a new helper such as:

`store::connect_server(storage_dir) -> Result<ServerStoreClient>`

### Layer 2: API parity for run discovery and deletion

The server adds durable endpoints for:

- listing runs from the store catalog rather than only in-memory managed runs
- deleting run store state

The CLI keeps local run-dir fallback/orphan detection logic from `run_lookup.rs`, but its durable run summary source becomes the server.

### Layer 3: Optional execution consolidation

If we want strict compliance with "server is the only process accessing SlateDB", then detached/resume/start flows must stop opening run stores locally. That likely means:

- CLI creates/starts runs by calling server APIs
- server-owned scheduler/executor performs writes
- attach/logs become purely server-backed observers

This is separable from the read-path migration and should be treated as a deliberate second stage.

### Artifact handling boundary in phase 1

Artifact metadata and binary reads can move to server-backed endpoints in phase 1, but local run-directory artifact scanning may remain temporarily filesystem-based where commands are acting as local debugging tools rather than store clients. Implementation should make that boundary explicit rather than leaving a silent mixed-mode design.

## Implementation Units

- [ ] **Unit 1: Add CLI server bootstrap and generated-client construction**

**Goal:** Replace raw local `SlateStore` bootstrap in `fabro-cli` with "find or start server, then connect over Unix socket".

**Requirements:** R1, R2, R4, R5, R8

**Files:**
- Modify: `lib/crates/fabro-cli/src/store.rs`
- Create: `lib/crates/fabro-cli/src/server_client.rs`
- Modify: `lib/crates/fabro-cli/src/main.rs`
- Modify: `lib/crates/fabro-cli/Cargo.toml`

**Approach:**
- Add `ensure_server_running(storage_dir: &Path) -> Result<Bind>` in `server/start.rs` or a sibling helper module.
- Reuse:
  - `acquire_lock`
  - `active_server_record`
  - daemon spawn path
  - readiness polling
- Fast path: if `active_server_record()` exists, return its bind.
- If no record exists, start the daemon on the default Unix socket path and wait for readiness.
- In a new `server_client.rs`, build:
  - `reqwest::ClientBuilder::new().unix_socket(socket_path)`
  - `fabro_api::Client::new_with_client("http://fabro", reqwest_client)`

**Patterns to follow:**
- `lib/crates/fabro-cli/src/commands/server/start.rs`
- `lib/crates/fabro-cli/src/commands/server/record.rs`
- `lib/crates/fabro-api/src/lib.rs`

**Test scenarios:**
- Existing active server record returns immediately without spawning
- Missing server record triggers daemon start and waits for readiness
- Stale server record is ignored and replaced by a working daemon
- Unix socket connection errors produce a clear CLI-facing error

**Verification:**
- `cargo build -p fabro-cli`
- targeted tests for server start/status helpers in `fabro-cli`

- [ ] **Unit 2: Add durable server endpoints needed for CLI parity**

**Goal:** Close the durable API gaps that block CLI migration.

**Requirements:** R2, R3, R4, R5

**Files:**
- Modify: `docs/api-reference/fabro-api.yaml`
- Modify: `lib/crates/fabro-server/src/server.rs`
- Modify: `lib/crates/fabro-api/build.rs` only if codegen constraints require it
- Regenerate: `lib/crates/fabro-api` via normal build

**Required endpoints:**
- `GET /api/v1/store/runs`
  - durable run summaries from `state.store.list_runs(...)`
- `DELETE /api/v1/store/runs/{id}`
  - durable store deletion for a run

**Contract guardrail:**
- Do not change existing `GET /api/v1/runs` response semantics in this unit.
- The existing `/runs` route remains the board-oriented runtime view backed by `state.runs`.
- Durable catalog access belongs on the distinct `/api/v1/store/runs` surface unless a separate reviewed plan intentionally merges the concepts.

**Optional endpoint for phase-1 cleanup if needed:**
- `GET /api/v1/runs/{id}/artifacts`
  - if we decide to stop using local artifact directory scanning for listing/copy

**Patterns to follow:**
- Existing store-backed handlers in `lib/crates/fabro-server/src/server.rs`

**Test scenarios:**
- Durable run list returns runs persisted before current server boot
- Durable run delete removes store state for existing run
- Deleting missing run is idempotent or returns a clearly documented 404 behavior
- New endpoints are represented in `fabro-api` codegen output

**Verification:**
- `cargo build -p fabro-api`
- `cargo nextest run -p fabro-server`

- [ ] **Unit 3: Migrate run discovery to server-backed durable summaries**

**Goal:** Stop CLI run discovery from reading store summaries via direct `SlateStore`.

**Requirements:** R2, R3, R4

**Files:**
- Modify: `lib/crates/fabro-workflow/src/run_lookup.rs`
- Modify: `lib/crates/fabro-cli/src/commands/runs/list.rs`
- Modify: `lib/crates/fabro-cli/src/commands/system/df.rs`
- Modify: `lib/crates/fabro-cli/src/commands/pr/list.rs`
- Modify: `lib/crates/fabro-cli/src/commands/run/mod.rs`
- Add tests in:
  - `lib/crates/fabro-cli/tests/it/cmd/`
  - `lib/crates/fabro-workflow` tests if `run_lookup` signatures change

**Approach:**
- Decouple `run_lookup` from concrete `SlateStore` inputs.
- Introduce a smaller input shape for durable summaries, likely:
  - a plain `Vec<RunSummary>`, or
  - a small trait implemented by both local tests and the new CLI client wrapper
- Preserve local run-dir fallback/orphan detection from filesystem scanning.
- Use server-provided durable summaries as the authoritative store source.

**Patterns to follow:**
- `lib/crates/fabro-workflow/src/run_lookup.rs`

**Test scenarios:**
- Persisted run appears in list even after server restart
- Local orphan run still appears when store summary is absent
- Prefix resolution still works against durable summaries plus local paths
- `runs list` behavior remains unchanged for filters and JSON output

**Verification:**
- `cargo nextest run -p fabro-cli runs_list`
- targeted `run_lookup` tests

- [ ] **Unit 4: Migrate read-heavy CLI commands to server-backed state/events/blob/artifact access**

**Goal:** Move the majority of CLI read flows off direct SlateDB access.

**Requirements:** R2, R3, R4, R6, R7, R8

**Files:**
- Modify: `lib/crates/fabro-cli/src/commands/run/logs.rs`
- Modify: `lib/crates/fabro-cli/src/commands/run/attach.rs`
- Modify: `lib/crates/fabro-cli/src/commands/run/diff.rs`
- Modify: `lib/crates/fabro-cli/src/commands/run/ssh.rs`
- Modify: `lib/crates/fabro-cli/src/commands/run/output.rs`
- Modify: `lib/crates/fabro-cli/src/commands/store/dump.rs`
- Modify: `lib/crates/fabro-cli/src/commands/pr/create.rs`
- Modify: `lib/crates/fabro-cli/src/commands/pr/view.rs`
- Modify: `lib/crates/fabro-cli/src/commands/run/wait.rs`
- Modify: `lib/crates/fabro-cli/src/commands/run/preview.rs`

**Approach:**
- Replace direct `open_run_reader()` usage with calls to:
  - `get_run_state`
  - `list_run_events`
  - `attach_run_events`
  - `read_run_blob`
  - `list_stage_artifacts`
  - artifact download endpoint
- Build one SSE parsing helper in CLI for `attach_run_events()` byte streams.
- Continue using `RunProjection` as the coarse-grained read model.

**Patterns to follow:**
- `lib/crates/fabro-server/src/server.rs` attach SSE response shape
- Existing CLI attach/log rendering logic

**Test scenarios:**
- `logs --follow` continues to stream events to completion
- `attach` replays existing events then follows live events
- state-driven commands still read final patch, PR data, sandbox, checkpoint, and retro from `RunProjection`
- blob and artifact download remain raw bytes

**Verification:**
- targeted `fabro-cli` command tests:
  - logs
  - attach
  - diff
  - pr view/create
  - store dump

- [ ] **Unit 5: Migrate write/delete CLI operations that should hit the server**

**Goal:** Stop CLI deletion and similar store mutations from touching SlateDB directly.

**Requirements:** R2, R3, R4, R8

**Files:**
- Modify: `lib/crates/fabro-cli/src/commands/runs/rm.rs`
- Modify: `lib/crates/fabro-cli/src/commands/run/start.rs` if status validation moves to server reads only
- Modify: `lib/crates/fabro-cli/src/commands/run/rewind.rs` only if it currently depends on direct store writes in the targeted path

**Approach:**
- Replace direct `store.delete_run()` calls with server API delete.
- Replace direct event append for `RunRemoving` with `POST /runs/{id}/events`.
- Keep local run-dir deletion and sandbox cleanup local unless a server-owned deletion flow is explicitly introduced.

**Test scenarios:**
- removing a completed run deletes local run dir and durable store state
- removing a run with missing store state still behaves predictably
- server-unreachable deletion path returns actionable error text

**Verification:**
- `cargo nextest run -p fabro-cli runs_rm`

- [ ] **Unit 6: Decide and document the execution-ownership boundary**

**Goal:** Explicitly close the gap between "CLI reads via server" and "server is the only process accessing SlateDB".

**Requirements:** R3, R9

**Files:**
- Modify: this plan document after implementation decision, or create follow-on plan
- Review:
  - `lib/crates/fabro-cli/src/commands/run/create.rs`
  - `lib/crates/fabro-cli/src/commands/run/start.rs`
  - `lib/crates/fabro-cli/src/commands/run/detached.rs`
  - `lib/crates/fabro-cli/src/commands/run/resume.rs`

**Decision fork:**

Option A: **Phase-1 complete means CLI read/write store access is server-backed, but local engine subprocesses remain**
- Faster
- Leaves a strict reading of R3 partially unmet

Option B: **Phase-2 also migrates execution ownership to the server**
- CLI create/start/resume become HTTP calls
- Detached local engine path is retired or reduced to server-only launch
- Strictly satisfies "server is the only process accessing SlateDB"

**Recommendation:**
- Treat Option A as the first implementation milestone
- Open a follow-on plan immediately for Option B if the requirement remains strict

**Implementation note (2026-04-04):**
- This branch follows Option A.
- CLI durable run discovery, wait/logs/diff/preview/PR listing, and run deletion are moving behind the server-backed store client.
- Detached/create/start/resume execution ownership remains local for now and still needs a follow-on server-owned execution plan if strict single-owner SlateDB semantics remain required.

**Test scenarios:**
- explicit documentation of whichever boundary we choose
- no silent mixing of local-store and server-store code paths remains after the chosen milestone

## Sequencing

1. Unit 1 first: auto-start and client bootstrap
2. Unit 2 second: add missing durable API surface
3. Unit 3 third: migrate run discovery and durable summary reads
4. Unit 4 fourth: migrate read-heavy commands
5. Unit 5 fifth: migrate store mutations that still happen in CLI
6. Unit 6 last: finalize the execution-ownership boundary and either defer or continue

## Risks and Mitigations

- **Risk: `/api/v1/runs` semantics are runtime-board state, not durable store state**
  - Mitigation: add distinct `/api/v1/store/runs` routes rather than silently repurposing `/runs`

- **Risk: `run_lookup` is coupled to concrete `SlateStore`**
  - Mitigation: extract a smaller durable-summary input shape before touching multiple commands

- **Risk: SSE parsing via generated client is lower-level than current in-process stream usage**
  - Mitigation: centralize byte-stream-to-event parsing in one helper and cover it with focused tests

- **Risk: some CLI commands still depend on local runtime files rather than store data**
  - Mitigation: migrate only true store access in this plan; do not conflate run-dir filesystem concerns with SlateDB consolidation

- **Risk: execution paths still open the store directly after read-path migration**
  - Mitigation: explicitly treat execution consolidation as a tracked decision point, not an accidental omission

## Verification Strategy

Per-unit targeted verification:

- `cargo build -p fabro-api`
- `cargo nextest run -p fabro-server`
- `cargo nextest run -p fabro-cli`

Focused command coverage should include:

- server start/status/stop
- runs list
- logs
- attach
- diff
- pr view/create/list
- runs rm
- store dump

Manual smoke flow after Units 1-5:

1. stop any running server
2. run a CLI command that needs store access
3. verify server auto-starts
4. verify the command succeeds through the Unix socket path
5. verify no CLI path in the tested flow opens local SlateDB directly

Durability smoke flow:

1. create or identify a persisted run
2. stop the server
3. start the server again
4. verify durable run listing still finds the run through the HTTP path
5. verify run state for that run is still readable through the HTTP path

## Change Summary

The old plan is no longer the right implementation guide. The repo already has daemon management, Unix socket support, store-backed server endpoints, and a generated Rust client that can be used over UDS. The remaining plan is to:

- auto-start the server from CLI store bootstrap
- use `fabro-api::Client` over Unix socket
- add the missing durable run-list/delete endpoints
- migrate CLI store consumers off direct `SlateStore`
- then explicitly decide whether execution ownership also moves fully into the server
