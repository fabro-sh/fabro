# Shared Test Storage + Production Auto-Start for CLI Tests

## Summary
Use one shared test `storage_dir` per test session and rely on the existing production auto-start behavior to converge on a single shared daemon for that storage root.

Session identity rules:
- when `NEXTEST_RUN_ID` is present, use one shared storage dir for the full `cargo nextest run`
- when `NEXTEST_RUN_ID` is absent, use one shared storage dir per test process

The test harness will not implement its own daemon bootstrap protocol. Instead:
- every test process points `FABRO_STORAGE_DIR` at the same test-run storage root
- the first CLI command that needs the daemon triggers normal production auto-start
- existing `server.lock`, `server.json`, and `fabro.sock` behavior prevents duplicate servers for that shared storage dir
- the test harness is responsible only for:
  - selecting the shared test storage root
  - making test assertions/helpers safe under shared storage
  - cleaning up the shared daemon and temp root at the end of the test run

This keeps test behavior aligned with production daemon identity semantics.

## Implementation Changes
### 1. Shared `storage_dir` in `fabro-test`
In `lib/crates/fabro-test/src/lib.rs`:
- Change `TestContext::new` to detect `NEXTEST_RUN_ID`.
- Derive a shared test root under temp:
  - if `NEXTEST_RUN_ID` is present: `$TMPDIR/fabro-nextest/<NEXTEST_RUN_ID>/`
  - otherwise: `$TMPDIR/fabro-test-process/<pid>/`
  - shared storage dir: `<root>/storage`
- Keep `temp_dir` and `home_dir` per context; only `storage_dir` becomes shared for the session.
- Do not explicitly start the daemon from the harness.
- Keep using the normal CLI command path so the first server-backed command triggers production auto-start against the shared storage dir.
- Add concrete session cleanup coordination using marker files:
  - under the session root, create `clients/<pid>` marker files
  - protect create/remove/scan operations with a session lock file
  - on `TestContext` init:
    - acquire lock
    - create or refresh this process marker
    - remove stale markers for dead PIDs
    - release lock
  - on process teardown:
    - acquire lock
    - remove this process marker
    - remove any other stale dead markers
    - if no markers remain, call `fabro server stop --storage-dir <shared>` and remove the shared temp root
    - release lock
- Crash behavior:
  - crashed processes may leave stale markers behind
  - future init/teardown paths reap dead markers under the same lock
  - teardown cleanup is best-effort; if `fabro server stop` or root removal fails, later harness initialization remains responsible for authoritative stale-session reaping
- Add stale-run cleanup on harness initialization:
  - scan old `fabro-nextest/*` roots
  - if all tracked PIDs for a root are dead, stop any server tied to that root and remove it
  - likewise scan old `fabro-test-process/*` roots and reap dead process-owned sessions

The harness should only coordinate test-run ownership and cleanup, not daemon startup.

### 2. Per-test labeling for shared-state safety
Add a per-`TestContext` test case ULID and expose:
- `fabro_test_run=<NEXTEST_RUN_ID>`
- `fabro_test_case=<test-case-ulid>`

Update run-creation helpers to append these labels to created runs:
- `run`
- `create`
- detached/create-start helpers
- any workflow fixture helpers that create runs internally

Use existing production `--label KEY=VALUE` support; do not invent a new namespacing mechanism.

### 3. Replace isolated-storage helper assumptions
Update helpers in:
- `lib/crates/fabro-cli/tests/it/cmd/support.rs`
- `lib/crates/fabro-cli/tests/it/workflow/mod.rs`

Specifically:
- remove helpers that assume there is exactly one run in `storage_dir/runs`
- replace `only_run(context)`-style logic with:
  - exact run-id lookup when the helper already has the run id, or
  - test-case-label-based lookup when the helper needs to discover “the run created by this test”
- keep direct store inspection helpers, but always resolve a concrete run first

### 4. Make shared-daemon tests robust
Update tests to match the shared-daemon model:
- exact-run tests remain strict and use ULIDs directly
- global/listing tests (`ps`, `runs list`, `system df`, workflow-slug/recency lookup) should assert the presence/properties of the current test’s run(s), not exact global emptiness/counts unless explicitly scoped
- when a command offers structured output, prefer parsing that output and filtering to the current test’s run(s) over broad transcript snapshots or loose substring matching
- destructive tests must always be scoped:
  - exact run IDs when possible
  - otherwise use existing `--label` filters
- broad destructive operations like “delete everything” are not allowed in shared-daemon tests

Rewrite current tests that depend on ambient exclusivity, especially:
- helpers asserting a single run in storage
- `ps` tests expecting global count equality
- `system prune` tests filtering only by common workflow names like `Simple`

## Important Interface / Behavior Changes
- `fabro-test::TestContext` uses a shared `storage_dir` per nextest run when `NEXTEST_RUN_ID` is set.
- Test-created runs gain deterministic labels:
  - `fabro_test_run`
  - `fabro_test_case`
- Test code must treat shared storage as normal under nextest and avoid assumptions based on storage exclusivity.

## Test Plan
- `fabro-test` coverage:
  - multiple contexts in one nextest run resolve to the same shared storage dir
  - multiple contexts without `NEXTEST_RUN_ID` but in the same process resolve to the same shared storage dir
  - contexts still get distinct `temp_dir` and `home_dir`
  - marker-file cleanup removes stale prior session roots
  - last-process cleanup stops the daemon and removes the shared root
- CLI integration updates:
  - helper coverage for resolving runs by exact ULID or test-case label
  - `ps`, `runs list`, and `system prune` tests updated to shared-storage-safe assertions
  - destructive tests verified to target only test-owned runs
- End-to-end acceptance:
  - a full `cargo nextest run -p fabro-cli` should converge on one daemon per `NEXTEST_RUN_ID` storage root
  - a `cargo test --workspace` invocation should converge on one daemon per test process, not per `TestContext`
  - after the run, no test-owned `fabro.sock` daemon remains for that root
  - add an early validation test or harness check that concurrent auto-start against the same shared `storage_dir` converges on one daemon under parallel load

## Assumptions and Defaults
- Production auto-start semantics for a single `storage_dir` are the source of truth and already provide duplicate-server protection via the existing lock/record path.
- `NEXTEST_RUN_ID` is used when available to derive the shared nextest-run root; otherwise the current process PID is used to derive a shared per-process root.
- The shared test storage root is fully separate from production storage in both modes.
- No separate harness-managed daemon bootstrap protocol is added.
- No changes to production daemon lifetime semantics are part of this plan.
