---
title: "refactor: collapse settings resolve indirection layer"
type: refactor
status: completed
date: 2026-04-23
---

# refactor: collapse settings resolve indirection layer

## Overview

Delete `fabro_config::Resolver`, the per-`*Settings` `*_into(&mut errors)` methods, the private `ResolvedSettingsTree` in `fabro-workflow`, and the `resolve_settings_tree` free fn. Add a `WorkflowSettings` sibling to `ServerSettings`/`UserSettings` so workflow ops have one entrypoint. Standalone `resolve_*_from_file` helpers stay (single-namespace consumers depend on them) but get rewritten to not go through `Resolver`.

`WorkflowSettings` is **workflow-only**: `{ project, workflow, run }`. It does NOT hold `server` or `features` — even though the per-request layer DOES carry `features` and a subset of `server` (storage, scheduler, artifacts, web, api) per `materialize_settings_layer`. Excluding them is an **ownership choice**: we want the live `ServerSettings` (resolved at server boot) to remain the single authority for deployment-level config, and we don't want a workflow-side parallel copy that can quietly drift. `ServerSettings` and `UserSettings` keep `features` because they're constructed from a complete owned settings layer at boot — they ARE the authority.

Today's lifted `server_storage_root` field on `ResolvedSettingsTree` was an asymmetric workaround for "create_run needs one server-ops field but has no access to live `ServerSettings`." The fix: `create_run` gains a `storage_root: PathBuf` parameter so the caller (which owns the live `ServerSettings`) supplies it. No more lifted server fields anywhere.

PR 2 (Combine trait + derive, uv pattern) is a separate, larger refactor — instructions in `docs/plans/2026-04-23-002-refactor-combine-trait-uv-pattern-plan.md`.

## Problem Frame

Today three layers sit between `SettingsLayer` and consumer code:

1. `Resolver` (introduced in 52d24531d) — caches `apply_builtin_defaults` across multiple per-namespace resolves.
2. `*_into(&mut errors)` methods on `Resolver` — let `ServerSettings::from_layer` and `UserSettings::from_layer` accumulate errors across two namespaces.
3. `resolve_settings_tree` + `ResolvedSettingsTree` in `fabro-workflow/src/operations/create.rs` — bundles four namespaces for `create_run`.

`Resolver` exists only because `create_run` was running 4× `apply_builtin_defaults(file.clone())` per request. It's a perf micro-fix dressed up as an API primitive. `ResolvedSettingsTree` is the missing `WorkflowSettings` sibling of `ServerSettings`/`UserSettings` — wrong crate, wrong name, asymmetric shape (lifts `storage_root: InterpString` instead of holding a full `ServerNamespace`).

## Requirements Trace

- R1. Single entrypoint per consumer (Server/User/Workflow), each with `from_layer(&SettingsLayer) -> Result<...>`.
- R2. Multi-namespace error accumulation preserved — `*Settings::from_layer` must surface errors from ALL its namespaces, not first-only.
- R3. Standalone `resolve_*_from_file` helpers continue to work for single-namespace consumers (dozens of callers across the workspace).
- R4. **`WorkflowSettings` holds workflow-owned namespaces only** (`project`, `workflow`, `run`). `server` and `features` are excluded as an ownership choice — the live `ServerSettings` is the single authority for deployment-level config, even though `materialize_settings_layer` does flow `features` and a subset of `server.*` (storage, scheduler, artifacts, web, api) through into the per-request layer. `ServerSettings` and `UserSettings` are constructed at boot from a complete owned settings layer and hold every namespace their consumer reads. Server-ops fields needed by `create_run` (today: just `storage.root`) come from the caller as parameters, not from the bundle.
- R5. No change to resolved-settings semantics or to persisted `Event::RunCreated.settings` shape. **`record.run_dir` is now derived from the new `storage_root` parameter, not from `request.settings.server.storage.root` as today.** For the production HTTP caller (which passes the same resolved `server.storage.root` it would have read from the live `ServerSettings`), `record.run_dir` matches today's value. For direct `create()` callers (test helpers, future API consumers) that pass a different path, `record.run_dir` will reflect the parameter — that is the new invariant. The other consumer-visible changes are: (a) `create_run`'s signature gains one parameter (R7); (b) the storage-root interpolation-failure error path moves from workflow-side to transport-side with different error text — see Risks.
- R6. **Workflow-settings resolution errors preserve `render_resolve_errors` formatting** (today: `; `-joined). `WorkflowSettings::from_layer` returns `Result<Self, Vec<ResolveError>>` (matching the `resolve_*_from_file` convention) so `create_run` keeps formatting via `render_resolve_errors`. R6 covers ONLY the `resolve_*` failure path inside `from_layer` — not the storage-root interpolation failure, which moves out of `create_run` per R5.
- R7. `create_run`'s signature gains a `storage_root: PathBuf` parameter. Callers supply it from the live deployment context: `fabro-server` reads it from its boot-time `ServerSettings`; tests pass a fixture path.

## Scope Boundaries

- Out of scope: the Combine refactor (PR 2).
- Out of scope: migrating every `resolve_*_from_file` caller to a bundle. Standalone helpers stay; only `create_run` gets the bundle treatment.
- Out of scope: removing `render_resolve_errors` (used by 4+ callers; orthogonal).
- Out of scope: changing the `*Layer` / `*Namespace` types in `fabro-types`.
- Out of scope: changing `apply_builtin_defaults`, `defaults.toml`, or any per-namespace `resolve_*(layer, &mut errors)` function body.

## Context & Research

### Relevant Code and Patterns

- `lib/crates/fabro-config/src/context.rs` — current `ServerSettings` and `UserSettings` (use `Resolver`).
- `lib/crates/fabro-config/src/resolve/resolver.rs` — entire file deletes.
- `lib/crates/fabro-config/src/resolve/mod.rs` — `resolve_storage_root` + 6× `resolve_*_from_file` helpers (today: thin wrappers over `Resolver`; rewrite to inline `apply_builtin_defaults` + per-namespace resolve).
- `lib/crates/fabro-config/src/resolve/{cli,server,project,features,run,workflow}.rs` — per-namespace `resolve_*` fns; bodies unchanged. Visibility stays `pub` (callers in tests + helper crates).
- `lib/crates/fabro-config/src/defaults.rs` — `apply_builtin_defaults(layer: SettingsLayer) -> SettingsLayer`. One clone per call.
- `lib/crates/fabro-workflow/src/operations/create.rs:63-68` — `ResolvedSettingsTree`; `:116` use site; `:166` `combined_labels` call; `:287-297` `resolve_settings_tree`; `:299-304` `combined_labels` fn.
- `lib/crates/fabro-config/src/resolve/server.rs:68-75` — `resolve_storage` already substitutes `default_storage_dir()` when `server.storage.root` is missing. `Resolver::storage_root()` was duplicating this. With `server` excluded from `WorkflowSettings`, the duplication moves: the single authority for storage_root is now whatever `ServerSettings::server.storage.root` resolves to at server boot (used by the existing `state.server_storage_dir()` helper).
- `lib/crates/fabro-server/src/server.rs:577,643,659` — `AppState.settings` is a raw `Arc<RwLock<SettingsLayer>>`; `AppState.server_settings` is a separate `RwLock<Arc<ServerSettings>>` accessed via `state.server_settings()` (line 643). There IS a `state.server_storage_dir() -> PathBuf` helper (line 659), **but it panics on interpolation failure** (`.expect("server storage root should be resolved at startup")`). Today's `create()` returns `Error::Precondition` on the same failure (`create.rs:118-126`). Unit 3 must NOT use `server_storage_dir()` — the HTTP handler has to do its own `resolve_interp_string` and map errors to `ApiError`. See Unit 3 Approach.
- `lib/crates/fabro-config/src/effective_settings.rs:55-64` — documents that `server_settings.features` AND a small subset of `server_settings.server` (storage, scheduler, artifacts, web, api) ARE applied authoritatively into the per-request layer. Server-ops fields (auth, listen, ip_allowlist, slatedb, logging, integrations) are stripped. So the per-request layer DOES carry storage and features — `WorkflowSettings` excludes them by **ownership choice** (we want the live `ServerSettings` to be the single authority for deployment-level config), not by capability constraint.

### Call Site Audit (verified)

`resolve_*_from_file` standalones — KEEP (dozens of callers):
- `resolve_run_from_file`: 20+ callers across fabro-cli, fabro-server, fabro-workflow, tests
- `resolve_server_from_file`: 12+ callers across fabro-cli, fabro-server, fabro-install, tests
- `resolve_features_from_file`: `fabro-server/src/server.rs:1364`
- `resolve_project_from_file` / `resolve_workflow_from_file` / `resolve_cli_from_file`: callers in `fabro-config/src/project.rs`, fabro-cli, tests
- `resolve_storage_root`: 1 external caller (`fabro-cli/src/local_server.rs:15`) + tests

`Resolver` — DELETE (1 caller):
- `lib/crates/fabro-workflow/src/operations/create.rs:288` (in `resolve_settings_tree`)

`ResolvedSettingsTree` / `resolve_settings_tree` / `combined_labels` (free fn) — DELETE (private to `create.rs`, all uses local).

### Institutional Learnings

None directly applicable. The original `Resolver` commit (52d24531d) explicitly framed itself as a perf optimization, not an API improvement — its rationale evaporates once the per-consumer bundles each do their own apply-defaults.

## Key Technical Decisions

- **Principle: `WorkflowSettings` holds workflow-owned namespaces only.** `{ project, workflow, run }` — the namespaces whose authority belongs to the workflow consumer. `server` and `features` are excluded as an *ownership choice* (live `ServerSettings` is the single authority for deployment-level config), even though `materialize_settings_layer` makes them available in the per-request layer. `ServerSettings` and `UserSettings` are constructed from a complete owned layer at boot and hold every namespace their consumer reads.
- **`create_run` gains `storage_root: PathBuf` parameter.** Storage root is deployment-level config — it belongs to whoever booted the server. Lifting it through a per-request bundle (today's `ResolvedSettingsTree.server_storage_root`) was an asymmetric workaround. Pushing it to a parameter eliminates the asymmetry and makes the data-flow honest. The caller resolves env-var interpolation before passing and maps any resolution error to the appropriate transport-level response.
- **Persisted settings layer is the request-derived/materialized artifact; do NOT overwrite it.** What lands in `Event::RunCreated.settings` is not the raw submitted layer — `create()` runs `materialize_run(settings, ...)` first (`create.rs:407`) which normalizes and applies graph-time materialization. Treat the persisted field as "the materialized request artifact." `Event::RunCreated` carries two separate fields (`create.rs:234-265`): `settings: <JSON of materialized SettingsLayer>` and `run_dir: String` (where the run actually went). They serve different purposes and should remain separately recorded. The `storage_root` parameter is the runtime authority for `create_run`; `record.run_dir` is the persisted authority for "where is this run?" downstream. `record.settings.server.storage.root` is whatever the materialized layer carries — possibly an env-template that resolved differently at runtime than what `record.run_dir` records. Future code that needs the path should read `run_dir`, not re-resolve the layer. Audit (one grep) confirmed no current `fabro-workflow` consumer reads `settings.server.storage.root` from a persisted record beyond the line being removed.
- **Keep standalone `resolve_*_from_file` helpers; rewrite without `Resolver`.** Each becomes `apply_builtin_defaults(file.clone())` + `resolve_<ns>(&layer.<ns>.unwrap_or_default(), &mut errors)`. Rationale: dozens of single-namespace callers (e.g. "is this a dry run?" → `resolve_run_from_file(...)?.execution.mode`); forcing them through a bundle would pay full validation cost for one field.
- **`WorkflowSettings::from_layer` returns `Result<Self, Vec<ResolveError>>`.** Asymmetric with `ServerSettings::from_layer` and `UserSettings::from_layer` (which return `fabro_config::Result<Self>` wrapping `Error::Resolve`), but symmetric with `resolve_*_from_file`. Reason: preserves `create_run`'s existing user-visible error format (`; `-joined via `render_resolve_errors`); changing it would be an unowned scope expansion (R6). Existing `Server`/`UserSettings` callers undisturbed.
- **`combined_labels` becomes a method on `WorkflowSettings`.** Free fn was only callable in one place.
- **Each `*Settings::from_layer` calls `apply_builtin_defaults(layer.clone())` independently.** Acceptable — `create_run` is human-paced, not per-request.
- **Visibility of per-namespace `resolve_*` fns stays `pub`.** Called by `*_from_file` standalones and tests.

## Open Questions

### Resolved During Planning

- **Should `WorkflowSettings` hold `ServerNamespace` and/or `FeaturesNamespace`?** Neither. Per R4, `WorkflowSettings` holds workflow-owned namespaces only. Server-ops and features ARE available in the per-request layer (via `materialize_settings_layer`), but excluded from the bundle as an ownership choice — live `ServerSettings` is the single authority for deployment-level config.
- **Where does `create_run` get `storage.root` if not from the bundle?** From a `storage_root: PathBuf` parameter. Caller (fabro-server, tests) supplies it. See R7.
- **Should we migrate every `resolve_*_from_file` caller to a bundle?** No. Standalones stay; bundles are for multi-namespace consumers only.
- **Does `combined_labels` belong as a method or free fn?** Method on `WorkflowSettings`.
- **What error type does `WorkflowSettings::from_layer` return?** `Result<Self, Vec<ResolveError>>` (matches `resolve_*_from_file`, preserves `create_run`'s existing error message format via `render_resolve_errors`). `Server`/`UserSettings` keep their existing `fabro_config::Result<Self>` shape.
- **Should `resolve_storage_root` standalone keep its separate identity?** Yes, keep it (rewrite inline). It's used by `fabro-cli/src/local_server.rs:15` to compute a runtime path without resolving full server settings; that use case remains. Don't migrate that caller in this PR.

### Deferred to Implementation

- **Test layout for `WorkflowSettings`.** Either add to `lib/crates/fabro-config/tests/resolve_root.rs` (existing multi-namespace tests live there) or new file `tests/workflow_settings.rs`. Pick during implementation based on what reads better.

## Implementation Units

- [x] **Unit 1: Add `WorkflowSettings` to `fabro-config::context`**

**Goal:** New consumer bundle for workflow ops, sibling to `ServerSettings`/`UserSettings`. Workflow-only namespaces.

**Requirements:** R1, R2, R6

**Dependencies:** None.

**Files:**
- Modify: `lib/crates/fabro-config/src/context.rs`
- Modify: `lib/crates/fabro-config/src/lib.rs` (export `WorkflowSettings`)
- Test: `lib/crates/fabro-config/tests/resolve_root.rs` OR new `tests/workflow_settings.rs`

**Approach:**
- Add a `pub struct WorkflowSettings` with all fields `pub`:
  - `pub project:  ProjectNamespace`
  - `pub workflow: WorkflowNamespace`
  - `pub run:      RunNamespace`
  Every field is `pub` so cross-crate field access from `fabro-workflow` works.
- Add `WorkflowSettings::from_layer(layer: &SettingsLayer) -> Result<Self, Vec<ResolveError>>`. Returns `Vec<ResolveError>` (NOT wrapped in `fabro_config::Error::Resolve`) so callers can format via existing `render_resolve_errors`. Body: `apply_builtin_defaults(layer.clone())`, then call `resolve_project`, `resolve_workflow`, `resolve_run` into a shared `Vec<ResolveError>`. Return `Ok(Self { ... })` if `errors.is_empty()`, else `Err(errors)`.
- Add `WorkflowSettings::combined_labels(&self) -> HashMap<String, String>` — extend `project.metadata`, then `workflow.metadata`, then `run.metadata` (later wins on key conflict). Match today's `combined_labels` free fn ordering exactly.
- Do NOT yet delete `*_into` methods or `Resolver`. This unit is purely additive.

**Patterns to follow:**
- `resolve_*_from_file` helpers in `lib/crates/fabro-config/src/resolve/mod.rs` for the `Result<_, Vec<ResolveError>>` return shape and the per-namespace inline pattern.
- For the `combined_labels` ordering, today's free fn in `lib/crates/fabro-workflow/src/operations/create.rs:299-304`.

**Test scenarios:**
- Happy path: `SettingsLayer::default()` resolves successfully. (Unlike `resolve_server` — which requires explicit `server.auth.methods` per `tests/resolve_root.rs:8` — the workflow-only namespaces have all required defaults in `defaults.toml`.)
- Happy path: layer with `project.metadata`, `workflow.metadata`, `run.metadata` produces `combined_labels` containing the union, with later sections winning on key conflict.
- Error path: layer with invalid `run.sandbox.provider` (e.g. `"not-a-provider"`) returns `Err(errors)` containing a `ResolveError` for `run.sandbox.provider`. (See existing fixture in `tests/resolve_root.rs:38`.)
- Error path: layer with multiple invalid fields *within* `run` (e.g. invalid `sandbox.provider` PLUS another resolve_run-emitting error — pick from existing run-resolve test fixtures during implementation) returns ALL of them in one `Err`. Proves R2 — the shared `errors` vec is being threaded through all `resolve_*` calls in `from_layer`. (Note: `resolve_project` and `resolve_workflow` cannot emit errors today — `lib/crates/fabro-config/src/resolve/{project,workflow}.rs` both ignore the `errors` vec — so a "two namespaces both error" test isn't constructible. The within-`run` test is the closest structural proof we can write.)

**Verification:**
- New tests pass.
- `WorkflowSettings` is exported from `fabro_config` and constructable from any test fixture.
- All three fields publicly accessible from outside `fabro_config` (smoke-test by reading `wf.run.execution.mode` from a test file).

---

- [x] **Unit 2: Delete `Resolver`; rewrite `*Settings::from_layer` and `resolve_*_from_file` to not depend on it**

**Goal:** Remove the `Resolver` indirection. Each consumer-bundle constructor and each standalone helper inlines `apply_builtin_defaults` + the per-namespace resolve.

**Requirements:** R2, R3, R5

**Dependencies:** Unit 1 (so `WorkflowSettings` exists; not strictly required for this unit, but Unit 3 depends on both and ordering is cleaner).

**Files:**
- Delete: `lib/crates/fabro-config/src/resolve/resolver.rs`
- Modify: `lib/crates/fabro-config/src/resolve/mod.rs` (remove `mod resolver;` and `pub use resolver::Resolver;`; rewrite the 6 `resolve_*_from_file` helpers + `resolve_storage_root` to inline)
- Modify: `lib/crates/fabro-config/src/context.rs` (rewrite `ServerSettings::from_layer` and `UserSettings::from_layer` without `Resolver`)
- Modify: `lib/crates/fabro-config/src/lib.rs` (remove `Resolver` from re-exports)

**Approach:**
- Each `resolve_*_from_file(file)` becomes:
  ```
  let layer = apply_builtin_defaults(file.clone());
  let mut errors = Vec::new();
  let value = resolve_<ns>(&layer.<ns>.clone().unwrap_or_default(), &mut errors);
  if errors.is_empty() { Ok(value) } else { Err(errors) }
  ```
- `resolve_storage_root(file)` becomes a thin extraction from a defaulted layer (same logic as today's `Resolver::storage_root()`).
- `ServerSettings::from_layer` and `UserSettings::from_layer` build a defaulted layer once, accumulate errors from both their namespaces into one `Vec<ResolveError>`. Preserves today's "errors from BOTH namespaces" behavior — verify with existing tests in `tests/resolve_server.rs` and `tests/resolve_cli.rs`.

**Patterns to follow:**
- Today's `Resolver::server_into(&mut errors)` body shows the inline shape per namespace.
- Today's `ServerSettings::from_layer` shows the dual-namespace error accumulation pattern.

**Test scenarios:**
- Happy path: existing `tests/resolve_server.rs`, `tests/resolve_cli.rs`, `tests/resolve_run.rs`, `tests/resolve_workflow.rs`, `tests/resolve_project.rs`, `tests/resolve_features.rs`, `tests/defaults.rs`, `tests/resolve_root.rs` all pass without modification (proves no behavior change for any consumer).
- Edge case: `ServerSettings::from_layer` with multiple errors *within* `server` (e.g. invalid `listen.address` AND invalid `ip_allowlist` CIDR — see existing fixture pattern in `tests/resolve_root.rs:22`) returns ALL errors in one `Err`. Proves the shared `errors` vec is threaded through. (`resolve_features` cannot emit errors per `lib/crates/fabro-config/src/resolve/features.rs:5`, so the "errors from both namespaces" pairing isn't constructible — within-namespace multi-error is the closest structural proof.)

**Verification:**
- `cargo build -p fabro-config` clean.
- `cargo nextest run -p fabro-config` green with no test changes.
- `cargo nextest run -p fabro-config-tests` (if separate) or whatever test target covers `tests/` — green.
- `grep -rn "Resolver\b" lib/crates/fabro-config/src` returns no hits except in deleted file.

---

- [x] **Unit 3: Migrate `create_run` to `WorkflowSettings`; add `storage_root` parameter; delete `ResolvedSettingsTree` + `resolve_settings_tree` + `combined_labels` free fn**

**Goal:** `create_run` consumes `WorkflowSettings` for workflow-owned config and `storage_root` from the caller for deployment-owned config. `WorkflowSettings::from_layer` failures keep today's `; `-joined `render_resolve_errors` format (R6). Storage-root interpolation failures move out to the HTTP handler with new error attribution (R5).

**Requirements:** R1, R5, R6, R7

**Dependencies:** Unit 1 (needs `WorkflowSettings`), Unit 2 (needs `Resolver` gone — though Unit 3 could technically land before Unit 2).

**Files:**
- Modify: `lib/crates/fabro-workflow/src/operations/create.rs`
  - Add `storage_root: PathBuf` parameter to `pub async fn create(...)`.
  - Delete `struct ResolvedSettingsTree` (lines 63-68).
  - Delete `fn resolve_settings_tree` (lines 287-297).
  - Delete `fn combined_labels` (lines 299-304).
  - Replace `let resolved_settings = resolve_settings_tree(&settings)?;` (line 116) with `WorkflowSettings::from_layer(&settings).map_err(|errors| Error::Precondition(render_resolve_errors(&errors)))?`.
  - Replace lines 118-127 (today's storage_root resolution + Storage::new call): use the `storage_root` parameter directly. No more `InterpString::resolve(env)` here — caller did it.
  - Pass `settings` (unmodified) into `PersistCreateOptions`. **Do NOT overwrite `settings.server.storage.root`** — see Key Technical Decisions: persisted layer is the request-derived/materialized artifact (post-`materialize_run`); `record.run_dir` is the path-of-truth.
  - Replace `combined_labels(&resolved_settings)` (line 166) with `resolved_settings.combined_labels()`.
  - Imports: remove `Resolver` if directly imported; remove `HashMap` import if no longer used; add `PathBuf` if not already imported.
- Modify: `lib/crates/fabro-server/src/server.rs:4019` — production caller. Resolve `storage_root` from the live `ServerSettings` and map any interpolation failure to a transport error. **Do NOT use `state.server_storage_dir()`** — that helper panics on resolution failure (`server.rs:659`), which would replace today's `Error::Precondition` graceful-failure path with a process abort. The handler must do its own `resolve_interp_string` (or equivalent) and map errors to an `ApiError` shape used elsewhere in the file.
- Modify: `lib/crates/fabro-workflow/src/operations/start.rs:1008,1190` — test helpers (`persisted_workflow` and the bundled-workflow test). Pass a temp dir or fixture path.
- Audit and modify: any other callers of `fabro_workflow::operations::create::create()` in workspace tests. Run `grep -rn "operations::create(\|workflow::operations::create" lib/crates/fabro-workflow/tests/ lib/crates/fabro-server/tests/` during implementation.

**Approach:**
- Production caller pattern (sketched — verify error constructor and locking shape during implementation):
  ```
  // Read the live ServerSettings (Arc<ServerSettings>) and resolve env-var template
  let storage_root = match resolve_interp_string(&state.server_settings().server.storage.root) {
      Ok(s)  => PathBuf::from(s),
      Err(e) => return ApiError::<chosen-shape>(format!("storage root: {e}")).into_response(),
  };
  let created = match Box::pin(operations::create(state.store.as_ref(), create_input, storage_root)).await { ... };
  ```
  This preserves today's "interpolation failure becomes a graceful HTTP error" semantics. The exact `ApiError` constructor (e.g. `bad_request`, `internal`, or a new variant) is implementer's choice — pick whatever matches today's error shape for similar request-time validation failures in this file. `resolve_interp_string` already exists in the same module (used by `server_storage_dir`).
- Test helpers can use `tempfile::tempdir()` or a hardcoded path; pass `tmp.path().to_path_buf()` (or similar) as the third argument.
- `validate_sandbox_provider` at line 306 still uses `fabro_config::resolve_run_from_file(&resolved.settings)` — leave it. Different code path, same as today.

**Patterns to follow:**
- Today's `to_error` closure in `create.rs:289-290` for the `render_resolve_errors` mapping shape.
- `Error::Precondition` is the established envelope for create-time settings failures.

**Behavior change in scope (limited to R7 signature change):**
- `pub async fn create()` gains a `storage_root: PathBuf` parameter. Three production/test caller sites + workspace test audit.
- **No new resolve-time validation surfaces.** The bundle now resolves only `project`/`workflow`/`run`, exactly as today's `ResolvedSettingsTree` did. Server-ops fields (`auth`, `listen`, `ip_allowlist`, `integrations`) are NOT touched by this PR; they remain owned by the live `ServerSettings`.
- The InterpString env-var resolution that today happens inside `create()` (lines 119-126) moves to the caller. fabro-server today already has plenty of `InterpString::resolve` patterns to mirror.

**Test scenarios:**
- Happy path: existing `create_run` tests (in `create.rs`'s `mod tests`, in `lib/crates/fabro-workflow/tests/`, in `lib/crates/fabro-server/tests/`) pass after caller updates.
- Happy path: `combined_labels()` method produces the same map as today's free fn for the same fixture (proves ordering preservation).
- Error path: `SettingsLayer` with malformed `run.sandbox.provider` causes `create_run` to fail with the expected `; `-joined message (proves R6 format preservation).
- Edge case: caller passes a non-existent `storage_root` — `create_run` either creates the dir (matches today's behavior via `Storage::new`) or fails with the same error today's `Storage::new` would produce. Verify behavior is identical to today.

**Verification:**
- `cargo build -p fabro-workflow` clean.
- `cargo build -p fabro-server` clean.
- `cargo nextest run -p fabro-workflow` green.
- `cargo nextest run -p fabro-server` green (HTTP handler still works end-to-end).
- `grep -rn "ResolvedSettingsTree\|resolve_settings_tree" lib/` returns no hits.

---

- [x] **Unit 4: Workspace build + lint sweep**

**Goal:** Confirm no caller across the workspace still references deleted symbols.

**Requirements:** R5

**Dependencies:** Units 1-3.

**Files:** None modified directly; this unit is verification only.

**Approach:**
- `cargo build --workspace` from repo root.
- `cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings` (per CLAUDE.md).
- `cargo nextest run --workspace`.
- `grep -rn "fabro_config::Resolver\|ResolvedSettingsTree\|resolve_settings_tree" lib/ apps/` returns no hits.
- `cargo +nightly-2026-04-14 fmt --check --all`.

**Test expectation:** none — pure verification unit. No new test scenarios; existing tests must pass unchanged.

**Verification:**
- All commands green.
- No grep hits for deleted symbols.

## System-Wide Impact

- **Interaction graph:** `create_run`'s signature gains a `storage_root: PathBuf` parameter. ~3 production/test callers updated to pass it. No new resolve-time validation runs anywhere — the bundle resolves the same namespaces today's `ResolvedSettingsTree` did.
- **Error propagation:** `Server`/`UserSettings` error envelopes unchanged. `WorkflowSettings::from_layer` returns `Result<Self, Vec<ResolveError>>` (a new shape, asymmetric with the other two bundles); `create_run` formats it via `render_resolve_errors` to keep its user-visible error message format identical to today (`; `-joined). See R6. The InterpString-resolution error path that today lives inside `create_run` moves up to the caller — same error class, different attribution.
- **State lifecycle risks:** None — pure refactor, no persistent state touched.
- **API surface parity:** `fabro_config` public API loses `Resolver` (and the `*_into` methods accessible through it). Gains `pub struct WorkflowSettings`. All other re-exports unchanged. `render_resolve_errors` stays public (used by `create_run` and others). `fabro_workflow::operations::create::create()` signature changes (one new parameter).
- **Integration coverage:** Existing tests in `lib/crates/fabro-config/tests/` exercise both single-namespace (`resolve_*_from_file`) and multi-namespace (`*Settings::from_layer`) paths; they cover the integration shape.
- **Server-ops authority preserved:** No part of this PR re-resolves server-ops fields (`auth`, `listen`, `ip_allowlist`, `integrations`, `slatedb`, `logging`) per-request. The live `ServerSettings` (owned by `fabro-server` at boot) remains the single authority. `WorkflowSettings` doesn't pretend to hold them.
- **Unchanged invariants:** `*Layer` and `*Namespace` types in `fabro-types` — untouched. `apply_builtin_defaults` + `defaults.toml` — untouched. Per-namespace `resolve_*` function bodies — untouched. Consumer field-access patterns (`ctx.user_settings().cli.output.verbosity`) — untouched. `Server`/`UserSettings::from_layer` signatures — untouched.

## Risks & Dependencies

| Risk | Mitigation |
|------|------------|
| `create_run` signature change misses a workspace test caller; build break | Audit step listed in Unit 3 Files (`grep -rn "operations::create("`). Compiler will catch all production paths. |
| fabro-server's storage_root resolution call site fails with a new error class (env var missing, parse error) where today the failure happened inside `create_run` | Same error semantics; just attributed to the caller. Mention in PR description so reviewers don't read it as a regression. |
| One extra `apply_builtin_defaults(layer.clone())` per bundle on the create path vs. shared-via-Resolver | Acceptable — `create_run` is human-paced. Not in any hot path. |
| `combined_labels` method ordering subtly differs from free fn (both extend project → workflow → run) | Match today's free-fn ordering exactly. Existing tests will catch label-ordering regressions. |
| Asymmetric `from_layer` return type (`Result<Self, Vec<ResolveError>>` for `Workflow`; `fabro_config::Result<Self>` for `Server`/`User`) reads as inconsistency | Documented as a Key Technical Decision; reason is preserving `create_run`'s error-message format. Future PR could converge all three when the format change is intentional. |
| `record.settings.server.storage.root` (the request-derived/materialized artifact) and `record.run_dir` (where the run actually lives) can disagree — e.g. user submitted env-template `"{{ env.X }}"` that resolved at runtime to a different path than the parameter | Documented design (see Key Technical Decisions): persisted settings = request-derived/materialized artifact; `run_dir` = path-of-truth. Audit grep verifies no current consumer reads `settings.server.storage.root` from a persisted record beyond the line being removed. Future readers must use `run_dir` for paths. |
| HTTP handler's storage-root resolution failure path differs from today's `Error::Precondition` text | Documented behavior change — moves from a workflow-side `Error::Precondition` to a transport-side `ApiError`. Same failure class (graceful HTTP error), different attribution and error-message text. Mention in PR description. |

## Documentation / Operational Notes

- No user-facing docs to update (internal refactor).
- No migration scripts, feature flags, or rollout concerns.
- One signature change: `fabro_workflow::operations::create::create()` gains `storage_root: PathBuf`. Three caller sites + workspace test audit. No resolved-settings semantics change.
- Net code: ~150 lines deleted, ~50 added (smaller WorkflowSettings = smaller diff).

## Sources & References

- Origin: design conversation in this session.
- Related code: `lib/crates/fabro-config/src/resolve/resolver.rs` (introduced commit 52d24531d).
- Follow-up plan: `docs/plans/2026-04-23-002-refactor-combine-trait-uv-pattern-plan.md` (PR 2 — Combine trait + derive).
