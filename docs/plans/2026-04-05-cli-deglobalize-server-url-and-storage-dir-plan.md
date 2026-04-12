# CLI De-globalize Server URL And Storage Dir

## Summary
Make `--storage-dir` and `--server-url` command-scoped instead of global so the CLI surface matches the architecture we now have.

This pass should:

- keep only truly global flags in `GlobalArgs`
- move local-storage selection onto the commands that actually use local storage
- move remote-server selection onto the commands that actually support remote targeting
- remove false affordances from help output, docs, and env-var wiring

This plan is the direct follow-on to [2026-04-05-next-steps-after-cli-mode-removal.md](../ideation/2026-04-05-next-steps-after-cli-mode-removal.md).

## Scope Boundaries
In scope:
- de-globalize `--storage-dir` / `FABRO_STORAGE_DIR`
- de-globalize `--server-url` / `FABRO_SERVER_URL`
- update clap types, parser tests, dispatch signatures, and command help
- keep `model` and `exec` behavior consistent with the recent cleanup, but expressed through command-local args
- update docs/examples that still show top-level target flags

Out of scope:
- changing server/runtime behavior for `run`, `model`, or `exec`
- adding new remote-capable commands
- making `exec` server-owned
- changing `[server].base_url` semantics again
- broad config/schema changes outside the CLI arg surface

## Problem Frame
The code no longer has a meaningful whole-program “mode”, but the CLI still advertises target-selection flags as if every command can choose between local storage and a remote server.

That is now misleading in several different ways:

- many commands still show `--server-url` even though they never use it
- many commands still show `--storage-dir` even though they do not read local runtime state
- docs still describe these flags as global CLI surface area
- help snapshot churn is broader than the real behavior surface because the flags leak into unrelated commands

The goal is not to invent new behavior. The goal is to make the CLI honest about which commands actually support which target-selection controls.

## Key Decisions
- `GlobalArgs` remains, but only for true global flags:
  - `--json`
  - `--debug`
  - `--no-upgrade-check`
  - `--quiet`
  - `--verbose`
- `--storage-dir` and `--server-url` stop being top-level/global clap args entirely.
- target-selection args belong to leaf commands, not parent namespaces.
  - Rationale: keep natural syntax such as:
    - `fabro run foo.fabro --storage-dir /tmp/fabro`
    - `fabro model list --server-url https://fabro.example.com/api/v1`
    - `fabro server start --storage-dir /tmp/fabro`
  - Avoid awkward parent-namespace syntax like `fabro model --server-url ... list`.
- `FABRO_STORAGE_DIR` and `FABRO_SERVER_URL` remain supported, but only for commands that define the corresponding arg.
- command-local target syntax becomes the supported surface.
  - Old forms like `fabro --server-url ... model list` and `fabro --storage-dir ... run ...` are intentionally removed in this pass.
- during implementation, temporary duplication between global target args and the new leaf-command args is acceptable.
  - Rationale: this refactor touches many commands, so the migration should stay compile-safe until the old global fields are fully unused and can be removed in one cleanup step.
- `settings` keeps `--storage-dir`, because it changes the resolved local settings output.
- `settings` does not keep `--server-url`.
  - Rationale: command-local remote target overrides should not masquerade as merged durable config.
- `preflight` keeps `--storage-dir`, because it resolves a local run-oriented settings stack and should continue to allow explicit local storage selection.
- `exec` keeps only `--server-url`.
  - `exec` does not need `--storage-dir`.
- `model list` and `model test` keep both:
  - `--server-url` for explicit remote server targeting
  - `--storage-dir` for explicit local auto-start/storage selection
- the existing `model` config/defaulting contract stays intact:
  - explicit `--server-url` wins
  - explicit `--storage-dir` suppresses configured `[server].base_url`
  - otherwise `model` may default to configured `[server].base_url`
- the existing `exec` contract stays intact:
  - server routing only happens when the command-local `server_url` value is set, whether by CLI flag or `FABRO_SERVER_URL`
  - configured `[server].base_url` alone still does not reroute `exec`
- `fabro model` with no subcommand remains the convenience alias for default listing behavior.
  - target overrides are not a compatibility goal for the bare alias in this pass; users should use `fabro model list ...` when specifying target args explicitly.

## Command Matrix
### `--server-url` only
- `fabro exec`

### `--storage-dir` and `--server-url`
- `fabro model list`
- `fabro model test`

### `--storage-dir` only
- `fabro run`
- `fabro create`
- `fabro start`
- `fabro attach`
- `fabro logs`
- `fabro resume`
- `fabro rewind`
- `fabro fork`
- `fabro wait`
- `fabro diff`
- hidden `fabro __runner`
- `fabro ps`
- `fabro rm`
- `fabro inspect`
- `fabro artifact list`
- `fabro artifact cp`
- `fabro sandbox cp`
- `fabro sandbox preview`
- `fabro sandbox ssh`
- `fabro store dump`
- `fabro pr create`
- `fabro pr list`
- `fabro pr view`
- `fabro pr merge`
- `fabro pr close`
- `fabro system prune`
- `fabro system df`
- `fabro server start`
- `fabro server stop`
- `fabro server status`
- hidden `fabro server __serve`
- `fabro settings`
- `fabro preflight`

### Neither target arg
- `fabro validate`
- `fabro graph`
- `fabro parse`
- `fabro doctor`
- `fabro install`
- `fabro repo init`
- `fabro repo deinit`
- `fabro workflow list`
- `fabro workflow create`
- `fabro provider login`
- hidden `fabro skill install`
- `fabro secret get`
- `fabro secret list`
- `fabro secret set`
- `fabro secret rm`
- `fabro docs`
- `fabro discord`
- `fabro completion`
- `fabro upgrade`
- internal analytics/panic commands

## Implementation Changes
### 1. Narrow `GlobalArgs` and add command-scoped target arg structs
In `lib/crates/fabro-cli/src/args.rs`:
- add the new command-scoped target arg structs first, while temporarily leaving `storage_dir` and `server_url` on `GlobalArgs`
- keep `GlobalArgs` as the long-term home only for the true-global output/logging/upgrade-check surface
- add small reusable arg structs:
  - `StorageDirArgs { storage_dir: Option<PathBuf> }`
  - `ServerUrlArgs { server_url: Option<String> }`
  - `ModelTargetArgs { storage_dir: Option<PathBuf>, server_url: Option<String> }`
- keep the `storage_dir` / `server_url` conflict on `ModelTargetArgs`
- keep the existing env bindings:
  - `FABRO_STORAGE_DIR`
  - `FABRO_SERVER_URL`

This temporary duplication is intentional. The crate should continue to compile while commands migrate off `GlobalArgs`.

Because the target args will now live on leaf commands, convert inline/anonymous clap variants into explicit args structs where needed. The important conversions are:
- `Exec(AgentArgs)` -> `Exec(ExecArgs)` where `ExecArgs` flattens:
  - `ServerUrlArgs`
  - `fabro_agent::cli::AgentArgs`
- `ModelsCommand::List { ... }` -> `ModelsCommand::List(ModelListArgs)`
- `ModelsCommand::Test { ... }` -> `ModelsCommand::Test(ModelTestArgs)`
- `RunCommands::Start { run: String }` -> `RunCommands::Start(StartArgs)` where `StartArgs` flattens `StorageDirArgs`
- `RunCommands::Attach { run: String }` -> `RunCommands::Attach(AttachArgs)` where `AttachArgs` flattens `StorageDirArgs`
- `RunCommands::Runner { ... }` -> `RunCommands::Runner(RunnerArgs)` where `RunnerArgs` flattens `StorageDirArgs`
- `ServerCommand::Start { ... }` -> `ServerCommand::Start(ServerStartArgs)` where `ServerStartArgs` flattens:
  - `StorageDirArgs`
  - the existing `foreground` flag
  - `ServeArgs`
- `ServerCommand::Stop { ... }` -> `ServerCommand::Stop(ServerStopArgs)` where `ServerStopArgs` flattens `StorageDirArgs`
- `ServerCommand::Status { ... }` -> `ServerCommand::Status(ServerStatusArgs)` where `ServerStatusArgs` flattens `StorageDirArgs`
- `ServerCommand::Serve { ... }` -> `ServerCommand::Serve(ServerServeArgs)` where `ServerServeArgs` flattens:
  - `StorageDirArgs`
  - the existing `record_path` field
  - `ServeArgs`

Also flatten `StorageDirArgs` into the leaf arg types that currently rely on global storage selection, including:
- `RunArgs`
- `LogsArgs`
- `DiffArgs`
- `ResumeArgs`
- `RewindArgs`
- `ForkArgs`
- `WaitArgs`
- `RunsListArgs`
- `RunsRemoveArgs`
- `InspectArgs`
- `ArtifactListArgs`
- `ArtifactCpArgs`
- `CpArgs`
- `PreviewArgs`
- `SshArgs`
- `StoreDumpArgs`
- `PrCreateArgs`
- `PrListArgs`
- `PrViewArgs`
- `PrMergeArgs`
- `PrCloseArgs`
- `RunsPruneArgs`
- `DfArgs`
- `SettingsArgs`
- `PreflightArgs`

### 2. Rework parser and dispatch wiring around leaf-command target args
In `lib/crates/fabro-cli/src/main.rs`:
- keep `Cli { globals, command }`, but with the narrower `GlobalArgs`
- update dispatch pattern matches to use the new wrapper arg structs:
  - `ExecArgs`
  - `ModelListArgs`
  - `ModelTestArgs`
  - `StartArgs`
  - `AttachArgs`
  - `RunnerArgs`
- replace parser tests that assume global target flags with command-local parser tests

Parser behavior to lock down:
- `fabro run test/simple.fabro --storage-dir /tmp/fabro` parses
- `fabro model list --server-url http://localhost:3000/api/v1` parses
- `fabro exec --server-url http://localhost:3000/api/v1 "prompt"` parses
- `fabro model list --storage-dir /tmp/fabro --server-url http://localhost:3000/api/v1` fails with the command-local conflict
- `fabro --server-url http://localhost:3000/api/v1 model list` no longer parses
- `fabro --storage-dir /tmp/fabro run test/simple.fabro` no longer parses

These parser assertions belong at the end of the migration, after `storage_dir` and `server_url` have actually been removed from `GlobalArgs`. Before that cleanup step, the old top-level forms will still parse and should not be treated as failures yet.

### 3. Replace “with globals” settings helpers with explicit local/remote override helpers
The current helper layer in `lib/crates/fabro-cli/src/user_config.rs` is still shaped around global target args. That should be simplified to explicit command-local override helpers.

In `lib/crates/fabro-cli/src/user_config.rs`:
- remove or rename the generic helpers that imply target args are global:
  - `user_layer_with_globals(...)`
  - `load_user_settings_with_globals(...)`
  - `apply_global_overrides(...)`
- replace them with explicit helpers such as:
  - `user_layer_with_storage_dir(storage_dir: Option<&Path>) -> anyhow::Result<ConfigLayer>`
  - `load_user_settings_with_storage_dir(storage_dir: Option<&Path>) -> anyhow::Result<Settings>`
  - `exec_server_target(args: &ServerUrlArgs, settings: &Settings) -> Option<ServerTarget>`
  - `model_server_target(args: &ModelTargetArgs, settings: &Settings) -> Option<ServerTarget>`
- keep `build_server_client(...)`

Behavior to preserve:
- local-storage commands can still resolve settings with an explicit storage-dir override
- `model` keeps its existing defaulting behavior
- `exec` only resolves a remote target when its command-local `server_url` value is set
- TLS continues to come from `[server].tls` when a remote target is selected

### 4. Repoint command implementations to explicit target args
Update the commands that currently read target selection through `GlobalArgs`.

In `lib/crates/fabro-cli/src/commands/model.rs`:
- switch from `GlobalArgs`-based target lookup to `ModelTargetArgs`
- keep the recent server-canonical behavior unchanged

In `lib/crates/fabro-cli/src/commands/exec.rs`:
- switch from `GlobalArgs`-based target lookup to `ServerUrlArgs`
- remove any remaining dependence on `storage_dir`
- load settings with plain `load_user_settings()`, not a storage-dir-aware helper
  - `exec` does not need local storage override semantics anymore
  - it should then pass `&ServerUrlArgs` to `exec_server_target(...)`

In the local-storage command modules:
- replace `load_user_settings_with_globals(globals)` with the explicit storage-dir helper
- read `storage_dir` from the command’s own args struct, not from `globals`

Key files here include:
- `lib/crates/fabro-cli/src/commands/run/mod.rs`
- `lib/crates/fabro-cli/src/commands/run/command.rs`
- `lib/crates/fabro-cli/src/commands/run/logs.rs`
- `lib/crates/fabro-cli/src/commands/run/resume.rs`
- `lib/crates/fabro-cli/src/commands/run/rewind.rs`
- `lib/crates/fabro-cli/src/commands/run/fork.rs`
- `lib/crates/fabro-cli/src/commands/run/wait.rs`
- `lib/crates/fabro-cli/src/commands/run/diff.rs`
- `lib/crates/fabro-cli/src/commands/run/cp.rs`
- `lib/crates/fabro-cli/src/commands/run/preview.rs`
- `lib/crates/fabro-cli/src/commands/run/ssh.rs`
- `lib/crates/fabro-cli/src/commands/runs/list.rs`
- `lib/crates/fabro-cli/src/commands/runs/rm.rs`
- `lib/crates/fabro-cli/src/commands/runs/inspect.rs`
- `lib/crates/fabro-cli/src/commands/artifact/list.rs`
- `lib/crates/fabro-cli/src/commands/artifact/cp.rs`
- `lib/crates/fabro-cli/src/commands/store/dump.rs`
- `lib/crates/fabro-cli/src/commands/pr/*.rs`
- `lib/crates/fabro-cli/src/commands/system/*.rs`
- `lib/crates/fabro-cli/src/commands/server/mod.rs`
- `lib/crates/fabro-cli/src/commands/preflight.rs`
- `lib/crates/fabro-cli/src/commands/config/mod.rs`

Important internal-path detail:
- hidden commands still need explicit local storage wiring
- `fabro server __serve` must keep receiving `--storage-dir` from `lib/crates/fabro-cli/src/commands/server/start.rs`
- hidden `fabro __runner` should continue to accept explicit storage-dir selection through its own args struct rather than through `GlobalArgs`

### 5. Make help output and docs reflect the new command contract
In `docs/reference/cli.mdx`:
- remove `--storage-dir` and `--server-url` from the “Global options” table
- add a short “Command-scoped target selection” section or matrix
- update examples to use command-local placement:
  - `fabro model list --server-url ...`
  - `fabro server start --storage-dir ...`

In `docs/reference/user-configuration.mdx`:
- keep `[server].base_url` documentation
- clarify that it is a default only for commands that support remote server targets
- keep the `model` / `exec` asymmetry explicit
- update examples away from top-level `fabro --server-url ...`

In `docs/administration/deploy-server.mdx`:
- update “point the CLI at a server” examples to command-local syntax
- clarify that `model` can use configured `[server].base_url`, while `exec` still needs explicit `--server-url`

In `docs/reference/run-directory.mdx`:
- change “global `--storage-dir` flag” wording to command-local run-family wording

Also scan for stale examples in nearby docs and changelog/admin references and update only the ones that would now be actively misleading.

### 6. Update unit tests, parser tests, and help snapshots
#### Unit and parser coverage
In `lib/crates/fabro-cli/src/main.rs` tests:
- replace the old global target-flag parse tests with command-local parse tests
- add the explicit “old global placement no longer parses” cases

In `lib/crates/fabro-cli/src/user_config.rs` tests:
- update helper tests to use the new arg structs
- preserve coverage for:
  - `exec` CLI/env `server_url` routing
  - `exec` ignoring configured `[server].base_url`
  - `model` configured `[server].base_url` defaulting
  - `model` `storage_dir` suppressing configured remote target
  - TLS inheritance

#### Integration coverage
Update behavior/help coverage in:
- `lib/crates/fabro-cli/tests/it/cmd/exec.rs`
- `lib/crates/fabro-cli/tests/it/cmd/model.rs`
- `lib/crates/fabro-cli/tests/it/cmd/model_list.rs`
- `lib/crates/fabro-cli/tests/it/cmd/model_test.rs`
- `lib/crates/fabro-cli/tests/it/cmd/config.rs`
- `lib/crates/fabro-cli/tests/it/cmd/run.rs`
- `lib/crates/fabro-cli/tests/it/cmd/create.rs`
- `lib/crates/fabro-cli/tests/it/cmd/attach.rs`
- `lib/crates/fabro-cli/tests/it/cmd/logs.rs`
- `lib/crates/fabro-cli/tests/it/cmd/resume.rs`
- `lib/crates/fabro-cli/tests/it/cmd/rewind.rs`
- `lib/crates/fabro-cli/tests/it/cmd/wait.rs`
- `lib/crates/fabro-cli/tests/it/cmd/diff.rs`
- `lib/crates/fabro-cli/tests/it/cmd/runner.rs`
- `lib/crates/fabro-cli/tests/it/cmd/ps.rs`
- `lib/crates/fabro-cli/tests/it/cmd/inspect.rs`
- `lib/crates/fabro-cli/tests/it/cmd/store.rs`
- `lib/crates/fabro-cli/tests/it/cmd/store_dump.rs`
- `lib/crates/fabro-cli/tests/it/cmd/artifact_list.rs`
- `lib/crates/fabro-cli/tests/it/cmd/artifact_cp.rs`
- `lib/crates/fabro-cli/tests/it/cmd/sandbox_cp.rs`
- `lib/crates/fabro-cli/tests/it/cmd/sandbox_preview.rs`
- `lib/crates/fabro-cli/tests/it/cmd/sandbox_ssh.rs`
- `lib/crates/fabro-cli/tests/it/cmd/pr.rs`
- `lib/crates/fabro-cli/tests/it/cmd/pr_list.rs`
- `lib/crates/fabro-cli/tests/it/cmd/pr_view.rs`
- `lib/crates/fabro-cli/tests/it/cmd/pr_merge.rs`
- `lib/crates/fabro-cli/tests/it/cmd/pr_close.rs`
- `lib/crates/fabro-cli/tests/it/cmd/system.rs`
- `lib/crates/fabro-cli/tests/it/cmd/system_df.rs`
- `lib/crates/fabro-cli/tests/it/cmd/system_prune.rs`
- `lib/crates/fabro-cli/tests/it/cmd/server_start.rs`
- `lib/crates/fabro-cli/tests/it/cmd/server_stop.rs`
- `lib/crates/fabro-cli/tests/it/cmd/server_status.rs`
- `lib/crates/fabro-cli/tests/it/cmd/preflight.rs`
- `lib/crates/fabro-cli/tests/it/cmd/fabro.rs`

Behavior scenarios to add or preserve:
- `exec` still routes through the server only when its command-local `server_url` is set
- `model` still honors configured `[server].base_url` when no explicit `storage_dir` is set
- `model` command-local `server_url` still overrides configured base URL
- `model` command-local `storage_dir` still forces local behavior
- old top-level target-flag placement fails at parse time

For snapshot churn:
- use the repo workflow from `CLAUDE.md`
  - `cargo insta pending-snapshots`
  - verify the expected help/output changes
  - `cargo insta accept`

## Dependencies And Sequencing
Apply the change in this order:

1. add the new command-scoped target arg structs in `args.rs`, but keep `storage_dir` and `server_url` on `GlobalArgs` temporarily so the crate still compiles during migration
2. add the new explicit helpers in `user_config.rs` alongside the old global-based helpers
3. repoint `exec` and `model` to the new target arg structs and helpers
4. repoint the local-storage and server command modules, plus test harness/env wiring, off `GlobalArgs`
5. remove `storage_dir` and `server_url` from `GlobalArgs`, then delete the old global-based helper path once it is unused
6. update parser tests for the final clap shape, then update docs and help snapshots

This ordering keeps the refactor compile-safe: helper and command migration happen before the old global fields are removed, and parser assertions about the old top-level syntax move to the final cleanup step where they become true.

## Test Plan
- targeted parser/unit tests:
  - `cargo test -p fabro-cli --lib main::tests -- --nocapture`
  - `cargo test -p fabro-cli --lib user_config::tests -- --nocapture`
- targeted CLI integration tests:
  - `cargo nextest run -p fabro-cli cmd::exec:: --no-fail-fast`
  - `cargo nextest run -p fabro-cli cmd::model:: cmd::model_list:: cmd::model_test:: cmd::config:: --no-fail-fast`
  - `cargo nextest run -p fabro-cli cmd::run:: cmd::create:: cmd::attach:: cmd::logs:: cmd::resume:: cmd::rewind:: cmd::wait:: cmd::diff:: cmd::runner:: --no-fail-fast`
  - `cargo nextest run -p fabro-cli cmd::server_start:: cmd::server_stop:: cmd::server_status:: --no-fail-fast`
- broader CLI sweep after targeted coverage is green:
  - `cargo nextest run -p fabro-cli --no-fail-fast`
- final verification:
  - `cargo fmt --check --all`
  - `cargo clippy --workspace --all-targets -- -D warnings`

## Assumptions And Defaults
- Pre-production status means removing the old global target-flag placement is acceptable; no compatibility shim is required.
- `FABRO_STORAGE_DIR` and `FABRO_SERVER_URL` remain useful and should stay, but only where the corresponding command actually supports the underlying behavior.
- The recent `model` and `exec` behavioral contracts are already correct; this pass is about CLI honesty and arg ownership, not product redefinition.
- If we later want a broader “remote-capable command matrix” abstraction, it should be built on top of these command-local args rather than by reintroducing misleading global target flags.
