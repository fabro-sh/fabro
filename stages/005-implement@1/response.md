Good. All new files are listed. Let me now provide a comprehensive summary of what was implemented.

## Implementation Summary

Here is a summary of all the changes I made, organized by the plan's tasks:

### 1. Settings Schema and Merge Behavior (✅ Complete)

**New files:**
- **`lib/crates/fabro-config/src/layers/llm.rs`** — `LlmLayer`, `ProviderSettingsLayer`, `ModelSettingsLayer`, `ModelLimitsLayer`, `ModelFeaturesLayer`, `CostRatesLayer`, `ModelCostTableLayer`, `ModelControlsLayer`, and `CredentialRef` types. Includes custom `knowledge_cutoff` deserializer that handles both TOML dates and strings.
- **`lib/crates/fabro-config/src/tests/llm_settings.rs`** — Comprehensive tests for LLM settings parsing: provider settings, model settings, speed costs, credential ref validation (rejects literals, empty IDs/names), run model controls.

**Modified files:**
- **`lib/crates/fabro-config/src/layers/mod.rs`** — Added `llm` module and re-exports
- **`lib/crates/fabro-config/src/layers/settings.rs`** — Added `llm: Option<LlmLayer>` to `SettingsLayer`
- **`lib/crates/fabro-config/src/layers/combine.rs`** — Added `Combine` impls for new LLM types and `CredentialRef`
- **`lib/crates/fabro-config/src/layers/run.rs`** — Added `RunModelControlsLayer` with `reasoning_effort` and `speed` fields
- **`lib/crates/fabro-config/src/parse.rs`** — Added `"llm"` to `ALLOWED_TOP_LEVEL_KEYS`, removed legacy migration hint
- **`lib/crates/fabro-config/src/lib.rs`** — Added exports for all new types

### 2. Catalog Model (✅ Complete — ProviderId, adapter metadata, catalog migration)

**New files:**
- **`lib/crates/fabro-model/src/provider_id.rs`** — `ProviderId` and `ModelId` string-backed newtypes with `Display`, `FromStr`, `Serialize`/`Deserialize`, `PartialEq<str>`, `From<Provider>` impls
- **`lib/crates/fabro-model/src/reasoning_effort.rs`** — `ReasoningEffort` enum moved here from `fabro-llm` (added `PartialOrd`/`Ord`)
- **`lib/crates/fabro-model/src/adapter.rs`** — `AgentProfileKind`, `ApiKeyHeaderPolicy`, `AdapterControlCapabilities`, `AdapterMetadata`, `builtin_adapter_metadata()`, `adapter_metadata()` for the 4 built-in adapters (anthropic, openai, gemini, openai_compatible)

**Modified files:**
- **`lib/crates/fabro-model/src/types.rs`** — `Model.provider` changed from `Provider` to `ProviderId`. Updated accessor to return `&ProviderId`.
- **`lib/crates/fabro-model/src/catalog.rs`** — All methods updated: `list()`, `default_for_provider()`, `probe_for_provider()`, `closest()`, `build_fallback_chain()` now take `&str` instead of `Provider`. Tests updated with inline snapshots.
- **`lib/crates/fabro-model/src/billing.rs`** — `ModelRef.provider` changed to `ProviderId`. `ModelBillingFacts::for_provider()` takes `&str`. `pricing_for()` uses string matching instead of enum pattern matching. Unknown providers get OpenAI-compatible billing.
- **`lib/crates/fabro-model/src/model_ref.rs`** — `ModelHandle` uses `ProviderId` instead of `Provider`
- **`lib/crates/fabro-model/src/lib.rs`** — Added module declarations and re-exports
- **`lib/crates/fabro-model/Cargo.toml`** — Added `chrono` dependency
- **`lib/crates/fabro-llm/src/types.rs`** — `ReasoningEffort` re-exported from `fabro_model` instead of defined here

### 3. Downstream Crate Migration (✅ Complete)

Updated all crates that use `Provider` for model/catalog identity:

- **`lib/crates/fabro-llm/src/model_test.rs`** — Uses `info.provider.as_str()` instead of `<&str>::from(info.provider)`
- **`lib/crates/fabro-agent/src/cli.rs`** — Converts `Provider` to `ProviderId` for `ModelHandle`, uses string-based catalog lookups
- **`lib/crates/fabro-agent/src/tools.rs`** — Test code uses `ProviderId::from("anthropic")`
- **`lib/crates/fabro-server/src/server/handler/models.rs`** — Provider filter is now string-based, configured providers are `HashSet<String>`, no more Provider::from_str validation (returns empty list for unknown providers)
- **`lib/crates/fabro-server/src/diagnostics.rs`** — Uses `&provider.to_string()` for catalog lookups
- **`lib/crates/fabro-server/src/run_manifest.rs`** — Added `controls: None` to `RunModelLayer`
- **`lib/crates/fabro-workflow/src/operations/start.rs`** — Uses string conversion for `build_fallback_chain`
- **`lib/crates/fabro-workflow/src/outcome.rs`** — `billing_facts_for_stage_usage` takes `&str`, uses `ProviderId` for `ModelRef`
- **`lib/crates/fabro-workflow/src/run_materialization.rs`** — Uses string conversion for `default_for_provider`
- **`lib/crates/fabro-cli/src/commands/model.rs`** — `ModelTestRow.provider` is now `String`, uses `to_string()` for display
- **`lib/crates/fabro-cli/src/shared/provider_auth.rs`** — Uses string-based catalog lookups
- **`lib/crates/fabro-api/tests/model_round_trip.rs`** — Uses `ProviderId::from("anthropic")`

### Verification

All verification commands pass:
- `cargo build -p fabro-api` ✅
- `cargo nextest run --workspace` — **5070 tests pass, 0 failures** ✅
- `cargo +nightly-2026-04-14 fmt --check --all` ✅
- `cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings` ✅

### What's Still Needed (Future PRs per Plan)

The following plan tasks are not yet implemented and would be follow-up work:
- **OpenAPI and generated clients** — Changing provider schema from enum to string in fabro-api.yaml, removing `with_replacement("Provider", ...)` from build.rs
- **Credentials and auth** — Changing `AuthCredential`, `ApiCredential`, `CredentialResolver` from `Provider` enum to `ProviderId`
- **LLM client adapter registry** — Moving hardcoded provider base URLs and adapter construction from `Client::from_credentials` to a catalog-driven registry
- **Validation** — Moving catalog-dependent checks from `built_in_rules()` to catalog-bound rule instances
- **Workflow/server plumbing** — Storing `Arc<Catalog>` in app state, replacing `Catalog::builtin()` production call sites
- **Controls and request validation** — Typed `Speed` for request, control allow-lists
- **Billing** — Per-speed cost overrides from settings data
- **Built-in providers/models as defaults data** — Moving catalog.json content into defaults.toml settings layers
- **Bootstrap catalog and CI enforcement** — Adding bootstrap/defaults constructor, CI test for `bootstrap_catalog` references