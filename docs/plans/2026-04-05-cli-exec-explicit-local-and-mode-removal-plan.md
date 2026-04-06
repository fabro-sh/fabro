# CLI Exec Explicit Local Sessions And Mode Removal

## Summary
Simplify the last misleading CLI mode seam by:

- making `fabro exec` explicitly CLI-owned/local
- removing the global `ExecutionMode` / `resolve_mode` abstraction entirely
- deleting `mode` from user config and resolved `Settings`
- treating Fabro server usage as a command-specific connection choice instead of a whole-program execution mode

This pass does **not** move the agent loop into the server. `fabro exec` remains a local agent session. The only server-backed `exec` behavior in scope is optional model transport through `FabroServerAdapter` and the existing `/completions` endpoint.

## Scope Boundaries
In scope:
- `fabro exec`
- removal of `ExecutionMode`, `ResolvedMode`, and `resolve_mode`
- removal of `mode` from `user.toml` / `Settings`
- command-scoped server-target resolution for `exec`
- small adjacent `model` cleanup needed so `[server]` remains meaningful after `mode` is removed
- docs/help/config/test cleanup for the new contract

Out of scope:
- server-owned interactive agent sessions
- new `/exec`, `/sessions`, or worker-RPC server APIs
- moving tool execution, MCP, permissions, or output rendering into the server
- changing the run lifecycle commands
- changing the `/completions` API contract
- adding a new persistent `exec` config field for default server routing

## Key Decisions
- `fabro exec` always owns the agent session locally:
  - prompt loop
  - tool execution
  - permission prompts
  - MCP connections
  - event/output rendering
- when `fabro exec` uses a Fabro server, only the LLM transport changes:
  - requests go through `fabro_llm::providers::FabroServerAdapter`
  - the adapter continues to call the existing `/completions` endpoint
- remove `mode` entirely instead of narrowing it.
  - Once `exec` stops using it, there is no legitimate runtime consumer left.
- `fabro exec` remote routing stays an explicit per-invocation choice in this pass:
  - `--server-url` enables server-routed model transport
  - configured `[server].base_url` alone does **not** reroute `exec`
  - Rationale: this keeps `exec` honest as a local command and avoids inventing a new “force direct” override surface immediately.
- `[server]` remains a real config surface after `mode` removal, but its meaning changes:
  - it stores connection information for commands that support a remote Fabro server target
  - it is not a whole-program execution mode switch
- `fabro model` should honor configured `[server].base_url` as its default remote target once `mode` is gone.
  - `--server-url` still overrides config
  - explicit `--storage-dir` still forces local auto-start for `model`
  - this is a small collateral cleanup to keep the config contract coherent
- keep the global `--server-url` / `--storage-dir` clap conflict unchanged in this pass.
  - Rationale: reducing the misleading mode abstraction does not require broad CLI flag-surface churn, and `exec` does not need both simultaneously.
- global help/docs must stop claiming:
  - `--storage-dir` implies standalone mode
  - `--server-url` implies server mode
- no backward-compatibility shim is required for `mode` in `user.toml`, `fabro settings`, or docs.
- `mode = "server"` / `mode = "standalone"` should disappear completely from the supported config surface:
  - remove it from docs, examples, tests, and resolved settings output
  - existing user config files that still contain it are out of contract after this pass
  - if deserialization happens to ignore the stale key, that is incidental behavior, not preserved product surface

## Implementation Changes
### 1. Remove global mode from shared config and settings types
Delete the dead mode concept from the shared config graph.

In `lib/crates/fabro-types/src/settings/user.rs`:
- delete `ExecutionMode`

In `lib/crates/fabro-types/src/settings/mod.rs`:
- remove the `ExecutionMode` re-export
- remove `Settings.mode`

In `lib/crates/fabro-config/src/user.rs`:
- stop re-exporting `ExecutionMode`

In `lib/crates/fabro-config/src/config.rs`:
- remove `ConfigLayer.mode`
- remove `mode` combine logic

In `lib/crates/fabro-config/src/settings.rs`:
- stop mapping `value.mode` into `Settings`

Behavior:
- `fabro settings` no longer emits a `mode` field
- `user.toml` no longer documents or accepts `mode` as a meaningful setting in this pass

### 2. Replace `resolve_mode` with command-scoped server-target helpers
Stop encoding command behavior as a fake global mode decision.

In `lib/crates/fabro-cli/src/user_config.rs`:
- delete:
  - `ResolvedMode`
  - `resolve_mode(...)`
- keep `build_server_client(...)`
- add one small shared remote-target type, for example:
  - `ServerTarget { base_url: String, tls: Option<ClientTlsSettings> }`
- update `apply_global_overrides(...)` so it only applies:
  - `storage_dir`
  - `server.base_url`
  - and does **not** synthesize any `mode`
- if `apply_global_overrides(...)` becomes a trivial two-field merge helper after `mode` removal, inline it at the call sites instead of preserving it mechanically
- add concrete helpers that reflect the real command boundaries:
  - `exec_server_target(globals: &GlobalArgs, settings: &Settings) -> Option<ServerTarget>`
  - `model_server_target(globals: &GlobalArgs, settings: &Settings) -> Option<ServerTarget>`

Expected helper semantics:
- `exec` helper:
  - returns a remote server target only when `--server-url` is present
  - uses configured `[server].tls` if available
  - ignores configured `[server].base_url` when no CLI flag is present
- `model` helper:
  - returns `--server-url` when present
  - otherwise returns configured `[server].base_url` when present and no explicit `--storage-dir` was provided
  - otherwise returns no remote target so the command falls back to local server auto-start

This helper split is intentional. `exec` and `model` are both server-aware, but they do not have the same defaulting rules.

### 3. Make `fabro exec` explicitly local with optional server-routed model transport
Remove the last server-vs-standalone branching from the command implementation.

In `lib/crates/fabro-cli/src/commands/exec.rs`:
- remove the `resolve_mode(...)` call
- remove the `match resolved.mode { ... }` branch
- always build the session as a local CLI-owned agent session
- when the `exec` server-target helper returns `Some(target)`:
  - build the HTTP client with `build_server_client(...)`
  - create a `FabroServerAdapter`
  - register it on a `fabro_llm::Client`
  - call `run_with_args_and_client(...)`
- when the helper returns `None`:
  - keep the direct provider path via `run_with_args(...)`
- replace logging from `mode = "server"/"standalone"` to something transport-shaped such as:
  - `transport = "server"`
  - `transport = "direct"`

Keep unchanged:
- permission behavior
- MCP server wiring
- output format behavior
- sub-agent behavior
- sandbox/tool execution ownership

### 4. Keep `[server]` meaningful after removing `mode`
Make the shared server config still useful without preserving the abstract mode layer.

In `lib/crates/fabro-cli/src/commands/model.rs`:
- stop hand-rolling the `globals.server_url` match
- use the new model-target helper from `user_config.rs`
- preserve current user-visible `model` behavior apart from the new config defaulting:
  - remote target when `--server-url` is passed
  - remote target when `[server].base_url` is configured and no explicit `--storage-dir` is passed
  - local auto-start otherwise

This is the only planned collateral behavior change outside `exec`, and it exists to keep `[server]` coherent after `mode` removal.

### 5. Remove stale docs and help text
Rewrite the user-facing contract around explicit server targets instead of execution modes.

In `lib/crates/fabro-cli/src/args.rs`:
- no `--mode` flag exists today, so there is no parser flag removal in this file
- update the global flag docstrings:
  - `--storage-dir` should describe local data/storage selection only
  - `--server-url` should describe targeting a Fabro API server for commands that support it
- remove any wording that says either flag “implies” a mode

In docs:
- `docs/reference/user-configuration.mdx`
  - remove the `mode` section
  - rewrite `[server]` as connection info for remote-target-capable commands
  - clarify the `exec` vs `model` behavior split explicitly
- `docs/reference/cli.mdx`
  - update the global option descriptions for `--storage-dir` and `--server-url`
- `docs/administration/deploy-server.mdx`
  - remove guidance telling users to set `mode = "server"` in `user.toml`
  - rewrite the “Pointing the CLI at a server” section around:
    - `--server-url`
    - configured `[server].base_url`
    - command-specific behavior
- update any nearby docs that still describe “whole CLI server mode” rather than explicit server-target selection

### 6. Remove or rewrite stale tests
Delete tests that only exist to preserve `mode` semantics, and add coverage for the real command boundaries.

In `lib/crates/fabro-cli/src/user_config.rs` tests:
- replace `resolve_mode_*` tests with helper-focused tests covering:
  - `exec` has no server target by default
  - `exec` uses CLI `--server-url`
  - `exec` ignores configured `[server].base_url` without CLI `--server-url`
  - `model` uses configured `[server].base_url`
  - `model` CLI `--server-url` overrides configured base URL
  - `model` explicit `--storage-dir` suppresses configured remote targeting
  - TLS is still taken from `[server].tls` when a remote target is selected

In `lib/crates/fabro-cli/tests/it/cmd/exec.rs`:
- update help snapshots for the new global flag wording
- add one behavior test proving `--server-url` changes the failure mode away from local missing-provider-key validation
  - for example: with no local provider key configured and an unreachable `--server-url`, the command should fail on remote connection rather than `API key not set for provider ...`
- add one regression test proving configured `[server].base_url` alone does not reroute `exec`
  - expected outcome: with no local provider key, `exec` still fails on the local missing-key path
- add one explicit override test proving CLI `--server-url` wins over configured `[server].base_url` for `exec`
  - expected outcome: with both present, the command targets the CLI URL and the failure shape reflects that URL/path rather than the configured one

In `lib/crates/fabro-cli/tests/it/cmd/model.rs` and/or `lib/crates/fabro-cli/tests/it/cmd/model_list.rs`:
- add coverage showing configured `[server].base_url` is honored without passing `--server-url`

In `lib/crates/fabro-cli/tests/it/cmd/config.rs`:
- remove `ExecutionMode` imports and expectations
- update `fabro settings` assertions so `mode` is no longer expected in resolved output
- keep coverage that `--server-url` still overrides configured `[server].base_url`

In help snapshots:
- update any snapshots whose global options block still mentions implied server/standalone mode
  - especially:
    - `lib/crates/fabro-cli/tests/it/cmd/fabro.rs`
    - `lib/crates/fabro-cli/tests/it/cmd/exec.rs`
    - `lib/crates/fabro-cli/tests/it/cmd/model.rs`
    - `lib/crates/fabro-cli/tests/it/cmd/model_list.rs`
    - `lib/crates/fabro-cli/tests/it/cmd/config.rs`
- use the repo snapshot workflow from `CLAUDE.md`:
  - run `cargo insta pending-snapshots`
  - verify the expected help/output changes
  - then run `cargo insta accept`

## Test Plan
- Targeted unit tests:
  - `cargo test -p fabro-cli user_config::tests -- --nocapture`
- Targeted CLI integration tests:
  - `cargo nextest run -p fabro-cli cmd::exec:: cmd::model:: cmd::model_list:: cmd::config:: --no-fail-fast`
- Broader regression sweep after the targeted tests are green:
  - `cargo nextest run -p fabro-cli --no-fail-fast`
- Final verification:
  - `cargo fmt --check --all`
  - `cargo clippy --workspace --all-targets -- -D warnings`

## Assumptions And Defaults
- `fabro exec` remains a local agent session in this pass. If we later want server-owned interactive sessions, that should be a separate product/architecture plan.
- The existing `/completions` endpoint and `FabroServerAdapter` are sufficient for optional server-routed `exec` model traffic.
- Pre-production status means removing `mode` outright is acceptable; no shim or migration warning is required.
- `[server].base_url` remains valuable as a remote target config surface for commands like `model`, even after `mode` is removed.
- `exec` staying CLI-flag-only for server routing is intentional in this pass. If users later need a default server-routed `exec`, that should be introduced as an explicit `exec`-scoped config surface rather than reintroducing a fake whole-program mode.
