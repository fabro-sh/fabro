# Run-Adjacent Server-Only Cleanup

## Summary

Make the remaining user-facing run-adjacent commands server-only:

- `fabro resume`
- `fabro diff`
- `fabro fork`
- `fabro rewind`
- `fabro artifact list`
- `fabro artifact cp`
- `fabro pr create|list|view|merge|close`
- `fabro sandbox preview`
- `fabro sandbox ssh`
- `fabro sandbox cp`

After this pass, those commands should no longer resolve runs through local `runs/` directories or flatten `--storage-dir`. They should all target a server the same way the core lifecycle commands already do:

1. explicit `--server` / `FABRO_SERVER`
2. configured `[server].target`
3. default local server instance, auto-started via the default local storage dir

There is intentionally no separate “force local” override for these commands in this pass. If `[server].target` is configured, that target wins unless the user passes an explicit `--server` value pointing at a local Unix socket. This is a simplicity tradeoff, not an accidental regression.

This is cleanup/compaction, not a new subsystem. The goal is to finish the architectural move already made for `run`, `create`, `start`, `attach`, `logs`, `wait`, `ps`, `inspect`, and `rm`.

## Scope Boundaries

In scope:
- the commands listed above
- shared run-resolution helpers in `fabro-cli`
- missing thin server APIs needed to make sandbox-oriented commands truly server-only
- CLI help/docs/snapshots for the new contract

Out of scope:
- `fabro store dump`
- `fabro system df`
- `fabro system prune`
- hidden/internal commands like `__runner`
- changing run execution topology
- changing checkpoint format or git metadata format
- changing GitHub auth/secrets flows

## Problem Frame

The run lifecycle is only partially simplified today.

Core lifecycle commands are already server-only, but the rest of the run-adjacent surface still falls back to local path-based lookup through `ServerRunLookup` in [server_runs.rs](lib/crates/fabro-cli/src/server_runs.rs). That creates three kinds of drift:

- some commands still expose `--storage-dir` even though the real abstraction is now a server target
- some commands read local run files even when equivalent data already exists in server state/store
- some commands still reconnect directly from the CLI to sandboxes, which breaks the “server-owned runs” abstraction for remote targets

That is unnecessary complexity in a greenfield app with no production compatibility constraints.

## Key Decisions

- All in-scope commands become server-only.
  - They flatten `ServerTargetArgs`, not `StorageDirArgs`.
  - `--storage-dir` is removed from their public CLI surface.
  - `FABRO_STORAGE_DIR` is not part of the public contract for these commands.
  - If `[server].target` is configured, these commands use it unless an explicit `--server` is passed.
  - There is no separate “force local default instance” flag.

- `ServerRunLookup` becomes local/admin-only.
  - Keep it only for genuinely local maintenance commands like `store dump`, `system df`, and `system prune`.
  - User-facing run commands should resolve selectors through `ServerSummaryLookup` plus server state/events.

- `fabro diff` is simplified to stored server-backed output only.
  - Drop the live sandbox reconnect fallback.
  - Drop `--stat` and `--shortstat`.
  - Output comes only from stored diff data already present in run state (`final_patch` and per-node `diff`).
  - If no stored diff exists, return a clear error.

- `fabro fork` and `fabro rewind` remain local git operations, but not local run-store operations.
  - Run selection and run event/state loading come from the target server.
  - Repo mutation still happens against the caller’s local checkout, which is the correct boundary.
  - Before mutating the repo, the CLI should compare the current checkout against a durable stored repo identity and fail fast on obvious mismatch.

- `fabro pr *` stays CLI-owned for GitHub network calls, but server-owned for run lookup and pull-request record loading.
  - This keeps the current GitHub App model intact while removing local run-dir dependence.
  - `pr create` should use the same repo-mismatch guard as `fork` and `rewind` before operating on the caller’s checkout.

- `fabro artifact *` should reuse the existing stage artifact API instead of walking local `artifacts/` directories.
  - The CLI can derive relevant stage IDs from `RunProjection.nodes` and call the existing stage artifact list/download endpoints.

- `fabro sandbox preview`, `fabro sandbox ssh`, and `fabro sandbox cp` should stop reconnecting to sandboxes directly from the CLI.
  - The server owns the sandbox record and should own the reconnect.
  - The CLI becomes a thin consumer of server responses.

- The server API should stay thin and capability-shaped.
  - Reuse existing APIs where they already exist.
  - Add only the missing endpoints required for truly server-only sandbox operations.

- Backward compatibility is not a goal.
  - Remove obsolete flags and behavior instead of shimming them.
  - Prefer deleting speculative unused surface over preserving it.

## Sequencing

This plan should land in two phases.

Phase 1: pure CLI/server-state cleanup using APIs that already exist

- `resume`
- `fork`
- `rewind`
- `pr *`
- `artifact list`
- `artifact cp` (the stage artifact list/download routes already exist and are implemented)
- `diff`
- Phase 1 OpenAPI cleanup for `diff`:
  - delete `/api/v1/runs/{id}/files`
- shared `ServerSummaryLookup` / `ServerRunLookup` narrowing
- help/docs/snapshot churn for those commands

Phase 2: thin server API completion for sandbox-owned commands

- `sandbox preview`
- `sandbox ssh`
- `sandbox cp`

This keeps the simpler repoints from being blocked on the few missing server capability routes.

The final `ServerRunLookup` narrowing should happen only after the last in-scope command that still imports it has been migrated. Do not try to enforce an admin-only boundary halfway through the sequence while migrated and unmigrated command families still coexist.

## Implementation Changes

### 0. Add a durable repo identity field for local-repo operations

The current `RunRecord.host_repo_path` is a filesystem path, not a durable cross-machine identity. It should not be used for a server-targeted repo-mismatch guard.

Add a new persisted field to `RunRecord`, for example:

- `repo_origin_url: Option<String>`

Recommended semantics:

- source it from `RunManifest.git.origin_url`, which is already sanitized before it reaches the server
- persist it in the run record when a manifest-sourced run is created
- treat it as the canonical repo-identity signal for CLI commands that mutate or inspect the caller’s local checkout on behalf of a server-selected run

Normalization rules should be explicit and shared:

- strip embedded credentials
- normalize GitHub-style SSH URLs to HTTPS form
- trim a trailing `.git`
- trim trailing `/`

Guard behavior:

- `fork`, `rewind`, and `pr create` detect the current checkout’s origin URL locally
- normalize it with the same helper used for the stored field
- if both sides are present and clearly differ, fail with a targeted repo-mismatch error
- if the stored field is absent, skip the guard rather than inventing a heuristic fallback

Files expected to change:

- [run.rs](lib/crates/fabro-types/src/run.rs)
- the manifest-backed run creation path that already receives `manifest.git.origin_url`
- any serialization/projection paths that persist and reload `RunRecord`

### 1. Convert remaining run-adjacent args to `ServerTargetArgs`

In [args.rs](lib/crates/fabro-cli/src/args.rs):

- replace `StorageDirArgs` with `ServerTargetArgs` for:
  - `ArtifactListArgs`
  - `ArtifactCpArgs`
  - `CpArgs`
  - `PreviewArgs`
  - `SshArgs`
  - `DiffArgs`
  - `ResumeArgs`
  - `RewindArgs`
  - `ForkArgs`
  - `PrCreateArgs`
  - `PrListArgs`
  - `PrViewArgs`
  - `PrMergeArgs`
  - `PrCloseArgs`

Also simplify `DiffArgs`:

- remove `stat`
- remove `shortstat`
- update help text to describe stored diff output only

Resulting CLI contract:

- `fabro diff <run> --server http://127.0.0.1:3000/api/v1`
- `fabro artifact list <run> --server /var/run/fabro.sock`
- `fabro sandbox ssh <run> --server https://fabro.example.com/api/v1`
- no `--storage-dir` on these commands
- no public `FABRO_STORAGE_DIR` support on these commands

### 2. Narrow shared lookup helpers

In [server_runs.rs](lib/crates/fabro-cli/src/server_runs.rs):

- keep `ServerSummaryLookup` as the default user-facing selector path
- add any missing helper methods needed for:
  - selector resolution
  - filtered summary listing
  - summary-to-state/event follow-up work
- keep `ServerRunLookup` only for commands that remain explicitly local/admin-only

Do not try to finish that narrowing until the last in-scope migrated command is off `ServerRunLookup`.

In [server_client.rs](lib/crates/fabro-cli/src/server_client.rs):

- add thin wrappers for the server APIs the CLI now needs, split by phase:
  - Phase 1:
    - list stage artifacts
    - download stage artifact
  - Phase 2:
    - generate preview URL
    - create SSH access
    - sandbox file listing/download/upload

Do not introduce a parallel target-resolution stack. Reuse:

- `connect_server_only(...)`
- `server_only_command_connection(...)`

If tests still need `FABRO_STORAGE_DIR` internally to steer the default local server instance, treat that as harness-only plumbing rather than user-facing behavior.

### 3. Repoint `resume` to the server-only lifecycle helpers

In [resume.rs](lib/crates/fabro-cli/src/commands/run/resume.rs):

- stop using `ServerRunLookup`
- resolve the run via `ServerSummaryLookup`
- call the existing direct-client start helper
- for foreground resume:
  - attach through the existing direct-client attach helper
  - print the existing server-backed summary output with no local `run_dir`

This is intentionally a small mechanical repoint. It should make `resume` match the already-simplified `start`/`attach` contract without introducing new behavior.

### 4. Repoint `fork` and `rewind` to server-backed run state

In [fork.rs](lib/crates/fabro-cli/src/commands/run/fork.rs) and [rewind.rs](lib/crates/fabro-cli/src/commands/run/rewind.rs):

- stop using `ServerRunLookup`
- resolve the run via `ServerSummaryLookup`
- fetch events/state via the resolved `ServerStoreClient`
- keep the local git/checkpoint mutation logic unchanged
- before mutating the local repo, validate obvious identity against stored run metadata:
  - compare the current checkout’s detected repo identity against `RunRecord.repo_origin_url` when present
  - if they clearly do not match, fail with a targeted error instead of mutating the wrong checkout

In [rewind.rs](lib/crates/fabro-cli/src/commands/run/rewind.rs):

- remove dependence on `run.path` for rewound-state cleanup
- if a small local run-dir cleanup is still required, compute it server-side or remove it
- keep durable run-state restoration (`run.rewound`, restored checkpoint, `run.submitted`) server-backed

The point is to make rewind/fork depend on the local repo, not on local run-store layout.

### 5. Repoint `pr *` to server-backed run selection and record loading

In [pr/mod.rs](lib/crates/fabro-cli/src/commands/pr/mod.rs) and subcommands:

- remove `runs_base(...)` / `ServerRunLookup::connect_from_runs_base(...)`
- resolve runs through `ServerSummaryLookup`
- load pull-request state from `get_run_state(...)`

Specific changes:

- [pr/list.rs](lib/crates/fabro-cli/src/commands/pr/list.rs)
  - replace `scan_runs_with_summaries(...)` with summary iteration from `ServerSummaryLookup`
  - keep current GitHub detail-fetch fanout in the CLI

- [pr/create.rs](lib/crates/fabro-cli/src/commands/pr/create.rs)
  - rebuild run state from server events instead of local path lookup
  - keep local repo detection via `detect_repo_info(...)`
  - apply the same repo-mismatch guard used by `fork` / `rewind` before proceeding

- [pr/view.rs](lib/crates/fabro-cli/src/commands/pr/view.rs)
- [pr/close.rs](lib/crates/fabro-cli/src/commands/pr/close.rs)
- [pr/merge.rs](lib/crates/fabro-cli/src/commands/pr/merge.rs)
  - load the stored PR record from the target server only

### 6. Make `diff` fully server-backed and simpler

In [diff.rs](lib/crates/fabro-cli/src/commands/run/diff.rs):

- stop using `ServerRunLookup`
- resolve via `ServerSummaryLookup`
- load `RunProjection` from the target server
- keep only two sources of diff output:
  - per-node `diff`
  - run-level `final_patch`
- remove sandbox reconnect and live diff generation entirely

Behavior:

- `fabro diff <run>` prints `final_patch`
- `fabro diff <run> --node <id>` prints the stored node diff
- if no stored diff exists, error with a clear message

Because this pass intentionally simplifies the product surface:

- remove `--stat`
- remove `--shortstat`
- update docs/tests accordingly

Also remove dead speculative server API surface tied to the old diff shape:

- delete `/api/v1/runs/{id}/files` from [fabro-api.yaml](docs/api-reference/fabro-api.yaml)
- remove the corresponding `not_implemented` route from [server.rs](lib/crates/fabro-server/src/server.rs)

This is a Phase 1 OpenAPI/spec change and should be treated as part of that phase explicitly.

That API is currently unimplemented and unused by the CLI. Keeping it around only adds drift.

### 7. Repoint `artifact list` and `artifact cp` to the existing server artifact API

In [artifact/list.rs](lib/crates/fabro-cli/src/commands/artifact/list.rs) and [artifact/cp.rs](lib/crates/fabro-cli/src/commands/artifact/cp.rs):

- stop using `ServerRunLookup` and `RuntimeState`
- resolve the run via `ServerSummaryLookup`
- fetch `RunProjection`
- enumerate stage IDs from `RunProjection.nodes`
- use the existing stage artifact routes for each relevant stage:
  - list artifact filenames
  - download artifact bytes

Keep filtering behavior in the CLI:

- `--node`
- `--retry`
- tree/no-tree output layout
- filename collision handling

This preserves the current artifact UX while removing local artifact-dir reads.

### 8. Finish preview/SSH/file-transfer as real server-owned sandbox operations

This is the only part of the plan that needs new or completed server APIs.

#### 8a. Preview

The route already exists in [fabro-api.yaml](docs/api-reference/fabro-api.yaml) and [server.rs](lib/crates/fabro-server/src/server.rs), but the real handler is still `not_implemented`.

Implement it in [server.rs](lib/crates/fabro-server/src/server.rs):

- load the run’s sandbox record from store/state
- reconnect server-side
- for Daytona:
  - generate signed or unsigned preview URL as requested
- return `409` if:
  - no active sandbox
  - sandbox provider does not support preview

Then repoint [preview.rs](lib/crates/fabro-cli/src/commands/run/preview.rs) to the server API instead of direct Daytona reconnect.

#### 8b. SSH

Add a new route to [fabro-api.yaml](docs/api-reference/fabro-api.yaml):

- `POST /api/v1/runs/{id}/ssh`

Recommended request/response shape:

- request:
  - `ttl_minutes`
- response:
  - `command`

Implement the handler in [server.rs](lib/crates/fabro-server/src/server.rs):

- load sandbox record
- reconnect server-side
- generate SSH access for supported providers
- return `409` when unsupported or unavailable

Then repoint [ssh.rs](lib/crates/fabro-cli/src/commands/run/ssh.rs):

- `--print` prints the returned command
- non-`--print` locally `exec`s the returned command

That preserves current UX while removing direct CLI sandbox reconnect.

#### 8c. Sandbox file transfer (`fabro sandbox cp`)

Add a small server-owned file-transfer surface for sandboxes.

Recommended routes:

- `GET /api/v1/runs/{id}/sandbox/files`
  - query:
    - `path`
    - optional `depth`
  - returns directory entries

- `GET /api/v1/runs/{id}/sandbox/file`
  - query:
    - `path`
  - returns raw file bytes

- `PUT /api/v1/runs/{id}/sandbox/file`
  - query:
    - `path`
  - request body:
    - raw file bytes

Implementation in [server.rs](lib/crates/fabro-server/src/server.rs):

- load sandbox record
- reconnect server-side
- delegate to the existing `Sandbox` trait:
  - `list_directory`
  - `download_file_to_local` equivalent via temp file or direct read/write helper
  - `upload_file_from_local` equivalent via temp file or direct write helper

CLI changes in [cp.rs](lib/crates/fabro-cli/src/commands/run/cp.rs):

- stop reconnecting to sandboxes directly
- resolve runs via `ServerSummaryLookup`
- for recursive download:
  - list directory via server
  - download files one by one via server
- for upload:
  - recursively walk local input
  - upload files one by one via server

This keeps the current UX and avoids inventing a tar/archive protocol.

### 9. Update docs/help text to match the new surface

Update:

- [docs/reference/cli.mdx](docs/reference/cli.mdx)
- [docs/reference/user-configuration.mdx](docs/reference/user-configuration.mdx)
- [docs/core-concepts/how-fabro-works.mdx](docs/core-concepts/how-fabro-works.mdx)

The docs should explicitly reflect:

- in-scope run-adjacent commands now use `--server`, not `--storage-dir`
- `diff` is stored-output only
- sandbox preview/SSH/file transfer are server-mediated
- local storage-dir maintenance commands still exist, but they are not the normal user-facing run lifecycle

## Test Plan

### Help/parser coverage

Update snapshots in:

- [artifact_list.rs](lib/crates/fabro-cli/tests/it/cmd/artifact_list.rs)
- [artifact_cp.rs](lib/crates/fabro-cli/tests/it/cmd/artifact_cp.rs)
- [diff.rs](lib/crates/fabro-cli/tests/it/cmd/diff.rs)
- [fork.rs](lib/crates/fabro-cli/tests/it/cmd/fork.rs)
- [resume.rs](lib/crates/fabro-cli/tests/it/cmd/resume.rs)
- [rewind.rs](lib/crates/fabro-cli/tests/it/cmd/rewind.rs)
- [pr_create.rs](lib/crates/fabro-cli/tests/it/cmd/pr_create.rs)
- [pr_list.rs](lib/crates/fabro-cli/tests/it/cmd/pr_list.rs)
- [pr_view.rs](lib/crates/fabro-cli/tests/it/cmd/pr_view.rs)
- [pr_close.rs](lib/crates/fabro-cli/tests/it/cmd/pr_close.rs)
- [pr_merge.rs](lib/crates/fabro-cli/tests/it/cmd/pr_merge.rs)
- [sandbox_cp.rs](lib/crates/fabro-cli/tests/it/cmd/sandbox_cp.rs)
- [sandbox_preview.rs](lib/crates/fabro-cli/tests/it/cmd/sandbox_preview.rs)
- [sandbox_ssh.rs](lib/crates/fabro-cli/tests/it/cmd/sandbox_ssh.rs)

Scenarios:

- help shows `--server`
- help no longer shows `--storage-dir`
- `diff --help` no longer shows `--stat` or `--shortstat`
- no docs/help text implies these commands honor `FABRO_STORAGE_DIR`

Use the normal snapshot workflow:

1. `cargo insta pending-snapshots`
2. inspect changes
3. `cargo insta accept`

### CLI targeting behavior

Add or update CLI tests for each in-scope command family:

- explicit `--server` wins
- configured `[server].target` is used when no flag is passed
- no explicit target uses the default local server instance

Concrete tests:

- `resume` uses configured server target without local run-dir lookup
- `artifact list` uses configured server target without local artifact-dir lookup
- `artifact cp` uses configured server target and downloads through the server
- `pr list` uses configured server target without scanning local runs/
- `pr view`/`pr merge`/`pr close` resolve records from the server target
- `sandbox preview` uses the server endpoint instead of direct Daytona reconnect
- `sandbox ssh --print` uses the server endpoint and prints the returned command
- `sandbox cp` upload/download works against a target server without CLI-side sandbox reconnect
- when `[server].target` is configured, these commands use it by default
- there is no separate local override path besides passing an explicit local `--server`

### Diff behavior coverage

In [diff.rs](lib/crates/fabro-cli/tests/it/cmd/diff.rs):

- completed run with stored final patch still prints patch
- missing stored final patch errors cleanly
- stored node diff still works
- remove tests that depend on live diff fallback semantics

### Fork/rewind/resume behavior coverage

In:

- [fork.rs](lib/crates/fabro-cli/tests/it/cmd/fork.rs)
- [rewind.rs](lib/crates/fabro-cli/tests/it/cmd/rewind.rs)
- [resume.rs](lib/crates/fabro-cli/tests/it/cmd/resume.rs)

Add server-target coverage:

- configured `[server].target` works without local run-store lookup
- explicit `--server` overrides configured target
- rewind/fork/resume continue to mutate only the local repo, not local run-store metadata files
- rewind/fork/pr-create fail fast when the selected run’s stored `repo_origin_url` clearly does not match the current checkout
- rewind/fork/pr-create skip the guard cleanly when older runs do not have a stored durable repo identity yet

### Server API coverage

Add server tests in [server.rs](lib/crates/fabro-server/src/server.rs) or the server integration suite for:

- preview URL generation for a supported sandbox
- preview rejects missing/unsupported sandboxes with `409`
- SSH command generation for a supported sandbox
- SSH rejects missing/unsupported sandboxes with `409`
- sandbox file list/download/upload round-trip
- stage artifact list/download continues to work for the CLI use case

### Full verification

- `cargo fmt --check --all`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo nextest run --workspace`

## Risks

- The biggest risk is accidentally preserving local run-path assumptions under a server-only CLI surface.
  - Mitigation: delete `--storage-dir` from in-scope commands and remove local lookup usage outright instead of trying to support both models.

- `fork`, `rewind`, and `pr create` still depend on the caller’s local repo matching the selected run closely enough.
  - Mitigation: keep that boundary, but add an explicit repo-mismatch guard using stored run metadata so obvious mistakes fail fast.

- `sandbox cp` is the largest unit because it needs a new server-owned file transfer surface.
  - Mitigation: keep the API thin and capability-shaped; do not design a generic virtual filesystem protocol.

- Preview/SSH capability is provider-specific.
  - Mitigation: standardize on `409` for unsupported or unavailable sandbox capability.

## Follow-on

After this lands, the remaining local/admin seam should be small and explicit:

- `store dump`
- `system df`
- `system prune`
- any hidden/internal commands that truly operate on local storage

At that point, `ServerRunLookup` should either:

- be deleted entirely if those commands are also repointed later, or
- be clearly renamed/documented as a local maintenance helper rather than a normal user-facing run abstraction
