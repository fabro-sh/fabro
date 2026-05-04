Goal: # Settings-Driven LLM Providers And Models Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement a settings-driven LLM provider/model catalog so new providers and models can be configured through TOML when they use an existing adapter.

**Architecture:** Treat provider and model identity as layered settings data. Keep adapters, agent profiles, auth schemes, billing policy shapes, and request control kinds as explicit Rust behavior. Build a resolved `Arc<Catalog>` from settings and pass that catalog through server, workflow, CLI, auth, and LLM client seams.

**Tech Stack:** Rust, serde/TOML settings layers, chrono `NaiveDate`, strum enums for code-owned control values, OpenAPI/progenitor, TypeScript API client generation, cargo nextest.

---

## Summary

This is a breaking cross-crate refactor. `fabro_model::Provider` stops being the product identity type; provider identity becomes a string-backed `ProviderId`. OpenAPI provider fields become strings, and the resolved settings catalog becomes the source of truth for model lookup, provider lookup, default selection, credential resolution, adapter registration, and `/models`.

All settings layers are trusted execution configuration, including project TOML and workflow/run TOML. That trust model allows repository-provided settings to define or override provider routing. It does not make every credential interchangeable: Codex OAuth remains locked to the fixed ChatGPT Codex backend because it is a long-lived account-scoped credential, not a normal API key for arbitrary `base_url` routing.

Built-in providers and models ship as default settings data. User, server, project, and workflow/run settings merge on top of those defaults using the existing settings-layer model.

## Key Interface Decisions

- Add trusted, mergeable `[llm]` settings. Provider `adapter` is a registry key implemented in Rust; new providers can use existing adapter keys without code changes, while new adapters still require Rust.

```toml
[llm.providers.kimi]
display_name = "Kimi"
adapter = "openai_compatible"
base_url = "https://api.moonshot.ai/v1"
credentials = ["credential:kimi", "env:KIMI_API_KEY"]
priority = 60
enabled = true
aliases = ["moonshot"]

[llm.models."kimi-k2.5"]
provider = "kimi"
api_id = "kimi-k2.5"
display_name = "Kimi K2.5"
family = "kimi"
knowledge_cutoff = 2025-01-01
default = true
enabled = true
aliases = ["kimi"]
estimated_output_tps = 50

[llm.models."kimi-k2.5".limits]
context_window = 262144
max_output = 32768

[llm.models."kimi-k2.5".features]
tools = true
vision = false
reasoning = true
effort = false

[llm.models."kimi-k2.5".costs]
input_cost_per_mtok = 0.60
output_cost_per_mtok = 2.50
cache_input_cost_per_mtok = 0.15
```

- `api_id` is the model identifier sent to the provider API; when omitted, it defaults to the catalog model ID.
- `features.reasoning`, `features.effort`, and `controls.reasoning_effort` are separate. `features.reasoning` records whether the model has reasoning behavior at all and is used for catalog capability display plus fallback/model matching. `features.effort` records whether the model supports the provider's native effort parameter. `controls.reasoning_effort` is the user-facing allow-list for native effort values Fabro may accept for that model.
- Do not add a provider-level `profile` field in v1. The agent profile is inferred from the adapter registry entry, for example `anthropic -> anthropic`, `openai -> openai`, `gemini -> gemini`, and `openai_compatible -> openai`. New profile behavior is a Rust change.
- Do not add provider-level `cli_backend` in v1. Existing graph/workflow `cli_backend` behavior remains separate from provider catalog data. `codex_mode` remains credential-derived and is not configurable through provider settings.
- Add fixed, typed model controls. Supported control kinds and enum values are Rust-owned. Current v1 controls are `reasoning_effort = ["low", "medium", "high", "xhigh", "max"]` and non-default `speed = ["fast"]`. A model only declares values allowed by its adapter metadata; v1 does not expose non-native reasoning-effort fallback strategies as catalog data.

```toml
[llm.models."claude-opus-4-6".controls]
reasoning_effort = ["low", "medium", "high"]
speed = ["fast"]

[llm.models."claude-opus-4-6".costs.speed.fast]
input_cost_per_mtok = 90.0
output_cost_per_mtok = 450.0
cache_input_cost_per_mtok = 9.0
```

- `Speed::Standard` is always available and is not listed in `controls.speed`. `controls.speed` enumerates additional speeds only, so `costs.speed.standard` is not a valid override.
- `controls.speed` and `costs.speed` have one invariant: every `costs.speed.<speed>` key must be declared in `controls.speed`. A declared non-standard speed without a price override is allowed and uses base costs. An override whose speed is not declared is a catalog build error. Built-in Anthropic fast-mode models must declare both `controls.speed = ["fast"]` and explicit `costs.speed.fast` rows so the current fast multiplier becomes data.
- Omitted control lists are not wildcards. If `controls.reasoning_effort` is omitted and `features.effort = true`, it resolves to the adapter's native reasoning-effort defaults. If `features.effort = false`, it resolves to an empty list. If `controls.speed` is omitted, it resolves to an empty list of additional speeds.
- Add `[run.model.controls]` for run defaults. Node and style values still win over run defaults.

```toml
[run.model.controls]
reasoning_effort = "high"
speed = "fast"
```

- Credential entries are a typed `CredentialRef` enum. Accepted forms are only `credential:<id>` and `env:<NAME>`; literal secret strings fail deserialization or validation and are never represented as a successful settings value.
- `credential:<id>` reads structured credentials from the existing `fabro-vault` crate. API-key credentials must match the provider ID they are attached to. `env:<NAME>` reads the process environment first, then falls back to an existing raw `fabro-vault` secret with the same name.
- `credential:openai_codex` is special. It is only valid for canonical provider ID `openai`, maps to vault ID `openai_codex`, sets `codex_mode = true`, and always uses `https://chatgpt.com/backend-api/codex`. It ignores `[llm.providers.openai].base_url` and cannot be used by aliases or custom providers.
- OpenAPI changes are breaking: provider schemas become `type: string`, `Model.provider` becomes a provider ID string, `Model.controls` is added, and `knowledge_cutoff` becomes `format: date`.

## Implementation Plan

- [ ] **Settings schema and merge behavior**
  - Add `LlmSettings`, `ProviderSettings`, `ModelSettings`, `ModelControls`, `ModelCostTable`, `CostRates`, and `CredentialRef` to `fabro-config`.
  - Store built-in providers and models in defaults settings data so production catalog construction starts from the same layered settings path as user/project/workflow overrides.
  - Preserve sparse field-merge semantics for `[llm.providers.<id>]` and `[llm.models.<id>]`. Arrays such as `credentials`, `aliases`, `controls.reasoning_effort`, and `controls.speed` replace as whole arrays. To add one credential to a built-in provider, redeclare the full `credentials` list in the higher layer.
  - Keep the targeted legacy `[llm]` migration error for old keys such as `provider` or `model`; accept only the new `[llm.providers]` and `[llm.models]` subtrees. Do not regress to a generic serde unknown-field error.
  - Parse adapter keys as strings in `fabro-config`. Do not make `fabro-config` depend on `fabro-llm`; adapter-key validation happens when building the resolved catalog.

- [ ] **Catalog model**
  - Add `ProviderId` and `ModelId` string newtypes where they improve type clarity across crates.
  - Replace product identity uses of `fabro_model::Provider` with `ProviderId`. Keep Rust enums for behavior that is still code-owned, including `ReasoningEffort` and `Speed`.
  - Move `ReasoningEffort` from `fabro-llm` to `fabro-model` or another shared vocabulary crate so catalog data, request validation, OpenAPI replacement types, and LLM requests use one enum.
  - Add code-owned adapter metadata beside the catalog, not in `fabro-config`. This metadata is still Rust code; only provider/model rows are data.
  - Add concrete metadata vocabulary types in the shared model/catalog layer so model validation and LLM factory registration share one contract:

    ```rust
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum AgentProfileKind {
        Anthropic,
        OpenAi,
        Gemini,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ApiKeyHeaderPolicy {
        Bearer,
        Custom { name: &'static str },
    }

    pub struct AdapterMetadata {
        pub key: &'static str,
        pub default_profile: AgentProfileKind,
        pub api_key_header: ApiKeyHeaderPolicy,
        pub controls: AdapterControlCapabilities,
    }

    pub struct AdapterControlCapabilities {
        pub native_reasoning_effort: &'static [ReasoningEffort],
        pub additional_speeds: &'static [Speed],
    }

    // Implemented in fabro-auth, not fabro-model, to avoid a dependency cycle.
    pub fn build_api_key_header(policy: ApiKeyHeaderPolicy, key: String) -> ApiKeyHeader {
        match policy {
            ApiKeyHeaderPolicy::Bearer => ApiKeyHeader::Bearer(key),
            ApiKeyHeaderPolicy::Custom { name } => ApiKeyHeader::Custom {
                name: name.to_string(),
                value: key,
            },
        }
    }
    ```

  - `AgentProfileKind` is an internal dispatch key that `fabro-agent` maps to concrete `AgentProfile` implementations; it is not a settings field. `ApiKeyHeaderPolicy` describes how an API key becomes an `ApiKeyHeader` without carrying secret values.
  - `native_reasoning_effort` is every reasoning-effort value that can be sent through the provider's native effort field. After omitted controls are filled from adapter defaults, resolved model `controls.reasoning_effort` must be a non-empty subset of `native_reasoning_effort` when `features.effort = true`; it must be omitted or empty when `features.effort = false`. V1 does not expose generic non-native effort fallback in catalog data.
  - Model `controls.speed` must be a subset of adapter `additional_speeds`. `Speed::Standard` is implicit and must not appear in either list.
  - Build `Catalog` from resolved settings and return catalog-build errors for malformed provider/model data.
  - Validate provider `adapter` strings against the adapter metadata while building the catalog. `fabro-llm` has the matching factory registry and tests must prove every metadata key has a factory.
  - Build provider and model alias indexes after all layers merge and after disabled entries are filtered out of runtime lookup. Canonical IDs and aliases for enabled entries share one namespace within their kind. Any enabled-entry collision is fatal, including canonical ID versus another enabled entity's alias. Disabled entries do not reserve aliases; re-enabling a disabled entry can fail if its aliases collide with currently enabled entries.
  - Surface alias/catalog failures at catalog construction: server startup fails, CLI run/validate fails, and workflow materialization fails before requests are issued.
  - Replace hardcoded provider precedence with provider `priority`. Higher priority wins; missing priority is `0`; ties sort by canonical provider ID. `enabled = false` removes the provider/model from runtime selection but does not delete vault entries.
  - Retire `Catalog::builtin()` from production lookup paths. Gate the old singleton behind `#[cfg(any(test, feature = "test-support"))]` for tests. Add a narrowly named bootstrap/defaults constructor for install and API-key validation flows that need built-in provider definitions before project settings are loaded.
  - Put the bootstrap/defaults constructor behind an explicit module such as `fabro_model::bootstrap_catalog` and document it as install-only.
  - Add a CI-enforced workspace test that scans for `bootstrap_catalog` references and allows only bootstrap/install/test-support paths. Request-serving crates and handlers must fail that test if they call the bootstrap constructor.

- [ ] **OpenAPI and generated clients**
  - Change provider fields in `docs/public/api-reference/fabro-api.yaml` from the closed `Provider` schema to strings or a shared `ProviderId` newtype.
  - Remove `with_replacement("Provider", "fabro_model::Provider", &[])` from `lib/crates/fabro-api/build.rs`.
  - Delete or replace `lib/crates/fabro-api/tests/provider_round_trip.rs`; add JSON parity coverage for `ProviderId` if that type is reused by `fabro-api`.
  - Regenerate Rust API types with `cargo build -p fabro-api`.
  - Regenerate the TypeScript API client after the OpenAPI change.

- [ ] **Credentials and auth**
  - Change `AuthCredential`, `ApiCredential`, resolver errors, and credential lookup helpers from closed `Provider` to `ProviderId`.
  - Preserve existing vault JSON by deserializing old provider strings as provider IDs.
  - Keep `credential_id_for` compatibility: API-key credentials use their canonical provider ID; Codex OAuth still maps only to `openai_codex`.
  - Resolve provider `credentials` in list order. For `env:` refs, build an API credential for the current provider using the adapter registry's auth-header policy. For `credential:` refs, require structured credential/provider compatibility before attaching it. The first successfully resolved credential wins, so built-in ordering should put the preferred credential type first; for OpenAI, place `credential:openai_codex` before API-key refs only when Codex OAuth should be preferred over API-key traffic.
  - Keep Codex OAuth outside configurable provider routing: the resolver produces the fixed ChatGPT Codex base URL and `codex_mode = true` only for canonical `openai` plus `openai_codex`.
  - Define `fabro auth list` behavior for absent or disabled providers: list vault entries regardless, annotate catalog status as enabled, disabled, or unknown, and do not treat unknown entries as runtime-configured providers.
  - Ensure new credential-ref Display/Debug/error paths redact secret values and never log resolved env values. Env names and credential IDs may appear only in non-secret diagnostic text.

- [ ] **LLM client and adapter registry**
  - Introduce an adapter factory registry in `fabro-llm` keyed by the same strings as catalog adapter metadata: `anthropic`, `openai`, `gemini`, and `openai_compatible`.
  - Keep factory behavior in `fabro-llm`; keep static metadata needed by `fabro-model` and `fabro-auth` in the shared catalog/model layer to avoid dependency cycles.
  - Change `Client::from_source` and `Client::from_credentials` call paths so provider settings and the resolved catalog are available before adapter registration.
  - Register adapters by provider ID from the resolved catalog. `Client::resolve_provider` must use the injected catalog to map model IDs and aliases to provider IDs; it must not call `Catalog::builtin()`.
  - Keep install/API-key validation working by using the bootstrap/defaults catalog for the provider currently being configured.
  - Leave custom auth schemes and data-driven adapter implementations out of scope.

- [ ] **Validation**
  - Do not change the public `LintRule` trait signature.
  - Remove catalog-dependent model/provider-known checks from `rules::built_in_rules()`.
  - Reintroduce those checks as catalog-bound rule instances, for example `model_support::rules_for_catalog(Arc<Catalog>)`, passed through the existing `extra_rules` argument after settings resolution.
  - Thread the resolved catalog to CLI, server, workflow, and parser validation call sites that should report unknown models/providers.
  - Keep pure graph-shape validation available without runtime settings.

- [ ] **Workflow, server, agent, and hooks plumbing**
  - Store `Arc<Catalog>` in server app state and workflow service state.
  - Replace production `Catalog::builtin()` call sites in server handlers, workflow operations, workflow transforms, hooks, diagnostics, completions, pull-request creation, and agent profile/session code.
  - Ensure project and workflow/run TOML settings are merged before model resolution, validation, fallback-chain construction, and LLM client construction.
  - Infer agent profile from the provider adapter registry entry. Do not make profiles data-driven in v1.
  - Continue to expose existing node/workflow `cli_backend` behavior independently of provider settings.

- [ ] **Controls and request validation**
  - Add model control allow-lists to catalog data. Validate control values against existing Rust enums: `ReasoningEffort::{Low, Medium, High, XHigh, Max}` and `Speed::{Standard, Fast}`.
  - Change `fabro_llm::types::Request.speed`, `fabro_llm::generate::GenerateParams.speed`, and agent/workflow speed config plumbing from `Option<String>` to `Option<Speed>`. Keep serde wire compatibility through the existing snake_case `Speed` representation and parse strings only at API/settings/graph boundaries.
  - Validate model-declared controls against adapter capabilities at catalog build time.
  - Define "explicit control" narrowly: a value from `[run.model.controls]`, a node attribute after stylesheet/import transforms, or a style-applied attribute. Define "legacy default" as the current hardcoded fallback returned only when no explicit value exists.
  - Avoid a broad provenance refactor. Add helper methods that can distinguish "attribute present" from "fallback returned" at the control resolution sites.
  - Explicit unsupported controls fail before building provider requests. Legacy defaults are omitted for models that do not declare the control.

- [ ] **Billing**
  - Do not collapse `ModelPricingPolicy` variants in this change.
  - Change model costs to a base `CostRates` plus optional `speed: BTreeMap<Speed, CostRates>` overrides.
  - Update `pricing_for(speed)` so selected rates are `costs.speed[speed]` when present, otherwise base rates.
  - Preserve provider-shaped pricing policies. Anthropic cache-write 5m/1h rates continue to derive from the selected input rate, so Anthropic fast-mode cost rows produce the same cache-write rates as today's multiplier path.
  - Remove the hardcoded `(Provider::Anthropic, Speed::Fast, claude-opus-4-7/4-6)` branch after the equivalent rows exist in defaults data.

## Test Plan

- `fabro-config`: parse and merge `[llm]`; reject literal credential refs; preserve the legacy `[llm] provider/model` migration hint; cover field-merge and whole-array replacement behavior.
- `fabro-model`: dynamic catalog lookup, adapter key validation, enabled-only alias collision behavior, duplicate-alias failure surfaces, defaults, provider `priority`, disabled entries, `NaiveDate` knowledge cutoff, model controls, adapter capability validation, absent-control defaults, non-empty `features.effort` controls, speed subset validation, and per-speed pricing.
- `fabro-auth`: existing vault credential JSON still parses; `credential:` and `env:` resolution order works; structured credential/provider mismatches fail; Codex OAuth remains restricted to canonical `openai` and fixed ChatGPT Codex base URL even when `[llm.providers.openai].base_url` is overridden.
- `fabro-llm`: built-in Kimi/Zai/Minimax/Inception register through `openai_compatible` settings without provider-specific branches; every catalog adapter metadata key has a production factory and every production factory is reachable from a metadata key; `Request.speed` is typed as `Option<Speed>` internally; request validation rejects explicit unsupported controls and omits legacy defaults for unsupported models.
- `fabro-validate`: built-in rules no longer call `Catalog::builtin()`; catalog-bound model/provider-known rules work through `extra_rules`.
- `fabro-api`: OpenAPI provider schema no longer replaces with `fabro_model::Provider`; provider string/`ProviderId` JSON parity is covered; TypeScript client generation reflects string providers.
- `fabro-server`/`fabro-workflow`/`fabro-cli`: `/models?provider=<id>` works with string IDs; project/workflow TOML can add a custom provider/model for a run; install/API-key validation uses bootstrap defaults; CLI model commands and server-returned models use the resolved catalog.
- Workspace policy test: CI enforces the `bootstrap_catalog` reference allowlist across the workspace so request-serving modules cannot call bootstrap/default constructors.
- Verification commands:
  - `cargo build -p fabro-api`
  - `cargo nextest run -p fabro-config -p fabro-model -p fabro-auth -p fabro-llm -p fabro-validate -p fabro-workflow -p fabro-server -p fabro-api`
  - `cargo +nightly-2026-04-14 fmt --check --all`
  - `cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings`

## Assumptions And Deferred Work

- All settings layers are trusted execution configuration. Provider routing may attach server credentials to outbound HTTP, so credential-specific invariants still matter even though project/workflow TOML is trusted.
- Field-merge for provider/model tables is intentional. Whole-array replacement for controls can mask future built-in values; more granular array merge operations are deferred.
- V1 does not support custom auth schemes, data-driven profile templates, provider-level CLI backend routing, data-driven adapter implementations, or new request control kinds.
- Adding a new value to an existing Rust-owned control enum, such as a new speed value beyond `standard` and `fast`, remains a Rust change.
- Existing imprecise knowledge cutoff labels migrate to exact normalized dates, e.g. `May 2025` becomes `2025-05-01`; presentation can render lower precision.


## Completed stages
- **toolchain**: succeeded
  - Script: `command -v cargo >/dev/null || { curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y && sudo ln -sf $HOME/.cargo/bin/* /usr/local/bin/; }; cargo --version 2>&1`
  - Stdout:
    ```
    cargo 1.95.0 (f2d3ce0bd 2026-03-21)
    ```
  - Stderr: (empty)
- **preflight_compile**: succeeded
  - Script: `cargo check -q --workspace 2>&1`
  - Stdout: (empty)
  - Stderr: (empty)
- **preflight_lint**: succeeded
  - Script: `cargo +nightly-2026-04-14 clippy -q --workspace -- -D warnings 2>&1`
  - Stdout: (empty)
  - Stderr: (empty)


Read the plan file referenced in the goal and implement every step. Make all the code changes described in the plan. Use red/green TDD.