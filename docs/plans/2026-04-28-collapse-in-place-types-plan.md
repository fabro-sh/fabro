# Plan: collapse checkpoint/in-place type sprawl

## Context

The `--in-place` execution mode is currently encoded with two separate booleans across three layers:

- CLI: `--in-place` + `--allow-no-checkpoints` (paired, must always be set together)
- Manifest wire: `ManifestArgs.in_place: Option<bool>` + `ManifestArgs.allow_no_checkpoints: Option<bool>`
- Internal: `RunSpec.checkpoints_disabled`, `RunCreatedProps.checkpoints_disabled`, `RunSummary.checkpoints_disabled`, `RunOptions.checkpoints_disabled`, `PreparedManifest.checkpoints_disabled`, `CreateRunInput.checkpoints_disabled`

Plus a parallel concept (`WorkdirStrategy` enum + `SandboxSpec::workdir_strategy` method) that is structurally redundant with the existing `LocalSandboxLayer.worktree_mode` config and is barely consulted at runtime.

The bool pair has 9 representable states with only 2 valid, enforced via three runtime `bail!`s in `prepare_manifest`. The field name `checkpoints_disabled` is also misleading — it only disables *git* checkpoints; SlateDB/event-sourced checkpoints flow regardless.

This is a greenfield app with no production deployments. We can make breaking API changes with no shims.

## Goals

- Make the illegal state `(in_place=true, allow_no_checkpoints=false)` unrepresentable.
- Make the illegal state `Docker + in_place` validated once, in one place.
- Eliminate `WorkdirStrategy` and `RunOptions.checkpoints_disabled` (redundant types).
- Rename `checkpoints_disabled` → `in_place` on persisted types: it describes user intent, not the consequence; honest about SlateDB-still-on; decoupled from any future implementation that might allow git checkpoints in-place.
- Route `--in-place` through the existing `WorktreeMode::Never` config rather than a parallel field.

## Non-goals

- No backwards compat; no JSON aliases; no migration.
- Not unifying `RunSandboxLayer.{provider, local, docker, daytona}` into a tagged union (separate, larger refactor).
- Not implementing in-place + git-checkpoints (would require non-trivial git plumbing changes).
- Not touching the `Always | Clean | Dirty` variants of `WorktreeMode` — only adding a CLI path that emits `Never`.

## Design summary

**CLI surface**: keep `--in-place`. Drop `--allow-no-checkpoints` entirely. The flag's presence alone is sufficient consent.

**Wire**: drop both `in_place` and `allow_no_checkpoints` from `ManifestArgs`. The CLI emits a layer override that sets `run.sandbox.provider = "local"` and `run.sandbox.local.worktree_mode = Never`. No new wire fields.

**Server**: delete the three `bail!`s in `prepare_manifest` and the line-321 sandbox-default fixup. Compute `RunSpec.in_place = (resolved_provider == Local && resolved_worktree_mode == Never)` once during run creation.

**Runtime**: `pipeline/initialize.rs::resolve_worktree_plan` reads `worktree_mode == Never` instead of `checkpoints_disabled`. Delete `WorkdirStrategy` enum and the unused `workdir_strategy` method on `SandboxSpec`. Delete `RunOptions.checkpoints_disabled`; `lifecycle/mod.rs:109` simplifies to `has_run_branch` (since `git = None` already covers all skip cases).

**Persisted**: rename `checkpoints_disabled` → `in_place` on `RunSpec`, `RunCreatedProps`, `RunSummary`, plus the OpenAPI schema and generated TS client.

**Fork validation**: continues to read the persisted bool (now `in_place`) for clear error messages, e.g. "this run was created with `--in-place`; no git checkpoint history."

## Implementation phases

### Phase 1: CLI surface

**Files**:
- `lib/crates/fabro-cli/src/args.rs` — delete `allow_no_checkpoints` field; drop `requires = "allow_no_checkpoints"` from `in_place`'s `#[arg]`. Keep `conflicts_with = "sandbox"` (the rule "if you pass `--in-place`, don't also pass `--sandbox`" stays — implicit choice is local).
- `lib/crates/fabro-cli/src/manifest_builder.rs:175-202` — delete `allow_no_checkpoints` field plumbing. Replace `args.in_place.then_some(true)` ManifestArgs field write with a layer override emission: when `--in-place`, contribute a config-layer override that sets `run.sandbox.provider = "local"` and `run.sandbox.local.worktree_mode = "never"`. Drop the manifest-args `in_place` field entirely.

### Phase 2: Wire format

**Files**:
- `docs/public/api-reference/fabro-api.yaml:3696-3701` — delete `in_place` and `allow_no_checkpoints` from `ManifestArgs`.
- `lib/crates/fabro-api/build.rs` — verify regenerated types drop both fields; rebuild.
- `lib/packages/fabro-api-client/src/models/manifest-args.ts:40` — regenerated; verify drops both.

### Phase 3: Server validation

**File**: `lib/crates/fabro-server/src/run_manifest.rs`
- Delete `(in_place, allow_no_checkpoints)` parsing block (lines 72-83).
- Delete the `in_place requires sandbox=local` validation block (lines 84-94).
- Delete the line-321 fixup `(args.in_place == Some(true)).then(|| "local".to_string())`.
- Replace `prepared.checkpoints_disabled = in_place` (line 159) with: after the `WorkflowSettings` are resolved, compute `in_place = (settings.sandbox.provider == Local && settings.sandbox.local.worktree_mode == Never)`. Store on `PreparedManifest.in_place: bool`.
- Update `PreparedManifest` struct: rename field `checkpoints_disabled` → `in_place` (line 51).
- `create_run_input` (line 174): pass `in_place: prepared.in_place` to `CreateRunInput`. Rename `CreateRunInput.checkpoints_disabled` → `in_place`.
- Update test fixtures in this file (line 1177-1178) to drop `in_place`/`allow_no_checkpoints` ManifestArgs assignments.

### Phase 4: Internal type deletion

**`WorkdirStrategy` enum**: delete entirely.
- `lib/crates/fabro-sandbox/src/sandbox_spec.rs:50-54` — delete enum.
- `lib/crates/fabro-sandbox/src/sandbox_spec.rs:133-147` — delete `workdir_strategy` method.
- `lib/crates/fabro-sandbox/src/lib.rs:42` — remove `WorkdirStrategy` from re-exports.
- `lib/crates/fabro-workflow/src/pipeline/initialize.rs` — remove import; rewrite the match in `resolve_worktree_plan` (around lines 142-200). Replace the call to `workdir_strategy` and the three-arm match with direct logic:
  - If sandbox is not Local, follow today's "Cloud" path (set display_base_sha from pre_run_git, return None).
  - If sandbox is Local and `worktree_mode == Never`, follow today's "LocalDirectory" path (clear display_base_sha, return None).
  - Otherwise, follow today's "LocalWorktree" path (build `WorktreePlan`).

**`RunOptions.checkpoints_disabled`**: delete.
- `lib/crates/fabro-workflow/src/run_options.rs:38` — delete field.
- `lib/crates/fabro-workflow/src/lifecycle/mod.rs:109` — simplify to `has_run_branch` (or remove the local variable entirely if its only use was the bool).
- `lib/crates/fabro-workflow/src/pipeline/initialize.rs:57` — replace check with `worktree_mode == Some(WorktreeMode::Never)` reading the resolved mode (the function already has `options.worktree_mode` plumbed in).
- `lib/crates/fabro-workflow/src/handler/manager_loop.rs:215` — delete the field assignment (was setting both `checkpoints_disabled: true` and `git: None`; the `git: None` alone is the canonical signal).
- `lib/crates/fabro-workflow/src/operations/start.rs:703` — drop the field write.
- `lib/crates/fabro-workflow/src/operations/create.rs` — delete `checkpoints_disabled` from `PersistCreateOptions` (line 72) if it only feeds `RunOptions`. Verify by tracing.

### Phase 5: Persisted rename `checkpoints_disabled` → `in_place`

**Core types**:
- `lib/crates/fabro-types/src/run.rs:118` — rename field on `RunSpec`.
- `lib/crates/fabro-types/src/run_event/run.rs:42` — rename field on `RunCreatedProps`.
- `lib/crates/fabro-types/src/run_summary.rs:22,54,78` — rename field, parameter, and constructor body on `RunSummary`.

**Operations and event encoding**:
- `lib/crates/fabro-workflow/src/event.rs:68,1533,1552` — rename field on `Event::RunCreated` variant and serializer.
- `lib/crates/fabro-workflow/src/operations/create.rs:48,108,146,239,358,387` — rename field and parameters on `CreateRunInput` and persistence helpers.
- `lib/crates/fabro-workflow/src/operations/fork.rs:115-118,190` — update validation read site and propagation. Update error message to: `"source run was created with --in-place; cannot fork (no git checkpoint history)"`.
- `lib/crates/fabro-workflow/src/run_lookup.rs:480` — rename field write.
- `lib/crates/fabro-workflow/src/pipeline/persist.rs:175` — rename in event-construction.

**Store**:
- `lib/crates/fabro-store/src/run_state.rs:65,399,1004` — rename field.
- `lib/crates/fabro-store/src/slate/mod.rs:368` — rename field.

**CLI consumers**:
- `lib/crates/fabro-cli/src/server_runs.rs:71-73` — rename method to `in_place()`.
- `lib/crates/fabro-cli/src/commands/runs/list.rs:49,116,143-152` — rename JSON field, table-cell function. Decide UI display string ("in-place" vs "yes/no" — recommend "yes/no" with a column header "In-place").

**Server demo**:
- `lib/crates/fabro-server/src/server.rs:9740,9761` — rename in test/demo fixtures.
- `lib/crates/fabro-server/src/demo/mod.rs:919` — rename field write.

**OpenAPI schema** (`docs/public/api-reference/fabro-api.yaml`):
- Line 4527 (RunSpec required), 4563 (RunSpec property), 4663 (RunSummary property), 5318 (RunListItem property) — rename `checkpoints_disabled` → `in_place`.

**Generated TS client** (post-`bun run generate`):
- `lib/packages/fabro-api-client/src/models/run-spec.ts:43`
- `lib/packages/fabro-api-client/src/models/run-summary.ts:37`
- `lib/packages/fabro-api-client/src/models/run-list-item.ts:57`

### Phase 6: Test fixtures

**Bulk renames** (replace `checkpoints_disabled:` → `in_place:` in struct literals; replace `"checkpoints_disabled":` → `"in_place":` in JSON assertions):
- `lib/crates/fabro-types/tests/run_spec_serde.rs:41,51`
- `lib/crates/fabro-types/tests/run_event_serde.rs:41,52`
- `lib/crates/fabro-types/tests/run_spec_methods.rs:29`
- `lib/crates/fabro-store/tests/serializable_projection.rs:28`
- `lib/crates/fabro-cli/tests/it/cmd/attach.rs:458` — JSON assertion.
- `lib/crates/fabro-cli/tests/it/cmd/create.rs:56`, `run.rs:173` — drop `--allow-no-checkpoints` from CLI help-text golden output.
- `lib/crates/fabro-workflow/tests/it/integration.rs` — ~50+ fixtures (mechanical).
- `lib/crates/fabro-workflow/tests/it/daytona_integration.rs` — 7 fixtures.
- `lib/crates/fabro-workflow/tests/it/git_integration.rs:163` — 1 fixture.
- Test helpers: `fabro-workflow/src/run_dump.rs:439`, `sandbox_git.rs:1197`, `runtime_store.rs:146,171`, `pipeline/finalize.rs:450`, `pipeline/initialize.rs:797,824,848,875`, `pipeline/retro.rs:227,246,291`, `pipeline/pull_request.rs:1183,1202,1250,1269`.

### Phase 7: Docs

- `docs/internal/events.md:61,83` — rename JSON example and description-table row.
- `docs/plans/2026-04-27-sandbox-native-git-metadata-plan.md` — leave as-is (historical plan; reflects state at that time).

## Verification

**Build & lint**:
```sh
cargo build --workspace
cargo +nightly-2026-04-14 fmt --check --all
cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings
```

**Unit & integration tests**:
```sh
cargo nextest run --workspace
```

**API client regeneration**:
```sh
cd lib/packages/fabro-api-client && bun run generate
cd ../../../apps/fabro-web && bun run typecheck
```

**End-to-end manual checks** (set `FABRO_TEST_MODE=twin` if needed):

1. **Standard local run produces checkpoints**:
   ```sh
   fabro run smoke
   fabro inspect <run-id>  # verify in_place=false on the run; checkpoints have git_commit_sha
   ```

2. **In-place run skips git checkpoints**:
   ```sh
   fabro run smoke --in-place
   fabro inspect <run-id>  # verify in_place=true; SlateDB checkpoints exist (no git_commit_sha)
   ```

3. **CLI rejects bad combinations**:
   ```sh
   fabro run smoke --allow-no-checkpoints  # expected: clap unknown-flag error
   fabro run smoke --in-place --sandbox docker  # expected: clap conflicts_with error
   ```

4. **Fork against in-place run gives clear error**:
   ```sh
   fabro fork <in-place-run-id>:1  # expected: "source run was created with --in-place; cannot fork"
   ```

5. **`fabro ps`/`fabro runs list` show `in_place` correctly** for both run flavors.

## Open question for the implementer

The existing `CliSandboxProvider` enum and the args-layer override path do not currently support emitting `LocalSandboxLayer.worktree_mode = Never` from CLI args. Phase 1 adds that capability. Read `manifest_args_overrides` in `run_manifest.rs:308` to see the existing layer-construction pattern; the new override mirrors how `args.docker_image` is translated to `DockerSandboxLayer.image` today.

## Suggested commit boundaries

Split into focused commits to keep review tractable:
1. Phase 1 + Phase 2 + Phase 3 — drop `--allow-no-checkpoints` and the wire fields; simplify server validation.
2. Phase 4 — delete `WorkdirStrategy` and `RunOptions.checkpoints_disabled`.
3. Phase 5 + Phase 6 — rename `checkpoints_disabled` → `in_place` (mechanical, large diff).
4. Phase 7 — docs.

If desired, this plan can be copied to `docs/plans/2026-04-28-collapse-in-place-types-plan.md` to follow the project's plan-archival convention.
