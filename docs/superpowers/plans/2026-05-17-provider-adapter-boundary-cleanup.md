# Provider/Adapter Boundary Cleanup Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Refactor provider and adapter boundaries so provider-specific facts live in catalog data and runtime protocol behavior stays on the LLM adapters.

**Architecture:** `AdapterKind` remains a closed registry key for serde and factory lookup only. Provider TOML owns auth, billing, agent profile, base URL, credentials, headers, and per-model probe markers. Runtime adapters own request/response behavior and adapter-specific request validation.

**Tech Stack:** Rust, serde/TOML catalog settings, strum enums, `fabro-model`, `fabro-auth`, `fabro-llm`, `fabro-server`, `fabro-cli`, `cargo nextest`.

---

## Summary

This is a greenfield breaking migration. Prefer simplicity and correctness over compatibility shims.

The end state:

- Adding a provider that uses an existing adapter is TOML-only plus focused tests.
- Adding a new adapter requires one adapter implementation, one `AdapterKind` variant, one factory entry, provider TOML, and targeted tests.
- No auth, billing, install, catalog, or diagnostics code matches on adapter kind. `AdapterKind` matches are limited to adapter factory lookup and adapter construction.

Keep per-model `probe = true`; do not add `probe_model` to `CatalogProvider`.

## Key Interfaces

Add provider-owned facts in `fabro-model`:

```rust
pub enum ProviderAuthConfig {
    ApiKey {
        credentials: Vec<CredentialRef>,
        header: ApiKeyHeaderPolicy,
    },
    HeaderOnly,
    None,
}

pub enum ApiKeyHeaderPolicy {
    Bearer,
    Custom { name: String },
}

pub enum BillingPolicy {
    OpenAi,
    Anthropic,
    Gemini,
    None,
}
```

Extend `CatalogProvider`:

```rust
pub struct CatalogProvider {
    pub id: ProviderId,
    pub display_name: String,
    pub adapter: AdapterKind,
    pub auth: ProviderAuthConfig,
    pub billing_policy: BillingPolicy,
    pub agent_profile: AgentProfileKind,
    pub api_key_url: Option<String>,
    pub base_url: Option<String>,
    pub extra_headers: HashMap<String, HeaderValueRef>,
    pub priority: i32,
    pub aliases: Vec<String>,
}
```

Use this provider TOML shape:

```toml
[providers.openai]
display_name = "OpenAI"
adapter = "openai"
agent_profile = "openai"
billing_policy = "openai"
api_key_url = "https://platform.openai.com/api-keys"
priority = 90

[providers.openai.auth]
type = "api_key"
credentials = ["credential:openai", "credential:openai_codex", "env:OPENAI_API_KEY"]
header = "bearer"
```

Custom API-key headers:

```toml
[providers.anthropic.auth]
type = "api_key"
credentials = ["credential:anthropic", "env:ANTHROPIC_API_KEY"]
header = { custom = "x-api-key" }
```

Header-only providers:

```toml
[providers.proxy.auth]
type = "header_only"

[providers.proxy.extra_headers]
x-portkey-api-key = { env = "PORTKEY_API_KEY" }
```

No-auth providers:

```toml
[providers.local]
display_name = "Local"
adapter = "openai_compatible"
agent_profile = "openai"
billing_policy = "none"
base_url = "http://localhost:11434/v1"

[providers.local.auth]
type = "none"
```

## Implementation Tasks

### Task 1: Add Provider Facts Without Removing Existing Consumers

**Files:**

- Modify: `lib/crates/fabro-model/src/catalog.rs`
- Modify: provider/catalog-oriented modules in `lib/crates/fabro-model/src/` if extracted from `catalog.rs`
- Modify: `lib/crates/fabro-model/src/lib.rs`
- Modify: `lib/crates/fabro-model/src/catalog/providers/*.toml`

- [ ] Add `ProviderAuthConfig`, `ApiKeyHeaderPolicy`, and `BillingPolicy` in provider/catalog-oriented `fabro-model` code, not in `adapter.rs`. Touch `adapter.rs` only to move the existing `ApiKeyHeaderPolicy` out.
- [ ] Add serde parsing for provider auth tables and billing policy strings.
- [ ] Require provider `agent_profile` in catalog settings instead of deriving it from the adapter.
- [ ] Build `CatalogProvider.auth` and `CatalogProvider.billing_policy` from TOML.
- [ ] Move existing `credentials` into `[providers.<id>.auth]` for built-in providers.
- [ ] Keep a temporary derived `CatalogProvider.credentials` field populated from `auth` so existing auth consumers continue compiling until Task 2 migrates them.
- [ ] Keep `extra_headers` outside auth so header-only and API-key providers can both use it.
- [ ] Move the `openai_compatible` `base_url` requirement out of catalog build. Catalog should parse provider data without branching on adapter kind.
- [ ] Add catalog tests for API-key auth, custom header auth, header-only auth, no-auth, billing policies, missing `agent_profile`, API-key auth with empty credentials, header-only with no `extra_headers`, invalid custom header names, and `auth.type = "none"` combined with credential/header fields.
- [ ] Run `cargo nextest run -p fabro-model catalog::tests`.

Expected interim state: existing downstream crates still compile because `CatalogProvider.credentials` remains as a temporary derived view.

### Task 2: Resolve Auth From Provider Auth Config

**Files:**

- Modify: `lib/crates/fabro-auth/src/resolve.rs`
- Modify: `lib/crates/fabro-auth/src/env_source.rs`
- Modify: `lib/crates/fabro-auth/src/vault_source.rs`
- Modify: `lib/crates/fabro-auth/src/credential_source.rs`
- Modify: `lib/crates/fabro-auth/src/strategies/api_key.rs`
- Modify: `lib/crates/fabro-auth/src/strategy.rs`
- Modify: `lib/crates/fabro-cli/src/shared/provider_auth.rs`
- Modify: `lib/crates/fabro-cli/src/commands/install.rs`

- [ ] Build API-key headers from `CatalogProvider.auth`, not adapter metadata.
- [ ] For `ProviderAuthConfig::ApiKey`, resolve credentials in declared order and build the configured header.
- [ ] For `ProviderAuthConfig::HeaderOnly`, resolve `extra_headers` and register the provider without a primary auth header.
- [ ] For `ProviderAuthConfig::None`, produce an `ApiCredential { provider, auth_header: None, extra_headers: {}, base_url, codex_mode: false, org_id: None, project_id: None }` so `Client::from_credentials` registers the provider when adapter construction requirements are satisfied.
- [ ] Update vault and env configured-provider discovery so API-key, header-only, and no-auth providers all become API credentials when their auth requirements are satisfied. For no-auth providers, this means `auth.type = "none"` plus adapter construction requirements such as `base_url` for `openai_compatible`.
- [ ] Make CLI install/provider auth discovery use `ProviderAuthConfig::ApiKey` instead of checking provider credentials directly.
- [ ] Keep Codex OAuth restricted to canonical OpenAI.
- [ ] Add `fabro-auth` tests for bearer, custom header, header-only, no-auth, and Codex OAuth behavior.
- [ ] Run `cargo nextest run -p fabro-auth -p fabro-cli provider_auth install_llm`.

### Task 3: Drive Billing From Provider Billing Policy

**Files:**

- Modify: `lib/crates/fabro-model/src/billing.rs`

- [ ] Replace `ModelBillingFacts::for_adapter` with provider-policy-based construction.
- [ ] Replace adapter-based pricing policy selection with `CatalogProvider.billing_policy`.
- [ ] Add `BillingPolicy::None` behavior that returns no estimate/facts without error.
- [ ] Update tests so OpenAI-compatible provider billing depends on TOML policy, not adapter kind.
- [ ] Run `cargo nextest run -p fabro-model billing`.

### Task 4: Move Runtime Validation Behind `ProviderAdapter`

**Files:**

- Modify: `lib/crates/fabro-llm/src/adapter_registry.rs`
- Modify: `lib/crates/fabro-llm/src/provider.rs`
- Modify: `lib/crates/fabro-llm/src/client.rs`
- Modify: provider adapter files under `lib/crates/fabro-llm/src/providers/`
- Modify: `lib/crates/fabro-server/src/server/handler/sessions.rs`

- [ ] Add `fn validate_request(&self, request: &Request) -> Result<(), Error> { Ok(()) }` to `ProviderAdapter`.
- [ ] In the no-middleware path, call `provider.validate_request(request)?` before direct `complete`/`stream` dispatch.
- [ ] In the middleware path, call `provider.validate_request(&req)?` inside the terminal closure so the final middleware-mutated request is validated before adapter dispatch.
- [ ] Move adapter-native control support checks into adapter `validate_request` implementations. Catalog remains responsible only for model-declared control allow-lists.
- [ ] Update `AdapterConfig` comments so auth header policy is provider-derived.
- [ ] Validate `openai_compatible` missing `base_url` in adapter/client construction and return a configuration error instead of panicking or catalog-branching.
- [ ] Change session profile fallback to use `CatalogProvider.agent_profile`, not adapter metadata.
- [ ] Add tests proving unsupported controls fail before HTTP is sent and missing OpenAI-compatible base URL fails during client construction.
- [ ] Run `cargo nextest run -p fabro-llm`.

### Task 5: Use Real Probe Path For Install Validation

**Files:**

- Modify: `lib/crates/fabro-server/src/install.rs`
- Modify: `lib/crates/fabro-server/tests/it/api/install.rs`
- Modify: `lib/crates/fabro-server/tests/it/api/install_openai_compatible.rs`

- [ ] Replace raw install `GET /models` provider validation with the same LLM client plus `catalog.probe_for_provider()` generation path used by CLI/diagnostics.
- [ ] Remove install branches that add Anthropic version headers or infer base URLs from adapter kind.
- [ ] Use provider `base_url`, env base URL override, and test upstream override data before failing a missing base URL.
- [ ] Keep install API-key endpoints scoped to `ProviderAuthConfig::ApiKey` providers. Header-only and no-auth providers are not configured through the install API in this plan.
- [ ] Add install tests for OpenAI, Anthropic, Gemini, and OpenAI-compatible API-key providers. Add explicit rejection/omission tests showing header-only and no-auth providers are not offered as API-key install choices.
- [ ] Run `cargo nextest run -p fabro-server install`.

### Task 6: Remove Adapter Metadata And Transitional Provider Fields

**Files:**

- Modify: `lib/crates/fabro-model/src/adapter.rs`
- Modify: `lib/crates/fabro-model/src/catalog.rs`
- Modify: `lib/crates/fabro-model/src/lib.rs`
- Modify: downstream crates still importing adapter metadata or `CatalogProvider.credentials`

- [ ] Delete `AdapterMetadata`, `AdapterControlCapabilities`, `AdapterKind::metadata()`, and `ALL_ADAPTERS`.
- [ ] Remove the temporary derived `CatalogProvider.credentials` field.
- [ ] Keep `AdapterKind` and `AgentProfileKind` as closed enums.
- [ ] Update adapter tests to cover only enum round-trips.
- [ ] Run `rg -n "AdapterMetadata|AdapterControlCapabilities|metadata\\(\\)|adapter\\.metadata|CatalogProvider.*credentials|\\.credentials" lib/crates/fabro-model lib/crates/fabro-auth lib/crates/fabro-cli lib/crates/fabro-server lib/crates/fabro-llm`.
- [ ] Treat the broad `.credentials` results as a sweep: only provider catalog usages and the transitional `CatalogProvider.credentials` view are targeted for removal.
- [ ] Run `cargo nextest run -p fabro-model -p fabro-auth -p fabro-cli`.

### Task 7: Update Docs And Generated Reference

**Files:**

- Modify: `lib/crates/fabro-dev/src/commands/docs_options_reference.rs`
- Modify: `docs/public/reference/user-configuration.mdx`
- Modify: any changelog or internal strategy docs that describe provider catalog shape.

- [ ] Document provider `auth`, `billing_policy`, and required `agent_profile`.
- [ ] Keep `probe = true` documented as a model field.
- [ ] Run `cargo dev docs check`; if stale, run `cargo dev docs refresh` and inspect the diff.

### Task 8: Workspace Policy Check And Cleanup

**Files:**

- Modify: tests only if policy tests need updated allowlists.

- [ ] Run `rg -n "AdapterMetadata|AdapterControlCapabilities|metadata\\(\\)|adapter\\.metadata|for_adapter\\(|pricing_policy_for_adapter" lib/crates`.
- [ ] Confirm remaining `AdapterKind::` matches are limited to enum tests and `fabro-llm::adapter_registry::factory_for`.
- [ ] Run:

```bash
cargo nextest run -p fabro-model -p fabro-config -p fabro-auth -p fabro-llm
cargo nextest run -p fabro-server diagnostics install
cargo dev docs check
cargo +nightly-2026-04-14 fmt --check --all
cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings
```

- [ ] Fix any failures caused by stale docs, stale imports, or tests that asserted adapter-kind behavior.

## Acceptance Criteria

- `AdapterKind` no longer exposes metadata.
- Provider auth header policy is configurable in provider TOML.
- Provider billing policy is configurable in provider TOML.
- Session profile selection uses provider catalog data.
- Install validation uses the real LLM probe path.
- Per-model `probe = true` remains the only probe model selection mechanism.
- Adding a provider with an existing adapter no longer requires Rust changes outside TOML and focused tests.

## Assumptions

- Breaking TOML shape is acceptable because this is a greenfield app with no production deployments.
- Simplicity and correctness are more important than preserving old install validation behavior.
- `openai_compatible` still requires `base_url`, but validation happens during adapter/client construction rather than catalog build.
- Codex OAuth remains an OpenAI-only special case.
- `BillingPolicy::None` is valid for local, test, or no-cost providers.
