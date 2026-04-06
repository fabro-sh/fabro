# CLI Config, Socket, And Storage Separation

## Summary
Separate machine config, server target, and server storage so the CLI no longer conflates:

- config path: `~/.fabro/settings.toml`
- default socket target: `~/.fabro/fabro.sock`
- default storage dir: `~/.fabro/storage`

Normal user-facing commands should always talk to a server target. A Unix socket target may auto-start the daemon. An HTTP target may not. Serverless/direct-storage command behavior should be removed from normal commands in this pass.

## Scope Boundaries
In scope:
- add `FABRO_CONFIG` support for machine settings loading
- default the server target to `~/.fabro/fabro.sock`
- default server storage to `~/.fabro/storage`
- decouple socket-path resolution from storage-dir resolution
- keep daemon auto-start only for Unix socket targets
- remove normal-command fallback to direct storage-based targeting
- keep `fabro server *`, hidden `fabro __runner`, and `fabro install` as storage-owning commands
- keep user-facing `--server` on server-targeted commands
- remove user-facing `--storage-dir` from server-targeted commands

Out of scope:
- `fabro exec`
- `fabro system df`
- `fabro system prune`
- `fabro store dump`
- new remote maintenance endpoints for deferred commands

## Problem Frame
The current CLI still mixes together two different ideas:

- a local daemon reached over a Unix socket
- serverless behavior where the CLI uses `storage_dir` as the command target

That has produced the wrong defaults and the wrong abstractions:

- the socket path is currently derived from `storage_dir`
- many commands still model targeting as "server or storage dir"
- normal commands can still fall back to a storage-driven local connection shape
- autostart is keyed off the local/storage connection path instead of the actual server target type

The intended model is simpler:

- normal commands always resolve a server target
- the default server target is a Unix socket in `~/.fabro`
- Unix socket targets may auto-start a daemon
- HTTP targets may not auto-start a daemon
- storage is server-owned runtime state, not the primary targeting mechanism for normal commands

## Key Decisions
- `FABRO_CONFIG` selects the active machine settings file.
  - For server lifecycle commands and daemon auto-start, precedence is: explicit `--config` where supported, then `FABRO_CONFIG`, then `~/.fabro/settings.toml`.
  - Normal user-facing commands do not gain a new `--config` flag in this pass; they resolve settings from `FABRO_CONFIG` or the default path.
- `FABRO_SERVER` selects the effective server target for normal commands.
  - It accepts either an absolute Unix socket path or an `http(s)` URL.
  - If unset, use `settings.server.target`.
  - If that is unset, default to `~/.fabro/fabro.sock`.
- Server-targeted commands keep `--server` as the standard one-off override.
  - `FABRO_SERVER` remains the env-var equivalent.
- `FABRO_STORAGE_DIR` is no longer a normal command-targeting mechanism.
  - It remains an override for storage-owning commands only.
  - Precedence for storage-owning commands: explicit `--storage-dir` where still supported, then `FABRO_STORAGE_DIR`, then `settings.storage_dir`, then `~/.fabro/storage`.
- `settings.server.target` remains the durable place to configure the machine's server target.
- `settings.storage_dir` remains the durable place to configure where the local server stores data.
- `fabro server start` keeps `--config` and `--bind`.
  - Its default bind is `~/.fabro/fabro.sock`, not `<storage_dir>/fabro.sock`.
- `fabro server stop` and `fabro server status` remain storage-owning commands and continue to resolve the local server instance from storage-owned records.
- `fabro settings` is a local config-inspection command, not a server-targeted command.
  - It keeps its current local settings-resolution behavior in this pass.
- `fabro exec` is unchanged in this pass.

## Command Classification
### Storage-owning commands
These commands continue to resolve and use local storage directly:

- `fabro server start`
- `fabro server stop`
- `fabro server status`
- hidden `fabro server __serve`
- hidden `fabro run __runner`
- `fabro install`

### Local config-inspection commands
These commands stay outside the server-targeting cleanup in this pass:

- `fabro settings`

### Server-targeted commands
These commands should resolve a `ServerTarget` only and should not use storage-dir fallback semantics:

- `fabro run`
- `fabro create`
- `fabro preflight`
- `fabro validate`
- `fabro graph`
- `fabro model list`
- `fabro model test`
- `fabro doctor`
- `fabro repo init`
- `fabro provider login`
- `fabro secret list`
- `fabro secret rm`
- `fabro secret set`
- `fabro ps`
- `fabro rm`
- `fabro inspect`
- `fabro run start`
- `fabro run attach`
- `fabro run logs`
- `fabro run resume`
- `fabro run rewind`
- `fabro run fork`
- `fabro run wait`
- hidden `fabro run diff`
- `fabro artifact list`
- `fabro artifact cp`
- `fabro sandbox cp`
- `fabro sandbox preview`
- `fabro sandbox ssh`
- `fabro pr create`
- `fabro pr list`
- `fabro pr view`
- `fabro pr merge`
- `fabro pr close`

### Deferred local-maintenance commands
These remain unchanged in this pass:

- `fabro system df`
- `fabro system prune`
- `fabro store dump`

These deferred commands continue to use `StorageDirArgs` and the existing hybrid `ServerRunLookup::connect(storage_dir)` path in this pass, and should keep working against the new default storage dir without being reclassified as server-targeted commands yet.

## Implementation Changes
### 1. Shared settings and path resolution
- Add a shared helper for the active settings path used by CLI and server code.
  - This helper must honor `FABRO_CONFIG`.
- Add a shared helper for the default socket path: `~/.fabro/fabro.sock`.
- Change the default storage dir helper to return `~/.fabro/storage`.
- Stop deriving the socket path from `storage_dir`.

### 2. Target resolution model
- Refactor CLI target resolution so normal commands resolve a `ServerTarget`, not a "local vs target" union.
- Remove `ServerConnection::Local` as a normal command-routing concept.
- Split helpers into two categories:
  - server-target resolution for normal commands
  - storage-dir resolution for storage-owning commands
- Keep TLS handling attached to `HttpUrl` targets exactly as today.

### 3. Connection and auto-start behavior
- Update the server client helpers so they accept or derive a `ServerTarget`.
- If the resolved target is `UnixSocket(path)`:
  - attempt to connect to that socket
  - if unavailable, auto-start the daemon
  - auto-start the daemon bound to that exact socket path
- If the resolved target is `HttpUrl(url)`:
  - attempt to connect once
  - if unavailable, fail with a clean reachability error
  - do not auto-start anything
- Auto-start must pass the active config path through to the spawned server with `--config`.
- The auto-start helper should take the resolved active config path, resolved Unix socket path, and resolved local storage dir as explicit inputs.
  - It should not rediscover config via environment variables or reload settings internally during daemon launch.
- Auto-start may also pass the resolved storage dir for the local server process, but only as runtime/server lifecycle plumbing, not as the command target abstraction.
- Any active-server record lookup used by `server stop`, `server status`, `install`, or daemon auto-start should temporarily fall back to the legacy implicit storage root `~/.fabro` when no explicit storage location is provided and no record exists under the new default `~/.fabro/storage`.
  - This is only to find already-running daemons started before the default-storage change.

### 4. CLI arg surface cleanup
- Keep user-facing `--server` on all server-targeted commands listed above.
- Remove user-facing `--storage-dir` from all server-targeted commands listed above.
- Remove the `storage_dir_explicit` conflict plumbing for those commands.
- Remove the custom `--server` / `--storage-dir` conflict detection in `main.rs` once no supported command still accepts both flags together.
- Keep `--storage-dir` only on storage-owning commands in this pass.
- Keep `--config` only where already appropriate for server lifecycle.
- Update help text and parser tests so normal commands still advertise `--server` but no longer imply that storage-dir is a general targeting control.

### 5. Run/create local run-dir handling
- `run` / `create` currently thread a synthesized local run dir through the result object to print asset paths after completion.
- Preserve that behavior only when the effective target is the machine's local Unix socket and the effective local storage dir is known.
- For HTTP targets, do not synthesize a local run dir.
- This should be derived from "effective target is local socket" plus resolved storage dir, not from a `ServerConnection::Local` enum variant.

### 6. Docs and user-visible messaging
- Rewrite docs and examples so:
  - normal commands use config-driven target resolution by default
  - temporary overrides use `FABRO_SERVER=... fabro ...`
  - config-file overrides use `FABRO_CONFIG=... fabro ...`
- Update language to avoid "local mode" as a user-facing concept.
  - Use "Unix socket target" or "HTTP target".
  - Use "serverless" only for the behavior being removed.
- Make `doctor` and `settings` print:
  - active config path
  - effective server target
  - effective storage dir
  - any active env-var overrides

## Test Plan
- Unit tests for path and precedence helpers:
  - active config path honors `FABRO_CONFIG`
  - default socket path is `~/.fabro/fabro.sock`
  - default storage dir is `~/.fabro/storage`
  - `FABRO_SERVER` overrides `settings.server.target`
  - `FABRO_STORAGE_DIR` overrides `settings.storage_dir` for storage-owning commands only
  - `FABRO_CONFIG=/custom/path/settings.toml` loads that file's contents and the resolved `server.target` / `storage_dir` from that file actually take effect
- Unit tests for target resolution:
  - normal commands default to a Unix socket target when nothing is configured
  - normal commands no longer resolve a storage-dir fallback connection
  - HTTP targets never route into autostart code
- Integration tests for daemon behavior:
  - `fabro server start` defaults to binding `~/.fabro/fabro.sock`
  - auto-start for socket-targeted commands binds the requested socket, not `<storage_dir>/fabro.sock`
  - auto-start passes the active config path through to the daemon
  - HTTP-targeted commands fail cleanly when unreachable
  - `server stop` / `server status` still find an already-running daemon whose record lives under the legacy implicit storage root
- Parser/help tests:
  - server-targeted commands still accept `--server`
  - server-targeted commands no longer accept `--storage-dir`
  - storage-owning commands still accept `--storage-dir` where intended
  - `fabro settings` keeps its existing local settings override surface in this pass
- Workflow behavior tests:
  - `run`, `create`, `model`, `doctor`, `ps`, and `secret` work via the default socket target
  - local Unix socket runs still print local asset/run-dir info when appropriate
  - HTTP-targeted runs do not print synthesized local run-dir paths

## Assumptions
- This is a hard cut for CLI targeting semantics. No deprecation period for removed normal-command flags.
- `FABRO_SERVER` is the single override for user-facing command targeting; no separate `FABRO_SOCKET` env var is added.
- `server.target` remains the canonical durable target field in `settings.toml`.
- Deferred commands will be handled in a later pass rather than forced into this refactor.
