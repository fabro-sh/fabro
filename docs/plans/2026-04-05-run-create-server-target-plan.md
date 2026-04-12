# Run/Create Server Target Support

## Summary
Make `fabro run` and `fabro create` targetable via `--server` / `[server].target` now that run submission is manifest-based and server-owned.

This pass should:

- add `--server` support to `fabro run` and `fabro create`
- align their target-resolution semantics with `preflight`, `validate`, and `graph`
- remove the last local-storage-only assumptions from the run submission path
- keep local run behavior unchanged when a local server is selected

This is primarily cleanup/compaction, not a new subsystem. The manifest refactor already made remote submission possible; the CLI surface just has not caught up yet.

## Scope Boundaries
In scope:
- `fabro run`
- `fabro create`
- the internal start/attach/summary helpers required for `fabro run` to work against an explicit server target
- docs/help/tests for the new targeting contract

Out of scope:
- adding `--server` to top-level `fabro start`, `fabro attach`, `fabro wait`, `fabro logs`, `fabro inspect`, `fabro diff`, `fabro resume`, or `fabro rewind`
- changing the HTTP API
- changing manifest structure or workflow bundle persistence
- changing server-side run ownership or execution topology

Accepted temporary asymmetry:
- `fabro create --server ...` will be supported in this pass even though the standalone follow-up lifecycle commands remain local-only.
- That is acceptable because `create` already prints only a run ID and is useful for automation. A later pass can broaden remote targeting across the rest of the run lifecycle surface.

## Problem Frame
The CLI/server boundary is now inconsistent:

- `preflight`, `validate`, and `graph` already build manifests and target either a local auto-started server or an explicit remote `--server`
- `run` and `create` already build manifests, but they still only flatten `--storage-dir` and then hard-wire submission to `connect_server(settings.storage_dir())`
- `run` still assumes every submitted run has a meaningful local run directory for attach and final summary output

That is architectural drift. The system is already manifest-first and server-canonical. `run` and `create` are the remaining commands that still behave as if run submission is inherently local.

## Key Decisions
- `RunArgs` should flatten `ServerConnectionArgs`, not `StorageDirArgs`.
  - `fabro create` inherits the same args because it already reuses `RunArgs`.
- `run` and `create` should use the same connection contract as other server-backed commands:
  - explicit `--server` wins
  - explicit `--storage-dir` selects a local server and suppresses configured `[server].target`
  - otherwise configured `[server].target` may be used
  - otherwise the command defaults to the local server for the resolved storage dir
- `fabro run` should use one resolved server connection end-to-end for:
  - manifest submission
  - `POST /runs/{id}/start`
  - live attach / polling
- `fabro create` should remain run-ID-only output.
  - It should not pretend there is always a local `run_dir`.
- foreground `fabro run` against a remote/configured server should attach successfully.
  - This requires decoupling attach from local run-dir inference.
- remote foreground `run` should print a server-backed final summary that omits local-only fields.
  - Keep: run ID, status, duration, cost/tokens, failure reason, PR URL, final output
  - Omit: local run directory path and local artifact listing when there is no local run dir
- local `run` behavior should remain unchanged.
  - If the resolved connection is local, keep the existing local run-dir summary and asset listing behavior.
- No OpenAPI change is required.
  - This is CLI cleanup on top of the existing manifest-backed `POST /runs`.

## Implementation Changes

### 1. Add target args to `run` / `create`
In `lib/crates/fabro-cli/src/args.rs`:
- change `RunArgs` to flatten `ServerConnectionArgs`
- remove the dedicated `StorageDirArgs` field from `RunArgs`
- keep all existing workflow/run override flags unchanged

This updates both:
- `fabro run`
- `fabro create`

Help/CLI contract to lock down:
- `fabro run foo.fabro --server http://127.0.0.1:3000/api/v1`
- `fabro create foo.fabro --server /var/run/fabro.sock`
- `fabro run foo.fabro --storage-dir /tmp/fabro`
- `fabro create foo.fabro` still defaults to local storage unless `[server].target` is configured

### 2. Resolve run/create connections the same way as other server-backed commands
In `lib/crates/fabro-cli/src/commands/run/command.rs` and `lib/crates/fabro-cli/src/commands/run/create.rs`:
- keep using local user-config resolution for manifest defaults
  - load settings with storage-dir override only, using the command-local `storage_dir` value if present
- stop deriving the submission client from `settings.storage_dir()`
- instead resolve the server connection with the existing server-backed connection logic in `lib/crates/fabro-cli/src/user_config.rs`
- connect using the resolved connection, not a hard-coded local store path

Recommended shape:
- let `create_run(...)` return a richer value than `(RunId, PathBuf)`, for example:
  - `CreatedRun { run_id, local_run_dir: Option<PathBuf>, connection: ServerConnection }`

Rationale:
- `command::execute()` needs more than a run ID now
- remote runs have no trustworthy local run dir
- passing the resolved connection forward keeps the rest of the flow honest

In `lib/crates/fabro-cli/src/server_client.rs`:
- add a small helper that returns a `ServerStoreClient` from a resolved `ServerConnection`
- reuse the existing resolved API-client path rather than introducing parallel target parsing

### 3. Refactor `run` to start and attach through the resolved server connection
In `lib/crates/fabro-cli/src/commands/run/start.rs`:
- keep the current public/local helper for top-level `fabro start`
- add a connection-agnostic helper that can start a run from an already-connected `ServerStoreClient`

In `lib/crates/fabro-cli/src/commands/run/attach.rs`:
- preserve the existing top-level `attach_run(...)` entrypoint for local-storage workflows
- extract the existing server-backed attach logic into a helper that accepts:
  - `&ServerStoreClient`
  - `&RunId`
  - `Option<&Path>` for a local run dir
  - existing `kill_on_detach`, `styles`, and `json_output` flags
- make the current top-level local path delegate to that extracted helper after doing its storage-dir/run-id inference

In `lib/crates/fabro-cli/src/commands/run/command.rs`:
- for `fabro run`, use the resolved connection returned by `create_run(...)`
- if `--detach` is set:
  - print the run ID and exit exactly as today
- otherwise:
  - start via the resolved server client
  - attach via the extracted direct-client attach helper
  - print the final run summary using the same resolved connection

This keeps `fabro run` coherent for both:
- local auto-started server flows
- explicit/configured remote server flows

### 4. Decouple final summary rendering from local run-dir assumptions
In `lib/crates/fabro-cli/src/commands/run/output.rs`:
- split summary fetching from summary rendering
- make the renderer accept:
  - server-backed run state / conclusion / checkpoint
  - `Option<&Path>` for a local run dir

Concrete behavior:
- when `local_run_dir` is present:
  - keep printing the local run path
  - keep printing local artifact listings
- when `local_run_dir` is absent:
  - do not print a local run path line
  - do not attempt local artifact discovery
  - still print the rest of the run conclusion and final output

This is the smallest cleanup that makes remote foreground `run` feel intentional without broadening the whole remote lifecycle command surface.

### 5. Keep standalone follow-up lifecycle commands local for now
Do **not** add `--server` to these commands in this pass:
- `fabro start`
- `fabro attach`
- `fabro wait`
- `fabro logs`
- `fabro inspect`
- `fabro diff`
- `fabro resume`
- `fabro rewind`

Rationale:
- they form a larger remote lifecycle surface with selector semantics, replay UX, and local-path assumptions of their own
- broadening them now would turn a cleanup pass into a larger capability expansion

But the plan should call the temporary boundary out explicitly in docs/help text where useful:
- `fabro create --server ...` is valid, but follow-up manipulation of that run outside `fabro run` remains a later pass

### 6. Update docs and help text
Update the user-facing references that describe server targeting:
- `docs/reference/cli.mdx`
- `docs/reference/user-configuration.mdx`
- `docs/administration/deploy-server.mdx`

The docs should explicitly say:
- `fabro run` and `fabro create` now honor `--server` / `[server].target`
- `fabro exec` still requires explicit `--server`
- top-level run lifecycle follow-up commands are still local-storage commands in this pass

## Test Plan

### CLI help / parser surface
Update snapshots in:
- `lib/crates/fabro-cli/tests/it/cmd/run.rs`
- `lib/crates/fabro-cli/tests/it/cmd/create.rs`

Scenarios:
- `run --help` shows both `--storage-dir` and `--server`
- `create --help` shows both `--storage-dir` and `--server`

### `create` targeting behavior
In `lib/crates/fabro-cli/tests/it/cmd/create.rs`:
- `create --server <http-target>` submits to the explicit server and prints the created run ID
- configured `[server].target` reroutes `create` when no explicit target args are passed
- explicit `--storage-dir` suppresses configured `[server].target`
- explicit `--server` overrides configured `[server].target`
- remote-targeted `create` does not require local run-dir inspection to succeed

### `run` targeting behavior
In `lib/crates/fabro-cli/tests/it/cmd/run.rs`:
- `run --server <http-target> --detach ...` submits and prints a run ID without relying on a local run dir
- foreground `run --server <http-target> ...` creates, starts, attaches, and exits successfully
- configured `[server].target` reroutes `run` when no explicit target args are passed
- explicit `--storage-dir` suppresses configured `[server].target`
- explicit `--server` overrides configured `[server].target`
- remote foreground `run` prints a final summary without a local run-directory line
- local `run --storage-dir ...` still prints the local run-directory line and local artifact section exactly as today

### Test infrastructure
Prefer a real TCP-bound fabro test server over `httpmock` for `run`.

Reason:
- `run` needs multiple real endpoints (`POST /runs`, `POST /runs/{id}/start`, event replay, run-state polling, question polling)
- mocking all of that would verify request wiring but not the actual remote run lifecycle

If the current CLI integration helpers do not already provide this, add a small reusable helper in:
- `lib/crates/fabro-cli/tests/it/support.rs`
or
- `lib/crates/fabro-test/src/lib.rs`

That helper should:
- launch a real fabro server bound to loopback TCP
- return a usable `http://127.0.0.1:PORT/api/v1` target string
- keep fixture storage isolated from the invoking CLI’s local storage dir

## Risks
- The biggest risk is hidden local-run-dir assumptions in attach/summary code.
  - Mitigation: refactor those surfaces explicitly rather than trying to fake a local path for remote runs.
- Config-target defaulting could surprise users if docs are not updated.
  - Mitigation: update CLI docs and help snapshots in the same pass.
- Supporting `create --server` before the broader remote lifecycle commands is intentionally asymmetric.
  - Mitigation: call it out in the plan/docs instead of pretending the whole run lifecycle is remote-ready.

## Follow-on
After this lands, the next logical cleanup is a dedicated remote run-lifecycle plan for:
- `start`
- `attach`
- `wait`
- `logs`
- `inspect`
- `diff`
- `resume`
- `rewind`

That should be a separate pass, not folded into this one.
