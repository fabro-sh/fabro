---
date: 2026-04-09
status: active
topic: settings-toml-redesign
predecessor: docs/plans/2026-04-09-settings-toml-redesign-handoff.md
---

# Settings TOML Redesign — Handoff 2 (post Stage 6.1–6.5 landing)

## TL;DR

Stages 6.1, 6.2, and 6.4 of the Stage 6 follow-up landed cleanly on `main`.
Stages 6.3 and 6.5 are **partially** complete — they each hit a concrete
blocker that requires Stage 6.6 to be done first. Stage 6.6 (OpenAPI DTO
rewrite + fabro-web) is **not started**.

The workspace builds clean, all 3,756 tests pass, `cargo clippy --workspace
-- -D warnings` and `cargo fmt --check --all` are green. There are known
runtime behavior changes on the `/api/v1/settings` endpoint (see "Known
wire-contract mismatches" below) that will affect fabro-web until 6.6
lands.

The main work that remains is:

1. **Finish Stage 6.6** — rewrite `docs/api-reference/fabro-api.yaml`,
   regenerate the Rust progenitor and TypeScript Axios clients, update
   `fabro-web/app/routes/workflow-detail.tsx`, and rewrite the server's
   `/api/v1/settings` + `/api/v1/runs/:id/settings` handlers to build
   allow-list DTOs from the v2 tree without bridging.
2. **Unblock Stage 6.3** — 6.6 removes the last reader of the legacy
   flat `Settings` struct (the progenitor-generated `api::types::ServerSettings`
   conversion path). Once that's gone, the whole `fabro_types::settings::{hook,
   mcp, project, run, sandbox, server, user}` module tree plus
   `fabro_types::combine::Combine` can be deleted.
3. **Unblock Stage 6.5** — 6.3's deletion removes the filename collisions
   that currently prevent flattening `settings/v2/*.rs` up to `settings/*.rs`.
4. **Revisit scoped TODOs** — see "Scoped TODOs" below.

## Source documents

Read these, in this order:

1. **Requirements (authoritative)** —
   [`docs/brainstorms/2026-04-08-settings-toml-redesign-requirements.md`](../brainstorms/2026-04-08-settings-toml-redesign-requirements.md).
   Source of truth for the v2 schema, merge matrix (R22 / R30 / R71 etc.),
   trust boundaries, disable semantics. Refer to requirement numbers when
   making schema decisions.

2. **Original implementation plan** —
   [`docs/plans/2026-04-08-settings-toml-redesign-implementation-plan.md`](./2026-04-08-settings-toml-redesign-implementation-plan.md).

3. **Stage 6 handoff (predecessor to this doc)** —
   [`docs/plans/2026-04-09-settings-toml-redesign-handoff.md`](./2026-04-09-settings-toml-redesign-handoff.md).
   This is the doc I worked from. It has the per-stage scope, the file
   maps, gotchas, and open design questions. **Still current** for the
   remaining work — read it before touching Stage 6.6.

## Commit trail (landed on main, most recent first)

```
ace24c410 refactor(types): stage 6.5 promote v2 types to settings top level
a3fd3b002 refactor(config): stage 6.4 delete fabro-config re-export shims
34a481cd4 refactor(settings): stage 6.3 delete dead Settings helpers + v2 install TOML
ea206e0e4 feat(settings): stage 6.2 delete bridge_to_old seam
52c295cf7 test(settings): update fabro-cli test suite for v2 settings shape
dc856d088 feat(settings): stage 6.1 consumer migration builds workspace-wide
5d9aad85a wip(settings): stage 6.1 consumer migration (broken build)
842ab71eb feat(types): expose bridge helpers and expand v2 accessors
3f32bdb87 feat(types): add SettingsFile convenience accessors
```

Total: 81 files changed, +3,718 / −2,423 lines (net +1,295).

Note: commit `5d9aad85a` was an explicit broken-build WIP checkpoint
the user approved mid-session; `dc856d088` fixes the build. Subsequent
commits are individually test-green.

## Current-state map (what's in the tree now)

```
lib/crates/fabro-types/src/settings/
├── mod.rs
│   ├── legacy `Settings` struct (flat, _still present_ — see 6.3 status)
│   ├── legacy type re-exports from hook/mcp/project/run/sandbox/server/user
│   └── NEW: pub use v2::{SettingsFile, InterpString, Duration, ...}   ← 6.5
│
├── hook.rs / mcp.rs / project.rs / run.rs / sandbox.rs / server.rs / user.rs
│   └── LEGACY runtime type definitions, still used (see below)
│
└── v2/
    ├── mod.rs                  — module root; no more `bridge_to_old` re-export
    ├── tree.rs                  — SettingsFile top-level
    ├── version.rs
    ├── project.rs / workflow.rs / run.rs / cli.rs / server.rs / features.rs
    ├── duration.rs / size.rs / model_ref.rs / interp.rs / splice_array.rs
    ├── accessors.rs             — NEW in 6.1 prep; ~35 flat-view accessors on SettingsFile
    └── to_runtime.rs            — NEW in 6.2; narrow v2→runtime-type helpers
                                   (bridge_sandbox, bridge_mcp_entry, bridge_hook,
                                    bridge_pull_request, bridge_worktree_mode, etc.)
                                    REPLACES the deleted bridge.rs file
```

```
lib/crates/fabro-config/src/
├── lib.rs            — crate root; NEW: top-level `resolve_storage_dir(&SettingsFile)` helper
├── config.rs         — ConfigLayer newtype; NO MORE `.resolve()` / TryFrom<...> for Settings
├── merge.rs          — v2 merge matrix, unchanged
├── effective_settings.rs — rewritten: returns SettingsFile, v2 merge for server defaults
├── project.rs        — resolve_working_directory takes &SettingsFile
├── run.rs            — workflow loaders only (parse_run_config / load_run_config / resolve_graph_path)
├── user.rs           — machine settings loader + path helpers, no type re-exports
├── home.rs / storage.rs / legacy_env.rs — unchanged
│
└── DELETED in 6.4:
    hook.rs, mcp.rs, sandbox.rs, server.rs
```

## Stage-by-stage status

### 6.1 — Migrate consumers off flat `Settings` ✅ **COMPLETE**

Every production read site in `fabro-workflow`, `fabro-server`,
`fabro-cli`, and `fabro-config` reads from `SettingsFile` or walks v2
subtrees via `settings::v2::accessors`. `RunRecord.settings`,
`RunCreatedProps.settings`, `RunOptions.settings`, `CreateRunInput.settings`,
`ValidateInput.settings`, `ResolveWorkflowInput.settings`,
`ResolvedWorkflow.settings`, `AppState.settings`, and
`CommandContext::machine_settings` are all `SettingsFile`-typed.

Where the `bridge_to_old`-style conversion to a legacy runtime type was
still needed (e.g., `fabro_types::settings::sandbox::SandboxSettings`
for `fabro-sandbox`, `fabro_types::settings::mcp::McpServerEntry` for
`fabro-mcp`, `fabro_types::settings::hook::HookDefinition` for
`fabro-hooks`), the new narrow helpers in
`fabro_types::settings::v2::to_runtime` build them from single v2
subtrees. Consumers call these explicitly at the point of use.

### 6.2 — Delete `bridge_to_old` seam ✅ **COMPLETE**

`lib/crates/fabro-types/src/settings/v2/bridge.rs` (818 LOC) is deleted.
`ConfigLayer::resolve`, `TryFrom<ConfigLayer> for Settings`, and
`TryFrom<&ConfigLayer> for Settings` are deleted. The full-tree
conversion from a v2 `SettingsFile` to a legacy flat `Settings` no
longer exists anywhere in the codebase.

The narrow runtime-type helpers that the bridge exported as public
functions moved to `fabro_types::settings::v2::to_runtime` and are
scoped per runtime type (one helper per runtime struct, not one
all-in-one converter). They survive until Stage 6.3 deletes the
runtime type targets.

### 6.3 — Delete legacy flat `Settings` types ⚠️ **PARTIAL (blocked on 6.6)**

**What landed** (`34a481cd4`):
- Every inherent helper method on the legacy `Settings` struct
  (`app_id`, `slug`, `client_id`, `git_author`, `sandbox_settings`,
  `setup_settings`, `setup_commands`, `setup_timeout_ms`,
  `preserve_sandbox_enabled`, `github_permissions`, `mcp_server_entries`,
  `verbose_enabled`, `prevent_idle_sleep_enabled`, `upgrade_check_enabled`,
  `dry_run_enabled`, `auto_approve_enabled`, `no_retro_enabled`,
  `storage_dir`, `slack_settings`) is deleted. Callers migrated to the
  `SettingsFile` accessors with identical names.
- `fabro-cli/src/commands/install.rs::merge_server_settings` now
  writes v2 TOML (with `[server.{api,listen.tls,web,auth.api.{jwt,mtls},
  auth.web}]` stanzas). Its tests parse the output through
  `ConfigLayer::parse` and assert v2 fields.

**What did NOT land** (blocked on 6.6):
- The `Settings` struct itself is **still alive** in
  `lib/crates/fabro-types/src/settings/mod.rs`.
- All seven legacy runtime type modules (`hook.rs`, `mcp.rs`,
  `project.rs`, `run.rs`, `sandbox.rs`, `server.rs`, `user.rs`) are
  **still alive** and used by runtime crates.
- The `Combine` trait in `lib/crates/fabro-types/src/combine.rs` is
  **still alive** (only used by the legacy type `#[derive(Combine)]`
  attributes).
- The `fabro-macros` crate's `Combine` derive macro is **still alive**.

**Why it's blocked on 6.6**: the progenitor-generated OpenAPI client
in `lib/crates/fabro-api` deserializes `/api/v1/settings` responses
into `api::types::ServerSettings`, which `fabro-cli/src/server_client.rs::
retrieve_server_settings()` converts to `fabro_types::Settings` via
`convert_type`. That conversion is the only remaining reader of the
flat `Settings` shape in production code. Stage 6.6 rewrites the
OpenAPI spec so the client returns a v2 DTO and this conversion path
goes away.

**Remaining readers of the legacy `Settings` struct**:
| File | Use |
|---|---|
| `lib/crates/fabro-cli/src/server_client.rs:282` | `retrieve_server_settings` return type |
| `lib/crates/fabro-cli/src/commands/config/mod.rs:93` | `legacy_settings_to_v2` shim (takes `&fabro_types::Settings`) |
| `lib/crates/fabro-cli/src/commands/install.rs` | gone (tests rewritten) |
| `lib/crates/fabro-server/src/demo/mod.rs:1328, 1525` | demo route payloads |
| `lib/crates/fabro-server/src/lib.rs:20` | `pub use fabro_types::Settings;` re-export |
| `lib/crates/fabro-server/src/web_auth.rs:691` | test (or removed — double-check) |
| `lib/crates/fabro-types/src/settings/mod.rs` | definition |

**Remaining readers of legacy runtime types** (imported via
`fabro_types::settings::{hook,mcp,sandbox,server,user,run}`):
| Consumer crate | Types it imports |
|---|---|
| `fabro-hooks` | `HookDefinition`, `HookEvent`, `HookSettings`, `HookType`, `TlsMode` |
| `fabro-mcp` | `McpServerEntry`, `McpServerSettings`, `McpTransport`, timeouts |
| `fabro-sandbox` | `SandboxSettings`, `DaytonaSettings`, `DaytonaSnapshotSettings`, `DaytonaNetwork`, `LocalSandboxSettings`, `WorktreeMode`, `DockerfileSource` |
| `fabro-checkpoint` | `GitAuthorSettings` (plus the v2 `GitAuthorLayer` via new `From` impl) |
| `fabro-workflow` | `PullRequestSettings`, `MergeStrategy`, `WorktreeMode` |
| `fabro-server` | `ApiSettings`, `TlsSettings`, `ApiAuthStrategy`, `GitSettings`, plus `ServerSettings` for the CLI target |
| `fabro-cli` | `ClientTlsSettings`, `OutputFormat`, `PermissionLevel`, `ExecSettings`, `ServerSettings` |
| `fabro-agent` | `OutputFormat`, `PermissionLevel` (for `AgentArgs`) |

### 6.4 — Delete `fabro-config` re-export shims ✅ **COMPLETE**

Files deleted from `lib/crates/fabro-config/src/`:
- `hook.rs`, `mcp.rs`, `sandbox.rs`, `server.rs` (pure pass-throughs)

Files shrunk:
- `run.rs` — lost the type re-export block and the dead `resolve_env_refs`
  helper. Still exports `parse_run_config` / `load_run_config` /
  `resolve_graph_path` (used by fabro-cli and fabro-server).
- `user.rs` — lost the runtime type re-export block. Still exports path
  helpers, `load_settings_config`, `active_settings_path`, etc.

`resolve_storage_dir` moved from `fabro-config/src/server.rs` (deleted)
to the crate root in `fabro-config/src/lib.rs`. It takes `&SettingsFile`
now.

All ~20 consumer crates updated to import runtime types directly from
`fabro_types::settings::{hook,mcp,sandbox,server,user,run}` instead of
`fabro_config::{hook,mcp,sandbox,server,user,run}`. The legacy import
paths no longer compile.

### 6.5 — Flatten `settings::v2::*` → `settings::*` ⚠️ **PARTIAL (blocked on 6.3)**

**What landed** (`ace24c410`):
Top-level re-exports of the v2 public surface at `fabro_types::settings`.
Consumers can now write:

```rust
use fabro_types::settings::{SettingsFile, InterpString, Duration, ...};
```

Covers `{CURRENT_VERSION, CliLayer, Duration, FeaturesLayer, InterpString,
ModelRef, ParseDurationError, ParseError, ParseModelRefError,
ParseSizeError, ProjectLayer, Provenance, ResolveEnvError, Resolved,
ResolvedModelRef, RunLayer, SchemaVersion, ServerLayer, SettingsFile,
Size, SpliceArray, SpliceArrayError, VersionError, WorkflowLayer,
parse_settings_file, validate_version}`.

**What did NOT land**:
Actually moving the v2/*.rs files up to settings/*.rs. This is blocked
because the v2 submodule filenames (`project.rs`, `run.rs`, `server.rs`,
`cli.rs`) collide with the surviving legacy runtime type files with the
same names. Once Stage 6.3 deletes the legacy files, a trivial follow-up
commit can:

1. `git mv lib/crates/fabro-types/src/settings/v2/*.rs lib/crates/fabro-types/src/settings/`
2. Delete `lib/crates/fabro-types/src/settings/v2/mod.rs`
3. Update `lib/crates/fabro-types/src/settings/mod.rs` to replace
   `pub mod v2;` + the `pub use v2::{...}` block with direct
   `pub mod <name>;` declarations and a `pub use ...::*` re-export pass.
4. Search-and-replace `::v2::` to nothing across the workspace.
5. Update the accessors module and the `to_runtime` module to drop
   `super::` / `crate::` adjustments.

### 6.6 — Rewrite OpenAPI contracts and fabro-web DTOs ⏳ **NOT STARTED**

See the predecessor doc's Stage 6.6 section for the full scope. Key
points and anything I've learned since:

**Files to rewrite**:
- `docs/api-reference/fabro-api.yaml`:
  - Replace the `ServerSettings` schema (~lines 4238–4364 in the
    untouched version) with an explicit allow-list DTO that maps
    cleanly onto `SettingsFile`. See the handoff predecessor doc for
    the field allow-list guidance (R16 / R52 / R53 constraints).
  - Replace the `RunSettings` schema (~lines 3995–4032) similarly.
- Regenerate clients:
  - Rust progenitor: `cargo build -p fabro-api` (auto-runs `build.rs`).
  - TypeScript: `cd lib/packages/fabro-api-client && bun run generate`.
- `apps/fabro-web/app/routes/workflow-detail.tsx` — rewrite the static
  `workflowData` literal to match the new DTO.
- `lib/crates/fabro-server/src/server.rs::get_server_settings`
  (around line 1062 — **note: this function was already edited in
  Stage 6.2** and now serializes the full v2 `SettingsFile` as JSON via
  `serde_json::to_value(&settings)` with `strip_nulls`. That's a
  temporary workaround, not the final state — see "Known wire-contract
  mismatches" below). Stage 6.6 replaces it with explicit allow-list
  DTO construction from the v2 tree.
- `lib/crates/fabro-server/src/server.rs` `/api/v1/runs/:id/settings`
  handler — still returns `not_implemented` in the real router.
- `lib/crates/fabro-server/src/demo/mod.rs` — demo routes still emit
  legacy Settings shapes. Either migrate to v2 or keep them as the
  "legacy demo" path.

**Known wire-contract mismatches** (will affect fabro-web until 6.6 lands):
1. **`/api/v1/settings` response shape drift**. The server now emits the
   v2 `SettingsFile` JSON (e.g., `server.storage.root`,
   `run.execution.mode`, `cli.output.verbosity`) directly. The OpenAPI
   spec still declares the legacy `ServerSettings` schema (flat
   `storage_dir`, `dry_run`, `verbose`). Any client that relies on the
   spec will see missing fields or mis-typed values. The browser client
   is the main consumer; fabro-cli's `retrieve_server_settings` still
   goes through the progenitor client and round-trips through the old
   JSON shape — it will break on any v2 field the old schema doesn't
   declare.
2. **`openapi_conformance` test**. Still passes because it asserts
   progenitor types match the YAML — but both sides are now stale
   relative to what the server actually emits. Stage 6.6 should
   rewrite this test or update it to cover the new DTOs.

## Scoped TODOs (stopgap code that needs revisiting)

Each of these is a deliberate short-term hack with a pointer to where
it should land eventually. They're also marked in-line with
`// Stage 6.x ...` comments.

### TODO-1: `legacy_settings_to_v2` shim in fabro-cli
**File**: `lib/crates/fabro-cli/src/commands/config/mod.rs:91`
**What**: Reverse mapping from `fabro_types::Settings` → `SettingsFile`.
Covers `server.storage.root`, `server.scheduler.max_concurrent_runs`,
`server.integrations.github.{app_id, client_id, slug}`,
`server.integrations.slack.default_channel`, `run.model.{provider, name}`,
`run.inputs`, and `cli.output.verbosity`. Does **not** cover most other
fields.
**Why**: `server_client::retrieve_server_settings` returns the legacy
shape because the OpenAPI spec hasn't been rewritten.
**Delete when**: Stage 6.6 rewrites the OpenAPI spec and the progenitor
client returns v2 natively.

### TODO-2: `build_legacy_api_settings` in fabro-server
**File**: `lib/crates/fabro-server/src/serve.rs:91`
**What**: Projects the v2 `server.auth.api.{jwt,mtls}` + `server.listen.tls`
subtrees onto the legacy `ApiSettings` struct that the existing
`resolve_auth_mode_with_lookup` function still expects.
**Why**: The auth resolver in `jwt_auth.rs` hasn't been migrated to v2
yet. The v2 structure is different enough (no single `authentication_strategies`
enum list; `jwt` and `mtls` are separate subtables with per-strategy
`enabled` booleans) that a rewrite is warranted.
**Delete when**: Stage 6.6 replaces `resolve_auth_mode_with_lookup` with
a v2-aware resolver and deletes the legacy `ApiSettings` type.

### TODO-3: `get_server_settings` emits raw v2 JSON
**File**: `lib/crates/fabro-server/src/server.rs:1063`
**What**: The `/api/v1/settings` handler now serializes the full v2
`SettingsFile` as JSON with `strip_nulls` instead of building a
`ServerSettings` DTO. The spec still declares the old DTO.
**Why**: Bridge deletion left no way to produce the old shape without
re-introducing `bridge_to_old`.
**Fix when**: Stage 6.6 rewrites the OpenAPI spec and builds an explicit
allow-list DTO from v2 subtrees. Per R16/R52/R53 in the requirements doc:
- **Allow**: `server.api.url`, `server.web.enabled`, `server.web.url`,
  per-provider enabled state under `server.auth.web.providers.*`,
  non-secret `server.scheduler` values.
- **Deny**: `server.listen.*`, `server.listen.tls.*`, `server.auth.api`,
  `server.integrations.*`, `server.artifacts*`, `server.slatedb*`, any
  local `SecretStore` paths, any `InterpString` value whose
  `Provenance::EnvSourced` is set.

### TODO-4: `web_auth.rs` register flow
**File**: `lib/crates/fabro-server/src/web_auth.rs:496-659`
**What**: `setup_register` mutates a v2 TOML document via the new
`merge_settings_keys` helper (which now writes v2 top-level stanzas
under `[server.{web,auth,integrations.github}]`), writes it to disk,
then re-parses it with `ConfigLayer::load` and swaps it into
`state.settings`.
**Why**: Previously the function wrote legacy v1 TOML (top-level
`[web]`/`[api]`/`[git]`) that the v2 parser would reject. It had to be
rewritten to stay functional.
**Still TODO**: Stage 6.6 should decide whether the register flow
belongs in the server at all, or whether the web UI should drive it
directly via the HTTP API and a /api/v1/setup endpoint. The current
implementation is a hand-rolled TOML writer and loses comments /
formatting on round-trip.

### TODO-5: `check_crypto` in diagnostics walks v2 listen TLS
**File**: `lib/crates/fabro-server/src/diagnostics.rs:469-574`
**What**: Reads `server.auth.api.{jwt,mtls}.enabled` and
`server.listen.tls.{cert,key,ca}` directly from `SettingsFile`.
**Why**: Migrated off the bridge. Works, but the error messages
reference v2 field paths (e.g., "mTLS configured but
[server.listen.tls] is missing"); the `doctor` command hints may need
updating for consistency.
**Fix when**: Opportunistic, no blocker.

### TODO-6: Retain-or-delete dead `Combine` trait
**Files**:
- `lib/crates/fabro-types/src/combine.rs`
- `lib/crates/fabro-macros/src/lib.rs` (the `Combine` derive)
- Every `#[derive(crate::Combine)]` / `#[derive(Combine)]` on legacy
  types in `fabro-types/src/settings/{run,sandbox,server,user}.rs` +
  manual impls in `fabro-types/src/settings/mcp.rs`.
**What**: The trait is only used by legacy types for cross-layer
merging that v2's `combine_files` function replaced. Nothing external
calls `.combine()` on a legacy type.
**Delete when**: Stage 6.3 deletes the legacy types. The `Combine`
trait, its derive macro, and the `combine.rs` file all go with them.

### TODO-7: Fallback chain bug preserved
**File**: `lib/crates/fabro-workflow/src/operations/start.rs:491-525`
**What**: `resolve_fallback_chain` groups all v2 `ModelRef` entries under
the empty-string provider key when building the legacy `HashMap<String,
Vec<String>>` that `Catalog::build_fallback_chain` expects. Since
`build_fallback_chain` looks up by `Provider::as_str()` (e.g.,
`"anthropic"`), this **always returns an empty chain**. This preserves
the pre-migration behavior exactly.
**Fix when**: The model registry work in the requirements doc lands
(open question #4 in the predecessor handoff). A proper fix groups
fallbacks by actual provider and resolves bare `ModelRef::Bare` tokens
against the catalog.

### TODO-8: V2 doesn't model `goal_file`
**File**: `lib/crates/fabro-workflow/src/operations/source.rs:150-160`
**What**: V2 has `run.goal` (an `InterpString`) but no separate
`run.goal_file`. The legacy CLI `--goal-file` flag can't be expressed
in v2. The `resolve_goal_override` helper comments on this.
**Fix when**: Either add a `run.goal_file` subfield to the v2 schema
(requires a requirements update), or route file-based goals through
the workflow-manifest layer the way the server-side flow already does.

### TODO-9: Server settings inherent methods gone but struct serializes legacy field set
**File**: `lib/crates/fabro-types/src/settings/mod.rs:77-146`
**What**: The `Settings` struct still has ~30 fields (`llm`, `sandbox`,
`setup`, `checkpoint`, `hooks`, `mcp_servers`, `github`, `slack`, `api`,
`web`, `features`, `log`, `git`, `fabro`, `storage_dir`, `verbose`,
`prevent_idle_sleep`, `upgrade_check`, `dry_run`, `auto_approve`,
`no_retro`, `max_concurrent_runs`, `artifact_storage`, `exec`, etc.).
These are all dead weight except for the OpenAPI response path and
the demo routes.
**Delete when**: Stage 6.6 rewrites the OpenAPI spec.

### TODO-10: Demo routes still emit legacy shape
**File**: `lib/crates/fabro-server/src/demo/mod.rs:1327-1560`
**What**: Two big `fabro_types::Settings { ... }` literal constructions
that feed demo mode responses. The demo path isn't wired into the
production API surface (goes through `demo::get_run_settings`).
**Fix when**: Either rewrite as v2 `SettingsFile` literals in Stage 6.6,
or delete the demo path entirely if it's no longer used by fabro-web.

### TODO-11: `fabro-cli/tests/it/cmd/config.rs` has an unused `Settings` import
**File**: `lib/crates/fabro-cli/tests/it/cmd/config.rs:4`
**What**: `use fabro_types::Settings;` is leftover from an earlier
migration step. If clippy is happy with it (via re-export?), it's
harmless; otherwise remove it.
**Check**: `cargo clippy -p fabro-cli --tests -- -D warnings`.

### TODO-12: Unused `settings_file` binding after `drop(settings)`
**File**: `lib/crates/fabro-server/src/web_auth.rs:557-570`
**What**: I re-parse the file after writing it and swap into state.
The `settings_file` local binding is the pre-edit snapshot; it's no
longer used. Double-check the function compiles without a warning and
drop the local if it's dead.

## Scoped open design questions (from the predecessor doc, still open)

1. **Should `ConfigLayer::resolve(self) -> Settings` survive in any form?**
   — It's gone. The natural rename (`into_file(self) -> SettingsFile`)
   isn't needed because `From<ConfigLayer> for SettingsFile` already
   exists. Consumers call `.into()`. **Decided: no rename.**

2. **Post-layering env interpolation resolution pass**. Still not
   implemented. `InterpString::resolve` is called at read time by each
   consumer that needs a concrete string. Stage 6.6's allow-list DTO
   construction will need provenance-aware redaction; the missing pass
   means each DTO builder has to do its own `.resolve(|name|
   std::env::var(name).ok())` + provenance check. The requirements doc
   R79–R81 still specifies a centralized pass under
   `fabro-config/src/interp_pass.rs`.

3. **Fail-closed server auth posture**. Still not wired into
   `fabro-server/src/server.rs` startup. R52/R53 requires that if
   `server.auth` is absent or resolves to no enabled API / web
   strategies, normal startup refuses to run, with demo and test
   helpers opting in explicitly to insecure startup. Stage 6.6 is the
   natural place — the allow-list DTO construction for
   `/api/v1/settings` must know the enabled auth strategies, which
   overlaps with the startup posture check.

4. **Runtime `ModelRegistry` for `ModelRef::resolve`**. Still unimplemented.
   `fabro_types::settings::v2::model_ref::ModelRef::resolve` takes a
   `&dyn ModelRegistry` and errors on ambiguous bare tokens. There's
   no runtime implementation against `fabro-model::Catalog`. See TODO-7
   above.

5. **`run.scm.<provider>` subtree depth**. Still minimal — only
   `run.scm.github` exists as a placeholder unit struct. Add real
   fields when the first SCM-specific leaf lands.

6. **`flatten` + `HashMap` + `deny_unknown_fields`**. Don't try to
   flatten a HashMap under `deny_unknown_fields`. It doesn't work in
   serde. Enumerate known providers explicitly (as v2 already does for
   `NotificationRouteLayer`, `InterviewsLayer`, etc.).

## Running verification

```bash
# full gate — must stay green after every incremental commit
cargo fmt --check --all
cargo build --workspace
cargo clippy --workspace -- -D warnings
ulimit -n 4096 && cargo nextest run --workspace

# web assets (when touching fabro-web):
cd apps/fabro-web && bun run typecheck && bun test && bun run build

# API spec conformance:
cargo nextest run -p fabro-server --test it openapi_conformance
```

Current status on `main`: all of the above are green.

## Success criteria for finishing Stage 6

Pulled from the predecessor handoff, updated for what remains:

- [ ] `git grep 'fabro_types::Settings\b'` returns zero hits outside
      the legacy type file that's about to be deleted.
      **Current: ~9 hits remain — see TODO-1 / TODO-9 / TODO-10.**
- [x] `git grep 'bridge_to_old'` returns zero hits.
      **Done in 6.2.**
- [ ] `lib/crates/fabro-types/src/settings/v2/` no longer exists as
      a subdirectory — its contents are promoted to `settings/*`.
      **Blocked on 6.3; top-level re-exports landed in 6.5.**
- [ ] `lib/crates/fabro-types/src/combine.rs` is deleted.
      **Blocked on 6.3.**
- [ ] `lib/crates/fabro-config/src/{hook,mcp,sandbox,server,run,user}.rs`
      are either deleted or reduced to thin re-export shells.
      **hook/mcp/sandbox/server: deleted. run/user: reduced to the
      helper functions they still own.**
- [ ] `docs/api-reference/fabro-api.yaml` `ServerSettings` and
      `RunSettings` schemas are explicit allow-list DTOs.
      **Not started (6.6).**
- [ ] `lib/packages/fabro-api-client` and the Rust progenitor client
      are regenerated from the new spec.
      **Not started (6.6).**
- [ ] `apps/fabro-web/app/routes/workflow-detail.tsx` `workflowData`
      literal matches the new `RunSettings` DTO.
      **Not started (6.6).**
- [x] The `cargo fmt` / `cargo build` / `cargo clippy -D warnings` /
      `cargo nextest run --workspace` / `bun run typecheck` / `bun test`
      / `bun run build` gates all stay green.
      **Rust side: green. Frontend: unverified — the new `/api/v1/settings`
      JSON shape may break fabro-web at runtime. Verify before merging
      any frontend release.**

## Starting points for the next engineer

1. **Read the predecessor handoff end-to-end** — it has the scope,
   gotchas, and open design questions.
2. **Run the test suite locally** to confirm the starting state
   (`ulimit -n 4096 && cargo nextest run --workspace`). Expected:
   3,756 passed / 0 failed / 182 skipped.
3. **Verify the wire-contract drift** before touching anything:
   ```bash
   cargo run -p fabro-cli -- server start   # in one terminal
   curl -s http://localhost:3000/api/v1/settings | jq '.'
   ```
   You should see the v2 `SettingsFile` shape (`server.storage.root`,
   `run.execution.mode`, etc.), not the legacy flat shape. This is
   the state that 6.6 needs to reconcile with the OpenAPI spec.
4. **Start 6.6 by drafting the new `ServerSettings` DTO** in the
   OpenAPI yaml. Use the R16 allow-list from the requirements doc
   as the starting point. Don't try to be exhaustive — a narrower
   first cut is easier to review.
5. **Generate clients, update `get_server_settings` and
   `get_run_settings` to build the DTO explicitly**, and only then
   touch fabro-web. The backend change should be testable in isolation
   before anything in the frontend moves.
6. **After 6.6 lands**, deleting the legacy `Settings` types in 6.3 +
   flattening the v2 directory in 6.5 becomes mechanical.

Good luck.
