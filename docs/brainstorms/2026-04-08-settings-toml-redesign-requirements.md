---
date: 2026-04-08
topic: settings-toml-redesign
---

# Settings TOML Redesign

## Problem Frame

Fabro has three layered TOML config files:

- `~/.fabro/settings.toml` for machine defaults
- `fabro.toml` for project defaults
- `workflow.toml` for workflow-local defaults

All three layer into one unified settings object. In same-host setups, the CLI and server may both read `~/.fabro/settings.toml`. In split-host setups, the CLI host and server host each read their own local `settings.toml` and consume only the sections relevant to that process.

The current config shape grew organically. It now has naming drift, mixed ownership boundaries, uneven merge semantics, and several top-level sections that no longer reflect a clean mental model. Fabro is still greenfield with no deployed compatibility burden, so this is the right time to make a hard cut and establish a coherent, future-proof config language.

The new design must optimize for:

- a small, elegant top-level structure
- coherent ownership boundaries between run, CLI, server, project, and workflow concerns
- paste-anywhere ergonomics across the three config files
- explicit and predictable layering semantics
- future provider growth without provider-specific sprawl in the core model

## Requirements

**Config language and layering**

- R1. `settings.toml`, `fabro.toml`, and `workflow.toml` must share the same schema. Files differ by precedence only, not by allowed sections.
- R2. Any config section may appear in any Fabro TOML file. Consumers must ignore sections they do not use.
- R3. The top-level schema must be strictly namespaced. The only top-level config domains are `[project]`, `[workflow]`, `[run]`, `[cli]`, `[server]`, and `[features]`, plus reserved underscore-prefixed meta keys.
- R4. The schema version key must be `_version`, not `version`.
- R5. Underscore-prefixed keys are reserved only at the top level for config-language metadata. Nested underscore keys are not part of the language.
- R6. The config language must not add a general unset mechanism in this pass.
- R7. Unknown config keys against the full union schema must be hard errors. This is schema validation, not consumer-specific validation.
- R8. Duplicate keys and duplicate hook `id` values within the same file must be hard errors.

**Object model and namespace boundaries**

- R9. `[workflow]` and `[run]` must be sibling top-level sections. Do not nest `[workflow.run]`.
- R10. `[workflow]` is descriptive for now. It must support first-class fields such as `name`, `description`, optional `graph`, and `metadata`. Structured workflow inputs are deferred.
- R11. `workflow.toml` remains the canonical workflow config filename. The default graph file remains `workflow.fabro`, with optional `[workflow].graph` override.
- R12. `[project]` must be a first-class project object with fields such as `name`, `description`, `directory`, and `metadata`.
- R13. `project.directory` replaces the old Fabro project root concept and means the Fabro-managed project directory inside the repo, defaulting to `fabro/`.
- R14. Workflow discovery remains conventional: `<project.directory>/workflows/<name>/workflow.toml`. Do not add a separate configurable workflows directory.
- R15. `[run]` is the shared execution domain. It may appear in all three files and layer normally.
- R16. `[cli]` and `[server]` are owner-first process domains. Settings belong to the process that reads them, not to whether the host is “local” or “remote.” For trust-boundary reasons, CLI and server processes consume their owner-specific sections only from the local `~/.fabro/settings.toml` plus explicit process-local overrides. Same-shaped `cli.*` and `server.*` stanzas in `fabro.toml` and `workflow.toml` remain schema-valid but inert for those processes.
- R17. `[features]` is a reserved cross-cutting namespace for Fabro capability flags only. It must have a high admission bar and must not become a junk drawer.
- R18. Logging is process-owned. Use `[cli.logging]` and `[server.logging]`; do not keep a shared logging section.

**Run model**

- R19. `[run]` must keep a small direct manifest surface for cross-cutting run fields such as `goal` and `working_dir`.
- R20. `working_dir` replaces `work_dir`.
- R21. `metadata` replaces Fabro-owned `labels` and exists on `project`, `workflow`, and `run` as flat string-to-string maps.
- R22. `run.inputs` replaces `vars`. `run.inputs` must accept TOML scalar values. `metadata` remains string-to-string. `run.inputs` intentionally replaces the full inherited map rather than merging by key.
- R23. `[run.model]` is the default model selection surface for LLM-backed workflow stages. `[run.agent]` is only for agent-specific settings.
- R24. `[run.agent]` owns agent-only knobs such as `permissions` and `mcps`. `[run.sandbox]` owns the sandbox selection and execution-environment surface, including `provider`, shared sandbox knobs, `env`, and provider-specific nested tables.
- R25. `run.agent.permissions` must remain a simple enum string, not an object.
- R26. `[run.git]` and `[run.scm]` must remain separate concepts. `git` is local Git behavior such as commit author; `scm` is remote host/provider behavior.
- R27. `[run.pull_request]` remains the provider-neutral run surface for PR behavior.
- R28. `[run.prepare]` is the run preparation surface and replaces the old `setup` naming.
- R29. `run.prepare` must be an ordered list of steps at `[[run.prepare.steps]]`.
- R30. `run.prepare.steps` replaces as a whole ordered list across layers.
- R31. `[run.execution]` groups run-conduct knobs such as `mode`, `approval`, and `retros`. In the first pass, `mode` is `normal | dry_run`, `approval` is `prompt | auto`, and `retros` is a positive-form boolean. Do not keep negated or ambiguous booleans like `no_retro`.
- R32. `[run.checkpoint]` remains its own run domain. `[[run.hooks]]` is the ordered run-hook surface for run lifecycle automation.
- R33. `[run.artifacts]` defines what run artifacts are collected. Server-side artifact storage is separate.
- R34. `[run.notifications.<name>]` is a keyed set of named notification routes. Notification routes merge by field across layers and support `enabled = false`.
- R35. `[run.interviews]` is a single optional external/default interview delivery surface. HTTP/API answering is always available and is not modeled as an interview provider.
- R36. Notification and interview event selection must use raw Fabro event names, not a second notification-specific vocabulary.

**CLI model**

- R37. CLI target resolution lives under `[cli.target]`, not `[server]` or `[cli.remote]`.
- R38. CLI target transport must be explicit with `type = "http" | "unix"` and transport-specific fields, not overloaded scheme strings.
- R39. CLI transport TLS lives under `[cli.target.tls]`.
- R40. CLI auth is a separate domain at `[cli.auth]`, with explicit `strategy` selection. `strategy = "none"` explicitly disables inherited auth.
- R41. `fabro exec` defaults live under `[cli.exec]`, with `[cli.exec.model]` and `[cli.exec.agent]` split cleanly.
- R42. Generic CLI output defaults live under `[cli.output]`, not under `exec`.
- R43. Upgrade checks live under `[cli.updates]`.
- R44. Idle sleep prevention lives under `[cli.exec]`.

**Server model**

- R45. `[server]` is a namespace container. Actual settings live in named subdomains.
- R46. The server binds the API and web surfaces on one shared listener. Bind transport must live under `[server.listen]`, not separately under `[server.api]` and `[server.web]`.
- R47. `[server.listen]` must use explicit transport types such as `tcp` and `unix`.
- R48. Shared listener TLS must live under `[server.listen.tls]`.
- R49. `[server.api]` holds only API-surface settings such as public URL, not bind/auth/TLS settings.
- R50. `[server.web]` holds only web-surface settings such as `enabled` and public URL, not auth settings.
- R51. Server auth is a cohesive domain at `[server.auth]`.
- R52. `[server.auth.api]` must support multiple strategies concurrently.
- R53. `[server.auth.web]` must support multiple providers concurrently via `[server.auth.web.providers.<provider>]`.
- R54. Web-auth access rules remain provider-neutral on `[server.auth.web]`; provider-specific config lives under each provider subtable.
- R55. Web-auth providers must support `enabled = true|false` to disable inherited provider config cleanly.
- R56. Inbound provider webhooks belong under provider integrations such as `[server.integrations.github.webhooks]`, not under generic server auth or web sections.
- R57. `[server.storage]` refers only to a managed local disk root on the host. It must expose a single managed `root`.
- R58. `[server.artifacts]` is separate from `[server.storage]` and is backed by an object store provider.
- R59. `[server.slatedb]` is separate from both `[server.storage]` and `[server.artifacts]`. It is backed by its own object store provider and may include database-specific tunables such as `flush_interval`.
- R60. `[server.scheduler]` owns server-managed execution policy such as concurrency limits. It must not compete with `[run]`.

**Provider and future-proofing rules**

- R61. Core Fabro concepts should be provider-neutral. Provider-specific details should live in provider-specific nested tables where the domain genuinely requires them.
- R62. Sandbox config must remain provider-specific because provider differences are too large to hide behind one flat abstraction.
- R63. Model config must remain intentionally provider-neutral. It should not grow provider-specific subtables. `run.model.fallbacks` is a single ordered array of model references. Each entry may be a bare provider token such as `openai`, a bare model alias or model id such as `gpt-5.4`, or a qualified reference such as `gemini/gemini-flash`. Bare references are allowed only when unambiguous. Ambiguous bare references must hard-error and require qualification. A bare provider token means “choose the best matching model from that provider.”
- R64. SCM config must be provider-neutral at the core (`[run.scm]`) with room for provider-specific nested tables such as `[run.scm.github]` only where necessary.
- R65. Chat platforms such as Slack, Discord, and Teams are integrations. Their server-owned setup lives under `[server.integrations.<provider>]`; run behavior lives under `[run.notifications.*]` and `[run.interviews]`.
- R66. Object-store-backed domains must use a shared pattern: a small provider-neutral envelope plus provider-specific nested tables.
- R67. For local object-store providers, default to `server.storage.root`, but allow explicit local override roots when needed.

**Merge, validation, and runtime semantics**

- R68. Scalars replace.
- R69. Structured tables merge by field.
- R70. Freeform maps replace by default.
- R71. A small, explicit set of maps may merge by key where additive inheritance is the least surprising behavior, including `run.sandbox.env` and provider-native maps such as `run.sandbox.daytona.labels`. These maps are intentionally sticky in v1: higher-precedence layers may overwrite keys but cannot remove inherited keys.
- R72. Arrays replace by default.
- R73. Arrays must support splice semantics via `...` in declared splice-capable string arrays, for example `["...", "c"]` for append and `["a", "..."]` for prepend. At most one exact `"..."` marker may appear per array. In the base layer with no inherited parent, the splice marker resolves to an empty inherited segment. In splice-capable arrays, the literal string value `"..."` is reserved and may not be used as data.
- R74. Security and policy lists must replace by default and only inherit via explicit `...`.
- R75. Keyed named objects such as notifications, MCPs, and web-auth providers must merge by field across layers. User-defined keyed object names in namespaces that also host provider-specific subtables must not equal built-in provider identifiers, to avoid ambiguous shapes such as `[run.notifications.slack.slack]`.
- R76. Named keyed objects that may need to be disabled must support `enabled = false`.
- R77. `[[run.hooks]]` remains an ordered list. Hooks may define an optional `id`; `name` remains human-facing only. Hooks without `id` append. Hooks with the same `id` replace whole entries in place. Hooks without `id` from a higher-precedence layer append after the fully merged inherited hook list, preserving per-file declaration order. Hook ordering remains significant.
- R78. Provider-specific required fields should only be validated when that provider/section is actually consumed.
- R79. Unresolved `${env.NAME}` references should only error when the field is actually consumed.
- R80. The config language must not require separate validation modes for CLI, server, and run config in this pass. Runtime consumption drives context-specific validation.

**String interpolation and value formats**

- R81. Any string field may use `${env.NAME}` interpolation, either as the whole value or as a substring inside a larger string. Multiple `${env.NAME}` tokens may appear in the same string.
- R82. Do not support config-to-config references such as `${run.inputs.foo}` in this pass.
- R83. All time-like values should use human-readable durations such as `"30s"`, `"1m"`, or `"1h"`, not `_ms` or `_secs` fields.
- R84. Memory and disk settings should accept generous human-readable size syntax. Bare values such as `8`, plus `8G`, `8GB`, and `8GiB`, should all parse successfully.
- R85. Docs and examples should use `GB` as the canonical style. Parsing should remain generous.
- R86. CPU remains an integer core count.

**Command execution shape**

- R87. Shell-evaluated actions use `script = "..."`.
- R88. Direct process launches use `command = ["..."]`.
- R89. `script` and `command` are mutually exclusive.
- R90. The `script` xor `command` rule must apply consistently across prepare steps, hooks, and MCP transports that launch a local process. Non-launching MCP transports such as plain HTTP do not use either field.

## Precedence and Override Order

The config language has one schema but two consumption models.

Shared layered domains such as `[project]`, `[workflow]`, `[run]`, and `[features]` use this override order:

1. Explicit process-local command args or flags
2. Explicit process-local environment override channels, where Fabro defines them
3. `workflow.toml`
4. `fabro.toml`
5. `~/.fabro/settings.toml`
6. Built-in defaults

Owner-specific process domains use a narrower trust boundary:

1. Explicit process-local command args or flags
2. Explicit process-local environment override channels, where Fabro defines them
3. `~/.fabro/settings.toml`
4. Built-in defaults

Additional rules:

- String interpolation via `${env.NAME}` is not a separate precedence layer. It is value resolution inside the winning layered config value.
- Server start flags override only the server-consumed settings for that process invocation. They do not change persisted TOML values.
- CLI flags override only the CLI-consumed settings for that process invocation.
- `cli.*` and `server.*` stanzas in `fabro.toml` and `workflow.toml` remain parseable but are not part of runtime precedence for those processes.
- If a future env override channel exists for a setting, it must sit between explicit args/flags and layered TOML.

## Validation Boundary

Schema validation and runtime validation are separate concerns:

- All config files validate against the full union schema before consumer-specific filtering.
- Unknown-key validation and duplicate-key validation run at schema-validation time, not at consumer-specific runtime.
- Lazy validation applies only to provider-specific required fields, selected strategies/providers, and `${env.NAME}` resolution for fields that a consumer actually uses.
- Unused but schema-valid `cli.*` and `server.*` stanzas in lower-trust files remain inert rather than invalid.

## Disable Semantics

The config language must use one explicit rule for inherited config suppression:

- Absence means inherit or express no opinion.
- `enabled = false` disables inherited keyed named objects such as notification routes, MCP entries, and web-auth providers.
- `provider = "none"` or `strategy = "none"` disables inherited singleton selectable sections such as interviews or auth.
- Disabled sections suppress provider-specific required-field validation for their disabled subtree.

## Public URL Semantics

`server.listen` is only the bind transport. It must not be treated as a public URL source.

- `server.api.url` and `server.web.url` are optional public URLs.
- They are not derived from `server.listen`.
- They are not derived from each other.
- If omitted, Fabro must treat the corresponding public URL as unspecified rather than synthesizing one implicitly.

## Normative Merge Matrix

This redesign should specify exact merge behavior for the first-pass config surface rather than relying only on structural categories.

| Path | Merge behavior |
|---|---|
| `project` direct scalar fields such as `name`, `description`, and `directory` | replace by field |
| `project.metadata` | replace |
| `workflow` direct scalar fields such as `name`, `description`, and `graph` | replace by field |
| `workflow.metadata` | replace |
| `run` direct scalar fields such as `goal` and `working_dir` | replace by field |
| `run.metadata` | replace |
| `run.inputs` | replace |
| `run.model` direct scalar fields such as `provider` and `name` | replace by field |
| `run.model.fallbacks` | replace, with `...` splice support |
| `run.git.author` | merge by field |
| `run.execution` | merge by field |
| `run.checkpoint` | merge by field |
| `run.sandbox` direct scalar fields such as `provider` and `preserve` | merge by field |
| `run.sandbox.<provider>` | merge by field |
| `run.sandbox.env` | merge by key |
| provider-native maps such as `run.sandbox.daytona.labels` | merge by key |
| notification route `events` arrays | replace, with `...` splice support |
| `run.pull_request` | merge by field |
| `run.interviews` | merge by field |
| `run.interviews.<provider>` | merge by field |
| `run.prepare.steps` | replace whole ordered list |
| `run.notifications.<name>` | merge by field |
| `run.notifications.<name>.<provider>` | merge by field |
| `run.agent.mcps.<name>` | merge by field |
| `cli.target` | merge by field |
| `cli.auth` | merge by field |
| `cli.exec` | merge by field |
| `cli.exec.model` | merge by field |
| `cli.exec.agent` | merge by field |
| `cli.output` | merge by field |
| `cli.updates` | merge by field |
| `server.listen` | merge by field |
| `server.api` | merge by field |
| `server.web` | merge by field |
| `server.auth.api` | merge by field |
| `server.auth.api.<strategy>` | merge by field |
| `server.auth.web.providers.<name>` | merge by field |
| `server.storage` | merge by field |
| `server.artifacts` | merge by field |
| `server.artifacts.<provider>` | merge by field |
| `server.slatedb` | merge by field |
| `server.slatedb.<provider>` | merge by field |
| `server.scheduler` | merge by field |
| `[[run.hooks]]` | ordered list with special optional-`id` replacement rule |

New config paths added later should declare one of these behaviors explicitly in docs and implementation. Do not let merge behavior be accidental from Rust type shape alone.

## Canonical Rendering

Fabro should parse generously but render consistently in docs and config-inspection output.

- Durations should render in human-readable form such as `30s`, `1m`, or `1h`.
- Memory and disk should render using `GB` in user-facing examples and normalized output.
- `fabro settings` or equivalent config-inspection output should emit canonicalized values rather than the user's original alternate spelling when values have been normalized internally.
- `fabro settings` or equivalent config-inspection output must redact values that were sourced from `${env.NAME}` by default, rather than printing the resolved secret-bearing value verbatim.

## Object Store Credential Semantics

First-pass object store configuration must work without a Fabro-specific secret reference language.

- Object store providers may rely on provider-native ambient auth such as IAM roles, workload identity, local credential files, or equivalent external mechanisms.
- Provider-specific object-store config fields may also take ordinary string values populated via `${env.NAME}`.
- This redesign does not add `${secret.NAME}` or a separate secret-backend reference syntax.

## Executable Config Trust Boundary

Config-executed actions are part of Fabro's trusted configuration model, not the agent permission model.

- `script` and `command` in prepare steps, hooks, and launching MCP transports are executable configuration, not passive metadata.
- These actions execute under the trust boundary of the consuming process.
- They are not mediated by `run.agent.permissions` or `cli.exec.agent.permissions`.
- Users should treat `fabro.toml` and `workflow.toml` as executable project configuration, not as untrusted data blobs.

## Migration and Failure Behavior

This is a hard-cut redesign, but migration still needs explicit failure semantics.

- Missing `_version` defaults to `1` in the first pass.
- `_version` values higher than the parser supports must hard-fail with an upgrade hint before deeper validation continues.
- The legacy top-level `version` key must hard-fail with a targeted rename hint to `_version`.
- Historical keys and obsolete top-level shapes should hard-fail rather than silently aliasing forward.
- Error messages should point to the new replacement path whenever the replacement is known.
- Historical file names that are no longer read should fail or warn deterministically with a rename hint.
- There should be no silent compatibility layer that keeps old and new shapes both alive indefinitely.
- Migration guidance must explicitly call out that the new default `project.directory = "fabro/"` changes workflow discovery relative to the old implicit project-root behavior.
- Historical string command forms such as `command = "cargo fmt"` must migrate to either `script = "cargo fmt"` or `command = ["cargo", "fmt"]`.
- Historical hook `name` remains display-only in the new language. Cross-layer hook replacement uses the optional `id` field, so users must add `id` explicitly where merge identity is intended.

Known first-pass migration mappings:

| Old shape | New shape |
|---|---|
| `version = 1` | `_version = 1` |
| top-level `goal` | `[run].goal` |
| top-level `work_dir` or `directory` | `[run].working_dir` |
| top-level `labels` | `[run.metadata]` |
| `[vars]` | `[run.inputs]` |
| `[llm]` | `[run.model]` |
| `[setup]` | `[run.prepare]` |
| `[sandbox]` | `[run.sandbox]` |
| `[checkpoint]` | `[run.checkpoint]` |
| `[pull_request]` | `[run.pull_request]` |
| `[artifacts]` | `[run.artifacts]` |
| `[exec]` | `[cli.exec]` |
| `[mcp_servers]` | `[run.agent.mcps]` or `[cli.exec.agent.mcps]`, depending on the consumer |
| `[api]` | `[server.api]` |
| `[web]` | `[server.web]` |
| `[artifact_storage]` | `[server.artifacts]` |
| Git commit author settings | `[run.git.author]` |
| GitHub App and webhook settings | `[server.integrations.github]` |

## Success Criteria

- The new config language has a small, defensible top-level schema with clear object ownership boundaries.
- Users can paste a stanza between `settings.toml`, `fabro.toml`, and `workflow.toml` and still parse successfully.
- Same-host and split-host deployments both fit the model without separate schema branches.
- Merge behavior is predictable enough that users can explain it from the docs without reading implementation code.
- Provider growth in SCM, chat integrations, and object stores does not force repeated top-level redesigns.
- Users can disable inherited singleton and keyed-object behavior without a general unset language.
- Users can predict flag/env/TOML precedence without reading implementation code.
- When users supply old config keys, Fabro fails with targeted upgrade guidance rather than silently ignoring or partially accepting them.

## Scope Boundaries

- No backwards-compatibility requirements. This is a hard-cut redesign.
- No secret-reference syntax such as `${secret.NAME}` in this pass.
- No secret backend configuration in this pass.
- No structured workflow input schema in this pass.
- No prompt-specific run config section in this pass.
- No separate validation modes such as “validate as server config” in this pass.
- No automatic migration tool in this pass.

## Key Decisions

- **Strict top-level namespaces**: Keep the root schema extremely small and reserve underscore-prefixed top-level keys for config-language metadata.
- **Same schema everywhere**: File type controls precedence, not which sections are legal.
- **Owner-first process config**: CLI and server settings belong to the process that reads them, even in same-host setups.
- **Provider-neutral core with provider-specific leaves**: Use this for SCM, notifications, interviews, sandboxes, and object stores where it improves long-term coherence.
- **No general unset**: Prefer explicit disable mechanisms such as `enabled = false` and `"none"` selectors.
- **Lazy validation for unused stanzas**: This preserves the “paste any stanza anywhere” rule without weakening strict unknown-key validation.
- **Shared server listener**: Bind transport and transport TLS are shared at `[server.listen]`; API and web remain separate surfaces above that.
- **Separate storage, artifacts, and SlateDB**: These are materially different server concerns and should not be collapsed into one storage section.
- **Ordered lists are rare**: Keep them where order is semantically important, especially hooks and prepare steps. Prefer keyed named objects elsewhere.
- **Hard-fail migration**: The system should aggressively reject obsolete keys and point users at replacements instead of carrying a compatibility burden into the new language.

## Canonical Shape

```toml
_version = 1

[project]
[workflow]
[run]
[cli]
[server]
[features]
```

Representative subtree:

```toml
_version = 1

[project]
name = "Fabro"
description = "AI workflow orchestration"
directory = "fabro/"

[workflow]
name = "Implement Feature"
description = "Turns a request into a code change"

[run]
goal = "Implement OAuth refresh tokens"
working_dir = "/workspace"

[run.model]
provider = "anthropic"
name = "sonnet"
fallbacks = ["openai", "gpt-5.4", "gemini/gemini-flash"]

[run.agent]
permissions = "read-write"

[run.notifications.ops]
enabled = true
provider = "slack"
events = ["run.failed"]

[run.notifications.ops.slack]
channel = "#ops"

[run.interviews]
provider = "slack"

[run.interviews.slack]
channel = "#approvals"

[cli.target]
type = "http"
url = "https://fabro.example.com/api/v1"

[cli.auth]
strategy = "mtls"

[cli.exec.model]
provider = "anthropic"
name = "claude-opus"

[cli.exec.agent]
permissions = "read-write"

[server.listen]
type = "tcp"
address = "127.0.0.1:32276"

[server.api]
url = "https://fabro.example.com/api/v1"

[server.web]
enabled = true
url = "https://fabro.example.com"

[server.storage]
root = "/var/lib/fabro"

[server.artifacts]
provider = "s3"
prefix = "artifacts"

[server.slatedb]
provider = "s3"
prefix = "runs"
flush_interval = "1s"
```

## Canonical File Examples

Minimal `~/.fabro/settings.toml`:

```toml
_version = 1

[cli.target]
type = "unix"
path = "~/.fabro/fabro.sock"

[cli.exec]
prevent_idle_sleep = true

[cli.exec.model]
provider = "anthropic"
name = "claude-opus"

[cli.exec.agent]
permissions = "read-write"

[cli.output]
format = "text"
verbosity = "normal"

[cli.updates]
check = true

[server.listen]
type = "unix"
path = "~/.fabro/fabro.sock"

[server.storage]
root = "~/.fabro/storage"

[run.interviews]
provider = "slack"

[run.interviews.slack]
channel = "#approvals"
```

Minimal `fabro.toml`:

```toml
_version = 1

[project]
name = "Fabro"
description = "AI workflow orchestration"
directory = "fabro/"

[run.model]
provider = "anthropic"
name = "sonnet"

[run.sandbox]
provider = "daytona"

[[run.prepare.steps]]
script = "bun install"
```

Minimal `workflow.toml`:

```toml
_version = 1

[workflow]
name = "Implement Feature"
description = "Turns a request into a code change"

[run]
goal = "Implement OAuth refresh tokens"

[run.inputs]
repo = "fabro"

[run.notifications.ops]
enabled = true
provider = "slack"
events = ["run.failed", "run.completed"]

[run.notifications.ops.slack]
channel = "#ops"
```

## Outstanding Questions

### Deferred to Planning

- [Affects R64][Technical] What exact run-side SCM targeting fields should live under `[run.scm]` in the first pass: repo slug, owner/repo split, base branch defaults, or additional checkout/ref context?
- [Affects R66][Technical] What exact shared field set should the object-store envelope expose before provider-specific subtables begin?
- [Affects R90][Technical] What exact field set should the MCP launcher schema expose in addition to `script` xor `command`, `type`, and timeouts?
- [Affects R34][Technical] What minimal first-pass notification route surface is required beyond `enabled`, `provider`, and `events`?
- [Affects R83][Technical] What duration parser will Fabro standardize on, and what canonical normalization should be shown in error messages and generated examples?

## Next Steps

- Update the user-facing config docs to match this new object model.
- `/ce:plan` for a migration and implementation plan covering parser changes, merge semantics, docs, and test updates.
