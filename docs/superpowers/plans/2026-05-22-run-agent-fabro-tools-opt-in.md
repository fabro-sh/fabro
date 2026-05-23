# Run Agent Fabro Tools Opt-In Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `[run.agent] fabro_tools = true/false`, defaulting to `false`, so workflow agents only get Fabro run tools and the `agent:run_tools` worker JWT scope when a run opts in.

**Architecture:** Treat `run.agent.fabro_tools` as the source of truth in resolved run settings. The server reads the effective run setting before spawning `__run-worker` and issues the worker token with or without `agent:run_tools`. The CLI worker decodes the already-present worker token and registers Fabro run tools only when the scope contains both `run:worker` and `agent:run_tools`. Do not add a second worker-side env flag or hidden CLI argument for this capability; the signed JWT scope is the worker-side authority.

**Tech Stack:** Rust, Serde TOML config layers, Fabro worker JWT scopes, `cargo nextest`.

---

## File Map

- Modify `lib/crates/fabro-types/src/settings/run.rs`: add resolved `RunAgentSettings::fabro_tools`.
- Modify `lib/crates/fabro-config/src/layers/run.rs`: add optional layered `[run.agent] fabro_tools` and options metadata.
- Modify `lib/crates/fabro-config/src/resolve/run.rs`: resolve missing config to `false`.
- Modify `lib/crates/fabro-config/src/tests/resolve_run.rs`: cover default, true, false, and layer override behavior.
- Modify `lib/crates/fabro-server/src/worker_token.rs`: make the base worker scope constructor available to production code and retain `run_worker_with_agent_run_tools`.
- Modify `lib/crates/fabro-server/src/server.rs`: compute the opt-in flag from run settings and choose worker JWT scopes.
- Modify `lib/crates/fabro-server/src/server/tests.rs`: cover default and opted-in worker token scopes.
- Modify `lib/crates/fabro-cli/src/commands/run/runner.rs`: register `FabroRunToolServices` from the decoded worker token scope.
- Modify docs generator/reference docs: `lib/crates/fabro-dev/src/commands/docs_options_reference.rs`, `docs/public/reference/user-configuration.mdx`, and `docs/public/execution/run-configuration.mdx`.

---

## Task 1: Add Resolved Run Config

**Files:**
- Modify: `lib/crates/fabro-types/src/settings/run.rs`
- Modify: `lib/crates/fabro-config/src/layers/run.rs`
- Modify: `lib/crates/fabro-config/src/resolve/run.rs`
- Test: `lib/crates/fabro-config/src/tests/resolve_run.rs`

- [ ] Add resolver tests for default `false`, explicit `true`, explicit `false`, and higher-layer override behavior.
- [ ] Add `fabro_tools: bool` to `RunAgentSettings`.
- [ ] Add `fabro_tools: Option<bool>` to `RunAgentLayer` with `#[serde(default, skip_serializing_if = "Option::is_none")]` and options metadata.
- [ ] Resolve `agent.fabro_tools.unwrap_or(false)`.
- [ ] Run `cargo nextest run -p fabro-config run_agent_fabro_tools`.

---

## Task 2: Gate Worker JWT Scope

**Files:**
- Modify: `lib/crates/fabro-server/src/worker_token.rs`
- Modify: `lib/crates/fabro-server/src/server.rs`
- Test: `lib/crates/fabro-server/src/server/tests.rs`

- [ ] Add or keep constructors:

```rust
WorkerScopeSet::run_worker()
WorkerScopeSet::run_worker_with_agent_run_tools()
```

- [ ] Update worker command tests:
  - default run token scopes are exactly `run:worker`
  - opted-in run token scopes are exactly `run:worker agent:run_tools`
- [ ] Do not set a separate worker env var for Fabro tools.
- [ ] Load the effective setting from the run spec/settings available at worker-spawn time. If the current spawn path only exposes full projected run state, prefer a narrow run-spec/settings accessor or cached run record field over scanning/projecting full run history just to read this static setting.
- [ ] Pass the boolean into worker-token scope selection.
- [ ] Run `cargo nextest run -p fabro-server worker_command`.

---

## Task 3: Gate CLI Worker Tool Registration From JWT Scope

**Files:**
- Modify: `lib/crates/fabro-cli/src/commands/run/runner.rs`
- Test: `lib/crates/fabro-cli/src/commands/run/runner.rs`

- [ ] Add focused tests for `fabro_run_tools_enabled_from_worker_token`:
  - invalid token -> false
  - missing `scope` claim -> false
  - `run:worker` only -> false
  - `agent:run_tools` only -> false
  - unknown extra scope -> false
  - `run:worker agent:run_tools` -> true
- [ ] Decode only the unsigned claim locally for registration convenience. The server remains responsible for signature and scope enforcement.
- [ ] Gate `build_fabro_run_tool_services(...)` on `fabro_run_tools_enabled_from_worker_token(worker_token)`.
- [ ] Keep token presence as a second local guard inside `build_fabro_run_tool_services`.
- [ ] Run:

```bash
cargo nextest run -p fabro-cli fabro_run_tools_enabled_token_requires_run_tools_scope
cargo nextest run -p fabro-cli --test it runner
```

---

## Task 4: Update Docs And Generated Reference Text

**Files:**
- Modify: `lib/crates/fabro-dev/src/commands/docs_options_reference.rs`
- Modify: `docs/public/reference/user-configuration.mdx`
- Modify: `docs/public/execution/run-configuration.mdx`

- [ ] Add `fabro_tools = true` to the `[run.agent]` generated sample.
- [ ] Document that the setting defaults to `false`.
- [ ] Document that the setting controls built-in Fabro run-management tools and is separate from ordinary agent `permissions` and `[run.agent.mcps]`.
- [ ] Run:

```bash
cargo dev docs refresh
cargo dev docs check
```

---

## Full Verification

```bash
cargo nextest run -p fabro-config
cargo nextest run -p fabro-server
cargo nextest run -p fabro-cli
cargo +nightly-2026-04-14 fmt --check --all
cargo +nightly-2026-04-14 clippy -p fabro-types -p fabro-config -p fabro-server -p fabro-cli -p fabro-dev --all-targets -- -D warnings
```

## Acceptance Criteria

- Default run:
  - resolved `run.agent.fabro_tools == false`
  - worker JWT scope is `run:worker`
  - `StartServices.fabro_run_tools == None`
- Opted-in run:
  - resolved `run.agent.fabro_tools == true`
  - worker JWT scope is `run:worker agent:run_tools`
  - `StartServices.fabro_run_tools` is present
- No private worker env var or hidden CLI flag controls Fabro tool registration.
- Server-side authorization remains the enforcement point for the worker token signature, run id, and scopes.

## Assumptions And Defaults

- `fabro_tools` is a per-run opt-in setting only; this plan does not add a separate server-wide allow/deny policy.
- Defaulting to `false` intentionally changes existing behavior: runs that need Fabro run tools must set `[run.agent] fabro_tools = true`.
- `run.agent.permissions` remains about ordinary agent tool permissions and does not imply Fabro API access.
- `[run.agent.mcps]` remains independent; MCP tools are not enabled or disabled by `fabro_tools`.
- `fabro mcp start` and standalone MCP exposure of Fabro tools are out of scope.
