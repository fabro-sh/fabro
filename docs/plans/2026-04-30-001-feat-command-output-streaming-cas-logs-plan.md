---
title: "feat: Command output streaming and CAS log storage"
type: feature
status: active
date: 2026-04-30
---

# Command Output Streaming And CAS Log Storage

## Summary

Implement command-node stdout/stderr handling like CI logs: write streams to server scratch while the command runs, expose live reads through a REST tail endpoint, and finalize both streams into SlateDB CAS blobs when the process reaches a terminal state.

Key decisions:

- Use `StageId` (`tests@2`) as the concrete execution identity; no `StageAttemptId`.
- `command.output` and `command.stderr` context values become `blob://sha256/<hex>` CAS refs.
- Do not add `command.*_blob` context keys.
- Use REST tailing only for live reads; do not emit stdout/stderr chunk events.

## Key Changes

- Add one shared stream enum, `CommandOutputStream { Stdout, Stderr }`, reused by sandbox streaming and the new API.
- Keep `CommandStartedProps`; update `CommandCompletedProps` so `stdout` and `stderr` are CAS ref strings, with added `stdout_bytes` and `stderr_bytes`.
- Keep `NodeState.stdout` and `NodeState.stderr`, but store the same CAS refs there instead of inline output.
- Add `Sandbox::exec_command_streaming(...)` with a default fallback that calls existing `exec_command`; implement real streaming for local, Docker, and Daytona.
- Use Daytona Toolbox session APIs for Daytona streaming: create session, execute async command, poll command details/logs, and diff stdout/stderr from `/process/session/{sessionId}/command/{commandId}/logs`.
- In the command handler, write chunks to:
  - `RunScratch::runtime_dir()/stages/<encoded_stage_id>/command/stdout.log`
  - `RunScratch::runtime_dir()/stages/<encoded_stage_id>/command/stderr.log`
- Percent-encode the stage path segment using the same convention as `ArtifactStore` so unusual node IDs cannot escape the scratch directory.
- On terminal process state, write both scratch files to CAS, including empty streams, using `RunStoreHandle::write_blob`, then set context refs with `format_blob_ref`.
- Preserve current failure behavior as much as possible: successful and nonzero exits return outcomes with `command.output` / `command.stderr` refs; timeout/cancel still finalize CAS and emit `command.completed` before returning the existing error shape.

## API And UI

- Add OpenAPI route: `GET /api/v1/runs/{id}/stages/{stageId}/logs/{stream}`.
- `stream` is `stdout` or `stderr`.
- Query params:
  - `offset` default `0`
  - `limit` default `65536`, max `1048576`
- Response shape:
  - `stream`
  - `offset`
  - `next_offset`
  - `total_bytes`
  - `bytes_base64`
  - `eof`
  - `cas_ref`
- While a command is running, the endpoint reads from scratch files. After completion, or if scratch is gone, it reads from the CAS ref stored in `NodeState.stdout` / `NodeState.stderr`.
- Return `404` for unknown run/stage/log stream; return `200` with empty bytes when the stream exists but has no new data.
- Regenerate Rust and TypeScript API clients after editing `docs/public/api-reference/fabro-api.yaml`.
- Update the web run-stage command panel to treat command `stdout` / `stderr` event fields as refs, then poll the new REST endpoint by offset while the command is running and stop when `eof` is true.
- Update docs that currently say `command.output` and `command.stderr` contain inline text.

## Implementation Areas

- Sandbox/runtime:
  - `lib/crates/fabro-sandbox/src/sandbox.rs`
  - `lib/crates/fabro-sandbox/src/local.rs`
  - `lib/crates/fabro-sandbox/src/docker.rs`
  - `lib/crates/fabro-sandbox/src/daytona/mod.rs`
- Workflow/events/projection:
  - `lib/crates/fabro-workflow/src/handler/command.rs`
  - `lib/crates/fabro-workflow/src/event.rs`
  - `lib/crates/fabro-types/src/run_event/misc.rs`
  - `lib/crates/fabro-types/src/run_projection.rs`
  - `lib/crates/fabro-store/src/run_state.rs`
- API/web/docs:
  - `docs/public/api-reference/fabro-api.yaml`
  - `lib/crates/fabro-server/src/server.rs`
  - `apps/fabro-web/app/routes/run-stages.tsx`
  - `apps/fabro-web/app/lib/query-keys.ts`
  - `apps/fabro-web/app/lib/queries.ts`

## Test Plan

- Unit-test the scratch recorder: interleaved stdout/stderr chunks, byte offsets, empty streams, tails for failure messages, and path encoding.
- Unit-test `CommandCompletedProps` projection: `NodeState.stdout` / `stderr` store CAS refs and byte counts land in `script_timing`.
- Command handler tests: success, nonzero exit, stderr capture, long-running chunk written before completion, timeout finalizes CAS.
- Sandbox tests: local streaming chunks before process exit; Docker streaming from Bollard stdout/stderr frames; Daytona live/ignored test using session command logs when credentials are available.
- Server API tests: offset reads, limit enforcement, completed-run CAS fallback, missing run/stage/stream errors.
- Web tests: query key construction, base64 append behavior, polling stops on `eof`.
- Verification commands:
  - `cargo nextest run -p fabro-sandbox -p fabro-workflow -p fabro-store -p fabro-server`
  - `cargo build -p fabro-api`
  - `cd lib/packages/fabro-api-client && bun run generate`
  - `cd apps/fabro-web && bun test && bun run typecheck`

## Assumptions

- CAS refs use the existing `blob://sha256/<hex>` format.
- Command log CAS blobs store the actual stdout/stderr bytes from the scratch log files.
- REST tailing is the only live-log delivery mechanism for this pass.
- No new stage attempt identifier, no stdout/stderr chunk events, and no `_blob` context keys.
- Streaming CAS writes are out of scope for v1; finalization reads each scratch log once before writing it through the existing CAS API.
