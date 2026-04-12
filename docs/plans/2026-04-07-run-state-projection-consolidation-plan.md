# Run State Projection Consolidation Plan

## Summary
Consolidate the duplicated CLI and CLI-test run-state projection structs into the canonical read-model types owned by `fabro-store`.

This pass keeps `RunProjection` and `NodeState` in `fabro-store`, does not move them into `fabro-types`, and does not touch workflow events. The goal is to make the store projection the single source of truth for the `/api/v1/runs/{id}/state` response and for CLI-side consumption of that response.

## Key Decisions
- `fabro_store::RunProjection` and `fabro_store::NodeState` become the only production definitions.
- The server response shape remains strict.
  - Do not add backward-compat aliases, migration shims, or permissive serde fallbacks.
  - If the server and CLI drift, the build or tests should fail.
- `RunProjection` remains a store-owned read model in this pass.
  - Do not move it into `fabro-types`.
- The existing store projection helper surface is the shared API:
  - `node()`
  - `iter_nodes()`
  - `is_empty()`
  - `list_node_visits()`

## Implementation Changes
### 1. Make the store projection reusable as the client model
- Update [`lib/crates/fabro-store/src/run_state.rs`](/Users/bhelmkamp/p/fabro-sh/fabro-2/lib/crates/fabro-store/src/run_state.rs) so `RunProjection` and `NodeState` derive `serde::Deserialize` in addition to their current derives.
- Keep the field layout unchanged unless deserialization requires a minimal adjustment for the existing JSON shape.
- Keep the current helper methods on `RunProjection` as the public access surface for downstream crates.

### 2. Remove the duplicated CLI runtime projection
- Delete the local `RunProjection` and `NodeState` definitions from [`lib/crates/fabro-cli/src/server_client.rs`](/Users/bhelmkamp/p/fabro-sh/fabro-2/lib/crates/fabro-cli/src/server_client.rs).
- Import `fabro_store::RunProjection` instead.
- Keep `get_run_state()` structurally simple:
  - fetch `/api/v1/runs/{id}/state`
  - deserialize the response into the shared store projection type
- Do not recreate projection helper methods in the CLI.

### 3. Remove the duplicated CLI test projection
- Delete the mirrored `RunProjection` and `NodeState` definitions from [`lib/crates/fabro-cli/tests/it/cmd/support.rs`](/Users/bhelmkamp/p/fabro-sh/fabro-2/lib/crates/fabro-cli/tests/it/cmd/support.rs).
- Reuse `fabro_store::RunProjection` in test helpers that fetch `/api/v1/runs/{id}/state`.
- Rewrite any test call sites that depend on direct `nodes` map access to use the shared projection API instead:
  - `iter_nodes()`
  - `node()`
  - `list_node_visits()`

### 4. Keep the server boundary unchanged
- Leave [`lib/crates/fabro-server/src/server.rs`](/Users/bhelmkamp/p/fabro-sh/fabro-2/lib/crates/fabro-server/src/server.rs) behavior unchanged for `GET /api/v1/runs/{id}/state`.
- The endpoint already returns the store projection directly; this pass only removes downstream duplication.

## Public Interface Changes
- No HTTP API shape change is intended.
- No `fabro-types` change is intended.
- The effective shared contract becomes explicit:
  - `/api/v1/runs/{id}/state` is represented by `fabro_store::RunProjection`
  - the CLI no longer maintains a private mirror type for that payload

## Test Plan
- Add a `fabro-store` serde test that deserializes a representative run-state JSON payload into `RunProjection`, including stage-id string keys such as `build@2`.
- Add a `fabro-store` round-trip test that serializes and deserializes `RunProjection` and verifies:
  - `node()` returns the expected node state
  - `list_node_visits()` returns the expected visit list
- Run `cargo nextest run -p fabro-store`.
- Run `cargo nextest run -p fabro-cli`.
- Confirm existing CLI integration coverage that hits `/api/v1/runs/{id}/state` still passes with the shared projection type.

## Assumptions
- This plan covers only the projection-consolidation pass we discussed, not event cleanup or broader run-state refactors.
- Greenfield strictness is preferred over compatibility padding.
- `fabro-store` is the correct ownership boundary for this read model in the current architecture.
