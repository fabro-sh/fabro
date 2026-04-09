# Settings TOML Redesign Implementation Plan

## Summary

Use `docs/brainstorms/2026-04-08-settings-toml-redesign-requirements.md` as the source of truth and land this as a hard cut: replace the flat and organic config schema everywhere, update all loaders and consumers to the new namespaced model, and regenerate all outward-facing examples and contracts in the same change.

Fabro is still greenfield. This plan intentionally optimizes for the best steady-state code rather than backwards compatibility:

- one user-facing schema, not old and new in parallel
- one hard-cut contract update for config files and settings payloads
- no user-facing compatibility layer

This can still land as one cohesive PR. The staged sequence below is an internal implementation order so the work stays mechanically sane while the refactor is in flight.

This refactor is centered on four seams:

- schema and parsing in `lib/crates/fabro-types/src/settings/` and `lib/crates/fabro-config/src/config.rs`
- layering and trust-boundary resolution in `lib/crates/fabro-config/src/effective_settings.rs`
- CLI, workflow, agent, MCP, sandbox, and server consumers across the Rust workspace
- public contracts in `docs/api-reference/fabro-api.yaml`, generated clients, generated config files, and `apps/fabro-web`

## Public Types And Interfaces

- Replace the flat `fabro_types::Settings` shape with a resolved namespaced settings tree matching the redesign:
  - `_version`
  - `project`
  - `workflow`
  - `run`
  - `cli`
  - `server`
  - `features`
- Replace the current `ConfigLayer` shape with a sparse namespaced parse tree. A temporary in-repo bridge between old and new internal types is acceptable only to keep intermediate stages compiling; it must not become a user-visible compatibility layer and must be deleted by the end of the cut.
- Treat `cli.*` and `server.*` as schema-valid everywhere but runtime-consumed only from local `settings.toml` plus explicit process-local overrides.
- Replace legacy flat run sections and fields with namespaced equivalents, including:
  - `goal` and `working_dir` under `[run]`
  - `vars` to `[run.inputs]`
  - `labels` to `project.metadata`, `workflow.metadata`, and `run.metadata`
  - `llm` to `[run.model]`
  - `setup` to `[run.prepare]`
  - `mcp_servers` to `[run.agent.mcps.<name>]` or `[cli.exec.agent.mcps.<name>]`, depending on the consumer
  - `exec` to `[cli.exec]`
  - flat server sections to `[server.*]`
- Treat `vars -> run.inputs` as a behavioral change, not just a rename. `run.inputs` intentionally replaces the inherited map wholesale rather than merging by key.
- Replace legacy project shape `[fabro].root` with `[project].directory`.
- Replace hook merge identity from effective-name semantics to optional explicit `id`, while keeping `name` human-facing only.
- Replace string-command hook and launcher shorthand with one execution-language rule:
  - `script = "..."` for shell-evaluated commands
  - `command = ["..."]` for argv launches
  - mutually exclusive
- Treat `script` and `command` fields as trusted executable config. Repo-scoped config using these fields executes with the consuming process privileges. Env interpolation inside `script` is raw string substitution, not shell-escaped templating.
- Replace old MCP shapes with agent-scoped MCPs:
  - `[run.agent.mcps.<name>]`
  - `[cli.exec.agent.mcps.<name>]`
- Keep `SecretStore` and provider ambient auth as the credential sources for secrets. The redesigned config should describe selectors and non-secret knobs, not become a general secret transport.
- Keep `/api/v1/settings` as the endpoint path, but replace broad `Settings` serialization with an explicit public DTO. The hard cut is the schema and payload shape, not the path name.

## Resolved Deferred Questions

- `run.scm` first pass:
  - core fields are `provider`, `owner`, and `repository`
  - provider-specific capability leaves live under `[run.scm.<provider>]`
  - branch and PR behavior stay out of `run.scm` in this cut and remain on `[run.pull_request]` or runtime context
- object-store envelope first pass:
  - provider-neutral envelope fields are `provider` and optional `prefix`
  - provider-specific tables live under `[server.artifacts.<provider>]` and `[server.slatedb.<provider>]`
  - `local` uses `root`, defaulting to `server.storage.root` when omitted
  - `s3` carries bucket and region plus optional endpoint and path-style settings
  - provider credentials come from `SecretStore`, `${env.NAME}`, or ambient provider auth rather than first-pass secret fields in TOML
- MCP surface first pass:
  - common fields are `enabled`, `type`, `startup_timeout`, and `tool_timeout`
  - `startup_timeout` and `tool_timeout` use the shared duration type from the value-language helpers
  - `type = "http"` uses `url` plus optional `headers`
  - `type = "stdio"` requires exactly one of `script` or `command` and may include `env`
  - `type = "sandbox"` requires exactly one of `script` or `command`, requires `port` as an integer, and may include `env`
- notification route surface first pass:
  - route envelope fields are `enabled`, `provider`, and `events`
  - provider-specific destination fields live under `[run.notifications.<name>.<provider>]`
  - first-pass chat destinations for Slack, Discord, and Teams use `channel`
- duration parser first pass:
  - one shared parser accepts a single unit suffix per value: `ms`, `s`, `m`, `h`, or `d`
  - composed values like `1h30m` are not supported in first pass; use the smallest needed unit instead
  - one shared canonical renderer prints human-readable durations in the same single-unit form
- size parser first pass:
  - one shared parser accepts bare integers plus `B`, `KB`, `MB`, `GB`, `TB`, and `KiB`, `MiB`, `GiB`, `TiB`
  - `KB`, `MB`, `GB`, and `TB` are decimal (powers of 1000); `KiB`, `MiB`, `GiB`, and `TiB` are binary (powers of 1024)
  - bare values default to `GB`
  - fractional values are not supported in first pass
  - one shared canonical renderer prints human-readable sizes using the largest decimal unit that represents the value as an integer multiple
  - config-language parsing stays permissive; provider layers remain responsible for stricter admissible-value validation such as Daytona-specific CPU and memory limits
- object-store `provider` field is a closed enum. First-pass variants are `local` and `s3`. Unknown providers hard-fail against the schema rather than passing through as opaque strings.
- `SecretStore` access is not referenced from user TOML in first pass. Consumers read secrets via existing server-side `SecretStore` code paths; the config schema does not introduce a `${secret.NAME}` interpolation form. If TOML-level secret references become necessary later, they are a separate schema bump.

## Implementation Changes

### 1. Replace the config parse tree and resolved types

- Introduce a new namespaced parse tree for `_version`, `project`, `workflow`, `run`, `cli`, `server`, and `features`; do not alias old field names forward.
- Redesign `fabro_types::Settings` to match the new resolved schema rather than preserving the old flat representation internally.
- Treat strict unknown-key handling as a parse-architecture change, not just a derive tweak. The loader must validate against the full union schema before consumer-specific filtering and must surface targeted rename hints for legacy keys.
- Add explicit `_version` handling before deeper validation:
  - missing defaults to `1`
  - legacy `version` hard-fails with a rename hint
  - unsupported higher versions hard-fail with an upgrade hint
- Stage the new value-language helpers explicitly instead of bundling them into one opaque parser rewrite:
  - one shared duration type and parser for config-facing time values
  - one shared size type and parser for memory and disk values
  - one model-reference parser for `run.model.fallbacks`
  - one interpolation representation for `${env.NAME}` tokens, including substring interpolation and multiple tokens per string
  - one splice-capable string-array helper for the exact `"..."` semantics in the requirements doc
- Implement the resolved first-pass shapes from the previous section directly in the parse tree and resolved settings types rather than leaving them to implementer choice.
- Redesign run model types to cover:
  - `run.metadata`
  - `run.inputs`
  - `run.model`
  - `run.git`
  - `run.prepare.steps`
  - `run.execution`
  - `run.checkpoint`
  - `run.sandbox`
  - `run.notifications.<name>`
  - `run.interviews`
  - `run.agent`
  - `run.agent.mcps.<name>`
  - `run.hooks`
  - `run.scm`
  - `run.scm.<provider>`
  - `run.pull_request`
  - `run.artifacts`
- Redesign CLI types to cover:
  - `cli.target`
  - `cli.target.tls`
  - `cli.auth`
  - `cli.exec`
  - `cli.exec.model`
  - `cli.exec.agent`
  - `cli.exec.agent.mcps.<name>`
  - `cli.output`
  - `cli.updates`
  - `cli.logging`
- Redesign server types to cover:
  - `server.listen`
  - `server.listen.tls`
  - `server.api`
  - `server.web`
  - `server.auth.api`
  - `server.auth.web.providers.<provider>`
  - `server.storage`
  - `server.artifacts`
  - `server.slatedb`
  - `server.scheduler`
  - `server.logging`
  - `server.integrations.<provider>`
- Keep provider-neutral envelopes and provider-specific nested tables where the requirements already locked them:
  - sandbox
  - notifications
  - interviews
  - object stores
  - SCM provider leaves
- Keep model config intentionally provider-neutral and implement the fallback grammar exactly as specified in the requirements doc.

### 2. Narrow merge changes to the paths whose behavior actually changes

- Keep `Combine` as the default layering mechanism where it still matches the requirements. Add explicit custom merge only for paths whose behavior changes.
- Encode the merge matrix from the requirements doc directly in code, with custom logic only for:
  - replace-by-default maps like `run.inputs`, `project.metadata`, `workflow.metadata`, and `run.metadata`
  - sticky merge-by-key maps like `run.sandbox.env`
  - splice-aware string arrays
  - whole-list replacement for `run.prepare.steps`
  - field-merge keyed objects like notifications, MCPs, and web-auth providers
  - ordered hook merging by optional `id`
- Make splice-capable arrays explicit in the implementation rather than shape-driven. In the first pass, the only splice-capable array paths are:
  - `run.model.fallbacks`
  - `run.notifications.<name>.events`
- Treat `"..."` in all non-splice arrays as a hard error rather than data or a silent no-op.
- Keep inactive provider and strategy subtables inert when the selected provider changes; validate and consume only the selected subtree.
- Move env interpolation out of the current sandbox-only whole-value resolver and into a post-layering resolution pass that runs only on consumed string fields.
- If any `${env.NAME}` token in a consumed string fails to resolve, fail the entire field with an error that identifies both the unresolved token and the config path.
- Track interpolation provenance so env-sourced resolved values can be redacted consistently in outward-facing serialization, not just in the CLI.
- Keep hook ordering stable:
  - `id`-matched replacement happens in place
  - anonymous hooks from higher-precedence files append after the fully merged inherited hook list
  - duplicate `id` values in one file hard-fail

### 3. Rebuild resolution, trust boundaries, and safe serialization

- Rework `EffectiveSettingsLayers` and `resolve_settings()` so owner-specific domains are consumed only from `~/.fabro/settings.toml` plus flags and env overrides.
- Remove the current “merge everything, then strip server-owned fields” model. Build shared layered domains and owner-specific domains separately from the start.
- Preserve today’s `exec` routing behavior:
  - configured CLI target defaults affect commands that use server targeting
  - `fabro exec` still requires explicit `--server`
- Make the default server auth posture explicit and fail-closed:
  - if `server.auth` is absent or resolves to no enabled API or web auth configuration, normal server startup must refuse to start
  - demo and test helpers may continue to inject explicit insecure settings where needed, but insecure startup must be opt-in rather than accidental
- Settings API exposure:
  - replace raw resolved settings serialization with explicit public DTOs
  - two distinct exposure scopes, each with its own DTO:
  - scope 1: `/api/v1/settings` (server configuration view)
    - first-pass allow-list:
      - `server.api.url`
      - `server.web.enabled`
      - `server.web.url`
      - enabled state for `server.auth.web.providers.*`
      - non-secret `server.scheduler` values
    - denies everything else, including all `project.*`, `workflow.*`, `run.*`, `cli.*`, and any `server.*` path not explicitly allowed (notably `server.listen`, `server.listen.tls.*`, `server.auth.api`, `server.integrations.*`, `server.artifacts*`, `server.slatedb*`, local secret-store paths, and any env-resolved secret values)
  - scope 2: `/api/v1/runs/:id/settings` and run-settings snapshots exposed via API (run configuration view)
    - allows the resolved `run.*` tree so the frontend run-settings page and equivalent consumers can render it
    - denies:
      - any resolved string value tagged as `${env.NAME}`-sourced (via the interpolation provenance tracking)
      - provider-credential fields under `run.notifications.*.<provider>` even when not env-sourced
      - env values under `run.agent.mcps.*.env` that were env-interpolated
      - any field explicitly marked sensitive in its type (for example, tokens or keys)
    - also denies all `project.*`, `workflow.*`, `cli.*`, and `server.*`; these are not part of a run-configuration view
- Apply the matching exposure scope and redaction rules consistently across all outward-facing settings renderers:
  - `fabro settings` uses the server scope for server-facing rendering and the run scope for run-facing rendering
  - `/api/v1/settings` uses the server scope
  - `/api/v1/runs/:id/settings` and any API-exposed run-settings snapshots use the run scope
  - logs and emitted settings-like debug output use whichever scope matches the payload kind
- Trust model:
  - `script` and `command` fields in repo-scoped config are trusted executable config and should be reviewed like code
  - those fields execute with the consuming process privileges; the config system does not sandbox them
  - `${env.NAME}` interpolation inside `script` is raw substitution, not shell quoting or shell-safe templating
- Keep command-local override layering separate from machine settings loading:
  - `run`, `preflight`, and manifest code still build layered run defaults
  - `exec` still loads machine CLI defaults directly
  - `settings` still assembles effective layers deliberately
- Classify server settings as startup-only vs live-reloadable in the first pass:
  - live-reloadable:
    - `server.logging`
    - `server.scheduler`
  - startup-only:
    - `server.listen`
    - `server.listen.tls`
    - `server.api`
    - `server.web`
    - `server.auth`
    - `server.storage`
    - `server.artifacts`
    - `server.slatedb`
    - `server.integrations`
- Update server runtime application logic to stop assuming old flat fields like `storage_dir`, `artifact_storage`, `api`, and `web`.
- Make the persisted-settings decision explicit: old run-settings snapshots and local dev state are not guaranteed to survive the hard cut. Tests, fixtures, and generated examples should be rewritten; no snapshot migration layer is planned.

### 4. Migrate all consumers, scaffolds, and contracts

- Update CLI overrides, run manifest building, workflow discovery, project discovery, and remote and local-daemon settings application to the new schema.
- Update all crates that currently consume settings or config layers, not just the CLI and server entrypoints. At minimum this includes:
  - `fabro-cli`
  - `fabro-server`
  - `fabro-workflow`
  - `fabro-agent`
  - `fabro-mcp`
  - sandbox-facing config consumers
  - hook execution consumers
  - test helpers in `fabro-test`
- Update server start and foreground command flows to read and apply the new server config shape.
- Update `SecretStore` integration points so server and installer flows continue to source secrets out of band while the new config shape only carries non-secret selectors and toggles.
- Update scaffolding and installers so generated `settings.toml`, `fabro.toml`, and `workflow.toml` use `_version` and the new namespaced sections.
- Update install-time config writers to stop editing legacy `[git]`, `[web]`, `[api]`, and similar flat sections.
- Update the server `/api/v1/settings` response and any run-settings snapshot payloads to the new allow-listed resolved shape, then regenerate Rust and TypeScript clients from OpenAPI.
- Update `apps/fabro-web` and any generated TypeScript consumers to the new settings contract. The live `/settings` and `/runs/:id/settings` routes currently `JSON.stringify` the full response, so they remain shape-agnostic, but the static `workflowData` fallback in `apps/fabro-web/app/routes/workflow-detail.tsx` uses the old schema shape and must be rewritten against the new `RunSettings` type.
- Update docs and examples in `docs/reference/`, especially:
  - `user-configuration.mdx`
  - `cli.mdx`
  - any other config examples that currently show `[llm]`, `[exec]`, `[server]`, `[sandbox]`, `[fabro]`, or `version = 1`
- Update installer, repo-init, and workflow-create generated content so no new files are emitted in the old schema after the cutover lands.

## Sequencing

Implement in these internal compile-preserving stages:

1. Add the new value-language helpers and namespaced sparse parse structs alongside the current code so the repo still builds while parser architecture is being introduced.
2. Add the new resolved settings tree plus a temporary internal bridge between old and new types so callers can migrate incrementally without freezing the repo in an unbuildable state.
3. Switch parsing and layering to the new schema, strict validation, merge behavior, trust boundaries, and env interpolation. This is where legacy user config starts hard-failing.
4. Migrate consumers crate by crate:
   - `fabro-cli`
   - `fabro-server`
   - `fabro-workflow`
   - `fabro-agent`
   - `fabro-mcp`
   - hook, sandbox, and test-helper consumers
5. Update `/api/v1/settings`, OpenAPI, generated clients, `apps/fabro-web`, scaffolds, installers, and docs to the new contract.
6. Remove the old flat settings types, the temporary bridge, legacy fixtures, and any now-dead merge logic.

This remains a hard cut. These stages describe implementation order, not a staged user rollout.

## Test Plan

- Add parser and unit coverage for:
  - `_version` defaulting and failure modes
  - representative hard failures for legacy keys and unknown keys
  - model fallback token parsing and ambiguity errors
  - duration and size parsing
  - substring and multi-token `${env.NAME}` interpolation
  - splice-array rules on allowed paths
  - hard failure for `"..."` on non-splice paths
  - hook `id` replacement and anonymous append ordering
- Add layering and resolution coverage for:
  - `run.inputs` replace semantics
  - `run.sandbox.env` sticky merge semantics
  - keyed object merge and disable behavior
  - owner-specific trust boundaries for `cli.*` and `server.*`
  - inactive provider subtables remaining inert
  - default server auth fail-closed behavior when `server.auth` is absent
- Add serialization and exposure coverage for:
  - `fabro settings` redaction
  - `/api/v1/settings` allow-list behavior
  - exclusion of TLS paths, auth internals, object-store credentials, and env-resolved secrets
  - any API-exposed run-settings snapshot redaction behavior
- Add behavior coverage for:
  - `project.directory`-based workflow discovery
  - `run.inputs` replace semantics
  - hook identity via explicit `id`
- Update CLI integration tests in:
  - `lib/crates/fabro-cli/tests/it/cmd/config.rs`
  - `lib/crates/fabro-cli/tests/it/cmd/exec.rs`
  - `lib/crates/fabro-cli/tests/it/cmd/repo_init.rs`
  - `lib/crates/fabro-cli/tests/it/cmd/workflow_create.rs`
- Update server and API coverage for:
  - `/api/v1/settings`
  - startup-only vs live-reloadable server settings
  - run settings snapshots
  - any tests assuming old flat server settings fields
- Update frontend and generated-client expectations after the OpenAPI change.
- Update doc examples and snapshot tests that assert generated config files or `fabro settings` output.

## Assumptions And Defaults

- Hard cut only: one user-facing schema, no compatibility aliases, and no user-facing compatibility layer.
- A temporary internal bridge between old and new settings types is acceptable only to keep intermediate stages compiling and must be removed before the work is done.
- `run.inputs` replaces inherited values wholesale; `run.sandbox.env` remains merge-by-key and sticky.
- `cli.*` and `server.*` remain schema-valid in all files but are runtime-inert outside local `settings.toml`.
- Provider-specific subtables coexist inertly; only the selected provider or strategy subtree is validated and consumed.
- Object-store and integration credentials continue to come from `SecretStore`, `${env.NAME}`, or ambient provider auth rather than new first-pass secret fields in TOML.
- `/api/v1/settings` remains the endpoint path, but its payload shape becomes a new allow-listed public contract.
