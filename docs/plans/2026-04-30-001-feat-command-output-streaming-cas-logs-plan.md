---
title: "feat: Command output streaming and CAS log storage"
type: feature
status: active
date: 2026-04-30
---

# Command Output Streaming And CAS Log Storage

## Summary

Implement command-node stdout/stderr handling like CI logs: write output to server scratch while the command runs, expose live reads through a REST tail endpoint, and finalize both streams into SlateDB CAS refs when the command reaches a terminal state.

This plan deliberately separates two concerns:

- Live log bytes are raw scratch files used by the tail endpoint while the command is running.
- Durable context values are JSON-compatible CAS blobs. `command.output` and `command.stderr` hold `blob://sha256/<hex>` refs whose blob payloads are JSON strings, not raw log bytes.

Key decisions:

- `StageId` is `<node>@<attempt>` and is the only public execution identity; no `StageAttemptId`.
- `command.output`, `command.stderr`, `NodeState.stdout`, `NodeState.stderr`, and new `command.completed` stdout/stderr values are CAS refs after finalization, including empty streams.
- Existing old runs with inline stdout/stderr remain supported by treating these fields as text-or-blob-ref strings.
- REST tailing is chosen over stdout/stderr chunk events to avoid high-volume durable event logs and SSE backpressure from chatty commands. Events record lifecycle and final refs; log bytes live in scratch/CAS and are read on demand.
- No automatic redaction is applied to command stdout/stderr in this pass. Command output is trusted, authenticated run data like CI logs.

## Storage And Runtime

- Add one shared closed enum, `CommandOutputStream { Stdout, Stderr }`, for sandbox streaming and API path validation.
- Add `Sandbox::exec_command_streaming(...)` with a default compatibility fallback that calls `exec_command` and emits buffered stdout/stderr after process exit. Real streaming is required for local and Docker in this pass.
- Implement a command log recorder with one writer per `(run_id, stage_id, stream)`. The recorder appends bytes to scratch, flushes after each chunk or bounded batch, and tracks `stdout_bytes` / `stderr_bytes`.
- Scratch paths use the existing run scratch root and artifact-style encoding:
  - `RunScratch::runtime_dir()/stages/<encoded_node_id>@<attempt_padded_4>/command/stdout.log`
  - `RunScratch::runtime_dir()/stages/<encoded_node_id>@<attempt_padded_4>/command/stderr.log`
- The API accepts the normal `StageId` string (`tests@2`); filesystem storage percent-encodes only the node id and pads the attempt to match `ArtifactStore` conventions.
- The command handler must use the existing `run_dir: &Path` and `StageScope::for_handler(...)`; add a `StageScope::stage_id() -> StageId` helper if needed. Direct handler tests must seed or fall back to attempt `1`.
- Terminal command state means normal exit, nonzero exit, timeout after subprocess spawn, or cancellation after subprocess spawn. Before final CAS writes, stream drain tasks are joined and recorder file handles are flushed.
- On terminal command state, read each scratch log into memory, convert it to the command text value, JSON-encode that string, write it with `RunStoreHandle::write_blob`, and store `format_blob_ref(...)` in `command.output` / `command.stderr`.
- Empty streams are finalized the same way as non-empty streams: their JSON string value is `""`, byte count is `0`, and the context/projection/event fields still contain a CAS ref.
- After finalization, `command.output` and `command.stderr` are already blob refs, so the existing 100KB `offload_large_values` lifecycle step is a no-op for those values.
- If command spawn fails before logs are established, preserve the existing handler error path and do not invent CAS refs.
- If the server crashes mid-command, scratch files may remain readable while the run scratch directory exists, but no CAS refs are created because no `command.completed` finalization occurred. No recovery job or automatic partial CAS finalization is included in v1.
- Scratch files are kept after finalization for the normal run scratch lifetime. The tail endpoint prefers scratch if present and falls back to CAS only when scratch is missing.
- Finalization may hold stdout/stderr in memory. This is acceptable for v1 because expected command outputs are megabytes and deployment hosts have gigabytes of RAM.

## Events, Context, And Consumers

- Keep `CommandStartedProps` as the lifecycle start event; do not add chunk/delta events.
- Update `CommandCompletedProps` so new events keep `stdout` and `stderr`, but those fields contain final CAS refs rather than inline text. Add:
  - `stdout_bytes`
  - `stderr_bytes`
  - `streams_separated`
  - `live_streaming`
- Add matching optional fields to `NodeState` so projections can expose byte counts and provider fidelity. Existing old events without these fields must deserialize with defaults.
- For local and Docker, `streams_separated = true` and `live_streaming = true`.
- For Daytona, use Toolbox session APIs. Prefer direct HTTP handling of `/process/session/{sessionId}/command/{commandId}/logs` so JSON `{ stdout, stderr, output }` responses can be separated. Poll command logs every 1 second while the command is active; after 3 consecutive transient log-fetch failures, keep the command running but mark `live_streaming = false` and fall back to the final command response when available. If the SDK/API yields only a plain string or prefixed combined output, write combined output to stdout, write empty stderr, set `streams_separated = false`, and set `live_streaming` according to whether output was available before process completion.
- Durable context and checkpoints store refs. Execution-time consumers that need text must resolve `command.output` / `command.stderr` with a shared text-or-blob-ref helper:
  - if `parse_blob_ref(value)` succeeds, read the blob and decode it as a JSON string;
  - otherwise treat the value as legacy inline text.
- Use that helper for LLM preamble rendering, conditional/router stages, retros, run dumps, CLI displays, server/API consumers, and the web command panel. Models should continue to see the same tail-style command output as today, not a `blob://...` string.
- Nonzero exits still produce `Outcome::fail_classify`; timeout/cancel after spawn still emit `command.completed` with partial refs and then return the existing handler error shape. Failure text should include at most the last 4 KiB from each resolved stream so durable failure records do not embed unbounded logs.

## API And UI

- Add OpenAPI route: `GET /api/v1/runs/{id}/stages/{stageId}/logs/{stream}`.
- The route is under the existing run-scoped API router and must inherit the same auth translation, run authorization, and IP allowlist behavior as other run endpoints. Add an unauthenticated-request test.
- Path params parse into typed `RunId`, `StageId`, and closed `CommandOutputStream`; reject any non-`stdout`/`stderr` stream before touching the filesystem.
- Query params:
  - `offset` default `0`
  - `limit` default `65536`, max `1048576`
- Response shape:
  - `stream: "stdout" | "stderr"`
  - `offset: u64`
  - `next_offset: u64`
  - `total_bytes: u64`
  - `bytes_base64: string`
  - `eof: bool`
  - `cas_ref: Option<String>`
  - `live_streaming: bool`
- While running, `cas_ref` is `null`. After finalization, `cas_ref` is the final `blob://sha256/<hex>` ref.
- The endpoint is byte-offset based and returns raw bytes as base64. The server does not snap reads to UTF-8 boundaries; clients must use a streaming `TextDecoder` and preserve incomplete codepoints between polls.
- State resolution order:
  - If scratch exists and no finalized CAS ref is present, serve scratch with `eof: false` and `cas_ref: null`.
  - If finalized CAS ref is present, serve scratch if available, otherwise hydrate CAS and serve text bytes with `eof: true`.
  - If neither scratch nor CAS exists but the stage exists, return `200` empty with `eof` derived from stage terminal status.
  - If the run or stage does not exist, return `404`.
- During scratch-to-CAS transition, the endpoint must not 404. It should seamlessly continue serving from scratch or CAS according to the resolution order above.
- DoS posture: v1 relies on the trusted/single-tenant deployment model plus the 1 MiB per-request limit. No additional rate limiter is included in this pass.
- Regenerate Rust and TypeScript API clients after editing `docs/public/api-reference/fabro-api.yaml`.
- Update the web run-stage command panel:
  - show separate stdout and stderr panels, not interleaved output;
  - auto-expand stderr when non-empty or the command fails;
  - poll every 1 second while the command is running and always do one final poll after `command.completed`;
  - cap browser memory per stream to the last 5 MiB and show a truncation indicator;
  - preserve follow-tail unless the user scrolls away from the bottom;
  - show waiting, running, completed-empty, failed-fetch, timeout, cancel, and CAS-fallback states;
  - provide copy for visible log text;
  - do not mark the continuously streaming log body as assertive `aria-live`; use stable status text for state changes so screen readers are not flooded;
  - leave ANSI color rendering, download, and deep-link-to-byte-offset out of v1.

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
  - `lib/crates/fabro-workflow/src/handler/llm/preamble.rs`
  - conditional/router and retro paths that read command context values
- API/web/docs:
  - `docs/public/api-reference/fabro-api.yaml`
  - `lib/crates/fabro-server/src/server.rs`
  - `apps/fabro-web/app/routes/run-stages.tsx`
  - `apps/fabro-web/app/lib/query-keys.ts`
  - `apps/fabro-web/app/lib/queries.ts`
  - docs that describe `command.output` and `command.stderr`

## Test Plan

- Scratch recorder unit tests: interleaved stdout/stderr chunks, flushed byte counts, empty streams, bounded failure tails, and artifact-style stage path encoding.
- Command handler tests: success, nonzero exit, stderr capture, long-running chunk readable before completion, timeout finalizes partial CAS refs, and empty streams produce refs with zero byte counts.
- Context compatibility tests: preamble, condition/router, retros, dump, CLI display, and web/server consumers handle both legacy inline text and new blob refs via `parse_blob_ref`.
- Blob compatibility tests: command output CAS blobs contain JSON strings and work with existing blob hydration/dump paths.
- Projection/event tests: `command.completed` refs and byte metadata land in `NodeState`, old inline events still deserialize and project.
- Sandbox tests: local streaming chunks before process exit; Docker streaming from Bollard stdout/stderr frames; default fallback emits buffered output after completion; Daytona live/ignored test covers separated JSON logs and combined-output fallback when credentials are available.
- Server API tests: typed path parsing, unauthenticated rejection, offset reads, limit enforcement, `cas_ref: null` while running, completed-run CAS fallback, scratch-to-CAS transition, missing run/stage/stream errors, and orphan scratch behavior.
- Web tests: query key construction, streaming `TextDecoder` behavior across chunk boundaries, final poll after `command.completed`, memory cap/truncation, stdout/stderr panel states, and copy-visible-log behavior.
- Verification commands:
  - `cargo nextest run -p fabro-sandbox -p fabro-workflow -p fabro-store -p fabro-server`
  - `cargo build -p fabro-api`
  - `cd lib/packages/fabro-api-client && bun run generate`
  - `cd apps/fabro-web && bun test && bun run typecheck`

## Assumptions

- `StageId` uses the existing wire form `<node>@<attempt>`; implementation may still have internal names such as `visit`, but the plan treats that suffix as attempt count.
- Server and worker share the same server storage filesystem. Live tailing depends on this single-node deployment model.
- Command stdout/stderr are treated as text logs for durable context. The live endpoint is byte-oriented over the stored UTF-8/log bytes; arbitrary binary command output fidelity is out of scope.
- CAS refs use the existing `blob://sha256/<hex>` format.
- `command.output` and `command.stderr` are refs after finalization; no `_blob` context keys are added.
- No stdout/stderr chunk events are added in this pass.
- No automatic redaction, crash recovery, dedicated scratch GC, or additional request rate limiting is included in v1.
