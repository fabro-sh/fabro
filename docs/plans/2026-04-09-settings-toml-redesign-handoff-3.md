---
date: 2026-04-09
status: active
topic: settings-toml-redesign
predecessor: docs/plans/2026-04-09-settings-toml-redesign-handoff-2.md
---

# Settings TOML Redesign — Handoff 3 (post Stage 6.6 + 6.3b partial)

## TL;DR

Stage 6.6 (OpenAPI DTO rewrite + server handlers + CLI migration +
fabro-web literals + demo routes) landed cleanly on `main`, and Stage
6.3b's first pass — **deleting the legacy flat `fabro_types::Settings`
struct itself** — also landed. The legacy flat view is dead code
everywhere in production.

What remains is the *runtime type module cleanup*: the 7 files under
`lib/crates/fabro-types/src/settings/{hook,mcp,project,run,sandbox,
server,user}.rs` are still alive and consumed by 8 downstream crates.
These modules are what blocks Stage 6.5b (flatten `settings/v2/*.rs`
up to `settings/*.rs`). The blockers are filename collisions and ~33
import statements scattered across the workspace.

3,758 workspace tests pass. `cargo fmt --check --all` and
`cargo clippy --workspace -- -D warnings` are clean. `bun run
typecheck`, `bun test`, and `bun run build` for `apps/fabro-web` are
green.

Main work remaining:

1. **Finish Stage 6.3b** — migrate the 8 consumer crates off the
   runtime type modules, then delete those 7 files plus the
   `Combine` trait + derive macro.
2. **Stage 6.5b** — trivial once 6.3b finishes: `git mv
   lib/crates/fabro-types/src/settings/v2/*.rs
   lib/crates/fabro-types/src/settings/` and sweep `::v2::` out of
   the workspace.
3. **Stage 6.6g** — rewrite `fabro-server` auth resolver for v2
   (TODO-2 from handoff-2).
4. **Stage 6.6j** — review `setup_register` TOML writer in
   `web_auth.rs` (TODO-4 from handoff-2).
5. Remaining scoped TODOs (TODO-5, 7, 8, 11, 12 from handoff-2).

## Source documents

Read these, in this order:

1. **Requirements (authoritative)** —
   [`docs/brainstorms/2026-04-08-settings-toml-redesign-requirements.md`](../brainstorms/2026-04-08-settings-toml-redesign-requirements.md).
2. **Original implementation plan** —
   [`docs/plans/2026-04-08-settings-toml-redesign-implementation-plan.md`](./2026-04-08-settings-toml-redesign-implementation-plan.md).
3. **Stage 6 handoff (predecessor 1)** —
   [`docs/plans/2026-04-09-settings-toml-redesign-handoff.md`](./2026-04-09-settings-toml-redesign-handoff.md).
4. **Stage 6 handoff 2 (immediate predecessor)** —
   [`docs/plans/2026-04-09-settings-toml-redesign-handoff-2.md`](./2026-04-09-settings-toml-redesign-handoff-2.md).
   Full per-stage file maps and scoped TODOs; most content still
   applies.

## Commit trail (landed on main in this session, most recent first)

```
4a40c73b7 refactor(settings): stage 6.3b delete legacy flat Settings struct
65a9fd137 refactor(fabro-web): stage 6.6 rewrite workflowData literal to v2 shape
f5b9f82a2 feat(settings): stage 6.6 wire server + CLI to v2 SettingsFile DTO
7c8448ece refactor(api): stage 6.6 collapse settings DTOs to freeform v2 shape
```

Net effect: about −3,500 / +500 lines across the four commits.

## Stage-by-stage status

### 6.6 — OpenAPI DTO rewrite + fabro-web ✅ **COMPLETE (for the in-scope parts)**

**What landed** (`7c8448ece` + `f5b9f82a2` + `65a9fd137`):

- `docs/api-reference/fabro-api.yaml`:
  - Replaces `ServerSettings` with a `type: object,
    additionalProperties: true` freeform schema pointing at the v2
    `SettingsFile` docs.
  - Replaces `RunSettings` similarly.
  - Deletes the 20+ orphaned supporting schemas that only those two
    referenced (`LlmSettings`, `SandboxSettings`, `HookDefinition`,
    `WebSettings`, `ApiSettings`, `TlsSettings`, `GitSettings`,
    `AuthSettings`, `Features`, `LogSettings`, `CheckpointSettings`,
    `PullRequestSettings`, `ArtifactsSettings`, `McpServerEntry`,
    `GitHubSettings`, `DaytonaSettings`, `LocalSandboxSettings`,
    `DaytonaSnapshotSettings`, `SetupSettings`, `GitAuthorSettings`,
    `WebhookSettings`).
- Regenerates the Rust progenitor client — `RunSettings` and
  `ServerSettings` are now `#[serde(transparent)]` newtype wrappers
  over `serde_json::Map<String, Value>`.
- Regenerates the TypeScript Axios client — the orphan
  `run-settings.ts`, `server-settings.ts`, and 30+ nested model files
  are deleted; the API methods inline the freeform type
  as `{ [key: string]: any; }`.
- `fabro-server/src/settings_view.rs` (**new module**, ~220 LOC
  including tests): `redact_for_api(&SettingsFile) -> SettingsFile`
  drops `server.listen.*`, `server.auth.api.jwt.{issuer,audience}`,
  `server.auth.api.mtls.ca`, and
  `server.auth.web.providers.github.client_secret`. 5 unit tests
  cover each drop case plus a `preserves_run_cli_project_and_features`
  smoke test.
- `fabro-server/src/server.rs::get_server_settings` — now calls
  `settings_view::redact_for_api` before serializing.
- `fabro-server/src/server.rs::get_run_settings` — **new** real
  handler (was previously `not_implemented`) that opens the run
  reader, reads the persisted `RunRecord.settings`, redacts, and
  emits JSON. The demo route still points at `demo::get_run_settings`,
  which was also rewritten.
- `fabro-cli/src/server_client.rs::retrieve_server_settings` — now
  returns `SettingsFile` directly (not the legacy `Settings`). The
  body is decoded from the progenitor `types::ServerSettings`
  transparent newtype via `serde_json::from_value::<SettingsFile>(...)`.
- `fabro-cli/src/commands/config/mod.rs::legacy_settings_to_v2` —
  **deleted** (TODO-1 from handoff-2 resolved). `merged_config`
  passes the v2 file straight into
  `effective_settings::resolve_settings`.
- `fabro-cli/tests/it/cmd/config.rs` — rewrites
  `server_settings_fixture` to build a v2 `SettingsFile` via
  `ConfigLayer::parse` instead of the legacy flat TOML shape.
- `fabro-web` — defines local `type ServerSettings =
  Record<string, unknown>` and `type RunSettings = Record<string,
  unknown>` aliases in `settings.tsx` / `workflow-api.ts` since the
  generated client no longer exports named model types. The UI only
  `JSON.stringify`s these payloads. The static `workflowData`
  literal in `workflow-detail.tsx` is rewritten to v2 shape
  (`_version`, `run.goal`, `run.inputs`, `run.model`, `run.sandbox`,
  `run.prepare.steps`, with `"120s"` / `"8GB"` / `"10GB"` string
  forms).
- `fabro-server/src/demo/mod.rs` — the two demo settings fixtures
  (`runs::settings()` and `settings::server_settings()`) are
  rewritten as `serde_json::json!(...)` literals in v2 shape
  (TODO-10 from handoff-2 resolved).

**Known remaining wire-contract concerns**:

1. `openapi_conformance::server_settings_keys_match_openapi_spec`
   was **deleted** in 6.3b because the new freeform-object schema
   has no `properties` to diff against. `all_spec_routes_are_routable`
   remains.
2. `bun run dev` / browser sanity check against a real running
   server is still unverified — the new wire shape should work
   because fabro-web only stringifies it, but this should be smoke-
   tested before the next frontend release.

### 6.6g — Rewrite auth resolver for v2 ⏳ **NOT STARTED**

TODO-2 from handoff-2 still stands:

**File**: `lib/crates/fabro-server/src/serve.rs:91` — the
`build_legacy_api_settings` stopgap builds a legacy
`fabro_types::settings::server::ApiSettings` from the v2
`server.auth.api.{jwt,mtls}` + `server.listen.tls` subtrees so that
`resolve_auth_mode_with_lookup` in `jwt_auth.rs` still works.

**Fix**: rewrite `resolve_auth_mode_with_lookup` to read
`SettingsFile` directly, delete `build_legacy_api_settings`, and
drop the `fabro_types::settings::server::{ApiSettings,
ApiAuthStrategy, TlsSettings}` imports from `serve.rs` / `jwt_auth.rs`
/ `tls.rs`.

### 6.6j — setup_register review ⏳ **NOT STARTED**

TODO-4 from handoff-2 still stands:

**File**: `lib/crates/fabro-server/src/web_auth.rs:496-659`. The
`setup_register` function hand-rolls a v2 TOML document and writes
it to disk. It works but loses comments / formatting on round-trip.
Plus TODO-12: double-check and drop any dead `settings_file` local
binding after the `drop(settings)` write-and-reparse dance at
`web_auth.rs:557-570`.

### 6.3b — Delete legacy flat `Settings` types ⚠️ **PARTIAL**

**What landed in this session** (`4a40c73b7`):

- `fabro_types::Settings` struct itself: **deleted** from
  `lib/crates/fabro-types/src/settings/mod.rs`. All ~65 fields gone.
- `fabro_types::Settings` re-export from `fabro_types/src/lib.rs:56`:
  **deleted**.
- `fabro_types::settings::Settings` usage in
  `fabro-server/src/lib.rs::server_config` module: re-export
  **deleted**. The `fabro_types::settings::server::*` pass-through
  is still there because downstream code still imports from it.
- `fabro-server/src/demo/mod.rs` — the two demo settings literals
  (runs::settings + settings::server_settings) were rewritten as
  v2 `serde_json::json!` literals (6.6i, simultaneously).
- `fabro-server/tests/it/openapi_conformance.rs` — deleted the
  `server_settings_keys_match_openapi_spec` test that built a
  fully-populated legacy `Settings` to diff against the spec. Kept
  `all_spec_routes_are_routable`.
- `fabro-store/src/run_state.rs` — test fixture switched from
  `Settings::default()` to `SettingsFile::default()`.
- `fabro-types/src/run_event/mod.rs` — two `RunCreated` round-trip
  tests switched from `Settings::default()` to
  `SettingsFile::default()`.
- `fabro-workflow/tests/it/integration.rs` — the two
  `hook_toml_*_parsing` tests that decoded top-level `[[hooks]]` into
  a legacy `Settings` were **deleted**. Those test the legacy parse
  path which had already been removed in Stage 6.1; the coverage
  moves to `fabro-types::settings::v2::tree::tests`.

**What did NOT land** (deferred to Stage 6.3c):

The 7 runtime type modules under
`lib/crates/fabro-types/src/settings/` are still alive:

- `hook.rs` — `HookDefinition`, `HookEvent`, `HookSettings`,
  `HookType`, `TlsMode`
- `mcp.rs` — `McpServerEntry`, `McpServerSettings`, `McpTransport`,
  `default_startup_timeout_secs`, `default_tool_timeout_secs`
- `project.rs` — `ProjectSettings`
- `run.rs` — `ArtifactsSettings`, `CheckpointSettings`, `GitHubSettings`,
  `LlmSettings`, `MergeStrategy`, `PullRequestSettings`, `SetupSettings`
- `sandbox.rs` — `DaytonaNetwork`, `DaytonaSettings`,
  `DaytonaSnapshotSettings`, `DockerfileSource`, `LocalSandboxSettings`,
  `SandboxSettings`, `WorktreeMode`
- `server.rs` — `ApiAuthStrategy`, `ApiSettings`,
  `ArtifactStorageBackend`, `ArtifactStorageSettings`, `AuthProvider`,
  `AuthSettings`, `FeaturesSettings`, `GitAuthorSettings`,
  `GitProvider`, `GitSettings`, `LogSettings`, `SlackSettings`,
  `TlsSettings`, `WebSettings`, `WebhookSettings`, `WebhookStrategy`
- `user.rs` — `ClientTlsSettings`, `ExecSettings`, `OutputFormat`,
  `PermissionLevel`, `ServerSettings`

Plus `fabro-types/src/combine.rs` (the `Combine` trait) and the
`fabro-macros` `Combine` derive macro that only these modules use.

These are blocked on migrating the 8 consumer crates that import
them. See "Consumer migration map" below.

### 6.5b — Flatten `settings::v2::*` → `settings::*` ⏳ **STILL BLOCKED ON 6.3b**

No change from handoff-2. When 6.3b finishes deleting the runtime
type modules, this becomes a trivial `git mv` + search-and-replace
pass. The file-name collisions to resolve are `project.rs`, `run.rs`,
`server.rs`, `cli.rs` — each exists in both `settings/` and
`settings/v2/`.

## Consumer migration map (for finishing 6.3b)

| Crate | Legacy types it still imports | Suggested destination |
|---|---|---|
| `fabro-agent` | `OutputFormat`, `PermissionLevel` from `settings::user` | Promote into `fabro-agent` itself — they're CLI/exec concerns. Or point at `settings::v2::cli::OutputFormat` / `v2::run::AgentPermissions` if shapes match. |
| `fabro-checkpoint` | `GitAuthorSettings` from `settings::server` | Promote into `fabro-checkpoint` or read directly from `v2::run::GitAuthorLayer` at the call site. |
| `fabro-hooks` | `HookDefinition`, `HookEvent`, `HookSettings`, `HookType`, `TlsMode` | Promote all of them into `fabro-hooks`. They are runtime behavior types (has `resolved_hook_type()` / `runs_in_sandbox()` methods), not parse-tree types, so they belong in the consumer crate. |
| `fabro-mcp` | `McpServerEntry`, `McpServerSettings`, `McpTransport`, `default_startup_timeout_secs`, `default_tool_timeout_secs` | Promote into `fabro-mcp`. Convert from v2 `run.agent.mcps.*` or `cli.exec.agent.mcps.*` at the call site. |
| `fabro-sandbox` | `SandboxSettings`, `DaytonaSettings`, `DaytonaSnapshotSettings`, `DaytonaNetwork`, `LocalSandboxSettings`, `WorktreeMode`, `DockerfileSource` | Already re-exported as `fabro_sandbox::daytona::*` with renames. Promote the source into `fabro-sandbox` directly and drop the re-export path. |
| `fabro-checkpoint` | `GitAuthorSettings` | Same as above. |
| `fabro-workflow` | `PullRequestSettings`, `MergeStrategy`, `WorktreeMode` | `MergeStrategy` and `WorktreeMode` have identical v2 equivalents in `v2::run` — point at them directly. `PullRequestSettings` should move into `fabro-workflow`. |
| `fabro-server` | `ApiSettings`, `TlsSettings`, `ApiAuthStrategy`, `GitSettings`, `ServerSettings` (as `UserServerSettings`), `GitHubSettings`, `WebSettings`, `AuthSettings`, `GitAuthorSettings`, `WebhookSettings`, `LogSettings`, `FeaturesSettings` | Part of Stage 6.6g — the auth resolver rewrite needs to walk `v2::server::auth` directly; likewise the TLS handling in `tls.rs`. Other types may just need to move into `fabro-server`. |
| `fabro-cli` | `ClientTlsSettings`, `OutputFormat`, `PermissionLevel`, `ExecSettings`, `ServerSettings` (as `UserServerSettings`) | Promote `ClientTlsSettings` / `ExecSettings` into `fabro-cli`. `OutputFormat` / `PermissionLevel` / `ServerSettings` are shared with `fabro-agent` — decide whether they belong in `fabro-agent` and re-export, or in a new shared crate. |

**Total import sites to rewrite**: about 33 `use` statements and
roughly that many call-sites, across ~15 files in 8 crates. Each
individual migration is small; the aggregate is the bulk of the
remaining 6.3b work.

### Combine trait

After the consumer migration:

1. `lib/crates/fabro-types/src/combine.rs` — delete.
2. `lib/crates/fabro-macros/src/lib.rs::Combine` derive — delete.
3. `fabro-macros` crate becomes empty or can go away entirely if
   there are no other derives in it.

## Scoped TODOs (handoff-2 status update)

| TODO | Subject | Status |
|---|---|---|
| TODO-1 | `legacy_settings_to_v2` shim in fabro-cli | ✅ **Deleted** in `f5b9f82a2` |
| TODO-2 | `build_legacy_api_settings` in fabro-server | ⏳ Still open (6.6g) |
| TODO-3 | `get_server_settings` emits raw v2 JSON | ✅ **Fixed** in `f5b9f82a2`. Handler now calls `settings_view::redact_for_api` |
| TODO-4 | `web_auth.rs` register flow | ⏳ Still open (6.6j) |
| TODO-5 | `check_crypto` in diagnostics | ⏳ Opportunistic, unchanged |
| TODO-6 | Dead `Combine` trait | ⏳ Still blocked on consumer migration |
| TODO-7 | Fallback chain bug preserved | ⏳ Unchanged — waiting on model registry work |
| TODO-8 | V2 doesn't model `goal_file` | ⏳ Unchanged — requirements decision needed |
| TODO-9 | Server settings inherent methods gone | ✅ **Fixed** — Settings struct is deleted entirely in 6.3b |
| TODO-10 | Demo routes still emit legacy shape | ✅ **Fixed** in `4a40c73b7`. Demo fixtures rewritten as v2 JSON |
| TODO-11 | Unused `Settings` import in `config.rs` tests | ✅ **Fixed** in `f5b9f82a2`. Test file rewritten to use `SettingsFile` |
| TODO-12 | Unused `settings_file` binding in `web_auth.rs` | ⏳ Still open (rolls up into 6.6j) |

## Running verification

```bash
# Rust side — should stay green after every incremental commit
cargo fmt --check --all
cargo build --workspace
cargo clippy --workspace -- -D warnings
ulimit -n 4096 && cargo nextest run --workspace

# Web side — should stay green when touching fabro-web
cd apps/fabro-web && bun run typecheck && bun test && bun run build

# API spec conformance — single test remaining
cargo nextest run -p fabro-server --test it openapi_conformance
```

Expected as of `4a40c73b7`: 3,758 tests pass / 0 fail / 182 skipped.

## Success criteria for finishing Stage 6

Updated from handoff-2:

- [x] `git grep 'fabro_types::Settings\b'` returns zero hits outside
      the comment in the conformance test.
      **Done in `4a40c73b7`.**
- [x] `git grep 'bridge_to_old'` returns zero hits.
- [ ] `lib/crates/fabro-types/src/settings/v2/` no longer exists
      as a subdirectory. **Blocked on finishing 6.3b.**
- [ ] `lib/crates/fabro-types/src/combine.rs` is deleted.
      **Blocked on finishing 6.3b.**
- [x] `lib/crates/fabro-config/src/{hook,mcp,sandbox,server,run,user}.rs`
      deleted or reduced to thin helpers.
      **Done in Stage 6.4.**
- [x] `docs/api-reference/fabro-api.yaml` `ServerSettings` and
      `RunSettings` schemas are not the legacy flat shape.
      **Done in `7c8448ece`** (freeform objects pointing at the v2
      SettingsFile Rust type).
- [x] `lib/packages/fabro-api-client` and the Rust progenitor client
      are regenerated.
      **Done in `7c8448ece`.**
- [x] `apps/fabro-web/app/routes/workflow-detail.tsx` `workflowData`
      literal matches the new shape.
      **Done in `65a9fd137`.**
- [x] `cargo fmt` / `cargo build` / `cargo clippy -D warnings` /
      `cargo nextest run --workspace` / `bun run typecheck` /
      `bun test` / `bun run build` gates all green.
      **Verified after each commit.**

## Starting points for the next engineer

1. **Read this doc and handoff-2 in full** — the consumer migration
   map above is the bulk of the remaining work and rewards careful
   per-crate thinking.
2. **Run the test suite locally** to confirm the starting state
   (`ulimit -n 4096 && cargo nextest run --workspace`). Expected:
   3,758 passed / 0 failed / 182 skipped.
3. **Pick the smallest consumer first** (suggested order:
   `fabro-checkpoint` → `fabro-agent` → `fabro-workflow` →
   `fabro-hooks` → `fabro-mcp` → `fabro-sandbox` → `fabro-cli` →
   `fabro-server`). For each:
   a. Move the types into the consumer crate with `git mv` or hand
      relocation.
   b. Update the consumer's public API to own them.
   c. Rewrite the consumer's `From<&SettingsFile>` / construction
      path to build from v2 subtrees directly.
   d. Delete the corresponding runtime type file in `fabro-types`.
   e. Verify `cargo build --workspace`, `cargo clippy --workspace
      -- -D warnings`, and the relevant nextest subset stay green
      before moving to the next crate.
4. **After the last consumer migrates**, delete `Combine` (trait,
   derive, crate file).
5. **Stage 6.5b** is a one-commit follow-up: `git mv v2/*.rs up`,
   drop the `::v2::` paths, done.
6. **Stage 6.6g and 6.6j** are independent of the above and can be
   sequenced whenever; 6.6g pairs naturally with the `fabro-server`
   consumer migration because both touch `jwt_auth.rs` / `serve.rs` /
   `tls.rs`.

Good luck.
