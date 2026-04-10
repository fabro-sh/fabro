---
date: 2026-04-09
status: active
topic: settings-toml-redesign
predecessor: docs/plans/2026-04-08-settings-toml-redesign-implementation-plan.md
---

# Settings TOML Redesign — Handoff to Stage 6 Follow-up

## TL;DR

Stages 1–5 of the settings TOML redesign landed on `main` across 13 commits. The
user-facing hard cut is complete: every Fabro config file now parses against
the v2 namespaced schema, legacy top-level keys hard-fail with targeted rename
hints, the merge matrix is implemented per the normative requirements doc,
trust boundaries work across all three resolution modes, all scaffolds and
docs are migrated, and the workspace is 100% tests-green (**3,760 passed / 0
failed**), clippy-clean, and correctly formatted.

The remaining work is **Stage 6: delete the legacy flat `Settings` shape and
the transitional `bridge_to_old` seam**, plus the OpenAPI + generated clients
+ fabro-web DTO rewrite that was explicitly deferred from Stage 5. This
document is everything you need to continue the work in a fresh session.

## Source documents

Read these before starting, in order:

1. **Requirements (authoritative)** —
   [`docs/brainstorms/2026-04-08-settings-toml-redesign-requirements.md`](./../brainstorms/2026-04-08-settings-toml-redesign-requirements.md).
   This is the source of truth for the v2 schema, merge matrix, trust
   boundaries, and disable semantics. Refer to requirement numbers (R1–R90)
   when you change schema rules so decisions stay traceable.

2. **Original implementation plan** —
   [`docs/plans/2026-04-08-settings-toml-redesign-implementation-plan.md`](./2026-04-08-settings-toml-redesign-implementation-plan.md).
   This is the 6-stage sequence and the scope of what needs to land. Stage 6
   in that document is the list of things this handoff still owes.

3. **Representative canonical example** — the `representative_full_tree_parses`
   test in
   [`lib/crates/fabro-types/src/settings/v2/tree.rs`](../../lib/crates/fabro-types/src/settings/v2/tree.rs#L295-L422).
   If you want a feel for how the whole v2 schema fits together, read this
   fixture before anything else.

## Current-state map

### What the tree looks like at handoff

```
lib/crates/fabro-types/src/settings/
├── mod.rs                  — transitional seam; hosts legacy flat Settings +
│                             module comment explaining the deletion plan
├── v2/                     — authoritative v2 schema (Stages 1–2 output)
│   ├── mod.rs              — module root; re-exports
│   ├── tree.rs             — SettingsFile top-level; parse_settings_file(),
│   │                         ParseError with rename-hint table
│   ├── version.rs          — _version pre-validation
│   ├── project.rs          — ProjectLayer
│   ├── workflow.rs         — WorkflowLayer
│   ├── run.rs              — RunLayer + all run subtree types (536 LOC)
│   ├── cli.rs              — CliLayer + cli subtree types
│   ├── server.rs           — ServerLayer + server subtree types
│   ├── features.rs         — FeaturesLayer
│   ├── duration.rs         — Duration value-language helper
│   ├── size.rs              — Size value-language helper
│   ├── model_ref.rs        — ModelRef + ambiguity resolution
│   ├── interp.rs           — InterpString with provenance tagging
│   ├── splice_array.rs     — SpliceArray "..." marker
│   └── bridge.rs           — TRANSITIONAL: bridge_to_old(&SettingsFile)->Settings
│                             (~820 LOC — this is the thing Stage 6 deletes)
├── hook.rs, mcp.rs, project.rs, run.rs, sandbox.rs, server.rs, user.rs
│                           — LEGACY flat type definitions. Delete in Stage 6.
└── (combine trait is in ../combine.rs — also legacy, also deletes)

lib/crates/fabro-config/
├── lib.rs                  — module tree (note: combine.rs + settings.rs
│                             deleted in Stage 6 initial cleanup)
├── config.rs               — ConfigLayer newtype over SettingsFile; exposes
│                             ::parse/::load/::combine/::resolve/::as_v2
├── merge.rs                — v2 merge matrix implementation (683 LOC,
│                             covers every row of the normative table)
├── effective_settings.rs   — EffectiveSettingsLayers + resolve_settings
│                             with LocalOnly/RemoteServer/LocalDaemon modes
│                             and trust-boundary stripping
├── project.rs              — workflow discovery + resolve_fabro_root
├── user.rs                 — load_settings_config + legacy file warnings
├── run.rs                  — parse_run_config + resolve_env_refs helper +
│                             re-export shim of resolved run types
├── sandbox.rs, server.rs,  — THIN re-export shims. Stage 6 deletes these
│ hook.rs, mcp.rs             once consumers stop importing through them.
├── storage.rs              — unrelated; stays
├── home.rs                 — 1-line Home re-export
└── legacy_env.rs           — 12-line legacy env var helper
```

### Dependency chain to understand

```
TOML file
  │
  ▼  parse_settings_file()      (fabro-types/src/settings/v2/tree.rs)
SettingsFile (v2)
  │
  ▼  combine_files()            (fabro-config/src/merge.rs)
SettingsFile (v2, merged)
  │
  ▼  bridge_to_old()            (fabro-types/src/settings/v2/bridge.rs)
Settings (legacy flat)
  │
  ▼  every consumer that reads  (~84 call sites across 15 files)
    settings.llm, settings.vars, settings.sandbox, ...
```

The **bridge is the only producer of the legacy flat `Settings` shape**.
Removing it requires every reader to consume `SettingsFile` directly.

## Commit log (Stages 1–5 landed on `main`)

```
c6d515be4 fix(lint): clean up fabro-config test clippy warnings
31db613aa docs(config): point new code at ConfigLayer::as_v2 rather than the bridge
3dd3c7bf8 refactor(config): delete unused legacy shim modules, document transitional seam
dba10e5e9 docs: migrate reference and guide examples to v2 config shape
b57248236 test(migration): land final Stage 4 fixes — 100% workspace tests green
2fc85282b fix(effective_settings): keep cli/server stanzas from user settings.toml
a6047250c fix(lint): clippy cleanup for Stage 3/4 consumer migration
f4a79b896 test(cli): migrate remaining config/exec/create fixtures to v2
f467bd23c fix(bridge): use hook command shorthand to avoid duplicate serde key
eabbca649 feat(tests): migrate fabro-cli fixtures and repo fabro.toml to v2
a0eec6aee feat(config): switch parser and layering to v2 schema
bb228643e feat(types): flesh out v2 subtrees and add legacy bridge
288e73321 feat(types): add settings v2 parse tree scaffolding
```

Total: 76 files changed, +6,413 / -2,151 lines.

## Stage 6 work breakdown

Stage 6 has **six independent subtasks**. Each subtask can land as its own PR
on top of `main` — they have a natural dependency order but can be paused
between steps because the transitional bridge keeps the workspace building at
every intermediate state.

### 6.1 — Migrate consumer read sites from flat `Settings` to v2 `SettingsFile`

**Scope**: ~84 field-access sites across 15 files (grep below).

**Files to touch** (ordered easy → hard):

```
lib/crates/fabro-workflow/src/run_options.rs        —  5 sites, mostly behind accessor methods
lib/crates/fabro-workflow/src/operations/source.rs  —  3 sites
lib/crates/fabro-workflow/src/operations/create.rs  — ~8 sites, touches LLM mutation
lib/crates/fabro-workflow/src/operations/start.rs   — ~10 sites, touches setup/hooks/llm
lib/crates/fabro-cli/src/commands/run/runner.rs     —  a few sites
lib/crates/fabro-cli/src/commands/exec.rs           —  a few sites
lib/crates/fabro-cli/src/manifest_builder.rs        —  2 sites (goal, goal_file)
lib/crates/fabro-server/src/run_manifest.rs         — ~11 sites in handlers + tests
lib/crates/fabro-server/src/server.rs               — ~14 sites (biggest file)
lib/crates/fabro-server/src/web_auth.rs             — ~20 sites (git settings heavy)
lib/crates/fabro-server/src/serve.rs                —  a few sites
lib/crates/fabro-config/src/effective_settings.rs   — apply_server_defaults copies every field
lib/crates/fabro-config/src/project.rs              — resolve_working_directory reads settings.work_dir
lib/crates/fabro-cli/tests/it/cmd/create.rs         — 7 sites in assertions
lib/crates/fabro-cli/tests/it/cmd/runner.rs         — 4 sites in assertions
```

Exact grep:

```bash
grep -rn 'settings\.llm\|settings\.vars\|settings\.sandbox\|settings\.setup\|settings\.hooks\|settings\.checkpoint\|settings\.pull_request\|settings\.mcp_servers\|settings\.artifacts\|settings\.git\|settings\.exec\|settings\.fabro\|settings\.goal\|settings\.work_dir\|settings\.labels\|settings\.github' lib/crates --include='*.rs'
```

**Migration pattern** (before → after):

```rust
// BEFORE (legacy flat)
let model = settings.llm.as_ref().and_then(|llm| llm.model.clone());
let provider = settings.llm.as_ref().and_then(|llm| llm.provider.clone());
```

```rust
// AFTER (v2 via ConfigLayer::as_v2())
let model = layer
    .as_v2()
    .run
    .as_ref()
    .and_then(|r| r.model.as_ref())
    .and_then(|m| m.name.as_ref())
    .map(InterpString::as_source);
```

**Recommended sequence**:

1. **Start with receive-side accessor methods** on `ConfigLayer` and
   `RunOptions`. For every flat field that consumers read, add an accessor
   method that walks the v2 tree. Land these additively (no caller changes
   yet). Example:
   ```rust
   impl ConfigLayer {
       pub fn run_model_name(&self) -> Option<String> {
           self.file.run.as_ref()
               .and_then(|r| r.model.as_ref())
               .and_then(|m| m.name.as_ref())
               .map(InterpString::as_source)
       }
   }
   ```
2. **Migrate one caller at a time**, file-by-file, smallest first. After each
   file: `cargo build -p <crate>` + `cargo nextest run -p <crate>` before
   moving on. Do not try to cover 15 files at once — incremental commits.
3. **Delete the flat-field helper methods on `Settings`** as nothing reads
   them. They are in
   [`lib/crates/fabro-types/src/settings/mod.rs`](../../lib/crates/fabro-types/src/settings/mod.rs#L111-L196):
   `app_id()`, `slug()`, `client_id()`, `git_author()`, `sandbox_settings()`,
   `setup_settings()`, `setup_commands()`, `setup_timeout_ms()`,
   `preserve_sandbox_enabled()`, `github_permissions()`,
   `mcp_server_entries()`, `verbose_enabled()`, `prevent_idle_sleep_enabled()`,
   `upgrade_check_enabled()`, `dry_run_enabled()`, `auto_approve_enabled()`,
   `no_retro_enabled()`, `storage_dir()`, `slack_settings()`. These will
   cascade compiler errors into callers that you can then migrate.

**Gotchas**:

- **`settings.vars` vs v2 `run.inputs`**: v2 replaces wholesale (R22). If a
  consumer was relying on `vars` merging across layers, its behavior was
  ambiguous before and is now explicit — it sees whichever layer set `inputs`
  last. Check tests after migration.
- **`settings.setup.commands` vs v2 `run.prepare.steps`**: v2 replaces the
  whole ordered list (R30). Several tests were re-asserted in Stage 4;
  similar audits will be needed for any newly-migrated code path.
- **`settings.work_dir`** is the bridge output of `run.working_dir`
  (`InterpString`). When consumers want the raw string, call
  `InterpString::as_source()`. When they want an env-resolved value, call
  `InterpString::resolve(|name| std::env::var(name).ok())` — the v2
  interpolation pass is not yet wired into the default resolve path.
- **`settings.github.permissions`** maps to
  `server.integrations.github.permissions` in v2, which means it lives in
  the owner-specific domain and is stripped from fabro.toml / workflow.toml
  layers per R16. Consumers in `fabro-workflow` that read it will need to
  either lift the read to a call site that has access to the server-local
  layer, or accept that workflow-level config cannot ask for GitHub token
  permissions. Flag this as an open design question if you hit it.

### 6.2 — Delete the `bridge_to_old` seam

**Files**:
- `lib/crates/fabro-types/src/settings/v2/bridge.rs` (818 LOC) — delete
  entirely.
- `lib/crates/fabro-types/src/settings/v2/mod.rs` — drop the
  `pub mod bridge;` and `pub use bridge::bridge_to_old;` lines.
- `lib/crates/fabro-config/src/config.rs` — delete the
  `TryFrom<ConfigLayer> for Settings` and `TryFrom<&ConfigLayer> for Settings`
  impls, the `bridge_to_old` import, and change `ConfigLayer::resolve(self) ->
  Settings` to `ConfigLayer::into_file(self) -> SettingsFile` (or just
  encourage `From<ConfigLayer> for SettingsFile` which already exists).

**Prerequisite**: 6.1 must be complete — there must be zero readers of flat
`Settings` left. `git grep 'fabro_types::Settings\b'` should return nothing
outside of the legacy type definitions themselves.

**Known consumer of `bridge_to_old`**: only `ConfigLayer::resolve` in
[`lib/crates/fabro-config/src/config.rs`](../../lib/crates/fabro-config/src/config.rs#L133-L140).
No external callers. This is the last thing to unwire before the bridge can
be deleted.

### 6.3 — Delete the legacy flat types

**Files to delete** (and remove from `mod.rs` re-export lists):

```
lib/crates/fabro-types/src/settings/mod.rs       — Settings struct, impls, tests
lib/crates/fabro-types/src/settings/hook.rs      — HookDefinition, HookEvent, HookType, HookSettings, TlsMode
lib/crates/fabro-types/src/settings/mcp.rs        — McpServerEntry, McpServerSettings, McpTransport
lib/crates/fabro-types/src/settings/project.rs   — ProjectSettings
lib/crates/fabro-types/src/settings/run.rs        — LlmSettings, SetupSettings, CheckpointSettings,
                                                     PullRequestSettings, ArtifactsSettings,
                                                     GitHubSettings, MergeStrategy
lib/crates/fabro-types/src/settings/sandbox.rs   — SandboxSettings, DaytonaSettings, DaytonaSnapshotSettings,
                                                     LocalSandboxSettings, DaytonaNetwork, WorktreeMode,
                                                     DockerfileSource
lib/crates/fabro-types/src/settings/server.rs    — ApiSettings, WebSettings, GitSettings, GitAuthorSettings,
                                                     AuthSettings, AuthProvider, ApiAuthStrategy, TlsSettings,
                                                     WebhookSettings, WebhookStrategy, GitProvider,
                                                     FeaturesSettings, LogSettings, SlackSettings,
                                                     ArtifactStorageSettings, ArtifactStorageBackend
lib/crates/fabro-types/src/settings/user.rs       — ClientTlsSettings, ExecSettings, OutputFormat,
                                                     PermissionLevel, ServerSettings
lib/crates/fabro-types/src/combine.rs             — Combine trait (unused after deletes above)
lib/crates/fabro-macros/src/lib.rs                — keep `#[derive(Combine)]` if any non-legacy use;
                                                     otherwise delete the derive macro entry
```

**Prerequisite**: 6.2 must be complete (bridge deleted).

**Dependency chain**: `Combine` is used _only_ by legacy flat type derives
today. Search with
```bash
grep -rn '#\[derive(.*Combine\|impl Combine\|fabro_types::combine\|fabro_types::Combine' lib/crates --include='*.rs'
```
If the only hits are inside `fabro-types/src/settings/*.rs` legacy files, the
trait + derive are safe to delete in the same PR.

### 6.4 — Delete the `fabro-config` re-export shims

**Files** (all are 1–62 LOC thin pass-throughs):

```
lib/crates/fabro-config/src/hook.rs        — re-exports fabro_types::settings::hook::*
lib/crates/fabro-config/src/mcp.rs          — re-exports fabro_types::settings::mcp::*
lib/crates/fabro-config/src/sandbox.rs     — re-exports fabro_types::settings::sandbox::*
lib/crates/fabro-config/src/server.rs       — re-exports fabro_types::settings::server::* + resolve_storage_dir()
lib/crates/fabro-config/src/user.rs         — re-exports fabro_types::settings::user::* + path helpers
lib/crates/fabro-config/src/run.rs          — re-exports fabro_types::settings::run::* +
                                                parse_run_config + resolve_env_refs + resolve_graph_path
```

**Before deleting**, migrate callers off them. The callers are listed in the
file-level commit `3dd3c7bf8` — summary: `fabro-hooks`, `fabro-mcp`,
`fabro-sandbox`, `fabro-agent`, `fabro-cli`, `fabro-server`,
`fabro-workflow`, plus a handful of test files import via
`fabro_config::<module>::...` paths. Each should import directly from
`fabro_types::settings::v2::...` once the legacy types are gone.

**Retain**:
- `fabro-config/src/run.rs` **`resolve_graph_path()`** — still used, not
  legacy. Move it to `fabro-config/src/project.rs` or `fabro-config/src/lib.rs`.
- `fabro-config/src/run.rs` **`parse_run_config()`** — still used by
  `fabro-server/src/run_manifest.rs` and `fabro-cli/src/manifest_builder.rs`.
  It's already a thin `ConfigLayer::parse` wrapper. Either keep it as a
  top-level function in `fabro-config/src/lib.rs` or inline at call sites.
- `fabro-config/src/run.rs` **`resolve_env_refs()`** — the legacy minimal env
  resolver. Once consumers use `InterpString::resolve` directly, delete.
- `fabro-config/src/user.rs` **path helpers** (`default_settings_path`,
  `default_socket_path`, `active_settings_path`, legacy path helpers,
  `load_settings_config`) — still used by CLI commands. Move them to
  `fabro-config/src/lib.rs` or a new `fabro-config/src/paths.rs`.

### 6.5 — Flatten `settings::v2::*` → `settings::*`

Once Stages 6.3 + 6.4 are done and `fabro-types/src/settings/` only contains
the old `v2/` directory plus a mostly-empty `mod.rs`, rename everything to
be the primary namespace:

```
fabro-types/src/settings/
├── mod.rs              (re-exports direct from subdirs, no more v2 prefix)
├── tree.rs
├── version.rs
├── project.rs
├── workflow.rs
├── run.rs
├── cli.rs
├── server.rs
├── features.rs
├── duration.rs
├── size.rs
├── model_ref.rs
├── interp.rs
└── splice_array.rs
```

Rewrite imports across the workspace — `use fabro_types::settings::v2::...`
becomes `use fabro_types::settings::...`.

**Recommendation**: one big mechanical commit with just the rename; do not
mix with behavior changes.

### 6.6 — Rewrite OpenAPI contracts + regenerate clients + fix fabro-web

This is the piece that was explicitly deferred from Stage 5 because the
current bridge-backed `/api/v1/settings` response still works against the
existing `ServerSettings` schema. Owning the explicit allow-list DTOs is the
end-state the plan calls for (requirements doc "Validation Boundary" +
implementation plan Stage 5).

**Files to rewrite**:

- `docs/api-reference/fabro-api.yaml` — replace the current flat
  `ServerSettings` schema (lines ~4238–4364) and `RunSettings` schema
  (lines ~3995–4032) with explicit allow-list DTOs. The allow-lists are
  spelled out in the implementation plan under "Rebuild resolution, trust
  boundaries, and safe serialization":
  - **`/api/v1/settings` (server scope)**: allow only `server.api.url`,
    `server.web.enabled`, `server.web.url`, per-provider enabled state for
    `server.auth.web.providers.*`, and non-secret `server.scheduler` values.
    Deny everything else — notably `server.listen.*`, `server.listen.tls.*`,
    `server.auth.api`, `server.integrations.*`, `server.artifacts*`,
    `server.slatedb*`, any local SecretStore paths, and any env-resolved
    values tagged via `InterpString` provenance.
  - **`/api/v1/runs/{id}/settings` (run scope)**: allow the resolved `run.*`
    tree. Deny: any `InterpString` value whose resolution provenance shows
    it was sourced from `${env.NAME}`, provider-credential fields under
    `run.notifications.*.<provider>`, env values under
    `run.agent.mcps.*.env` that were env-interpolated, and any field
    explicitly marked sensitive. Deny all `project.*`, `workflow.*`,
    `cli.*`, and `server.*` — they're not part of a run view.
- **Then regenerate**:
  - Rust progenitor client: `cargo build -p fabro-api` (auto-runs via
    `build.rs`).
  - TypeScript client: `cd lib/packages/fabro-api-client && bun run generate`.
- **Update fabro-web**:
  - `apps/fabro-web/app/routes/workflow-detail.tsx` has a static
    `workflowData` literal (lines 18+) typed as `RunSettings`. Rewrite each
    entry to match the new run-scope DTO shape. The live `/settings` and
    `/runs/:id/settings` routes use `JSON.stringify` and are shape-agnostic —
    they don't need code changes, just the type alignment that falls out of
    the client regen.
- **Update server handlers**:
  - `lib/crates/fabro-server/src/server.rs` `get_server_settings` (around
    line 1062) currently serializes the flat Settings into the legacy
    `ServerSettings` shape via `serde_json::to_value` and `strip_nulls`.
    Rewrite to build the new allow-list DTO explicitly from
    `state.settings` — it must _not_ use `serde_json::to_value` on the full
    Settings, otherwise the allow-list is leaky. There is a redaction
    helper path in `fabro-types/src/settings/v2/interp.rs`
    (`Provenance::EnvSourced`) — consult it when you build the run-scope DTO.
  - `/api/v1/runs/:id/settings` currently returns `not_implemented` in the
    real (non-demo) router (grep `server.rs:1012`). The run-scope DTO rebuild
    is the same mechanical shape as the server-scope one, just different
    fields. The demo router wires `demo::get_run_settings` around
    `server.rs:934` — don't confuse the two during migration.

**Provenance redaction helper you'll need**:

`InterpString::resolve` returns a `Resolved { value, provenance }`. When
`provenance == Provenance::EnvSourced`, the caller knows the field came
from an env var and must redact it before serializing into the run-scope
DTO. If you find yourself building the same `Resolved` → DTO conversion in
multiple handlers, pull it into `fabro-types/src/settings/v2/redact.rs` as
a new helper module.

## Verification recipe (run on every incremental step)

```bash
# full gate — must stay green between every sub-step
cargo fmt --check --all
cargo build --workspace
cargo clippy --workspace -- -D warnings
ulimit -n 4096 && cargo nextest run --workspace
cd apps/fabro-web && bun run typecheck && bun test && cd -

# sanity: no legacy top-level TOML keys remain in real config files
git grep -n '^version = 1' -- docs/ lib/ apps/ fabro/ test/ | \
  grep -v 'changelog\|_version'

# sanity: after Stage 6.2 the bridge should have no callers
git grep 'bridge_to_old' lib/
```

## Testing gotchas I hit

These are lessons learned during Stages 1–5. Save yourself the pain.

1. **`fabro-cli` integration tests use a shared CLI test daemon under
   parallel nextest load**. Raise the shell FD limit and cap threads:
   ```bash
   ulimit -n 4096
   cargo nextest run -p fabro-cli --no-fail-fast --test-threads=4
   ```
   macOS inherited sessions default to `ulimit -n 256`, which surfaces as
   misleading EMFILE test timeouts.

2. **Insta snapshots** — when you update a snapshot, check the pending
   diffs before bulk-accepting. `cargo insta pending-snapshots` lists
   what's about to change. `cargo insta accept` accepts everything;
   `cargo insta accept --snapshot <path>` accepts one at a time. During
   Stage 4 we chose bulk accept for the run / attach JSON snapshots after
   confirming the only diffs were `server.target` + `_version` leakage
   (which we then filtered out explicitly in the per-test filter code).

3. **Hook shorthand vs `#[serde(flatten)]`** — the legacy
   `HookDefinition` struct has `command: Option<String>` and
   `#[serde(flatten)] hook_type: Option<HookType>`, and `HookType::Command`
   _also_ has a `command: String` field. Setting both
   `hook_type = Some(HookType::Command { command: ... })` and trying to
   serialize (or round-trip through YAML) produces a duplicate `command`
   key and fails deserialization. The bridge emits script/command hooks
   via the shorthand (`HookDefinition.command`) and leaves
   `HookDefinition.hook_type` as `None` to work around this. See commit
   `f467bd23c`.

4. **`fabro-test` managed settings marker** — the helper writes a
   `# fabro-test managed storage_dir` comment as the first line of
   injected settings.toml files. Functions that read the file (like
   `settings_storage_dir` for `isolated_server`) must detect the marker
   and treat the managed storage root as _not_ user-explicit, or
   `isolated_server` will pick up the shared storage dir and the test
   will fail with `assertion left != right` on the storage dir. See
   commit `b57248236`.

5. **`effective_settings::apply_server_defaults`** copies the **full**
   server-side Settings shape (llm, sandbox, setup, checkpoint,
   pull_request, artifacts, hooks, mcp_servers, github, slack, fabro)
   into the resolved CLI settings in RemoteServer / LocalDaemon modes.
   This is intentional — it matches the pre-Stage-3 behavior and makes
   `fabro-server::server::tests::start_run_persists_full_settings_snapshot`
   work. If you refactor the bridge during Stage 6, make sure the
   equivalent propagation lands in whatever replaces it.

6. **User layer trust boundary**: `effective_settings` strips `cli` and
   `server` from the `workflow.toml` and `fabro.toml` layers in
   RemoteServer / LocalDaemon modes, but **the user layer
   (`~/.fabro/settings.toml`) is never stripped** — owner-specific
   domains are only legal there. If you're tempted to strip them
   uniformly, re-read R16 and commit `2fc85282b`.

7. **Clippy test warnings**: `cargo clippy --workspace --tests -- -D warnings`
   has two pre-existing issues in `fabro-interview/src/control.rs`
   (absolute paths for `tokio::task::yield_now`). They're unrelated to
   the settings refactor — leave them alone or fix them in a tiny
   side-quest PR. The workspace-level (non-tests) clippy is already
   green.

## Open design questions for you to decide

1. **Should `ConfigLayer::resolve(self) -> Settings` survive in any form?**
   The natural rename is `into_file(self) -> SettingsFile`, but many
   callers genuinely want a "final resolved view" that has applied env
   interpolation, applied defaults, etc. Decide whether that's an
   explicit `ResolvedSettings` type (new, v2-shaped) or whether it's
   just `SettingsFile` with a contract that consumers resolve
   `InterpString` themselves at read time.

2. **Post-layering env interpolation resolution pass** — the original
   plan calls for a pass in `fabro-config/src/interp_pass.rs` that
   resolves every `InterpString` in the merged `SettingsFile` using
   provenance tagging. I left this undone because `InterpString::resolve`
   is adequate for the bridge output. Stage 6 is the right moment to
   build the proper pass so the DTOs in Stage 6.6 can rely on
   provenance. Requirements R79–R81 and the "Validation Boundary"
   section of the requirements doc cover the rules.

3. **Fail-closed server auth posture** — R52/R53 + "Default server
   auth posture" in the plan say that if `server.auth` is absent or
   resolves to no enabled API / web auth strategies, normal server
   startup must refuse to start, with demo and test helpers free to
   opt in to insecure startup. I did not wire this into
   `fabro-server/src/server.rs`. Decide when it should land — doing it
   in the same PR as Stage 6.6 keeps auth-related changes together.

4. **`runtime.rs` model-ref ambiguity registry** — `ModelRef::resolve`
   takes a `&dyn ModelRegistry` and errors on ambiguous bare tokens.
   There's no runtime implementation of `ModelRegistry` yet. Decide
   whether to implement it against `fabro-model::Catalog` in Stage 6,
   or leave model-ref resolution as a consumption-time concern the
   model selector already handles.

5. **`run.scm.<provider>` subtree depth** — only `run.scm.github` is
   defined as a unit struct placeholder right now. Requirements R64 says
   "provider-specific details live in provider-specific nested tables".
   When the first real SCM provider leaf lands, add fields under
   `v2::run::ScmGitHubLayer` and mirror the pattern for future
   providers.

6. **`flatten` + `HashMap` + `deny_unknown_fields`** does NOT work
   together in serde. Every time you think "I can just flatten a
   HashMap here for provider-specific fields," resist. Use an
   enumerated list of known-provider subfields instead (that's why
   `RunSandboxLayer`, `NotificationRouteLayer`, `InterviewsLayer`,
   `RunScmLayer`, `ServerIntegrationsLayer`, etc. have explicit
   `github`/`slack`/`discord`/`teams`/`local`/`s3` fields). Adding a new
   provider means adding a new field.

## Repo conventions you'll hit

- **Rust import style** (from `CLAUDE.md`): types imported by name,
  functions via parent module, no glob imports in production code
  except in test modules. The v2 schema code follows this throughout.
- **Shell quoting in sandbox code**: always `shell_quote()` /
  `shlex::try_quote`. Don't hand-roll `.replace('\'', "'\\''")`.
- **Commits**: conventional style. Incremental commits per logical
  unit. Do not force-push. Do not amend. Stage 1–5 commits are the
  model.
- **Tests**: match existing patterns in each crate. `insta` for
  snapshots. `e2e_test` attribute for dual-mode tests. Use
  `fabro_test::test_http_client()` rather than `reqwest::Client::new()`
  for local HTTP in tests (macOS proxy discovery overhead).

## Success criteria for Stage 6

The refactor is **done** when:

- [ ] `git grep 'fabro_types::Settings\b'` returns zero hits outside of
      the legacy type file that's about to be deleted.
- [ ] `git grep 'bridge_to_old'` returns zero hits.
- [ ] `lib/crates/fabro-types/src/settings/v2/` no longer exists as a
      subdirectory — its contents are promoted to `settings/*`.
- [ ] `lib/crates/fabro-types/src/combine.rs` is deleted (the trait
      only existed to serve legacy flat types).
- [ ] `lib/crates/fabro-config/src/{hook,mcp,sandbox,server,run,user}.rs`
      are either deleted or reduced to a thin `pub use ...::v2::...`
      re-export shell, depending on your preference for the external
      surface.
- [ ] `docs/api-reference/fabro-api.yaml` `ServerSettings` and
      `RunSettings` schemas are explicit allow-list DTOs, not reflections
      of the flat legacy shape.
- [ ] `lib/packages/fabro-api-client` and the Rust progenitor client are
      regenerated from the new spec.
- [ ] `apps/fabro-web/app/routes/workflow-detail.tsx` `workflowData`
      literal matches the new `RunSettings` DTO.
- [ ] The `cargo fmt` / `cargo build` / `cargo clippy -D warnings` /
      `cargo nextest run --workspace` / `bun run typecheck` / `bun test`
      / `bun run build` gates all stay green.

Good luck! The hard cut is behind you — Stage 6 is mechanical from
here.
