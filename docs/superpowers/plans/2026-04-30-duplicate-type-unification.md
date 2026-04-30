# Duplicate Type Unification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Collapse duplicate and near-duplicate API/domain types while preserving distinct product concepts with precise names.

**Architecture:** Reuse canonical domain types from `fabro-types` and `fabro-model` through `fabro-api` `with_replacement(...)` whenever the wire contract is the same concept. Split the overloaded stage status vocabulary into terminal execution outcomes and lifecycle projection states, then expose those exact concepts in OpenAPI and the generated TypeScript client.

**Tech Stack:** Rust, serde, strum where variants are fieldless, progenitor OpenAPI codegen, OpenAPI Generator TypeScript Axios client, React/Vite frontend, cargo nextest.

---

## Summary

This is a greenfield breaking refactor and must land as a single merge unit. Do not split stage core changes from API/server/frontend updates, because intermediate states would leave emitted outcome values, server projections, generated clients, and frontend renderers out of sync.

Tasks 1-8 must be implemented in numeric order. Earlier tasks define types and contracts that later tasks consume; out-of-order execution will not compile.

Target unifications:

- Stage vocabulary: replace overloaded `StageStatus` with `StageOutcome` and `StageState`.
- Billing: make API `BilledTokenCounts` reuse `fabro_types::BilledTokenCounts`.
- Model catalog: make API model schemas reuse `fabro_model::{Provider, Model, ModelLimits, ModelFeatures, ModelCosts}`.
- Model testing: move `ModelTestMode` to `fabro_model` and reuse it from LLM, API, server, client, and CLI.
- Settings: keep `ServerSettings` reused as-is, and replace loose `WorkflowSettings` OpenAPI schema with exact dense settings schemas.

## Task 1: Introduce Precise Stage Types

**Files:**
- Modify: `lib/crates/fabro-types/src/outcome.rs`
- Modify: `lib/crates/fabro-types/src/lib.rs`
- Modify: `lib/crates/fabro-types/src/node_status.rs`
- Modify: `lib/crates/fabro-types/src/conclusion.rs`
- Modify: `lib/crates/fabro-types/src/run_event/stage.rs`

- [ ] Replace `StageStatus` with `StageOutcome` for terminal execution results.
- [ ] Add `StageState` for run-stage lifecycle projections.
- [ ] Keep existing field names that already mean "status"; do not rename them to `outcome`:
  - `Outcome.status: StageOutcome`
  - `NodeStatusRecord.status: StageOutcome`
  - `Conclusion.status: StageOutcome`
  - `StageCompletedProps.status: StageOutcome`
- [ ] Keep generated `RunStage.status`, but change its type to `StageState` in OpenAPI during Task 5.
- [ ] Implement `StageOutcome` as:

```rust
pub enum StageOutcome {
    Succeeded,
    PartiallySucceeded,
    Failed { retry_requested: bool },
    Skipped,
}
```

- [ ] Make retry intent unrepresentable on non-failed outcomes; do not add a sidecar `retry_requested` field to `Outcome<M>`.
- [ ] Implement `StageState` variants:
  - `Pending`
  - `Running`
  - `Retrying`
  - `Succeeded`
  - `PartiallySucceeded`
  - `Failed`
  - `Skipped`
  - `Cancelled`
- [ ] For `StageState`, derive `Serialize`, `Deserialize`, `strum::Display`, `strum::EnumString`, and `strum::IntoStaticStr` with `snake_case` serde/strum naming.
- [ ] For `StageOutcome`, hand-write `Serialize`, `Deserialize`, `Display`, and `FromStr`; do not derive strum or serde `rename_all` for this enum. The field-bearing `Failed { retry_requested: bool }` variant intentionally uses a flat, lossy external string form:
  - `succeeded`
  - `partially_succeeded`
  - `failed`
  - `skipped`
- [ ] Serialize `StageOutcome::Failed { retry_requested: true }` and `StageOutcome::Failed { retry_requested: false }` as `failed`.
- [ ] Deserialize external `failed` as `StageOutcome::Failed { retry_requested: false }`. Retry intent is internal control flow and is separately emitted through `StageFailedProps.will_retry`.
- [ ] Add helper methods on `StageOutcome`:
  - `is_successful(self) -> bool`
  - `is_failure(self) -> bool`
  - `retry_requested(self) -> bool`
- [ ] Implement `From<StageOutcome> for StageState` for terminal outcome projection.
- [ ] Add helper method on `StageState`: `is_terminal(self) -> bool`.
- [ ] Update `Outcome::default()`, `Outcome::success()`, `Outcome::fail(...)`, and `Outcome::skipped(...)`.

## Task 2: Preserve Retry Behavior Without Retry As An Outcome

**Files:**
- Modify: `lib/crates/fabro-core/src/outcome.rs`
- Modify: `lib/crates/fabro-core/src/executor.rs`
- Modify: `lib/crates/fabro-core/src/error.rs`
- Modify: `lib/crates/fabro-workflow/src/outcome.rs`
- Modify: `lib/crates/fabro-workflow/src/handler/human.rs`
- Test: `lib/crates/fabro-core/src/executor.rs`

- [ ] Replace executor checks for `StageStatus::Retry` with `outcome.status.retry_requested() && can_retry`. The helper covers exactly the `StageOutcome::Failed { retry_requested: true }` case.
- [ ] Keep retryable handler errors working through `Error::is_retryable()` and `NodeResult::from_error(...)`.
- [ ] Change `OutcomeExt::retry_classify(...)` to return `Outcome { status: StageOutcome::Failed { retry_requested: true }, failure: Some(...), .. }`.
- [ ] Update retry exhaustion tests to construct `StageOutcome::Failed { retry_requested: true }` instead of retry status outcomes.
- [ ] Delete the planned guard test for "retry requested on a non-failed outcome"; that state must be impossible to construct.
- [ ] Update `on_retries_exhausted(...)` tests so exhausted retryable failures become final `StageOutcome::Failed { retry_requested: false }`.

## Task 3: Update Workflow Semantics And Events

**Files:**
- Modify: `lib/crates/fabro-workflow/src/condition.rs`
- Modify: `lib/crates/fabro-workflow/src/event.rs`
- Modify: `lib/crates/fabro-workflow/src/graph/routing.rs`
- Modify: `lib/crates/fabro-workflow/src/pipeline/finalize.rs`
- Modify: `lib/crates/fabro-workflow/src/operations/resume.rs`
- Modify: `lib/crates/fabro-workflow/src/handler/agent.rs`
- Modify: `lib/crates/fabro-workflow/src/lifecycle/event.rs`
- Modify other `fabro-workflow` files found by `rg "\bStageStatus\b|StageOutcome|\.status\b" lib/crates/fabro-workflow/src`

- [ ] Update routing and finalization to branch on `Outcome.status: StageOutcome`.
- [ ] Keep event names unchanged: `stage.completed`, `stage.failed`, and `stage.retrying`.
- [ ] Keep event property name `status` where it stores the terminal outcome; change only the values and the Rust type.
- [ ] Update condition DSL comparisons to use new canonical strings:
  - `outcome=succeeded`
  - `outcome=partially_succeeded`
  - `outcome=failed`
  - `outcome=skipped`
- [ ] Update tests, docs, examples, `.fabro/workflows/*`, and fixtures found by:

```bash
rg 'outcome=(success|fail|partial_success)|outcome!=(success|fail|partial_success)|"outcome": "(success|fail|partial_success|retry)"' .fabro docs apps lib
```

- [ ] Update agent JSON extraction so `{ "outcome": "failed" }` parses as `StageOutcome::Failed { retry_requested: false }`.
- [ ] Remove fallback parsing paths for old outcome strings.
- [ ] Confirm `fabro-validate` rules and examples use the new strings, including any currently mixed values such as `outcome!=failure`.

## Task 4: Unify Billing And Model Domain Types

**Files:**
- Modify: `docs/public/api-reference/fabro-api.yaml`
- Modify: `lib/crates/fabro-api/build.rs`
- Modify: `lib/crates/fabro-api/Cargo.toml`
- Modify: `lib/crates/fabro-model/src/types.rs`
- Modify: `lib/crates/fabro-model/src/model_test.rs` if created
- Modify: `lib/crates/fabro-llm/src/model_test.rs`
- Modify: `lib/crates/fabro-server/src/server.rs`
- Modify: `lib/crates/fabro-client/src/client.rs`
- Modify: `lib/crates/fabro-cli/src/commands/model.rs`

- [ ] Add `fabro-model` as a direct dependency of `fabro-api` if needed for `with_replacement(...)`.
- [ ] Update OpenAPI `BilledTokenCounts` so all token count fields are required integers:
  - `input_tokens`
  - `output_tokens`
  - `total_tokens`
  - `reasoning_tokens`
  - `cache_read_tokens`
  - `cache_write_tokens`
  - `total_usd_micros` remains nullable/optional as represented by the domain type.
- [ ] Add `with_replacement("BilledTokenCounts", "fabro_types::BilledTokenCounts", ...)`.
- [ ] Delete server billing zero-omit adapters:
  - `nonzero_i64`
  - `api_billed_token_counts_from_domain`
  - `api_billed_token_counts_from_usage`
- [ ] Return domain `BilledTokenCounts` directly from aggregate and run billing code.
- [ ] Update OpenAPI model schemas to exactly match `fabro_model`:
  - `Provider` is an enum, not free string.
  - `ModelFeatures` includes required/defaulted `effort`.
  - `Model` includes `knowledge_cutoff`.
  - Nullability and required fields match domain serde behavior.
- [ ] Add replacements for `Provider`, `Model`, `ModelLimits`, `ModelFeatures`, and `ModelCosts`.
- [ ] Move `ModelTestMode` to `fabro_model`, derive serde/strum consistently, and reuse it from `fabro-llm`.
- [ ] Add replacement for `ModelTestMode` in `fabro-api/build.rs`.
- [ ] Update server query parsing for model tests to deserialize or parse through the shared `ModelTestMode` type.

## Task 5: Tighten OpenAPI Stage, Model, Billing, And Settings Schemas

**Files:**
- Modify: `docs/public/api-reference/fabro-api.yaml`
- Modify: `lib/crates/fabro-api/build.rs`
- Modify: `lib/crates/fabro-api/src/lib.rs`
- Delete: `lib/crates/fabro-api/tests/internal_stage_status_round_trip.rs`
- Add: `lib/crates/fabro-api/tests/stage_outcome_round_trip.rs`
- Add: `lib/crates/fabro-api/tests/stage_state_round_trip.rs`
- Add: `lib/crates/fabro-api/tests/billed_token_counts_round_trip.rs`
- Add: `lib/crates/fabro-api/tests/provider_round_trip.rs`
- Add: `lib/crates/fabro-api/tests/model_limits_round_trip.rs`
- Add: `lib/crates/fabro-api/tests/model_features_round_trip.rs`
- Add: `lib/crates/fabro-api/tests/model_costs_round_trip.rs`
- Add: `lib/crates/fabro-api/tests/model_round_trip.rs`
- Add: `lib/crates/fabro-api/tests/model_test_mode_round_trip.rs`
- Modify: `lib/crates/fabro-api/tests/workflow_settings_round_trip.rs`
- Modify: `lib/crates/fabro-api/tests/server_settings_round_trip.rs`

- [ ] Remove `InternalStageStatus` from OpenAPI and `fabro-api/build.rs`.
- [ ] Add `StageOutcome` schema as a flat string enum matching `fabro_types::StageOutcome` serde. Do not model it as `oneOf` or a tagged object:

```yaml
StageOutcome:
  type: string
  enum:
    - succeeded
    - partially_succeeded
    - failed
    - skipped
```

- [ ] Replace API `StageStatus` schema with `StageState`.
- [ ] Keep `RunStage.status`, but make it reference `StageState`.
- [ ] Add replacements for `StageOutcome` and `StageState` in `fabro-api/build.rs`.
- [ ] Keep `ServerSettings` replacements and existing type identity tests.
- [ ] Replace `WorkflowSettings: additionalProperties: true` with exact schemas for:
  - `WorkflowSettings`
  - `ProjectNamespace`
  - `WorkflowNamespace`
  - `RunNamespace`
  - Any nested run settings needed to represent the current dense serde shape.
- [ ] For every new `with_replacement(...)`, add one `fabro-api` test file proving both type identity and JSON parity with the OpenAPI shape. Do not combine multiple replacements into a vague umbrella test.
- [ ] Model each new test after the current `internal_stage_status_round_trip.rs` / `server_settings_round_trip.rs` pattern: `assert_same_type::<ApiType, DomainType>()`, serialize a representative domain value, assert key JSON fields, then deserialize through the API alias and compare.
- [ ] In `stage_outcome_round_trip.rs`, encode the intentionally lossy `Failed` contract instead of asserting strict round-trip equality for that variant:
  - `StageOutcome::Failed { retry_requested: true }` serializes to `"failed"`.
  - `"failed"` deserializes to `StageOutcome::Failed { retry_requested: false }`.
  - `serde_json::from_str::<StageOutcome>(&serde_json::to_string(&StageOutcome::Failed { retry_requested: true })?)?` equals `StageOutcome::Failed { retry_requested: false }`.
  - `StageOutcome::Failed { retry_requested: false }` may round-trip strictly.

## Task 6: Update Server Projections And Demo Data

**Files:**
- Modify: `lib/crates/fabro-server/src/server.rs`
- Modify: `lib/crates/fabro-server/src/demo/mod.rs`
- Modify server tests in `lib/crates/fabro-server/src/server.rs`

- [ ] Update imports from generated `StageStatus as ApiStageStatus` to shared `StageState`.
- [ ] Delete the manual `fabro_types::StageStatus -> ApiStageStatus` match currently mapping `Skipped` to `Cancelled`.
- [ ] Project completed nodes with `StageState::from(outcome.status)`.
- [ ] Map skipped outcomes to `StageState::Skipped`, not cancelled.
- [ ] Use `StageState::Running` for active next node by default.
- [ ] If the run has a latest `stage.retrying` event for the active next node, use `StageState::Retrying`.
- [ ] Use `StageState::Cancelled` only for user/system cancellation, not workflow skip.
- [ ] Update demo stage rows to use `status: StageState::*`.
- [ ] Update server API assertions and snapshots for the new enum strings while preserving the `status` field name.
- [ ] Add a concrete server test for a retrying-then-completed sequence: while the latest relevant event is `stage.retrying`, the active node projects as `StageState::Retrying`; after completion, it projects to the terminal `StageState` derived from the completed `StageOutcome`.

## Task 7: Regenerate TypeScript Client And Update Frontend

**Files:**
- Regenerate: `lib/packages/fabro-api-client/src/**`
- Modify: `apps/fabro-web/app/components/stage-sidebar.tsx`
- Modify: `apps/fabro-web/app/lib/stage-sidebar.ts`
- Modify affected routes under `apps/fabro-web/app/routes/`

- [ ] Run `cargo build -p fabro-api` after OpenAPI and replacements compile.
- [ ] Run `cd lib/packages/fabro-api-client && bun run generate`.
- [ ] Update frontend imports to use generated `StageState` and `RunStage.status`.
- [ ] Remove local handwritten `StageStatus` union from `stage-sidebar.tsx`.
- [ ] Add UI config entries for `retrying`, `partially_succeeded`, `skipped`, and `cancelled`.
- [ ] Update billing UI assumptions so zero token counts are numbers, not optional omitted fields.
- [ ] Update model UI assumptions for `provider`, `features.effort`, and `knowledge_cutoff`.
- [ ] Update workflow settings consumers to use generated exact settings types instead of `Record<string, unknown>`.

## Task 8: Verification And Cleanup

**Files:**
- Modify docs, fixtures, and snapshots found by final searches.

- [ ] Run `rg "\bStageStatus\b|InternalStageStatus|ApiStageStatus" lib/crates apps docs/public/api-reference/fabro-api.yaml` and remove stale stage status references.
- [ ] Run `rg "outcome=(success|fail|partial_success)|outcome!=(success|fail|partial_success)|\"outcome\": \"(success|fail|partial_success|retry)\"" .fabro docs apps lib` and update old outcome strings.
- [ ] Run `rg "api_billed_token_counts_from_domain|api_billed_token_counts_from_usage|nonzero_i64" lib/crates/fabro-server/src/server.rs` and confirm there are no hits.
- [ ] Run `rg "StageStatus::|ApiStageStatus|InternalStageStatus" lib/crates/fabro-server/src/server.rs` and confirm there are no hits.
- [ ] Run `cargo build -p fabro-api`.
- [ ] Run `cargo nextest run -p fabro-types -p fabro-model -p fabro-core -p fabro-workflow -p fabro-server -p fabro-api`.
- [ ] Run `cd apps/fabro-web && bun test`.
- [ ] Run `cd apps/fabro-web && bun run typecheck`.
- [ ] Run `cargo +nightly-2026-04-14 fmt --check --all`.
- [ ] Run `cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings`.
- [ ] If snapshots change, run `cargo insta pending-snapshots`, inspect expected changes, then accept only verified snapshots.

## Acceptance Criteria

- This lands as one PR / one merge unit; no intermediate commit is merged while API/server/frontend contracts disagree.
- Tasks 1-8 are executed in numeric order.
- `fabro-api` generated Rust types reuse canonical domain types for billing, model catalog, model test mode, stage outcome/state, server settings, and workflow settings.
- Every new `with_replacement(...)` has a dedicated `fabro-api` type identity and JSON parity test.
- No generated Rust API type remains for the unified concepts unless it represents a deliberate API-only projection.
- `RunStage.status` uses `fabro_types::StageState`.
- Terminal engine records keep the `status` field name but use `fabro_types::StageOutcome`.
- Both `StageOutcome` and `StageState` are stored on fields named `status`; the disambiguation is the field's type, not its name.
- Retry behavior is preserved, but `retry` is no longer a terminal outcome value.
- The manual server stage adapter match from domain `StageStatus` to `ApiStageStatus` is gone.
- The skipped/cancelled conflation is gone: skipped workflow outcomes project to `StageState::Skipped`.
- Billing zero-omit adapter functions are gone, and token count zeros serialize as numeric zeros.
- TypeScript client exposes exact model, billing, settings, stage outcome, and stage state shapes.
- All listed verification commands pass.

## Assumptions

- Backward compatibility is intentionally not preserved.
- No external customer-owned `.fabro/workflows/*/workflow.toml` files depend on old condition DSL strings. Repo workflows, docs, examples, and fixtures are updated in this PR.
- Persisted local run data may need to be deleted or recreated during development after this change.
- `retry_requested` on `StageOutcome::Failed` is transient executor state. Persisted records, including events on disk, conclusions, and node status, always deserialize with `retry_requested: false`. Callers that need to know whether a failed stage will be retried must consult `StageFailedProps.will_retry`, not the outcome enum.
- Event names stay stable; event property values may change.
- The `status` field name is retained for terminal outcome records to avoid gratuitous `outcome.status` -> `outcome.outcome` churn.
- The implementation should not introduce adapters whose only purpose is bridging accidental duplicate types.
