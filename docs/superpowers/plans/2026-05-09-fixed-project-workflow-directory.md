# Fixed Project Workflow Directory Plan

**Summary**
Remove the `[project].directory` feature entirely so project workflows are always discovered, created, listed, and resolved from `<repo_root>/.fabro/workflows/*`. Because this is greenfield, configs that still contain `[project] directory = ...` should fail schema validation instead of being ignored or migrated. User-level workflows under `~/.fabro/workflows` remain unchanged as an additional fallback/list section.

**Key Changes**
- Remove `directory` from the project config schema and resolved settings type:
  - Delete `ProjectLayer::directory` and `ProjectNamespace::directory`.
  - Remove `[project] directory = "."` from built-in defaults.
  - Update project resolver/tests so `[project]` only carries `name`, `description`, and `metadata`.
- Make project Fabro root fixed:
  - Change `resolve_fabro_root` to validate `.fabro/project.toml`, then return `config_path.parent()` directly.
  - Update callers to handle `Result<PathBuf>` and use `<discovered .fabro>/workflows`.
  - Delete now-unused path normalization code for joining `project.directory`.
- Update public API shape:
  - Remove `directory` from `ProjectNamespace` in `docs/public/api-reference/fabro-api.yaml`.
  - Update `workflow_settings_round_trip` expectations.
  - Regenerate `lib/packages/fabro-api-client` so TypeScript `ProjectNamespace` no longer has `directory`.
- Update docs and hints:
  - Remove references to `project.directory` and the old `fabro.root -> project.directory` rename hint.
  - Add/keep explicit docs that project workflows live at `<repo_root>/.fabro/workflows/<name>/`.

**Test Plan**
- Config tests:
  - Empty settings still resolve project metadata defaults.
  - `[project] directory = "..."` is rejected as an unknown field.
  - `resolve_fabro_root` always returns the `.fabro` directory containing `project.toml`.
- CLI integration tests:
  - Replace custom-root workflow create/list tests with fixed-root assertions.
  - Confirm `fabro workflow create <name>` writes `.fabro/workflows/<name>/workflow.{fabro,toml}`.
  - Confirm named workflow resolution reads `.fabro/workflows/<name>/workflow.toml`.
- API/client checks:
  - `cargo build -p fabro-api`
  - `cd lib/packages/fabro-api-client && bun run generate`
  - `cargo nextest run -p fabro-config -p fabro-api -p fabro-cli`
  - `cd apps/fabro-web && bun run typecheck`

**Assumptions**
- No backwards compatibility or migration period: old configs with `[project].directory` should fail.
- This change only fixes the project workflow directory. User workflows remain available unless a separate decision removes them later.
- GitHub retrieval itself is out of scope here; this refactor makes the repo path deterministic for that future work.
