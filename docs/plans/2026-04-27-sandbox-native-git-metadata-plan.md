---
title: "refactor: make git metadata sandbox-native and clarify run paths"
type: refactor
status: active
date: 2026-04-27
---

# refactor: make git metadata sandbox-native and clarify run paths

## For the engineer picking this up

Fabro currently mixes three different path concepts under names that imply they are interchangeable:

- The submitter/client path used to resolve workflow source.
- The sandbox execution path.
- A repo path the Fabro server process can open with `git2`.

That assumption is false for Docker and Daytona. Docker and Daytona clone into sandbox-owned workspaces, commonly `/workspace`, while the submitted source path may be something like `/Users/...` from a CLI client and may not exist on the Fabro server host. This causes metadata checkpoint writes to try to open a client path from the server process.

Greenfield app. No production deploys. Do not add compatibility shims, serde aliases, migration layers, or legacy fallback behavior. Prefer clear renames and direct deletion of stale concepts.

The metadata branch (`refs/heads/fabro/meta/<run_id>`) is written best-effort as a future-proofing archive. Nothing in this plan reads it at runtime — fork, rewind, and timeline all source their state from the durable run store (events and `RunProjection.checkpoints`). Fork-from-metadata-branch (recovering a run that was deleted from the server but still exists in git) is explicitly deferred to a future plan.

## Goals

- `source_directory` means the submitter-side path where run source was resolved. It is provenance/display only and may not exist on the Fabro server.
- `working_directory` means the sandbox execution path field on sandbox records/events, e.g. `/workspace`; use "sandbox execution path" for the concept in prose.
- Submitter-provided pre-run git context is optional. CLI submitters can report local dirty status, base SHA, and pre-run push outcome. Browser, Slack, webhook, MCP, and scheduled-job submitters usually cannot. The Fabro server never opens a submitter path.
- Runtime metadata branch writes happen through `Sandbox::exec_command` and are best-effort.
- Runtime fork, rewind, and timeline source state from the durable run store, not the metadata branch. Fork/rewind git setup validates source run-branch reachability through the sandbox clone origin before execution.
- Docker, Daytona, and local worktree execution share the same git metadata behavior.
- Stale bind-mount vocabulary is removed from records, events, docs, tests, and API schemas.
- Local in-place execution is preserved only as an explicit no-checkpoints opt-out with durable state and clear fork/rewind errors.

## Phase 1: Rename run path vocabulary and move pre-run git context to submitters

Primary files:

- `lib/crates/fabro-types/src/run.rs`
- `lib/crates/fabro-types/src/run_summary.rs`
- `lib/crates/fabro-types/src/run_event/run.rs`
- `lib/crates/fabro-workflow/src/event.rs`
- `lib/crates/fabro-workflow/src/operations/create.rs`
- `lib/crates/fabro-workflow/src/pipeline/initialize.rs`
- `lib/crates/fabro-server/src/run_manifest.rs`
- `lib/crates/fabro-cli` (CLI submitter pre-run git context)
- `docs/public/api-reference/fabro-api.yaml`

Required changes — vocabulary rename:

- Rename `RunSpec.working_directory` to `RunSpec.source_directory` and change its type from `PathBuf` to `Option<String>`. Provenance may originate on another machine, may be absent for server-originated runs, and is never opened by the server. `Option<String>` is more honest than `PathBuf` and avoids fake or normalized values.
- Remove `RunSpec.host_repo_path`.
- Rename `RunCreatedProps.working_directory` to `source_directory`; same `Option<String>` type.
- Remove `RunCreatedProps.host_repo_path`.
- Add the same durable fields to `RunCreatedProps` and `Event::RunCreated` as needed for event replay: `pre_run_git: Option<PreRunGitContext>`, `fork_source_ref: Option<ForkSourceRef>`, and `checkpoints_disabled: bool`. `RunSpec` is reconstructed from `RunCreatedProps` in `fabro-store`, so no new `RunSpec` field may exist only in memory.
- Delete `CreateRunInput.host_repo_path`; `operations::create` should always persist `source_directory` from resolved workflow context.
- Rename `PreparedManifest.working_directory` to `source_directory` in `fabro-server::run_manifest`; `PreparedManifest` is the server-side manifest assembled from submitted workflow source before `RunSpec` is persisted.
- Rename `RunSummary.host_repo_path` to `source_directory` and switch to `Option<String>`.
- Update `RunSummary::repository.name` derivation, end to end. This is needed because `source_directory` is now optional submitter provenance, while `repo_origin_url` is the best server-known repository identity:
  - Plumb `repo_origin_url` into `RunSummary::new` and into `build_summary` in `lib/crates/fabro-store/src/run_state.rs` alongside `source_directory`. Today both sites only carry the path.
  - Derivation order: prefer `repo_origin_url` owner/repo or repo basename when present; fall back to `source_directory` basename; fall back to `"unknown"`.
- Update CLI output and server summary helpers that currently expose `host_repo_path`.
- Regenerate and update API clients after the OpenAPI rename.

Required changes — submitter pre-run git context:

- Keep the existing git helper functions in `fabro-workflow::git` for now; move the call sites for dirty detection (`git::sync_status`), base-branch push (`git::branch_needs_push` + `git::push_branch`), and base SHA capture (`git::head_sha`) from `pipeline/initialize.rs` to the CLI submit path.
- Add `RunSpec.pre_run_git: Option<PreRunGitContext>`.
- Define `PreRunGitContext` as one coherent observation of the submitter checkout. It contains `display_base_sha: Option<String>`, `local_dirty: DirtyStatus` (`Clean`, `Dirty`, `Unknown`), and `push_outcome: PreRunPushOutcome`.
- Define `PreRunPushOutcome` as an enum, not an `Option<bool>`: `NotAttempted`, `Succeeded { remote, branch }`, `Failed { remote, branch, message }`, `SkippedNoRemote`, and `SkippedRemoteMismatch { remote, repo_origin_url }`.
- Keep `PreRunGitContext` grouped rather than flattening it into `RunSpec`; its fields are meaningful together as submitter-local provenance, and `push_outcome` is only meaningful in the context of the same checkout/base SHA observation.
- CLI submissions populate `pre_run_git` from the user's source checkout. Non-CLI submitters leave it `None`; the engine skips local dirty/push reporting and uses sandbox git setup output for display base SHA when available.
- For CLI submissions with `repo_origin_url`, use the current checkout's `origin` remote for pre-run push and compare it with `repo_origin_url` using `fabro_github::normalize_repo_origin_url(cli_remote_url) == fabro_github::normalize_repo_origin_url(repo_origin_url)`. If it does not match or cannot be proven, skip the pre-run push, record `SkippedRemoteMismatch` or `SkippedNoRemote`, and let sandbox clone/setup validate the runtime origin later. Fork/upstream layouts where `origin` is a user fork and `repo_origin_url` is upstream are treated as user configuration errors for this plan.
- `pipeline/initialize.rs` consumes `pre_run_git` and stops calling `options.sandbox.host_repo_path()` for git work. Delete the `SandboxSpec::host_repo_path()` accessor in this phase.

Decisions:

- Do not keep `host_repo_path` anywhere in active run specs, summaries, events, or runtime options. If a test needs a local git path, name that fixture variable `source_directory` or `repo_dir` according to its role.
- Delete both host-path entry points: `SandboxSpec::host_repo_path()` currently feeds initialization/pre-run host git behavior, and `RunOptions.host_repo_path` currently feeds runtime metadata/push behavior. Both are symptoms of the same server-local-git assumption and both go away.
- The Fabro server never inspects a submitter filesystem path; pre-run git facts arrive as data on `RunSpec`.

## Phase 2: Remove stale bind-mount fields

Primary files:

- `lib/crates/fabro-types/src/sandbox_record.rs`
- `lib/crates/fabro-types/src/run_event/infra.rs`
- `lib/crates/fabro-workflow/src/event.rs`
- `lib/crates/fabro-workflow/src/pipeline/initialize.rs`
- `lib/crates/fabro-store/src/run_state.rs`
- `lib/crates/fabro-sandbox/src/sandbox_spec.rs`
- `apps/fabro-web` and `lib/packages/fabro-api-client` (consumer audit)
- `docs/internal/events.md`
- `docs/internal/plan-events-as-source-of-truth.md`

Required changes:

- Delete `host_working_directory` and `container_mount_point` from `SandboxRecord`.
- Delete both fields from `SandboxInitializedProps` and `Event::SandboxInitialized`.
- Stop emitting both fields from `pipeline::initialize`.
- Stop storing both fields in `fabro-store` projections.
- Grep `apps/fabro-web` and `lib/packages/fabro-api-client` for either field name and update consumers; do not condition removal on OpenAPI presence alone.
- Update all tests and snapshots that construct sandbox records/events.
- Update docs to describe `sandbox.initialized.working_directory` as the sandbox execution path and remove host/container mount terminology.

Decision:

- Keep Docker clone metadata (`repo_cloned`, `clone_origin_url`, `clone_branch`) because it describes the sandbox clone source. It does not imply host bind mounts.

## Phase 3: Make metadata writes sandbox-native

Primary files:

- `lib/crates/fabro-workflow/src/sandbox_git.rs`
- `lib/crates/fabro-workflow/src/lifecycle/git.rs`
- `lib/crates/fabro-workflow/src/pipeline/finalize.rs`
- `lib/crates/fabro-workflow/src/run_dump.rs`
- `lib/crates/fabro-workflow/src/run_options.rs`
- `lib/crates/fabro-sandbox/src/sandbox.rs`
- `lib/crates/fabro-checkpoint/src/metadata.rs` (deletion)

Required changes — writer interface:

- Delete `RunOptions.host_repo_path`.
- Stop constructing `MetadataStore` in workflow runtime lifecycle code.
- Add a sandbox metadata writer near `sandbox_git` with an interface equivalent to:
  - input: sandbox, run id, metadata ref, `RunDump` (which already includes the full `RunProjection` as `run.json`), commit message, git author.
  - output: metadata commit SHA.

Required changes — writer implementation:

- Add a per-run `SandboxGitRuntime` helper near `sandbox_git`, shared by `GitLifecycle`, finalize, and parallel checkpoint code. It owns a `OnceLock`/cached result for the sandbox git capability probe plus metadata degradation state.
- Run the shared sandbox git capability probe once per run before the first sandbox git operation. Prefer an actual temp-index plumbing check over version parsing: under a hidden directory inside the sandbox execution path, create a temporary index, run the `read-tree --empty` / `hash-object -w` / `update-index --add --cacheinfo` / `write-tree` sequence against harmless temp data, then clean up. Do not assume `/tmp` or `$TMPDIR` is writable.
- Probe failure does not convert a run to `checkpoints_disabled`; that state is reserved for the explicit in-place opt-out. If the probe fails before run-branch checkpointing or fork setup, fail that operation with a clear "sandbox git unavailable" error. Metadata writes use the same cached probe result but remain best-effort: emit one warning, mark metadata degraded for the run, and skip future metadata writes.
- The probe is a startup capability check, not a guarantee for the full run lifetime. If an agent removes `git`, fills the disk, or otherwise breaks git after the probe passes, the real git operation fails with its concrete raw error; do not re-probe before every checkpoint.
- Create a sandbox temp directory for metadata files under the sandbox execution path.
- Write every dump entry into the metadata tree binary-safely. Use local temp files plus `Sandbox::upload_file_from_local` for bytes.
- The writer must include `run.json` (full `RunProjection` blob) at every checkpoint commit so that future fork-from-archive (deferred from this plan) can reconstruct the projection from a metadata commit alone.
- Build a temporary git index with `GIT_INDEX_FILE`.
- Load the previous metadata commit with `git read-tree <old_commit>` when the ref exists, else `git read-tree --empty`.
- Write blobs with `git hash-object -w`.
- Stage entries with `git update-index --add --cacheinfo`.
- Create the tree with `git write-tree`.
- Create commits with `git commit-tree`, setting author/committer env from `GitAuthor`.
- Update `refs/heads/fabro/meta/<run_id>` with `git update-ref`.
- Clean up temp files best-effort.

Required changes — integration:

- `GitLifecycle::on_run_start` initializes the metadata ref through this sandbox writer.
- `GitLifecycle::on_checkpoint` writes the snapshot through this sandbox writer and passes the returned SHA into `git_checkpoint` so the run-branch commit keeps the `Fabro-Checkpoint` trailer.
- After each successful metadata write, the sandbox metadata writer pushes `refs/heads/fabro/meta/<run_id>` best-effort through `git_push_ref`. `write_finalize_commit` only calls the writer for the final snapshot; it does not have a separate metadata push path.
- Metadata write failures remain best-effort warnings and must not fail a successful workflow run. They do not affect `Checkpoint.git_commit_sha`; that SHA comes from run-branch checkpoint commits. Fork/rewind require a checkpoint with `git_commit_sha`, so run-branch checkpoint failures affect forkability while metadata failures only affect future fork-from-archive recovery.
- Metadata write and metadata push failures share a noise budget: emit the first warning per run, mark metadata degraded, suppress repeated per-checkpoint warnings, and emit an end-of-run summary notice if metadata was degraded.

Decisions:

- Use git plumbing with a temporary index. Do not checkout the metadata branch.
- Do not mutate the sandbox worktree or real git index while writing metadata.
- Rejected alternative: relying on the first real git operation to fail was rejected because run-branch checkpointing, metadata writes, and parallel checkpoint code would each surface different failures. The shared probe gives one clear capability error and one cached best-effort degradation path.
- Delete `fabro-checkpoint::metadata::MetadataStore` entirely. The sandbox writer is the only writer. Tests that previously used `MetadataStore` migrate to either a local-sandbox-backed writer fixture or a focused test helper.

## Phase 4: Converge local git behavior on sandbox exec

Primary files:

- `lib/crates/fabro-sandbox/src/local.rs`
- `lib/crates/fabro-sandbox/src/worktree.rs`
- `lib/crates/fabro-sandbox/src/sandbox.rs`
- `lib/crates/fabro-workflow/src/pipeline/initialize.rs`
- `lib/crates/fabro-workflow/src/lifecycle/mod.rs`
- `lib/crates/fabro-cli` (CLI flag for opt-out)

Required changes:

- Runtime git checkpointing always uses `Sandbox::exec_command`.
- Make `WorktreeSandbox` the default local provider strategy for all local runs, regardless of submitter. Local runs use an isolated worktree; this is the path that shares behavior with Docker/Daytona.
- Add a CLI flag (e.g. `--in-place`) for the explicit opt-out that runs against the user's source tree directly. Require an explicit paired acknowledgement flag such as `--allow-no-checkpoints`; when opted in, disable git checkpointing, persist `checkpoints_disabled: true` on the run, surface that state in summaries/`fabro ps`, and make fork/rewind errors say the run was created without checkpoints. Do not `git checkout -b fabro/run/...` in the user source directory.
- Ensure `WorktreeSandbox` supports the same run-branch and metadata-branch git operations as Docker/Daytona through the `Sandbox` trait.
- Remove fallback host-side pushes from runtime git lifecycle. Push run and metadata refs from the sandbox.
- Replace `Sandbox::setup_git_for_run(&str)` with a setup intent method that returns `GitRunInfo` (`base_sha`, `run_branch`, `base_branch`) when it establishes sandbox git state:
  - `GitSetupIntent::NewRun { run_id }` creates `refs/heads/fabro/run/<run_id>` from the sandbox clone HEAD and returns the sandbox HEAD SHA as `base_sha`.
  - `GitSetupIntent::ForkFromCheckpoint { new_run_id, source_run_id, checkpoint_sha }` fetches the source run ref, creates `refs/heads/fabro/run/<new_run_id>` at `checkpoint_sha`, and checks it out before the first stage runs.
- Keep `GitSetupIntent` rather than passing full `RunSpec` into the sandbox trait. The workflow layer derives the intent from `RunSpec.fork_source_ref`, and the sandbox trait receives only the git setup data it needs.
- `WorktreeSandbox` owns local git setup. Its `initialize`/setup flow should keep the existing `git worktree add` behavior for `NewRun`, and add a fork path that creates the worktree branch at `checkpoint_sha`; it should not delegate git setup to bare `LocalSandbox`.
- Delete `Sandbox::host_git_dir` after the worktree migration. No runtime git operation may branch on a host-accessible repository path. Replace the current `host_git_dir` lifecycle gate with an explicit hook cwd rule: when sandbox git checkpointing is enabled, pass the sandbox execution path (`sandbox.working_directory()`) to `HookLifecycle`/`WorkflowRunStarted.worktree_dir`; when checkpoints are disabled, leave hook cwd unset. Do not infer hook behavior from host path accessibility.
- Rename `Sandbox::git_push_branch` to `git_push_ref` (in `lib/crates/fabro-sandbox/src/sandbox.rs`, the `delegate_sandbox!` macro, every implementor, and the shared `git_push_via_exec` helper). The method now pushes both run branches and metadata refs; `git_push_branch` is misleading.
- Define `git_push_ref` as taking a full refspec (e.g. `refs/heads/fabro/run/<id>:refs/heads/fabro/run/<id>`, `refs/heads/fabro/meta/<id>:refs/heads/fabro/meta/<id>`) so callers control source and destination explicitly. Preserve the existing `refresh_push_credentials()` call. Do not use force refspecs (`+src:dst`) for run or metadata refs; they are append-only/create-only by invariant, and a non-fast-forward push should fail loudly.

Decisions:

- The local source tree is never used as a server-side git repo for checkpoint metadata.
- `--in-place --allow-no-checkpoints` is the only path where local runs forgo checkpoints. It is opt-in, persisted on the run, and visible in list/detail and fork/rewind error paths.

## Phase 5: Source fork, rewind, and timeline from the durable run store

Primary files:

- `lib/crates/fabro-workflow/src/operations/run_git.rs`
- `lib/crates/fabro-workflow/src/operations/timeline.rs`
- `lib/crates/fabro-workflow/src/operations/fork.rs`
- `lib/crates/fabro-workflow/src/operations/rewind.rs`
- `lib/crates/fabro-workflow/src/operations/rebuild_meta.rs` (deletion)
- `lib/crates/fabro-workflow/src/operations/mod.rs`
- `lib/crates/fabro-server/src/server.rs`
- `lib/crates/fabro-sandbox/src/sandbox.rs`

Required changes — server-local git removal:

- Stop opening `RunSpec.source_directory` with `git2`. The server never opens any submitter path.
- Build checkpoint timelines from `RunProjection.checkpoints` directly. No metadata branch reads.
- Replace `TimelineEntry.metadata_commit_oid` (in `lib/crates/fabro-workflow/src/operations/timeline.rs`) with `checkpoint_seq: u32`. Every consumer that previously read `run.json` from a metadata commit (such as fork's historical projection reconstruction in `operations/fork.rs`) instead replays events up to that seq.
- Delete `rebuild_meta.rs`, `build_timeline_or_rebuild`, and `rebuild_metadata_branch`. Remove their `operations::mod` re-exports. Recovery from missing metadata branches is out of scope for this plan; a future fork-from-archive plan will reintroduce a recovery utility if needed.

Required changes — fork mechanics:

- Reconstruct the historical full `RunProjection` for the source run by composing `store.list_events(source_run_id)` with `RunProjection::apply_events`, stopping at the target `checkpoint_seq`. This becomes the new run's initial projection.
- Add `RunSpec.fork_source_ref: Option<ForkSourceRef>` and the matching `RunCreatedProps`/event field. `None` means a normal run. `Some(ForkSourceRef { source_run_id, checkpoint_sha })` means fork/rewind setup must fetch the source run branch and branch the new run from `checkpoint_sha`; this overrides normal clone-branch checkout after the initial clone is available.
- Before persisting the forked/rewound run, validate all store-local invariants: the target checkpoint has `git_commit_sha`, the source run did not record `checkpoints_disabled`, and source/new run specs point at the same normalized `repo_origin_url` using `fabro_github::normalize_repo_origin_url` on both sides. If these checks fail, return a validation-style error and do not create a new run row.
- Fork-from-running is supported, but only after a checkpoint has a git SHA and its run branch is reachable. Because an in-flight source run may still be pushing the checkpoint commit, sandbox reachability validation should use a short retry/wait window before failing.
- Sandbox initialization for a forked run must, before workflow execution starts:
  1. Fetch `refs/heads/fabro/run/<source_run_id>` from the sandbox clone's `origin` remote. In this plan, `origin` must correspond to `RunSpec.repo_origin_url`; fork/rewind across different origins is unsupported and should fail validation.
  2. Create the new run-branch `refs/heads/fabro/run/<new_run_id>` pointing at `checkpoint_sha`.
  3. Check that branch out before the first stage runs.
- If the source run ref fetch fails or the `checkpoint_sha` is not reachable from the fetched source run ref after the retry window, fail sandbox initialization with a clear fork setup error and mark the new run as failed during setup. Do not silently fall back to the sandbox default branch. Direct `git fetch origin <sha>` fallback is out of scope for this plan; source run branches must be pushed and reachable.
- Rewind continues the existing fork-then-archive behavior: create a new run id, create the new run branch at the checkpoint SHA, then archive/supersede the source run. This plan only changes the git setup mechanics; do not reintroduce rewind-in-place behavior.
- If the chosen checkpoint has no `git_commit_sha`, return the existing validation-style error.
- Fork creates the new run's branches/refs through the sandbox via `git_push_ref`, not `git2` against any server-local path.

Required changes — prefix resolution:

- Removing `host_repo_path` also removes repo/path-scoped prefix matching. Replace prefix-scoping logic in `find_run_id_by_prefix_or_store` with global prefix matching. If exactly one run matches, resolve it. If zero, return "not found." If multiple, return an ambiguity error listing each candidate (full run id, created_at, workflow name, origin URL) so the user can disambiguate.

Decisions:

- Server APIs do not require the original source checkout to exist on the Fabro server host.
- Events are the canonical source of run history at runtime; metadata branch is a write-only archive in this plan.
- Prefix ambiguity is a hard CLI error, not a silent filtering decision.

## Phase 6: API, generated clients, docs, and cleanup

Primary files:

- `docs/public/api-reference/fabro-api.yaml`
- `lib/crates/fabro-api/build.rs` and generated output as required
- `lib/packages/fabro-api-client`
- `apps/fabro-web`
- `lib/crates/fabro-cli`
- `docs/internal/events.md`
- `AGENTS.md` (remove any bind-mount references in agent guidance; otherwise drop from primary files)

Required changes:

- Update OpenAPI schemas from `host_repo_path` to `source_directory`.
- Add API/event schemas for `pre_run_git`, `fork_source_ref`, and `checkpoints_disabled` wherever `RunSpec`/`RunCreatedProps` are represented.
- Remove mount fields from sandbox initialized schemas if present.
- Rebuild Rust API types.
- Regenerate TypeScript API client.
- Update web and CLI consumers to display `source_directory` where submitter provenance is wanted and sandbox `working_directory` where execution path is wanted.
- Update CLI fork output to print full run id + workflow name + origin URL when a prefix is ambiguous.
- Update `/api/v1/runs/{id}/timeline` OpenAPI prose and regenerated TypeScript comments so they say the endpoint reads durable run-store checkpoints, not the metadata branch.
- Update docs to state Docker and Daytona are clone-based providers and never bind-mount the source repo.

Decision:

- No API aliases or transitional duplicated fields. Break callers cleanly.

## Test Plan

Focused tests:

- Submitter pre-run git context tests:
  - dirty detection, base-branch push, and `display_base_sha` are computed CLI-side and arrive on `RunSpec.pre_run_git`.
  - `pre_run_git` round-trips through `RunCreatedProps` and event replay; it is not only present in the in-memory `RunSpec`.
  - `PreRunPushOutcome` records success, failure, no-remote, and remote-mismatch cases explicitly.
  - non-CLI/server-created runs can leave `pre_run_git` as `None`; initialize uses sandbox setup output for display base SHA when available.
  - CLI pre-run push uses `origin`, compares with `repo_origin_url` via `fabro_github::normalize_repo_origin_url`, and is skipped/recorded when they do not match.
  - server `pipeline::initialize` no longer calls `git::sync_status` / `branch_needs_push` / `push_branch` / `head_sha`.
- Sandbox git capability tests:
  - shared capability probe exercises temp-index plumbing once per run under the sandbox execution path, not `/tmp`.
  - probe result is cached by the per-run `SandboxGitRuntime` and shared by lifecycle, finalize, and parallel checkpoint paths.
  - missing or broken git fails run-branch checkpointing/fork setup clearly.
  - metadata writer uses the same failed probe result to warn once and skip best-effort writes.
  - if git or disk availability breaks after a successful probe, the real checkpoint/fork git operation reports the concrete raw error.
- Sandbox metadata writer tests:
  - creates metadata branch from an empty ref.
  - appends snapshot commits while preserving prior files.
  - writes `run.json` (full `RunProjection`) at every checkpoint commit.
  - returns the metadata commit SHA.
  - does not change current branch, worktree files, or the real index.
  - handles binary dump entries.
  - pushes the metadata ref best-effort after each successful metadata write.
  - metadata write/push degradation emits one warning per run, suppresses repeated checkpoint noise, and emits an end-of-run summary notice.
- Workflow lifecycle regression tests:
  - Docker/Daytona-style run with `source_directory = /Users/client/project` never opens that path on the server.
  - metadata write success adds `Fabro-Checkpoint` trailer to the run checkpoint commit.
  - metadata write failure emits a single warning, does not fail the workflow, and does not remove the run-branch `git_commit_sha` needed for fork.
  - run-branch checkpoint failure leaves no forkable checkpoint and produces a clear fork/rewind validation error.
- Sandbox ref push tests:
  - `git_push_ref` pushes full run and metadata refspecs, preserves credential refresh, and rejects/non-forces non-fast-forward updates.
  - `git_push_via_exec`, `delegate_sandbox!`, Docker, Daytona, local, and worktree implementations all use the full-refspec method.
- Fork via event replay:
  - fork at checkpoint seq N reconstructs the historical `RunProjection` via `RunProjection::apply_events` over events with `seq <= N`.
  - fork succeeds without any metadata branch read.
  - fork's `ForkSourceRef` is optional on `RunSpec` and round-trips through `RunCreatedProps`/event replay: absent for normal runs, present for fork/rewind-created runs, and it overrides normal clone-branch checkout.
  - fork setup fetches `refs/heads/fabro/run/<source_run_id>` from the sandbox clone origin and creates `refs/heads/fabro/run/<new_run_id>` at the checkpoint SHA before the first stage runs.
  - fork store-local validation fails without creating a new run row when the source run has `checkpoints_disabled`, no target checkpoint SHA, or mismatched `repo_origin_url`.
  - fork-from-running retries briefly when the source run ref is not yet reachable, then either succeeds or fails sandbox setup clearly.
  - sandbox setup fails clearly when the source run ref cannot be fetched or the checkpoint SHA is unreachable from that ref after the retry window.
  - fork returns a clear error when target checkpoint has no git commit SHA.
- Timeline and rewind:
  - `TimelineEntry` exposes `checkpoint_seq` (not `metadata_commit_oid`).
  - timeline endpoint works from `RunProjection.checkpoints` without a local git repo.
  - rewind reuses the fork fetch + branch-from-sha mechanics, creates a new run id, and archives/supersedes the source run.
  - rewind target parsing works without opening `source_directory`.
- Repository summary derivation:
  - `repo_origin_url` plumbs through `build_summary` and `RunSummary::new`; name prefers origin owner/repo, falls back to `source_directory` basename, then `"unknown"`.
  - summary derivation works when `source_directory` is `None`.
- Prefix resolution:
  - exact-match prefix resolves to a single run.
  - ambiguous prefix returns an error listing every candidate.
  - zero matches returns "not found."
- Local strategy:
  - default local provider run uses `WorktreeSandbox` for CLI, browser, Slack, webhook, MCP, and scheduled-job submissions and produces checkpoints.
  - `WorktreeSandbox` owns `GitSetupIntent::NewRun` and `ForkFromCheckpoint`; it does not delegate branch setup to bare `LocalSandbox`.
  - `GitSetupIntent::NewRun` returns `GitRunInfo` with sandbox HEAD SHA as `base_sha`, and fork setup returns `GitRunInfo` for the new run branch.
  - `--in-place --allow-no-checkpoints` runs persist `checkpoints_disabled: true` through `RunCreatedProps`, surface that state in list/detail output, skip git checkpointing, and do not create a `fabro/run/...` branch in the user source directory.
  - fork/rewind against a run with `checkpoints_disabled: true` fails with an error that names the disabled-checkpoints cause.
  - `Sandbox::host_git_dir` is gone or unused by runtime git lifecycle code; `HookLifecycle` still receives the sandbox execution path as cwd when checkpointing is enabled.
- Naming and event tests:
  - run created/projection/summary JSON uses `source_directory`.
  - sandbox initialized JSON exposes sandbox `working_directory` and no mount fields.
  - repository summary name is derived from `repo_origin_url` before `source_directory`.
  - timeline OpenAPI/generated client prose says run-store checkpoints, not metadata branch.

Commands:

- `cargo build -p fabro-api`
- `cargo nextest run -p fabro-workflow`
- `cargo nextest run -p fabro-server`
- `cargo nextest run -p fabro-cli`
- `cd lib/packages/fabro-api-client && bun run generate`
- `cd apps/fabro-web && bun run typecheck`
- `cargo +nightly-2026-04-14 fmt --check --all`
- `cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings`

## Non-goals

- No production data migration.
- No compatibility aliases for old JSON fields.
- No server-side clone cache for metadata writes.
- No Docker bind-mount support.
- No branch mutation in the submitter source tree as a fallback.
- No fork, rewind, or timeline path that reads the metadata branch. Fork-from-archive (recovering a server-purged run from git alone) is deferred to a future plan.
- No prefix scoping by repo or origin. Ambiguous prefixes are user errors.
- No fork/rewind across different repository origins.
- No direct `git fetch origin <sha>` fallback for fork/rewind in this plan. Source run branches must be pushed and reachable from the sandbox clone origin.
- No GitHub repository-ID based origin matching in this plan. Origin equality is normalized URL equality, so repository renames/transfers that change normalized URLs are a known limitation; users should create new runs from the new origin or repair stored metadata in a future recovery tool.
