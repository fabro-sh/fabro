# Settings Command Server/Local Merge Plan

## Summary
Refactor `fabro settings` so it answers “what settings will actually be used?” instead of only dumping locally merged config.

The command contract becomes:

- `fabro settings`
  - show the effective merged settings for the selected server target plus local config
- `fabro settings --local`
  - show only locally resolved settings, with no server call
- `fabro settings WORKFLOW`
  - show the effective merged settings for that workflow after server defaults are applied
- `fabro settings --local WORKFLOW`
  - show the local-only merged settings for that workflow, with no server call

This plan is intentionally separate from the broader config/socket/storage cleanup. It can land independently as long as the command reuses the current shared target-resolution behavior that exists at implementation time.

## Problem Frame
The current `fabro settings` command is purely local. It resolves project/workflow config plus local machine settings and prints the result in YAML or JSON.

That is no longer the most useful answer for users, because workflows run on a server and the final settings are not purely local. The server already applies its own defaults and runtime adjustments during manifest preparation, so the current output is only a partial picture.

The goal of this pass is to make `fabro settings` answer two distinct questions clearly:

- what would the CLI resolve locally without talking to a server?
- what settings will actually be used when this workflow runs on the selected server?

## Key Decisions
- `fabro settings` becomes server-targeted by default.
  - It should fetch server settings and merge them with local config using the same merge logic the server uses for real runs.
- `fabro settings --local` skips the server entirely and preserves the current local inspection behavior.
- `fabro settings` keeps the optional positional `WORKFLOW`.
  - With no workflow argument, it shows the effective baseline settings for the current repo/config context.
  - With a workflow argument, it shows the effective run settings for that workflow.
- `fabro settings` should support `--server` as the normal one-off server-target override.
- `fabro settings --local` conflicts with `--server`.
- `fabro settings` should no longer take `--storage-dir`.
  - Storage targeting is not the purpose of this command.
- Output format stays the same:
  - default: YAML `Settings`
  - `--json`: JSON `Settings`
- The command should return only the resolved `Settings` object, not extra metadata fields.
  - Effective target/config-path diagnostics belong in `doctor` or other output, not in the `settings` payload itself.
- Bare `fabro settings` should use the same server-target resolution behavior as other server-targeted commands at the time this plan lands.
  - If the resolved target is a Unix socket, it may use the normal socket/autostart path.
  - If the resolved target is an HTTP target and the server is unreachable, the command should fail clearly.
  - It should not silently fall back to `--local`.
- The `/api/v1/settings` endpoint should return the full effective runtime `Settings` object from the server.
  - The shared merge helper remains responsible for precedence and for preserving the existing distinction between full server defaults and local-daemon-only overrides.

## Implementation Changes
### 1. CLI surface
- Change `SettingsArgs` in [`lib/crates/fabro-cli/src/args.rs`](/Users/bhelmkamp/p/fabro-sh/fabro/lib/crates/fabro-cli/src/args.rs) to:
  - add `ServerTargetArgs`
  - add `--local`
  - keep optional `WORKFLOW`
  - remove `StorageDirArgs`
- Update help text and parser tests in [`lib/crates/fabro-cli/tests/it/cmd/config.rs`](/Users/bhelmkamp/p/fabro-sh/fabro/lib/crates/fabro-cli/tests/it/cmd/config.rs) accordingly.

### 2. Shared merge logic
- Extract the server/default merge logic currently embedded in [`lib/crates/fabro-server/src/run_manifest.rs`](/Users/bhelmkamp/p/fabro-sh/fabro/lib/crates/fabro-server/src/run_manifest.rs) into a shared helper in `fabro-config` that both the server and CLI can call.
- The helper should live in `fabro-config` because both `fabro-cli` and `fabro-server` already depend on it, while `fabro-cli` should not depend on `fabro-server` merge internals.
- The shared helper should accept:
  - the local config layers already resolved by the CLI side
  - server settings fetched from the target server
  - a mode flag matching current manifest preparation semantics
- The helper must preserve the current distinction between:
  - normal remote/server merge behavior
  - local-daemon merge behavior
- The helper must make the precedence explicit:
  - `fabro settings`: `project + user + server_defaults`
  - `fabro settings WORKFLOW`: `workflow + project + user + server_defaults`
  - `fabro settings --local`: `project + user`
  - `fabro settings --local WORKFLOW`: `workflow + project + user`
- In server-targeted mode, the helper must apply `strip_server_owned_fields` to local layers (workflow, project, user) before combining with server defaults, matching `prepare_manifest_with_mode` semantics. Without this, fields like `exec` or `server` set in a local workflow config would override the server's values, which doesn't match actual run behavior.
- The helper should continue to apply the same server-side rules that exist today:
  - normal mode uses the full `server_defaults_layer()`
  - local-daemon mode uses `local_daemon_server_overrides_layer()`
  - any required post-resolution overrides, such as forcing `storage_dir` from the active server settings, remain in the shared helper rather than being reimplemented by the CLI
- The CLI determines the mode from the resolved `ServerTarget` type: `ServerTarget::UnixSocket` → local-daemon mode, `ServerTarget::HttpUrl` → remote mode.
- The shared helper's scope is layer combination + resolution + post-resolution overrides. `prepare_manifest_with_mode` continues to handle manifest-specific work (parsing workflow bundles, building `args_layer` from `ManifestArgs`, applying `manifest.goal`) and delegates to the shared helper for the combine/resolve step.
- The server should be switched to use the shared helper so `fabro settings` cannot drift from actual run semantics.

### 3. Server settings source
- Implement the real `/api/v1/settings` route in [`lib/crates/fabro-server/src/server.rs`](/Users/bhelmkamp/p/fabro-sh/fabro/lib/crates/fabro-server/src/server.rs), matching the existing OpenAPI contract in [`docs/api-reference/fabro-api.yaml`](/Users/bhelmkamp/p/fabro-sh/fabro/docs/api-reference/fabro-api.yaml).
- The route should return the server’s current effective runtime settings from `AppState`, not just a raw disk parse.
- The route should return the full runtime `Settings` object, not a reduced server-owned subset.
- The CLI settings command should call this route when `--local` is not set.

### 4. Command behavior
- In [`lib/crates/fabro-cli/src/commands/config/mod.rs`](/Users/bhelmkamp/p/fabro-sh/fabro/lib/crates/fabro-cli/src/commands/config/mod.rs):
  - preserve the existing local merge path for `--local`
  - add a server-targeted path for the default behavior
- Local-only resolution should keep the current semantics:
  - no workflow: project config from cwd + local settings
  - workflow: workflow config + project config + local settings
- Server-targeted resolution should:
  - resolve the server target using the normal shared target-resolution helpers
  - fetch server settings from `/api/v1/settings`
  - build the same local layers the command already knows how to build
  - combine them with fetched server settings using the shared merge helper
- If the server is unreachable:
  - Unix socket targets should follow the same socket/autostart behavior normal server-targeted commands use at the time this plan lands
  - HTTP targets should fail clearly
  - the command should not fall back to local-only mode unless the user explicitly asked for `--local`
- For `WORKFLOW`, the command should not call `preflight` or create a run.
  - It should compute and print the resolved settings only.

### 5. Separation from broader targeting cleanup
- This plan should not block on the larger config/socket/storage refactor.
- If the broader refactor lands first, `fabro settings` should reuse the new target-resolution helpers.
- If it lands first, `fabro settings` should reuse the current `--server` / configured server-target behavior and keep its implementation scoped to this command.
- This plan does not change `exec`, `system df`, `system prune`, or `store dump`.

## Test Plan
- CLI parser/help coverage in [`lib/crates/fabro-cli/tests/it/cmd/config.rs`](/Users/bhelmkamp/p/fabro-sh/fabro/lib/crates/fabro-cli/tests/it/cmd/config.rs):
  - `fabro settings --help` shows `--local` and `--server`
  - `fabro settings --local --server ...` is rejected
  - `--storage-dir` is no longer accepted
- Local behavior tests:
  - `fabro settings --local` preserves current merged local output
  - `fabro settings --local WORKFLOW` resolves workflow + project + local settings only
- Server-targeted behavior tests:
  - `fabro settings` fetches server settings and merges them with local config
  - `fabro settings WORKFLOW` merges workflow + project + local + server settings
  - `fabro settings --server http://...` uses the explicit target
  - socket-targeted settings resolution behaves the same way normal commands do at the time this plan lands
  - unreachable HTTP targets fail clearly and do not fall back to local-only output
- Shared merge helper tests:
  - `prepare_manifest_with_mode()` delegates to the shared helper for the merge/defaults path
  - helper tests directly cover the layer precedence for:
    - `project + user`
    - `workflow + project + user`
    - `project + user + server_defaults`
    - `workflow + project + user + server_defaults`
  - local-daemon merge semantics remain distinct from remote-server merge semantics where that distinction already exists
  - server-owned fields in local layers (workflow, project, user) are stripped before merging in server-targeted mode, matching `prepare_manifest_with_mode` behavior
- Server route tests:
  - real `/api/v1/settings` returns the structured settings shape from the OpenAPI contract
  - route reflects effective runtime settings, including active storage-dir/runtime overrides

## Assumptions
- Returning the raw `Settings` payload is sufficient; no new wrapper response is needed for the CLI command.
- Reusing the existing `/api/v1/settings` contract is preferable to adding a second settings-resolution endpoint in this pass.
- `fabro settings WORKFLOW` may perform local workflow/project discovery exactly as the command does today; the only new remote input is the selected server’s settings.
- This pass is command-focused and does not attempt to solve all settings introspection use cases elsewhere in the API.
