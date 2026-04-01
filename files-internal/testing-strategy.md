# Testing Strategy

This document defines the default testing rules for this repository, with extra emphasis on CLI integration tests.

The goal is to make the correct test shape obvious:

- put each test in the right layer
- create state through public interfaces
- prefer stable black-box assertions
- avoid brittle tests that mirror implementation details

## Core principles

- Test the public contract of the layer you are in.
- Prefer command-driven or API-driven setup over manually fabricating internal state.
- Prefer snapshots over ad hoc string matching.
- Prefer structured snapshots over parsing JSON and checking one field.
- If a test is only practical by writing internal runtime files directly, it probably belongs in a lower-level test.

## Test layers

Use the narrowest layer that can express the behavior cleanly.

### Unit and crate-level integration tests

Use unit tests or crate-local integration tests when the behavior under test is implementation-facing rather than CLI-facing.

This is the right place for:

- helper logic
- parsing and normalization
- rendering internals
- interview file claim/response mechanics
- retry bookkeeping
- asset manifest parsing
- event formatting

If the setup requires direct writes to internal run files or runtime directories, prefer this layer over `fabro-cli/tests/it`.

### `lib/crates/fabro-cli/tests/it/cmd/*.rs`

`cmd/*` tests are command-owned tests.

Each file should focus on one command's public contract. Setup may use other commands for convenience, but the final assertion should still be about the command under test.

Examples:

- `cmd/run.rs` tests `fabro run`
- `cmd/create.rs` tests `fabro create`
- `cmd/start.rs` tests `fabro start`
- `cmd/attach.rs` tests `fabro attach`

Good command-test assertions:

- help and clap behavior
- required-argument failures
- command-owned persisted state
- command-owned selection or lookup behavior
- user-visible output and lifecycle behavior owned by that command

Bad command-test assertions:

- long multi-command narratives where no single command is the subject
- behavior primarily owned by another command
- runtime internals that only exist because the test planted them by hand

### `lib/crates/fabro-cli/tests/it/workflow/*.rs`

`workflow/*` tests are black-box workflow-behavior tests.

Use this layer when the workflow content is the thing under test, even if the harness command is `fabro run`.

Examples:

- branching behavior
- conditional routing
- parallel execution shape
- representative fixture workflows

These tests should focus on the workflow's observed behavior, not on CLI help text or command argument validation.

### `lib/crates/fabro-cli/tests/it/scenario/*.rs`

`scenario/*` tests are cross-command lifecycle tests.

Use this layer when the point of the test is the interaction among commands or command families.

Examples:

- create -> start -> attach flows
- detached run -> attach flows
- rewind / fork recovery flows
- lookup behavior that spans several commands

Scenario tests are allowed to be broader, but they should still stay command-driven and black-box.

## Placement rules

When choosing where a test belongs, ask: "What is the main contract I am trying to prove?"

- If the answer is a single command, use `cmd/*`.
- If the answer is a workflow fixture or workflow shape, use `workflow/*`.
- If the answer is a multi-command flow, use `scenario/*`.
- If the answer is an implementation detail, use a lower-level test near the code.

If a test starts in `cmd/*` and grows into a workflow or lifecycle narrative, move it.

## State setup rules

Integration tests should create state through public interfaces.

Allowed setup:

- checked-in workflow fixtures
- temp `.fabro` workflow files
- temp `workflow.toml` and `fabro.toml`
- temp git repositories
- temp user config and environment variables
- invoking commands to create runs, checkpoints, branches, and persisted state

Disallowed setup in `fabro-cli/tests/it`:

- writing `run.json` directly
- writing `status.json` directly
- writing `progress.jsonl` directly
- writing `conclusion.json` directly
- writing runtime interview files directly
- writing cached workflow files into run dirs directly
- writing asset manifests directly
- planting files into run internals solely to simulate engine output

The rule is simple: do not hand-author run-directory internals in CLI integration tests.

### Exceptions

Exceptions should be rare.

Only keep a direct internal-state setup when all of the following are true:

- the behavior cannot be reproduced through public commands at reasonable cost
- the behavior is still best validated at the CLI integration layer
- the test clearly documents why the exception exists
- no cleaner lower-level test would cover the behavior better

If those conditions are not met, move the test down a layer.

## Assertion rules

Default to snapshot-first assertions.

### Use transcript snapshots for CLI behavior

Use `fabro_snapshot!` for:

- `--help`
- clap errors
- normal CLI stderr/stdout transcripts
- detached/start/attach lifecycle output

Do not replace full-output snapshots with a handful of `contains()` checks unless the output is intentionally partial and a full snapshot would be noisy or unstable.

### Use structured snapshots for persisted state

When verifying JSON or JSONL:

1. parse it
2. normalize or compact it if needed
3. snapshot the parsed structure

Use `insta` directly or shared helpers such as `fabro_json_snapshot!`.

Good structured snapshot targets:

- `run.json`
- `status.json`
- `inspect` output
- `live.json`
- compacted `progress.jsonl` event sequences
- workflow conclusions and checkpoint summaries

### Keep direct assertions for relational invariants

Use direct assertions when the point is an exact relationship rather than a representation.

Examples:

- exact selected run id
- equality before and after a rejected mutation attempt
- `live.json` equals the last progress event
- exact SHA lineage across rewind/fork
- exact file existence semantics

## Snapshot rules

- Prefer inline snapshots unless the payload is too large to read comfortably.
- Normalize unstable values: timestamps, durations, ULIDs, temp paths, storage paths, run dirs, and SHAs.
- Never accept snapshot churn blindly.
- Review pending snapshots before accepting them.

For CLI snapshot updates:

1. run `cargo insta pending-snapshots`
2. inspect each pending change
3. accept only the intended updates

## Helpers and fixtures

Use the test helpers that reinforce the rules above.

### `TestContext`

Use `TestContext` for CLI integration tests so each test gets isolated home, storage, and temp directories.

Prefer helpers like:

- `context.command()`
- `context.run_cmd()`
- `context.find_run_dir(...)`
- `context.single_run_dir()`

### Shared `tests/it/support`

Shared integration-test helpers may:

- locate fixtures
- read and parse JSON / JSONL
- normalize output
- compact structured events
- poll for stable command-created conditions

Shared integration-test helpers should not:

- fabricate run internals
- write runtime files the engine is supposed to own
- hide broad scenario setup behind opaque helper functions

### Fixtures

Prefer checked-in fixtures when they express a reusable workflow or scenario shape.

Use temporary inline fixtures when the test needs a small one-off input and a checked-in fixture would add noise.

Keep fixtures user-facing:

- workflow sources
- config files
- repo contents

Do not turn fixtures into prebuilt run directories.

## Determinism rules

Tests should be stable on any developer machine.

- Use fixed run ids when practical.
- Prefer dry-run where it still exercises the intended public behavior.
- Use local temp directories, never ambient user state.
- Mark tests that require real providers, real sandboxes, or external services with `#[ignore]` and a clear reason.
- Filter or normalize machine-specific output in snapshots.

## E2E env vars

Secret-backed E2E tests should use the shared test helpers instead of ad hoc env handling.

- `#[e2e_test(twin, live("VAR"))]` — dual-mode test that runs against twin-openai (fake server) or the real API.
- `#[e2e_test(twin)]` — twin-only test (e.g., scripted error scenarios). Skips in live/strict mode.
- `#[e2e_test(live("VAR"))]` — live-only test requiring real secrets. Skips in twin mode.
- `#[e2e_test()]` — sandbox test with no API deps. Runs in all modes.
- `FABRO_TEST_MODE` controls behavior: default is `twin`, `live` skips missing env vars, `strict` fails on missing env vars.
- `cargo nextest run --profile e2e ...` implies strict mode through nextest's runtime `NEXTEST_PROFILE=e2e` environment variable.
- Use `fabro_test::e2e_openai!()` in twin/dual-mode tests to get `(base_url, api_key)`. In twin mode this starts an in-process fake server and generates a per-test API key for isolation.
- Use `fabro_test::require_env()` inside helper functions that need to read E2E env vars after the test-level guard has run.
- Do not call `dotenvy` from tests. E2E env vars must come from the shell or the test runner profile.

If a test depends on `.env` or real credentials, it must be clearly marked and opt-in.

## Naming rules

Name tests after the contract they prove.

Prefer:

- `start_by_workflow_name_prefers_newly_created_submitted_run`
- `detached_uses_cached_graph_after_source_deleted`
- `attach_requires_run_arg`

Avoid:

- `bug4_test`
- `regression_123`
- names that describe setup rather than behavior

If a test exists because of a regression, mention the bug number in a comment or commit message, not in the primary test name unless the bug id is itself part of the contract.

## Review checklist

Before merging a test change, check:

- Is the test in the correct layer: unit, `cmd`, `workflow`, or `scenario`?
- Is the state created through public commands or public inputs?
- Does the test avoid hand-writing run-directory internals?
- Does the assertion use snapshots where snapshots are the better tool?
- Is JSON / JSONL asserted structurally rather than via substring matching?
- Are unstable values normalized?
- Is the test name describing the contract?
- Would a lower-level test be cleaner and less brittle?

## Anti-patterns

Avoid these patterns in CLI integration tests:

- manually creating fake run directories
- writing `progress.jsonl` lines by hand
- writing runtime interview files by hand
- writing asset manifests by hand
- scattering the same workflow setup across many files instead of using fixtures
- asserting one field from a parsed JSON payload when the full structure is the behavior
- using many `contains()` checks for output that should be snapshot-tested
- keeping scenario tests in `cmd/*`
- keeping workflow-shape tests in `cmd/*`

## Defaults for new tests

When adding a new CLI integration test, the default choice should be:

1. decide the layer (`cmd`, `workflow`, `scenario`, or lower-level test)
2. create input state through public files and commands
3. use `TestContext`
4. assert with snapshots
5. keep the test focused on one contract

If you need to break one of these defaults, document why in the test itself.
