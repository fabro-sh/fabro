---
title: fix: Align fabro_run_create MCP schema and accepted input
type: fix
status: active
date: 2026-05-22
---

# fix: Align fabro_run_create MCP schema and accepted input

## Overview

Fix the mismatch where MCP clients see `fabro_run_create` as accepting
`runs: string[]`, while the running server currently deserializes
`runs` as an array of `CreateRunSpec` objects. The fix should make the
tool robust for agents that follow the advertised string shorthand while
also preserving the richer object form used by existing tests and callers.

## Problem Frame

During manual MCP testing, this call failed before tool validation:

```json
{ "runs": ["sleeper"] }
```

The server returned a deserialization error because it expected a
`CreateRunSpec` object. This is a poor agent-facing failure mode: the
client-visible schema implied the call was valid, but the runtime contract
rejected it. The object form still worked:

```json
{ "runs": [{ "workflow": "sleeper", "auto_approve": true, "start": true }] }
```

## Requirements Trace

- R1. `fabro_run_create` must accept the string shorthand advertised to MCP
  clients, treating each string as the workflow selector.
- R2. Existing object-form `CreateRunSpec` calls must keep working with all
  current optional fields.
- R3. MCP `tools/list` must advertise a truthful schema for `runs` so clients
  can discover both accepted forms, or at minimum no longer advertise only a
  shape that fails at runtime.
- R4. Local validation errors must remain tool errors and must still happen
  before auth or network access.
- R5. Docs and QA notes should show the supported shapes so future manual
  testing does not rediscover the mismatch.

## Scope Boundaries

- Do not change the HTTP run creation API.
- Do not change manifest resolution semantics for object-form create specs.
- Do not add new create-run options beyond accepting string shorthand.
- Do not make `client_message_id`, pair APIs, or other MCP tools part of this
  fix.

## Context & Research

### Relevant Code and Patterns

- `lib/crates/fabro-tool/src/create.rs` owns `FabroRunCreateParams`,
  `CreateRunSpec`, validation, and create-run execution.
- `lib/crates/fabro-mcp-server/src/server.rs` registers the MCP tool using
  `Parameters<run_tools::FabroRunCreateParams>`.
- `lib/crates/fabro-cli/tests/it/cmd/mcp.rs` already exercises object-form
  `fabro_run_create`, schema listing, and pre-auth validation.
- `docs/internal/mcp-server-qa-test-plan.md` already records past
  schema/runtime mismatches, especially the `inputs` schema narrowing.
- `docs/public/agents/mcp.mdx` lists the MCP tools but does not show concrete
  `fabro_run_create` input examples.

### Institutional Learnings

- No `docs/solutions/` directory exists in this checkout.
- The MCP QA plan shows this class of bug has appeared before: schema/runtime
  agreement for MCP parameters needs explicit tests, not only happy-path calls.

### External References

- None. The issue is internal schema/deserialization parity; local patterns are
  sufficient.

## Key Technical Decisions

- Accept both string shorthand and object specs. This is additive, fixes the
  observed failed call directly, and preserves existing rich create semantics.
- Normalize inputs before validation. Convert raw string/object specs into the
  existing `ValidatedCreateRunSpec` path so all downstream manifest and run
  creation behavior stays centralized.
- Add schema assertions near the MCP boundary. Unit validation alone cannot
  catch a client-visible `tools/list` regression.

## Open Questions

### Resolved During Planning

- Should the object form remain supported? Yes. Existing tests and the MCP
  design require optional create settings like `dry_run`, `auto_approve`,
  `labels`, `parent_id`, and `start`.
- Should string shorthand be treated as workflow selector only? Yes. It maps
  cleanly to the one required object-form field.

### Deferred to Implementation

- Exact schema shape: use `anyOf`/`oneOf`, inline schemas, or a manual
  `JsonSchema` implementation depending on how `schemars` and `rmcp` emit the
  final `tools/list` schema.

## Implementation Units

- [ ] **Unit 1: Characterize and lock the current schema mismatch**

**Goal:** Add failing coverage that proves `fabro_run_create` advertises and
accepts the intended `runs` item shapes.

**Requirements:** R1, R3, R4

**Dependencies:** None

**Files:**
- Modify: `lib/crates/fabro-mcp-server/src/server.rs`
- Modify: `lib/crates/fabro-cli/tests/it/cmd/mcp.rs`

**Approach:**
- Add a server-level schema test for `fabro_run_create`, similar to the
  existing `fabro_run_pair` schema leakage test.
- Assert the schema for `runs` does not collapse to string-only if the runtime
  requires object fields.
- Add an MCP stdio integration case that calls `fabro_run_create` with
  `runs: ["simple.fabro"]` against an unreachable server and verifies local
  parameter validation succeeds far enough to require backend/auth, not fail
  with `expected struct CreateRunSpec`.

**Execution note:** Characterization-first. Capture the failing schema/runtime
contract before changing deserialization.

**Patterns to follow:**
- `fabro_run_pair_tool_is_registered_with_stage_based_schema` in
  `lib/crates/fabro-mcp-server/src/server.rs`.
- `mcp_create_validation_errors_happen_before_auth_or_network` in
  `lib/crates/fabro-cli/tests/it/cmd/mcp.rs`.

**Test scenarios:**
- Integration: `tools/list` for `fabro_run_create` exposes `runs` as an array
  whose items include the object form with a required `workflow` field.
- Error path: calling `fabro_run_create` with `runs: ["simple.fabro"]` no
  longer returns an MCP deserialization error mentioning `CreateRunSpec`.
- Error path: malformed non-string/non-object run items still fail before
  auth/network with an actionable tool or MCP parameter error.

**Verification:**
- The new tests fail against the current behavior and identify the mismatch
  without requiring a live Fabro API server.

- [ ] **Unit 2: Add string shorthand normalization for run create specs**

**Goal:** Make `runs: ["workflow"]` behave like
`runs: [{ "workflow": "workflow" }]`.

**Requirements:** R1, R2, R4

**Dependencies:** Unit 1

**Files:**
- Modify: `lib/crates/fabro-tool/src/create.rs`
- Test: `lib/crates/fabro-tool/src/create.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/mcp.rs`

**Approach:**
- Introduce a raw input representation for create specs that can deserialize
  either a string workflow selector or the current object form.
- Normalize both raw forms into the existing validated create spec structure
  before calling manifest resolution or backend methods.
- Preserve all existing object-form field handling and validation.
- Treat blank string workflows as invalid local input with a clear tool error.

**Patterns to follow:**
- `AnswerValue` in `lib/crates/fabro-tool/src/interact.rs` for custom
  schema/deserialization where the MCP surface needs a flexible input value.
- `RunInputValue` in `lib/crates/fabro-tool/src/create.rs` for schema-driven
  input constraints and local conversion.

**Test scenarios:**
- Happy path: `runs: ["simple.fabro"]` creates the same validated spec as
  `runs: [{ "workflow": "simple.fabro" }]`.
- Happy path: object form with `dry_run`, `auto_approve`, `labels`, and
  `start` continues to pass through unchanged.
- Edge case: `runs: ["  "]` returns a local validation error naming the
  workflow value.
- Error path: `runs: []` and 51 entries retain the existing min/max errors.
- Integration: string shorthand reaches the backend path in the MCP integration
  harness, proving it is not rejected by the MCP deserializer.

**Verification:**
- Existing object-form MCP create tests still pass.
- String shorthand can start a run in the same manual scenario that previously
  failed.

- [ ] **Unit 3: Make the advertised MCP schema client-friendly**

**Goal:** Ensure MCP clients can discover the actual supported input contract.

**Requirements:** R2, R3

**Dependencies:** Unit 2

**Files:**
- Modify: `lib/crates/fabro-tool/src/create.rs`
- Modify: `lib/crates/fabro-mcp-server/src/server.rs`
- Test: `lib/crates/fabro-mcp-server/src/server.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/mcp.rs`

**Approach:**
- Prefer a schema where `runs.items` clearly advertises both supported forms:
  a workflow string shorthand and the object-form create spec.
- If `schemars` emits `$defs` that client tooling misinterprets, inline the
  relevant schema or provide a manual `JsonSchema` implementation for the raw
  create-spec input.
- Keep the schema descriptive rather than loosening it to arbitrary JSON.

**Patterns to follow:**
- Manual `JsonSchema` implementations in `RunInputValue` and `AnswerValue`.
- Existing MCP schema assertions in `mcp.rs` that verify property schemas are
  objects and startup listing remains fast.

**Test scenarios:**
- Happy path: `tools/list` schema for `fabro_run_create` contains the string
  shorthand branch.
- Happy path: `tools/list` schema for `fabro_run_create` contains the object
  branch with `workflow`.
- Error path: schema does not advertise unsupported array/object input values
  for `inputs`; existing scalar-only assertion remains true.
- Integration: listing tools still does not construct the API client.

**Verification:**
- An MCP client inspecting `tools/list` can infer at least one valid shape that
  the runtime accepts.

- [ ] **Unit 4: Update docs and QA checklist**

**Goal:** Record the supported `fabro_run_create` shapes and the regression
  test so future manual testing uses the right contract.

**Requirements:** R5

**Dependencies:** Unit 2, Unit 3

**Files:**
- Modify: `docs/public/agents/mcp.mdx`
- Modify: `docs/internal/mcp-server-qa-test-plan.md`

**Approach:**
- Add a small `fabro_run_create` example showing both shorthand and object
  form, with object form recommended when options are needed.
- Add a QA note that this schema/runtime mismatch was fixed and should remain
  covered by schema-discovery and shorthand-call tests.

**Patterns to follow:**
- Existing terse MCP tool table in `docs/public/agents/mcp.mdx`.
- Existing resolved issue notes at the top of
  `docs/internal/mcp-server-qa-test-plan.md`.

**Test scenarios:**
- Test expectation: none -- documentation-only unit.

**Verification:**
- Public docs show an input shape that works when pasted into an MCP client.
- QA plan names the regression and where it is covered.

## System-Wide Impact

- **Interaction graph:** MCP clients call `tools/list`, infer parameter shape,
  and then call `tools/call`; this fix aligns both surfaces with the same
  deserializer.
- **Error propagation:** Invalid local input should continue to return MCP tool
  errors without killing the stdio server. Framework-level JSON type errors
  should only remain for truly unsupported JSON shapes.
- **State lifecycle risks:** No persistent data migration. The only durable
  effect is successful run creation for shorthand calls that previously failed.
- **API surface parity:** HTTP run creation remains unchanged. This is an MCP
  tool input compatibility fix.
- **Integration coverage:** Unit tests cover normalization; MCP stdio tests
  cover real schema discovery and tool-call deserialization.
- **Unchanged invariants:** Object-form create specs remain the full-fidelity
  path for labels, parent links, options, and overrides.

## Risks & Dependencies

| Risk | Mitigation |
|------|------------|
| Schema becomes too loose and agents send unsupported values | Use an explicit string-or-object schema and keep local validation narrow |
| Object-form callers regress while adding shorthand | Keep existing tests and add object-form pass-through assertions |
| MCP client tooling still summarizes the schema poorly | Make runtime accept the string shorthand so the summarized `string[]` shape still works |
| Validation accidentally moves after auth/network setup | Keep validation tests using an unreachable server target |

## Documentation / Operational Notes

- This fix should be called out as an MCP UX/compatibility fix, not an HTTP API
  change.
- Manual verification should include the exact previously failing call:
  `fabro_run_create({ "runs": ["sleeper"] })`.

## Sources & References

- Related code: `lib/crates/fabro-tool/src/create.rs`
- Related code: `lib/crates/fabro-mcp-server/src/server.rs`
- Related tests: `lib/crates/fabro-cli/tests/it/cmd/mcp.rs`
- Related QA doc: `docs/internal/mcp-server-qa-test-plan.md`
- Related docs: `docs/public/agents/mcp.mdx`
