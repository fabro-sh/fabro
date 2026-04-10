---
date: 2026-04-09
status: complete
topic: settings-toml-redesign
predecessor: docs/plans/2026-04-09-settings-toml-redesign-handoff-3.md
---

# Settings TOML Redesign — Handoff 4 (Stage 6 complete)

## TL;DR

**Stage 6 is done.** Every substage from 6.1 through 6.6j is
complete. The legacy flat `Settings` parse tree, its `Combine`-driven
layering, the `bridge_to_old` seam, the seven runtime type modules,
and the transitional `v2/` subdirectory are all deleted. The
`fabro_types::settings` module is now flat and v2-native.

3,758 workspace tests pass. `cargo fmt --check --all`,
`cargo clippy --workspace -- -D warnings`, and
`cd apps/fabro-web && bun run typecheck && bun test && bun run build`
are all green.

There is no remaining Stage 6 work to hand off. Any follow-ups from
here are *new* decisions (see "Deferred / new work" below).

## What landed in this wrap-up session

Fifteen commits on `main` on top of handoff-3's starting point:

```
c625747e0 refactor(settings): stage 6.5b sweep ::v2:: prefix out of consumers
d82d167f0 refactor(settings): stage 6.6g rewrite auth resolver for v2
15b799fb3 refactor(settings): stage 6.3b + 6.5b finish — delete last legacy server types and flatten v2/
3ac7ab903 refactor(settings): stage 6.3b shrink server runtime types + delete Combine
7f9640aac refactor(settings): stage 6.3b promote run runtime types + delete to_runtime
6df8bbeb3 refactor(settings): stage 6.3b promote sandbox runtime types into fabro-sandbox
38dacb874 refactor(settings): stage 6.3b promote mcp runtime types into fabro-mcp
2016c8e94 refactor(settings): stage 6.3b promote hook + project runtime types
db45511ff refactor(settings): stage 6.3b promote user runtime types into consumers
```

Each commit is small, test-green, and self-contained. The migration
walked one consumer crate at a time through the consumer migration
map from handoff-3.

### Stage 6.3b complete — runtime type module tree deletion

Every consumer that used to import from
`fabro_types::settings::{hook, mcp, project, run, sandbox, server,
user}` now owns its runtime types locally:

| Old module | Runtime types moved to |
|---|---|
| `hook.rs` | `fabro-hooks/src/config.rs` |
| `mcp.rs` | `fabro-mcp/src/config.rs` |
| `sandbox.rs` | `fabro-sandbox/src/config.rs` |
| `run.rs` → `PullRequestSettings`, `MergeStrategy`, `ArtifactsSettings` | `fabro-workflow/src/config.rs` |
| `user.rs` → `OutputFormat`, `PermissionLevel` | `fabro-agent/src/cli.rs` |
| `user.rs` → `ClientTlsSettings` | `fabro-cli/src/user_config.rs` |
| `server.rs` → `ApiAuthStrategy`, `ApiSettings`, `TlsSettings` | `fabro-server/src/jwt_auth.rs` (temporarily; see 6.6g) |
| `project.rs` | deleted (`ProjectSettings` was dead) |

Dead types deleted outright (no consumers remained):

- From `run.rs`: `LlmSettings`, `SetupSettings`, `CheckpointSettings`,
  `GitHubSettings`.
- From `user.rs`: `ExecSettings`, legacy `ServerSettings`.
- From `server.rs`: `AuthProvider`, `AuthSettings`, `GitProvider`,
  `GitSettings`, `GitAuthorSettings`, `WebSettings`, `WebhookSettings`,
  `WebhookStrategy`, `SlackSettings`, `FeaturesSettings`,
  `LogSettings`, `ArtifactStorageBackend`, `ArtifactStorageSettings`.

Narrow v2→runtime bridge helpers that used to live in
`fabro-types::settings::v2::to_runtime` moved alongside their target
types:

- `bridge_hook` → `fabro_hooks::config::bridge_hook`
- `bridge_mcp_entry` / `bridge_mcps` → `fabro_mcp::config::*`
- `bridge_sandbox` / `bridge_worktree_mode` → `fabro_sandbox::config::*`
- `bridge_pull_request` / `bridge_merge_strategy` / `bridge_run_artifacts`
  → `fabro_workflow::config::*`

`fabro-types/src/settings/v2/to_runtime.rs` is deleted.

### `Combine` trait machinery deleted

- `lib/crates/fabro-types/src/combine.rs` — deleted.
- `pub mod combine;` / `pub use fabro_macros::Combine;` removed from
  `fabro-types/src/lib.rs`.
- `#[proc_macro_derive(Combine)]` and its `syn::{Data, DeriveInput,
  Fields}` imports removed from `fabro-macros/src/lib.rs`. The
  `e2e_test` attribute macro is untouched.

### Stage 6.5b complete — v2 directory flatten

- `git mv lib/crates/fabro-types/src/settings/v2/*.rs
  lib/crates/fabro-types/src/settings/`
- `lib/crates/fabro-types/src/settings/v2/` — deleted.
- `settings/mod.rs` absorbs the old `v2/mod.rs` declarations and
  re-exports (accessors, cli, duration, features, interp, model_ref,
  project, run, server, size, splice_array, tree, version, workflow).
- A final workspace sweep rewrote every
  `fabro_types::settings::v2::*` import path to
  `fabro_types::settings::*` — 53 files, 10 crates.
- The transitional `pub mod v2 { pub use super::*; }` alias is also
  deleted; there is no `::v2::` namespace anywhere.

### Stage 6.6g complete — auth resolver v2-native

- `resolve_auth_mode_with_lookup` rewritten to take `&SettingsFile`
  directly and walk
  `settings.server.auth.api.{jwt,mtls}` +
  `settings.server.auth.web.allowed_usernames` +
  `settings.server.listen.tls`.
- Strategy presence uses the "subtree present unless `enabled = false`"
  semantics from R52.
- The `ApiSettings` and `ApiAuthStrategy` shim types and the
  `build_legacy_api_settings` helper in `serve.rs` are **deleted**
  (~60 LOC).
- `TlsSettings` survives as a local helper in
  `fabro-server/src/jwt_auth.rs` with a
  `TlsSettings::from_settings(&SettingsFile)` constructor that
  projects `server.listen.tls` into the resolved triple. It's only
  used by `tls.rs`'s rustls builder and the mTLS integration test.
- `serve.rs`'s bootstrap now calls the new resolver directly.

## Final status of every Stage 6 substage

| Substage | Status |
|---|---|
| 6.1 — Migrate consumers off flat `Settings` | ✅ COMPLETE (predecessor session) |
| 6.2 — Delete `bridge_to_old` seam | ✅ COMPLETE (predecessor session) |
| 6.3 — Delete legacy flat `Settings` helpers | ✅ COMPLETE (predecessor session) |
| 6.3b — Delete `Settings` struct + 7 runtime type modules | ✅ **COMPLETE** |
| 6.4 — Delete `fabro-config` re-export shims | ✅ COMPLETE (predecessor session) |
| 6.5 — Promote v2 types to top-level `settings::*` re-exports | ✅ COMPLETE (predecessor session) |
| 6.5b — Flatten `settings/v2/*.rs` → `settings/*.rs` | ✅ **COMPLETE** |
| 6.6a/b — Design allow-list DTOs in OpenAPI | ✅ COMPLETE (this session, prior) |
| 6.6c — Regenerate Rust + TS clients | ✅ COMPLETE (this session, prior) |
| 6.6d — Rewrite `get_server_settings` with redaction | ✅ COMPLETE (this session, prior) |
| 6.6e — Rewrite `get_run_settings` handler | ✅ COMPLETE (this session, prior) |
| 6.6f — Migrate `retrieve_server_settings` in fabro-cli | ✅ COMPLETE (this session, prior) |
| 6.6g — Rewrite auth resolver for v2 | ✅ **COMPLETE** |
| 6.6h — Update fabro-web `workflow-detail.tsx` DTO literal | ✅ COMPLETE (this session, prior) |
| 6.6i — Migrate demo routes to v2 | ✅ COMPLETE (this session, prior) |
| 6.6j — Rewrite `setup_register` web_auth flow | ✅ **COMPLETE** (was already v2-writing after predecessor session; TODO-12's dead `settings_file` binding turned out to not exist anymore) |

## Scoped TODO status (from handoff-2)

| TODO | Subject | Final status |
|---|---|---|
| TODO-1 | `legacy_settings_to_v2` shim in fabro-cli | ✅ Deleted |
| TODO-2 | `build_legacy_api_settings` in fabro-server | ✅ Deleted (6.6g) |
| TODO-3 | `get_server_settings` emits raw v2 JSON | ✅ Replaced with redacted DTO |
| TODO-4 | `web_auth.rs` register flow rewrite | ⚠️ **Partial** — the hand-rolled TOML writer now emits v2 shape and is tested. Comment/formatting preservation on round-trip is a nice-to-have left for a follow-up pass; see "Deferred / new work" |
| TODO-5 | `check_crypto` in diagnostics | ✅ Walks v2 listen TLS (predecessor session) |
| TODO-6 | Dead `Combine` trait | ✅ Deleted |
| TODO-7 | Fallback chain bug preserved | ⚠️ **Unchanged** — still preserves pre-migration behavior; needs the runtime `ModelRegistry` implementation |
| TODO-8 | V2 doesn't model `goal_file` | ⚠️ **Unchanged** — needs requirements-level decision |
| TODO-9 | Legacy Settings struct dead weight | ✅ Deleted (6.3b) |
| TODO-10 | Demo routes still emit legacy shape | ✅ Rewritten as v2 JSON |
| TODO-11 | Unused `Settings` import in config tests | ✅ Removed |
| TODO-12 | Unused `settings_file` binding in web_auth.rs | ✅ No such binding exists (already cleaned up) |

## Deferred / new work

These are *not* Stage 6 items. They are open questions or new
improvements that came up during the work and are worth considering
separately.

1. **`setup_register` comment-preserving TOML writes (ex-TODO-4).**
   The current hand-rolled writer uses `toml` + `toml::to_string_pretty`
   which loses comments and formatting on round-trip. A fix would use
   `toml_edit::DocumentMut` (new workspace dependency). Alternatively,
   the whole GitHub App registration flow might be better driven from
   fabro-web as a dedicated `/api/v1/setup` endpoint instead of living
   in `setup_register`.

2. **Runtime `ModelRegistry` for `ModelRef::resolve` (ex-TODO-7).**
   `fabro_types::settings::model_ref::ModelRef::resolve` still takes
   a `&dyn ModelRegistry` and errors on ambiguous bare tokens. There's
   no runtime implementation against `fabro-model::Catalog`, so the
   `resolve_fallback_chain` helper in
   `fabro-workflow/src/operations/start.rs` still groups all fallbacks
   under the empty-string provider key and never matches. This
   preserves pre-migration behavior exactly but isn't the correct
   fallback behavior. Open question from predecessor handoff #4.

3. **`run.goal_file` schema support (ex-TODO-8).** V2 has `run.goal`
   as an `InterpString` but no separate `run.goal_file`. The legacy
   CLI `--goal-file` flag can't be expressed in v2. Either add a
   `run.goal_file` subfield (requirements update) or route file-based
   goals through the workflow-manifest layer.

4. **Fail-closed server auth posture (open question #3).** The
   requirements doc R52/R53 specifies that startup should refuse to
   run if `server.auth` is absent or resolves to no enabled API/web
   strategies, with demo and test helpers opting in explicitly. The
   current `resolve_auth_mode_with_lookup` just logs a warning and
   builds an `AuthMode::Strategies(empty)`. A follow-up can tighten
   this — the hook point is already clean now that 6.6g landed.

5. **Post-layering env interpolation resolution pass (open question
   #2).** `InterpString::resolve` is still called at read time by
   each consumer that needs a concrete string. The requirements doc
   R79–R81 specifies a centralized pass under
   `fabro-config/src/interp_pass.rs` that runs once after layering.
   Not implemented in any handoff so far.

6. **OpenAPI freeform settings DTO vs formal allow-list DTO.** Stage
   6.6a/b chose to declare `ServerSettings` and `RunSettings` as
   `type: object, additionalProperties: true` freeform objects in the
   OpenAPI spec, pointing at the Rust `SettingsFile` type for the
   shape. This loses client-side type safety in TypeScript (the
   generated client returns `{ [key: string]: any }`). A follow-up
   could formalize the full v2 `SettingsFile` tree in OpenAPI yaml
   (tedious but not hard), or keep the loose shape and provide a
   hand-written TypeScript type declaration in
   `@qltysh/fabro-api-client` as a convenience.

7. **`TlsSettings` in `fabro-server/src/jwt_auth.rs`.** This 3-field
   struct is the last legacy-shaped leftover. It's technically owned
   by the right crate now, but putting it in `jwt_auth.rs` is a
   historical artifact — a dedicated `fabro-server/src/tls_config.rs`
   module would be a more natural home. Pure cleanup, no urgency.

## Running verification

```bash
cargo fmt --check --all
cargo build --workspace
cargo clippy --workspace -- -D warnings
ulimit -n 4096 && cargo nextest run --workspace

cd apps/fabro-web && bun run typecheck && bun test && bun run build
```

All green as of `c625747e0` on `main`: 3,758 tests passed / 0 failed
/ 182 skipped.

## Success criteria for Stage 6 (all resolved)

- [x] `git grep 'fabro_types::Settings\b'` returns zero hits.
- [x] `git grep 'bridge_to_old'` returns zero hits.
- [x] `lib/crates/fabro-types/src/settings/v2/` no longer exists as
      a subdirectory.
- [x] `lib/crates/fabro-types/src/combine.rs` is deleted.
- [x] `lib/crates/fabro-types/src/settings/{hook,mcp,project,run,
      sandbox,user}.rs` legacy runtime modules — deleted.
      `settings/server.rs` now exists as the *v2* server layer file
      (promoted from `v2/server.rs` in 6.5b).
- [x] `lib/crates/fabro-config/src/{hook,mcp,sandbox,server,run,user}.rs`
      deleted or reduced to helpers.
- [x] `docs/api-reference/fabro-api.yaml` `ServerSettings` and
      `RunSettings` schemas are not the legacy flat shape.
- [x] `lib/packages/fabro-api-client` and the Rust progenitor client
      are regenerated.
- [x] `apps/fabro-web/app/routes/workflow-detail.tsx` `workflowData`
      literal matches the new shape.
- [x] `cargo fmt` / `cargo build` / `cargo clippy -D warnings` /
      `cargo nextest run --workspace` / `bun run typecheck` /
      `bun test` / `bun run build` gates all green.

Stage 6 is closed. Next work should be driven by the deferred items
list above or by new requirements.
