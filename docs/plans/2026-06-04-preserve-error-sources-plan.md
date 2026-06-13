# Preserve Error Sources Across Failure Paths

## Summary

Fix the current error-handling policy violations by keeping structured errors intact until diagnostic/render boundaries. The implementation should preserve source chains through `fabro-core`, workflow lifecycle hooks, workflow operations, server-managed run failures, and error events that currently expose only `error: String`.

## Key Changes

- Add a source-aware core error variant in `fabro-core`, for example `Error::OtherWithSource { message, source: SharedError }`, plus `Error::other_with_source(message, source)`. Use `fabro_util::error::SharedError` and `collect_chain`; add `anyhow.workspace` to `fabro-core` if needed for the constructor.
- Update `fabro-core::Error::to_fail_outcome()` so source-aware errors produce `FailureDetail.causes`, while preserving existing retry/category behavior for `Other`.
- Update `fabro-workflow/src/pipeline/execute.rs` to convert source-aware core errors into `WorkflowError::engine_with_source(...)` instead of the catch-all `Error::engine(e.to_string())`.

## Implementation Changes

- Replace lifecycle string flattening with source-aware core errors:
  - `lifecycle/git.rs`: emit `checkpoint.failed` with `error = e.to_string()` and `causes = collect_causes(&e)`, then return `CoreError::other_with_source("git checkpoint commit failed for node ...", e)`.
  - `lifecycle/fidelity.rs`: wrap artifact context/outcome resolution errors with `CoreError::other_with_source(...)`.
  - `lifecycle/artifact.rs`: wrap artifact ledger and tempdir failures with `CoreError::other_with_source(...)`.
- Sweep workflow operation/store conversions where internal errors currently use `Error::engine(err.to_string())`. Use `Error::engine_with_source("<specific operation context>", err)` for store, serde, IO, and task/join failures in operations such as start, retry, resume, rewind, archive, fork, timeline, create, `pipeline/persist.rs`, `pipeline/initialize.rs`, and `handler/manager_loop.rs`. Keep intentional user-facing parse/validation string projections unchanged.
- Add optional `causes: Vec<String>` with `#[serde(default, skip_serializing_if = "Vec::is_empty")]` to these diagnostic event props and internal event variants:
  - `checkpoint.failed`
  - `pull_request.failed`
  - `agent.failover`
  - `agent.mcp.failed`
- Update event conversion and emitters:
  - Pull request flow should return `anyhow::Result` or a local typed error instead of `Result<_, String>`, use `.context(...)`, and emit `causes` from the error chain.
  - Failover events should collect causes from the `fabro_llm::Error` before moving it.
  - MCP failures should preserve causes from structured MCP startup/handshake errors; update `fabro-mcp` error wrapping to use context instead of interpolating the source error into `anyhow!`.
- Update server managed-run failure rendering:
  - Add helpers that accept `WorkflowError` or `(message, causes)` rather than only `String`.
  - Store compact cause-aware text in `managed_run.error` using existing render helpers.
  - Reuse the same source-aware `WorkflowError` for persisted `run.failed` and in-memory managed-run state.

## Public Interfaces

- `fabro-types` run event props gain optional `causes` fields for the four diagnostic events above. This is backward-compatible because old events deserialize with empty causes and empty causes are omitted from serialization.
- `docs/internal/events.md` examples/tables should document `causes`.
- No TypeScript client regeneration is required for per-event properties because OpenAPI models `RunEvent.properties` as arbitrary JSON, but Rust API round-trip tests should be updated.

## Test Plan

- `fabro-core/src/error.rs`: assert `OtherWithSource` preserves `source()` and `to_fail_outcome().failure.causes`.
- `fabro-workflow/src/pipeline/execute.rs` tests: a lifecycle/core error with a nested source becomes a workflow error whose `FailureDetail.causes` contains the leaf cause.
- `fabro-workflow/src/event/convert.rs`: checkpoint, PR failure, failover, and MCP failure events map `causes` into `fabro-types` props.
- `fabro-workflow/src/pipeline/pull_request.rs`: PR content/client/deserialization failures retain source-chain causes.
- `fabro-agent`/`fabro-mcp` tests: MCP server failure events serialize causes when the underlying startup/handshake error has a chain.
- `fabro-server/src/server/tests.rs`: managed-run failure summaries render cause-aware text, and persisted `run.failed` still carries structured causes.
- Run `cargo nextest run -p fabro-core`, `cargo nextest run -p fabro-workflow`, `cargo nextest run -p fabro-agent`, `cargo nextest run -p fabro-mcp`, and targeted `fabro-server` tests. Run `cargo +nightly-2026-04-14 fmt --check --all` after edits.

## Assumptions

- API error responses such as `ApiError::new(..., err.to_string())` are not part of this sweep unless they create run diagnostics; the policy explicitly allows curated HTTP JSON messages.
- Projection-only summary strings may remain strings, but they must be rendered from preserved causes at the boundary.
- Empty `causes` means "no structured source available," not "error had no cause."
