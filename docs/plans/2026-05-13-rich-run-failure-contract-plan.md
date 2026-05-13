---
title: "refactor: Rich run failure contract"
type: refactor
status: completed
date: 2026-05-13
origin: https://github.com/fabro-sh/fabro/issues/198
---

# refactor: Rich run failure contract

## Summary

Refactor terminal run failures around a first-class `RunFailure` value object. Preserve rich internal errors and source chains until terminal event projection, then serialize a structured failure payload with message, causes, classification, optional signature, actor, and redacted exec output tail.

Assumption: this is greenfield. Do not preserve the old flat `run.failed.properties.error` / `causes` / `reason` contract or old `Conclusion.failure_reason` shape.

## Key Changes

- Add `fabro_types::RunFailure` with this wire shape:
  - `message: String`
  - `causes: Vec<String>`, omitted when empty
  - `reason: FailureReason`
  - `category: FailureCategory`
  - `system_actor: Option<SystemActorKind>`
  - `signature: Option<FailureSignature>`
  - `exec_output_tail: Option<ExecOutputTail>`
- Change `RunFailedProps` to contain:
  - `failure: RunFailure`
  - `duration_ms`
  - `final_git_commit_sha: Option<String>`, replacing failed-run `git_commit_sha`
  - `final_patch`, `diff_summary`, and optional `billing`
- Change `Conclusion` from `failure_reason: Option<String>` to `failure: Option<RunFailure>`. Keep `StageCompletion.failure_reason` unchanged; stage-level failure text is separate from terminal run failure diagnostics.
- Update OpenAPI `Conclusion` schemas and regenerate `lib/packages/fabro-api-client` models. `RunEvent.properties` is still generic, but docs/examples should show nested `failure`.

## Implementation

- In `fabro-workflow`, remove `fabro_workflow::Error` from `Event::WorkflowRunFailed`; the event carries `failure: RunFailure`.
- Add workflow-local projection helpers:
  - `run_failure_from_error(error, reason)` for rich `Error` values.
  - `run_failure_from_outcome_failure(failure_detail, reason)` for failed `Outcome` values without an error.
- Preserve source chains in `fabro_workflow::Error` before projection:
  - Replace rendered `causes: Vec<String>` fields on `Engine` / `Handler` with `source: Option<SharedError>`.
  - Make `engine_with_source` / `handler_with_source` take owned `impl Into<anyhow::Error>`, not borrowed `&dyn Error`.
  - Remove `Serialize` / `Deserialize` from `fabro_workflow::Error`; it is internal error transport, not durable wire data.
  - Keep `Error::causes()` and `display_with_causes()` as boundary helpers backed by `source()`.
- Populate `RunFailure` as follows:
  - `message`: concise public message; for `Engine` / `Handler`, use the stored message without the `"Engine error:"` / `"Handler error:"` prefix.
  - `causes`: collected source chain strings.
  - `category`: `error.failure_category()` or `FailureDetail.category`.
  - `signature`: existing `FailureDetail.signature` or `error.failure_signature_hint()`, wrapped as `FailureSignature`.
  - `system_actor`: from `FailureDetail.system_actor`; `None` for generic errors.
  - `exec_output_tail`: `fabro_sandbox::default_redacted_output_tail(error)`.
- Update consumers to read the new nested shape:
  - run projection status uses `props.failure.reason`
  - conclusion stores `Some(props.failure.clone())`
  - server managed-run summaries use `props.failure.message`
  - CLI progress/output renders `failure.message` plus `failure.causes` where full diagnostics are appropriate
  - event tracing logs failure metadata and tail byte/truncation metadata only, never tail contents

## Test Plan

- `fabro-types` serialization tests:
  - `run.failed` serializes with nested `properties.failure`.
  - no top-level `error`, `causes`, `reason`, or `git_commit_sha` remains.
  - `exec_output_tail` and empty `causes` are omitted when absent.
  - `Conclusion` serializes `failure: RunFailure`.
- `fabro-workflow` projection tests:
  - owned source chains survive through `Error::engine_with_source` / `handler_with_source`.
  - a nested sandbox exec error produces `failure.exec_output_tail`.
  - raw stdout/stderr never appear in `failure.message`.
  - secrets in exec output are redacted in the serialized event payload.
  - failed `Outcome` maps its `FailureDetail` category, actor, and signature into `RunFailure`.
- Projection/consumer tests:
  - run state projects failed status from `props.failure.reason`.
  - conclusion renders full message plus causes for CLI output.
  - server managed-run failure summaries continue showing a concise message.
  - existing `run.failed` fixtures and inline snapshots are updated intentionally.
- Verification commands:
  - `cargo nextest run -p fabro-types -p fabro-workflow -p fabro-store -p fabro-server -p fabro-cli`
  - `cd apps/fabro-web && bun run typecheck`
  - regenerate API client after OpenAPI edits, then typecheck affected TypeScript.
