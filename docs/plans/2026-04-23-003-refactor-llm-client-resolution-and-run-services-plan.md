---
title: "refactor: Source-based LLM client resolution + RunServices split"
type: refactor
status: completed
date: 2026-04-23
deepened: 2026-04-23
---

# refactor: Source-based LLM client resolution + RunServices split

## Overview

PR #168 revealed that `fabro-llm::generate::generate()` silently falls back to `Client::from_env()` when no explicit client is passed. Vault-configured credentials (`openai_codex`) get bypassed. PR #168 is a point fix that threads `Option<Client>` through the pipeline. This plan replaces that with the correct architecture.

**Design in one line:** credentials are the long-lived authority; clients are short-lived derivations built at the point of use.

**Greenfield context.** No production installs, no backwards-compat, no migration. The bar is elegance.

**Two changes:**

1. Replace the silent env fallback with `CredentialSource` as the credential authority and `Client::from_source(&source)` as the idiomatic constructor. Every `generate()` requires an explicit `Arc<Client>` (compile-time enforcement). Delete `DEFAULT_CLIENT`, `set_default_client`, `get_default_client`, and `Client::from_env`.

2. Split `EngineServices` into cross-phase `RunServices` (holds `llm_source: Arc<dyn CredentialSource>`) + execute-only `EngineServices` (holds `Arc<RunServices>` + execute-only state). Phase structs carry `Arc<RunServices>` or `Arc<EngineServices>`, not individual service fields. PR #168's `Option<Client>` plumbing unwinds.

**Prerequisite.** PR #168 must merge to main before Phase 2. Phase 1 is independent of PR #168's merge state.

## Problem Frame

Two smells, one architectural root:

- **Silent env fallback.** `generate()` without an explicit client silently uses `Client::from_env()` — env-only, vault-unaware. Every future call site that forgets a client reintroduces the class bug.
- **`Option<Client>` threading.** PR #168 plumbed `Option<Client>` through phase structs to fix one instance. In production the client is always present; the `Option` is a domain lie.

**Root cause:** `Client` plays two roles today — a short-lived RPC handle AND the point of credential resolution. When a caller forgets to pass one, the "resolution" role silently falls through to env. Separating the two roles — `CredentialSource` is the long-lived authority, `Client` is always derived from one — makes the class bug unrepresentable.

## Requirements Trace

- **R1.** `fabro_llm::generate::generate()` fails to compile when no client is supplied. No runtime fallback of any kind.
- **R2.** Every long-lived runtime context that needs LLM access (`RunServices`, `AppState`, `HookExecutor`) holds `Arc<dyn CredentialSource>`, not a pinned `Client`. CLI subcommands resolve sources on demand via a `CommandContext::llm_source()` helper, not a held field.
- **R3.** OAuth refresh behavior is preserved — clients get fresh tokens at point of use, not at process start.
- **R4.** `RunServices` (cross-phase) and `EngineServices` (execute-only) have distinct types. Retro/finalize/PR-body code cannot reach execute-only state.
- **R5.** PR #168's production bug stays fixed, covered by a pipeline-level regression test (run with vault-only `openai_codex`, auto-PR body succeeds).
- **R6.** Duplicate `build_llm_client` in `fabro-workflow/handler/llm/api.rs` and `fabro-server/server_secrets.rs` collapses to one implementation.

## Scope Boundaries

- Don't rework `Client` internals, provider adapters, `from_credentials`, or any `fabro-llm` provider registration logic.
- Don't redesign vault/credential storage — `VaultCredentialSource` adapts existing `CredentialResolver` logic.
- Don't change phase ordering or per-phase behavior — only where shared state lives.
- Don't touch the `fabro-client` crate extraction (`docs/plans/2026-04-20-002`).
- Don't create `docs/solutions/` (separate opportunity).

## Context & Research

### Relevant Code

**`fabro-llm`:**
- `lib/crates/fabro-llm/src/client.rs` — `Client::new`, `Client::from_env`, `Client::from_credentials`, `provider_names`. `Client` derives `Clone`.
- `lib/crates/fabro-llm/src/generate.rs` — `DEFAULT_CLIENT: OnceCell<Arc<Client>>`, `set_default_client`, `get_default_client`, `generate`, `stream_with_tool_loop`, `stream_generate`, `generate_object`. `GenerateParams.client: Option<Arc<Client>>`.

**`fabro-auth`:**
- `lib/crates/fabro-auth/src/resolve.rs` — `CredentialResolver { vault: Arc<AsyncRwLock<Vault>>, env_lookup: EnvLookup }`. `resolve(provider, usage)` returns `ResolvedCredential::Api(ApiCredential)` or error. Handles OAuth refresh and writes back to vault.
- `ApiCredential`, `ApiKeyHeader` — already imported by `fabro-llm::client::Client::from_env`.

**`fabro-workflow`:**
- `lib/crates/fabro-workflow/src/handler/mod.rs` — `EngineServices` (13 fields today).
- `lib/crates/fabro-workflow/src/handler/llm/api.rs` — `build_llm_client(resolver: Option<&CredentialResolver>)`; `AgentApiBackend::create_session_for` and `one_shot` rebuild client per session via `build_llm_client(self.resolver.as_ref()).await?.client` at `:268` and `:349`. **This per-session rebuild is load-bearing for OAuth refresh.**
- `lib/crates/fabro-workflow/src/handler/llm/cli.rs` — parallel `AgentCliBackend` with its own `new_from_env`.
- `lib/crates/fabro-workflow/src/pipeline/types.rs` — phase structs and options.
- `lib/crates/fabro-workflow/src/pipeline/initialize.rs:555-566` — `SandboxReady` hook fires BEFORE `build_registry` at `:587` builds the LLM client. Any design that requires a client at hook time regresses this.
- `lib/crates/fabro-workflow/src/pipeline/{execute,retro,finalize,pull_request}.rs` — phase implementations.
- `lib/crates/fabro-workflow/src/operations/start.rs` — `RunSession`, `StartServices`. `RunSession.vault: Option<Arc<AsyncRwLock<Vault>>>` threads to `InitOptions.vault` at `:714`.

**Consumers:**
- `lib/crates/fabro-cli/src/main.rs` — CLI entry.
- `lib/crates/fabro-cli/src/command_context.rs:61-72` — `CommandContext::with_target` and `with_connection` re-derive context per subcommand with different `storage_dir_override`. **A process-global source install is not correct here.**
- `lib/crates/fabro-cli/src/commands/pr/create.rs` — loads vault at `:103`, calls `maybe_open_pull_request(..., None)`.
- `lib/crates/fabro-cli/src/shared/provider_auth.rs` — already passes explicit client.
- `lib/crates/fabro-hooks/src/executor.rs` — `execute_prompt` (default client today), `execute_agent` (`Client::from_env()` at `:358`).
- `lib/crates/fabro-agent/src/cli.rs:436` — `Client::from_env()`.
- `lib/crates/fabro-server/src/server.rs:2488` — `build_app_state`; `Vault::load` at `:2500`.
- `lib/crates/fabro-server/src/server_secrets.rs:87-112` — duplicate `ProviderCredentials::build_llm_client`.

### Institutional Learnings

- **`docs/plans/2026-04-20-002-refactor-extract-fabro-client-crate-plan.md`** — `CredentialFallback` named-trait precedent. Same rationale applies here: named trait makes the role obvious at the call site.
- **`docs/plans/2026-04-05-server-canonical-secrets-doctor-repo-plan.md:171,485`** — Already flagged `from_env()` as a smell; this plan resolves the deferred follow-up.
- **`docs/plans/2026-04-22-003-refactor-lock-down-server-secrets-plan.md:74-78`** — Credential taxonomy. LLM provider credentials live on the Vault + `ProviderCredentials` track.
- **`docs/plans/2026-04-08-cli-services-command-context-refactor-plan.md:59-61`** and **`docs/plans/2026-04-23-001-refactor-command-context-alignment-plan.md:273-277`** — Repo has twice rejected adding a peer-wrapper alongside an existing context. This plan's `RunServices`/`EngineServices` split is **composition** (`EngineServices` contains `Arc<RunServices>`), not a peer wrapper — and is about the pipeline (`fabro-workflow`), not CLI (`fabro-cli`). The rejections do not apply.

## Key Technical Decisions

### 1. Credentials are the authority; clients are derived

`CredentialSource` trait lives in `fabro-auth`:

```rust
pub struct ResolvedCredentials {
    pub credentials: Vec<ApiCredential>,
    pub auth_issues: Vec<(Provider, ResolveError)>,
}

pub trait CredentialSource: Send + Sync {
    /// Full resolution with OAuth refresh. Expensive. Returns credentials + auth issues.
    async fn resolve(&self) -> Result<ResolvedCredentials, Error>;

    /// Cheap preflight: which providers have credentials configured at all?
    /// No OAuth refresh. Used for model selection and manifest building.
    async fn configured_providers(&self) -> Vec<Provider>;
}
```

The richer return type preserves today's partial-resolution diagnostics. `auth_issues` exists because `CredentialResolver::resolve` can fail for one provider (e.g. "OpenAI OAuth refresh failed") while another succeeds — callers produce user-facing messages like "OpenAI requires re-authentication; Anthropic is not configured." This is load-bearing at four sites: `pipeline/initialize.rs:301-318` (error message on "no usable providers"), `server/src/server.rs:6652` (diagnostics endpoint), `server/src/server.rs:6821-6833` (`create_completion` logs `warn!` per issue), `server/src/diagnostics.rs:90`.

Natural home: `fabro-auth` already owns `ApiCredential`, `CredentialResolver`, and `ResolveError`. No new dep cycles. `fabro-llm` exposes `Client::from_source(&dyn CredentialSource) -> Result<Arc<Client>, Error>` — a convenience that discards `auth_issues` and returns only the Client. Callers that need diagnostics call `source.resolve()` directly and inspect both halves.

**Implementors:**
- `VaultCredentialSource` in `fabro-auth` — wraps `CredentialResolver`. Iterates `Provider::ALL` calling `resolver.resolve(provider, CredentialUsage::ApiRequest)`, collecting credentials and auth issues. Two constructors:
  - `VaultCredentialSource::new(Arc<AsyncRwLock<Vault>>)` — default env lookup (used by workflow path).
  - `VaultCredentialSource::with_env_lookup(Arc<AsyncRwLock<Vault>>, env_lookup)` — server uses this with its own env policy (preserves today's `ProviderCredentials::with_env_lookup` behavior at `server_secrets.rs:67`). Replaces `build_llm_client` in both `fabro-workflow/handler/llm/api.rs` and `fabro-server/server_secrets.rs`.
- `EnvCredentialSource` in `fabro-auth` — reads env vars for each provider, emits `ApiCredential`s. Replaces `Client::from_env`. `auth_issues` is typically empty for this source.

**Server-side implication:** `ProviderCredentials` collapses into a `VaultCredentialSource` constructed via `with_env_lookup`. The server doesn't need its own `CredentialSource` impl — the env-lookup policy is a constructor argument, not a type-level distinction. This is the key insight that lets the consolidation actually happen rather than just moving duplication behind a trait.

### 2. No process-global state

No `DEFAULT_CLIENT`, no `defaults::install`, no `Client::available_default`. Every long-lived runtime context holds its own `Arc<dyn CredentialSource>`:

- `RunServices.llm_source` — built from `InitOptions.vault` at initialize (or `EnvCredentialSource` if vault is `None`; see §3).
- `AppState.llm_source` — built from `Vault::load` at `build_app_state`.
- `HookExecutor.llm_source` — received on construction from the invoker.
- Standalone agent CLI — builds source at startup from resolved vault or env.
- Tests — build stubs directly, no install ceremony.

**CLI subcommands resolve sources on demand.** `CommandContext` does not hold a source as a field. Instead it exposes a lazy helper:

```rust
impl CommandContext {
    pub async fn llm_source(&self) -> Result<Arc<dyn CredentialSource>> { ... }
}
```

This mirrors the existing `CommandContext::server()` pattern (`command_context.rs:106-134`). The helper is re-derived per `with_target`/`with_connection` call, so each subcommand gets the right source for its resolved storage dir — no binding hazard.

This design eliminates: CLI re-derivation hazard (`CommandContext::with_connection` can change storage_dir), test cross-contamination on shared statics, double-install policy questions, feature-gate hazards, and the question "which install site wins."

### 3. `RunServices.llm_source` is always `Some` — no `Option`

Today's `RunSession.vault: Option<Arc<AsyncRwLock<Vault>>>` is optional (workflows can run without a vault, e.g. dry-run or env-only). The plan keeps `RunServices.llm_source` **non-optional** to preserve the elegance of "every context has a source." Rule:

- **Vault is `Some`:** build `VaultCredentialSource` from it.
- **Vault is `None`:** build `EnvCredentialSource::new()` — reads env vars; may resolve empty. Same semantics as today's `build_llm_client` with `resolver: None` branch.
- **Dry-run:** `EngineServices.dry_run` (separate flag, as today) is the authoritative signal. When true, handlers skip LLM stages entirely; `llm_source` is constructed anyway but unused.
- **No credentials anywhere:** `source.resolve()` returns empty `Vec<ApiCredential>`. `Client::from_source` returns a Client with `provider_names().is_empty()`. Consumers that need to `generate()` detect this via the same check as today (`initialize.rs:302`) and error with a helpful message built from `auth_issues`.

The error "No usable LLM providers configured" becomes a point-of-use error rather than an initialize-time error, **except** for graphs where `graph::needs_llm_handler_type` is true — for those, `initialize` preflights `source.resolve()` once and errors early with the same message today produces. Preserves UX.

### 4. Clients are built at point of use

`AgentApiBackend` today rebuilds `Client` per session via `build_llm_client(resolver)` — load-bearing for OAuth refresh on long runs. The new design preserves this pattern: every caller that wants a client calls `Client::from_source(&source).await?` at the point of use. Under the hood, `source.resolve()` calls `resolver.resolve()` which refreshes OAuth tokens and writes back to the vault.

No pinned per-run `Client`. No stale-token regression.

### 5. `GenerateParams.client: Arc<Client>` is required

Not `Option`. `GenerateParams::new(model, client)` takes both. Compile error if a future caller forgets.

### 6. Delete `Client::from_env`

`Client::from_env` was the silent-fallback gateway. It's replaced by `Client::from_source(&EnvCredentialSource::new())` — explicit, one-line, same result, and doesn't leave a footgun for future contributors.

`Client::from_credentials` stays as the lowest-level constructor (used internally by `from_source`).

### 7. `SandboxReady` hooks get a source, not a client

Hooks run at `pipeline/initialize.rs:555`, before `build_registry` at `:587`. In the source-on-context design this is fine: `InitOptions.vault` already exists at hook time; `initialize` builds a `VaultCredentialSource` eagerly (or `EnvCredentialSource` if vault is `None`) and passes it to the hook runner. A `SandboxReady` hook that wants to `generate()` calls `Client::from_source(&source).await?` at its point of use.

### 8. `RunServices` / `EngineServices` split is composition, not peer wrapping

Today's single `EngineServices` (13 fields) mixes two lifetimes:

- **Cross-phase** (live past execute into retro/finalize/PR): `run_store`, `emitter`, `sandbox`, `hook_runner`, `cancel_requested`, `provider`, and the new `llm_source`.
- **Execute-only** (die at the execute→retro boundary): `registry`, `inputs`, `workflow_bundle`, `workflow_path`, `dry_run`, `env`, `git_state`.

Split into two structs with `EngineServices { run: Arc<RunServices>, ...execute_only_fields }`. Retroed/Concluded/PullRequestOptions carry `Arc<RunServices>` only — they can't reach execute-only state by construction.

This is composition (`EngineServices` contains `Arc<RunServices>`), not the peer-wrapper pattern the CLI plans rejected. Handlers read `services.run.emitter` for cross-phase state and `services.registry` for execute-only state. One access shape, two concerns.

### 9. Server's `ProviderCredentials` collapses into `VaultCredentialSource::with_env_lookup`

`fabro-server/src/server_secrets.rs` today owns two capabilities:
- `build_llm_client` (`:87-112`) — full resolution with OAuth refresh, plus `auth_issues`.
- `configured_providers` (`:114-119`) — cheap preflight used for model selection at `server.rs:3929` and `run_manifest.rs:355`.

Both collapse into `VaultCredentialSource`: `build_llm_client` → `source.resolve()`; `configured_providers` → `source.configured_providers()` (the second trait method). With `VaultCredentialSource::with_env_lookup` covering the server's env policy (§1), `ProviderCredentials` disappears — `build_app_state` constructs `VaultCredentialSource::with_env_lookup(vault, env_lookup)` directly and stores it as `Arc<dyn CredentialSource>` on `AppState`. Callers at `server.rs:3929` and `run_manifest.rs:355` swap `state.provider_credentials.configured_providers().await` → `state.llm_source.configured_providers().await`. One trait, one type, two capabilities.

## Open Questions

### Resolved During Planning

- **Trait home?** `fabro-auth` (owns `ApiCredential`, `CredentialResolver`). Avoids `fabro-auth → fabro-llm` cycle.
- **Keep `Client::from_env`?** No. Replaced by `EnvCredentialSource` + `Client::from_source`. One construction path.
- **Process-global static?** No. Every context holds its own source.
- **`Client::available_default`?** Not needed. Callers use `Client::from_source(&ctx.llm_source)` where `ctx` is whatever context they hold.
- **Per-run client caching?** No. `Client::from_source` is called at point of use. Cost is acceptable (one resolver iteration + `from_credentials`), and this is where OAuth refresh happens.
- **OAuth refresh preservation?** Automatic. `resolver.resolve()` handles refresh; it runs every time a caller builds a client from a source.
- **CLI install site?** None. Each `CommandContext`-derived command that needs LLM builds its own source from the context's storage dir.
- **Server install site?** None. `build_app_state` constructs `VaultCredentialSource` once and stores it on `AppState`.
- **`SandboxReady` hook ordering?** No change. `InitOptions.vault` already exists at hook time; source is built before the hook runs.
- **Duplicate `build_llm_client` in server?** Delete. Server uses `VaultCredentialSource`. Parity test in Unit 1.2 guards the deletion.
- **`install_mock_llm` test helper?** Delete. Tests construct sources and clients directly.
- **Partial resolution diagnostics (`auth_issues`)?** Preserved. Trait returns `ResolvedCredentials { credentials, auth_issues }`. Four diagnostic call sites (`initialize.rs:301`, `server.rs:6652`, `server.rs:6821-6833` per-issue `warn!`, `diagnostics.rs:90`) consume `auth_issues` directly via `source.resolve()`. `Client::from_source` is the convenience path for callers that don't need diagnostics.
- **What source exists when `InitOptions.vault` is `None`?** `EnvCredentialSource::new()`. `RunServices.llm_source` is never `Option`. Graph-needs-LLM preflight inside `initialize` produces the same user-facing error today produces.
- **CLI source ownership?** `CommandContext::llm_source(&self) -> Result<Arc<dyn CredentialSource>>` — lazy helper, re-derived per `with_target`/`with_connection`. Mirrors existing `CommandContext::server()`.

### Deferred to Implementation

- **Exact error type on the trait.** `Result<ResolvedCredentials, ?>` — use `fabro-auth::Error` or introduce a trait-level `CredentialSourceError`. Decide while writing the trait.
- **Where `CommandContext::llm_source` caches.** `OnceCell<Arc<dyn CredentialSource>>` on the context like `server: OnceCell<Arc<Client>>` today, or freshly built per call. Decide by measuring: if CLI subcommands only call it once per invocation anyway, a `OnceCell` is overkill.
- **`HookExecutor` construction signature.** Takes `Arc<dyn CredentialSource>`. Workflow callers pass `services.run.llm_source.clone()`; standalone callers construct their own.
- **Test harness fixture.** `RunServices::for_test()` builds a stub source + minimal services. Exact surface decided during Unit 2.1.

## High-Level Technical Design

> *Directional guidance for review, not implementation specification.*

### Client resolution flow

```
Caller has long-lived Arc<dyn CredentialSource>

When a generate() is needed:
  let client = Client::from_source(&source).await?;
  generate(GenerateParams::new(model, client).prompt("..."))

Under the hood:
  Client::from_source(&source)
    └── source.resolve() → ResolvedCredentials { credentials, auth_issues }
          └── VaultCredentialSource: iterate Provider::ALL, resolver.resolve() (with OAuth refresh)
              or EnvCredentialSource: iterate Provider::ALL, read env
    └── Client::from_credentials(resolved.credentials)     // Client::from_source discards auth_issues

For diagnostics-aware callers (initialize preflight, server diag endpoint):
  let resolved = source.resolve().await?;
  if resolved.credentials.is_empty() && graph_needs_llm {
      // build detailed error from resolved.auth_issues
  }
```

### Services topology

```
┌────────────────────── RunServices (Arc) ────────────────────────┐
│ run_store, emitter, sandbox, hook_runner, cancel_requested,     │
│ provider, llm_source: Arc<dyn CredentialSource>                 │
└───────────────────────────────┬─────────────────────────────────┘
                                │ Arc<RunServices>
                                ▼
┌──────────────────── EngineServices (Arc) ───────────────────────┐
│ run: Arc<RunServices>,                                          │
│ registry, inputs, workflow_bundle, workflow_path,               │
│ dry_run, env, git_state                                         │
└─────────────────────────────────────────────────────────────────┘

Phase data flow:
  Persisted → Initialized { engine: Arc<EngineServices> }
            → Executed    { engine: Arc<EngineServices> }
            → Retroed     { services: Arc<RunServices> }   // drops execute-only state
            → Concluded   { services: Arc<RunServices> }
            → Finalized   { /* no services — terminal */ }
```

### Where sources come from

| Context | Source construction |
|---|---|
| Workflow run | `pipeline/initialize.rs` — `VaultCredentialSource::new(vault)` if `InitOptions.vault` is `Some`, else `EnvCredentialSource::new()` |
| CLI subcommand | `CommandContext::llm_source().await?` — lazy helper built from the context's resolved storage dir |
| Server | `build_app_state` — `VaultCredentialSource::with_env_lookup(vault, env_lookup)` from `Vault::load` (preserves today's server env-lookup policy) |
| Standalone `fabro agent` | CLI startup — resolved vault or `EnvCredentialSource` |
| Tests | Inline stub impl |

## Implementation Units

### Phase 1 — Source-based client resolution

- [x] **Unit 1.1: Add `CredentialSource` trait + `VaultCredentialSource` + `EnvCredentialSource` + `Client::from_source`**

  **Goal:** Pure addition of the new abstractions. No consumer changes. Existing code compiles and runs unchanged.

  **Requirements:** R2, R3 (groundwork).

  **Dependencies:** None.

  **Files:**
  - Modify: `lib/crates/fabro-auth/src/lib.rs` (re-exports)
  - Create: `lib/crates/fabro-auth/src/credential_source.rs` (trait)
  - Create: `lib/crates/fabro-auth/src/vault_source.rs` (`VaultCredentialSource`)
  - Create: `lib/crates/fabro-auth/src/env_source.rs` (`EnvCredentialSource`)
  - Modify: `lib/crates/fabro-llm/src/client.rs` (add `Client::from_source`)
  - Test: inline unit tests in each new file; `lib/crates/fabro-llm/src/client.rs` test for `from_source`

  **Approach:**
  - `struct ResolvedCredentials { credentials: Vec<ApiCredential>, auth_issues: Vec<(Provider, ResolveError)> }`.
  - `trait CredentialSource: Send + Sync` with `async fn resolve(&self) -> Result<ResolvedCredentials, Error>`. Use `async_trait::async_trait`.
  - `VaultCredentialSource::new(Arc<AsyncRwLock<Vault>>) -> Self` wraps `CredentialResolver::new` (default env lookup).
  - `VaultCredentialSource::with_env_lookup(Arc<AsyncRwLock<Vault>>, F) -> Self` where `F: Fn(&str) -> Option<String> + Send + Sync + 'static` wraps `CredentialResolver::with_env_lookup` (preserves server env policy).
  - `resolve()` iterates `Provider::ALL`, matching today's logic at `handler/llm/api.rs:43-78` byte-for-byte — both credentials accumulation and `auth_issues` accumulation.
  - `configured_providers()` delegates to `CredentialResolver::configured_providers(&vault)` at `fabro-auth/src/resolve.rs:140`. Cheap vault read; no OAuth refresh. Matches today's `ProviderCredentials::configured_providers` at `server_secrets.rs:114-119` byte-for-byte.
  - For `EnvCredentialSource`, `configured_providers()` iterates `Provider::ALL` checking env-var presence for each (returns providers whose env vars are set).
  - `EnvCredentialSource::new()` reads env for each provider in `Provider::ALL`, emitting `ApiCredential`s. `auth_issues` is always empty. Replaces the body of today's `Client::from_env`.
  - `Client::from_source(source: &dyn CredentialSource) -> Result<Arc<Client>, Error>` — calls `source.resolve()`, discards `auth_issues`, calls `Client::from_credentials`, wraps in `Arc`. Callers that need diagnostics call `source.resolve()` directly.
  - Move `auth_issue_message` helper (today duplicated in `fabro-workflow/handler/llm/api.rs:82` and `fabro-server/server_secrets.rs:127`) to `fabro-auth` alongside the trait.
  - `ApiCredential` must have a redacting `Debug` impl (verify/add using `fabro-redact`). Add unit test asserting `format!("{:?}", credential)` does not echo key material.

  **Patterns to follow:**
  - `CredentialFallback` trait shape in `docs/plans/2026-04-20-002-refactor-extract-fabro-client-crate-plan.md`.
  - Existing `CredentialResolver` iteration in `lib/crates/fabro-workflow/src/handler/llm/api.rs:43-78` (preserve semantics exactly, including partial-resolution `auth_issues` accumulation).

  **Test scenarios:**
  - Happy path: `VaultCredentialSource::resolve()` with a vault containing `openai_codex` returns `ResolvedCredentials` with the OpenAI entry in `.credentials` and empty `.auth_issues`.
  - Happy path: `VaultCredentialSource::configured_providers()` with a vault containing OpenAI + Anthropic returns `[OpenAi, Anthropic]`. No OAuth refresh happens (verified via a stub vault that panics on write).
  - Happy path: `EnvCredentialSource::configured_providers()` with `ANTHROPIC_API_KEY` set returns `[Anthropic]`; with nothing set, returns empty.
  - Happy path (partial resolution): `VaultCredentialSource::resolve()` with a vault containing `openai_codex` whose refresh token has expired **and** a working Anthropic `ApiKey` returns both — Anthropic in `.credentials`, `(OpenAi, ResolveError::RefreshFailed{...})` in `.auth_issues`. Matches real auth shapes (`CodexOAuth` is OpenAI-only per `fabro-auth/src/credential.rs:69-73`).
  - Happy path: `EnvCredentialSource` with `ANTHROPIC_API_KEY` set returns one entry; with nothing set, returns an empty `ResolvedCredentials` (not an error).
  - Happy path: `Client::from_source(&stub)` where stub returns a single-provider `ResolvedCredentials` yields a Client with that provider registered.
  - Happy path: `Client::from_source(&VaultCredentialSource::new(vault))` with OAuth credential calls through `resolver.resolve` (verified via a stub vault that records calls).
  - Edge case: source returning empty credentials yields a Client with `provider_names().is_empty()` — no error.
  - Redaction: `format!("{:?}", credential)` does not contain the key/token.

  **Verification:**
  - `cargo nextest run -p fabro-auth -p fabro-llm` passes.
  - `cargo +nightly-2026-04-14 clippy -p fabro-auth -p fabro-llm --all-targets -- -D warnings` clean.

- [x] **Unit 1.2: Migrate every LLM consumer to hold/pass sources and build clients explicitly**

  **Goal:** Every `generate()`/`generate_object()`/`stream_generate()` call site constructs its `Arc<Client>` explicitly (via `Client::from_source`). Every long-lived backend/executor/context holds `Arc<dyn CredentialSource>`. `DEFAULT_CLIENT` and the default-client code path remain temporarily so each migration step compiles/tests cleanly.

  **Requirements:** R2, R3, R6.

  **Dependencies:** Unit 1.1.

  **Files:**
  - Modify: `lib/crates/fabro-workflow/src/handler/llm/api.rs` — `AgentApiBackend` holds `Arc<dyn CredentialSource>`; `create_session_for` and `one_shot` call `Client::from_source(&self.source)` per call (matching today's per-session rebuild). Delete `build_llm_client` and the local `auth_issue_message` helper (both moved to `fabro-auth` in Unit 1.1).
  - Modify: `lib/crates/fabro-workflow/src/handler/llm/cli.rs` — parallel treatment for `AgentCliBackend`.
  - Modify: `lib/crates/fabro-workflow/src/pipeline/initialize.rs` — `build_registry` takes/holds `Arc<dyn CredentialSource>`. Builds source from `InitOptions.vault` when `Some`, else `EnvCredentialSource::new()`. Passes source to backends. For `graph_needs_llm` paths, preflights `source.resolve()` once at initialize and produces the same "No usable LLM providers configured: ..." error at `:301-318` by consuming `ResolvedCredentials.auth_issues`. Phase 2 moves the source onto `RunServices`; for this unit, hold it alongside the registry return value.
  - Modify: `lib/crates/fabro-workflow/src/pipeline/initialize.rs:555-566` — `SandboxReady` hook invocation passes the already-built source into the hook runner.
  - Modify: `lib/crates/fabro-workflow/src/pipeline/pull_request.rs` — `build_pr_body`/`maybe_open_pull_request` accept an explicit client (PR #168 already added this; here it becomes non-Option).
  - Modify: `lib/crates/fabro-hooks/src/executor.rs` — `HookExecutor::new(..., source: Arc<dyn CredentialSource>)`. `execute_prompt` and `execute_agent` call `Client::from_source(&self.source)` at point of use. Remove `Client::from_env()` at `:358`.
  - Modify: `lib/crates/fabro-hooks/src/runner.rs` — `HookRunner` carries source, forwards to executor.
  - Modify: `lib/crates/fabro-cli/src/main.rs` — no process-global install.
  - Modify: `lib/crates/fabro-cli/src/command_context.rs` — add lazy helper `pub async fn llm_source(&self) -> Result<Arc<dyn CredentialSource>>`. Resolves from the context's current storage dir; builds `VaultCredentialSource` when a vault is present, `EnvCredentialSource` otherwise. Follow the existing `CommandContext::server()` pattern at `:106-134`.
  - Modify: `lib/crates/fabro-cli/src/commands/pr/create.rs` — replace today's direct `Vault::load` at `:103` with `ctx.llm_source().await?`. Call `Client::from_source(&source)`, pass into `maybe_open_pull_request`.
  - Modify: `lib/crates/fabro-cli/src/shared/provider_auth.rs` — already passes an explicit client. No change.
  - Modify: `lib/crates/fabro-agent/src/cli.rs` — replace `Client::from_env()` with source-based resolution (either via `CommandContext::llm_source` or an equivalent startup helper).
  - Modify/delete: `lib/crates/fabro-server/src/server_secrets.rs` — delete `ProviderCredentials` entirely (struct at `:61-75`, `build_llm_client` at `:87-112`, `configured_providers` at `:114-119`, local `auth_issue_message` at `:127`). Both capabilities move to `VaultCredentialSource` via the `CredentialSource` trait. Diagnostic callers at `server.rs:6652`, `server.rs:6821`, and `diagnostics.rs:90` consume `ResolvedCredentials.auth_issues` from `source.resolve()`.
  - Modify: `lib/crates/fabro-server/src/server.rs` — `build_app_state` (`:2488`) constructs `VaultCredentialSource::with_env_lookup(vault, env_lookup)` and stores it as `Arc<dyn CredentialSource>` on `AppState`. `create_completion` (`:6821-6833`) calls `state.llm_source.resolve()` directly (not `Client::from_source`), logs each `auth_issue` via `warn!` (preserving today's observability at `:6831-6833`), then builds the client via `Client::from_credentials(resolved.credentials)`. Server diagnostics endpoint (`:6652`) same pattern. Site at `:3929` swaps `state.provider_credentials.configured_providers().await` → `state.llm_source.configured_providers().await`.
  - Modify: `lib/crates/fabro-server/src/run_manifest.rs:355` — same swap: `state.provider_credentials.configured_providers().await` → `state.llm_source.configured_providers().await`.
  - Test: per-crate unit tests; a workflow-level integration test that runs a minimal pipeline with a vault-only `openai_codex` credential and asserts agent stages use a fresh client each session.

  **Approach:**
  - Mechanical migration. Each consumer that previously used the default client now constructs explicitly from a source it already has access to.
  - `AgentApiBackend` today: `build_llm_client(self.resolver.as_ref()).await?.client` per session. New: `Client::from_source(&self.source).await?` per session. Functionally equivalent; OAuth refresh still happens because `source.resolve()` calls `resolver.resolve()` which refreshes.
  - `build_registry` no longer needs to return a client — it returns the registry and the source (passed to backends). Clients are built per session inside the backends.
  - Diagnostic sites that previously called `build_llm_client(...).auth_issues` now call `source.resolve()` and consume `.auth_issues` directly. Four sites: `initialize.rs:301` (error message), `server.rs:6652` (diagnostics endpoint), `server.rs:6821-6833` (`create_completion` per-issue `warn!`), `diagnostics.rs:90`. Each of these stays on the `resolve()` path rather than using the `Client::from_source` convenience — they need both halves.

  **Patterns to follow:**
  - `AgentApiBackend::create_session_for:268` (today's per-session pattern) is the model.
  - `CommandContext::server()` (`command_context.rs:106-134`) is the model for the new `llm_source()` helper — lazy, re-derived per context, cached via `OnceCell` per context.

  **Test scenarios:**
  - Happy path (workflow, vault): pipeline run with vault-configured `openai_codex` produces a Client per session with `openai` registered. Verified via a test that wraps a stub source and counts `resolve()` calls (should equal session count + 1 for retro/PR + 1 for initialize preflight).
  - Happy path (workflow, no vault): `InitOptions.vault = None`, graph has no LLM handlers — initialize succeeds; `llm_source` is an `EnvCredentialSource` and is never queried.
  - Error path (workflow, no vault, graph needs LLM): `InitOptions.vault = None`, graph has LLM handlers, no env credentials — initialize preflight `source.resolve()` returns empty credentials; error produced is the same "No usable LLM providers configured: ..." message today builds from `auth_issues` (empty for env source → plain "No LLM providers configured" fallback).
  - Error path (workflow, vault with one bad credential + one good): partial resolution — `openai_codex` with expired refresh token + working Anthropic `ApiKey`. Initialize preflight succeeds (Anthropic is usable); `auth_issues` is logged or surfaced per today's behavior.
  - Happy path (CLI `pr create`): command calls `ctx.llm_source().await?`, builds client, generates PR body. No `None` parameter anywhere.
  - Happy path (CLI context re-derivation): `ctx.with_connection(args)` produces a new context with its own `llm_source` bound to the new storage dir. Verified by calling `llm_source` on two derived contexts with different storage dirs.
  - Happy path (hooks, workflow-invoked): `SandboxReady` hook that wants to generate receives source from `initialize`; builds a client and calls `generate`.
  - Happy path (hooks, standalone agent): `fabro agent` CLI builds source at startup, passes to `HookExecutor`; hooks build clients as needed.
  - Regression: server `create_completion` request generates successfully after `build_app_state` stores the source on `AppState`.
  - Regression: server diagnostic endpoint at `server.rs:6652` still reports auth issues for misconfigured providers.
  - Regression: server `create_completion` at `server.rs:6821-6833` still emits `warn!` per auth issue before building the client. Verified via a test with a partial-resolution fixture source (one working provider, one failing) that captures `warn!` events and asserts the expected log output.
  - Regression: `AgentCliBackend` (CLI-mode agent) works with source-based resolution.
  - Parity (resolve): `VaultCredentialSource::with_env_lookup(vault, env_lookup).resolve()` output (credentials + auth_issues) matches today's `ProviderCredentials::build_llm_client()` output for the same fixture vault + env policy.
  - Parity (configured_providers): `VaultCredentialSource::with_env_lookup(vault, env_lookup).configured_providers()` output matches today's `ProviderCredentials::configured_providers()` output for the same fixture. Required before deleting the old path.

  **Verification:**
  - `cargo nextest run --workspace` passes.
  - `cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings` clean.
  - `rg 'Client::from_env' lib/crates/` — only `client.rs` (constructor definition), `EnvCredentialSource` (if it reuses the body), and test files.
  - `rg 'build_llm_client' lib/crates/` — zero hits.

- [x] **Unit 1.3: Delete the default-client machinery and `Client::from_env`; require `GenerateParams.client`**

  **Goal:** Atomic cleanup. Every consumer already passes an explicit client (Unit 1.2). Now remove the fallback paths.

  **Requirements:** R1, R6.

  **Dependencies:** Unit 1.2.

  **Files:**
  - Modify: `lib/crates/fabro-llm/src/generate.rs` — delete `DEFAULT_CLIENT`, `set_default_client`, `get_default_client`, the `Client::from_env()` fallback inside `get_default_client`. `GenerateParams.client: Arc<Client>` (not `Option`). `GenerateParams::new(model, client)` takes both. Drop the `.client(...)` builder method.
  - Modify: `lib/crates/fabro-llm/src/lib.rs` — drop `set_default_client` re-export.
  - Modify: `lib/crates/fabro-llm/src/client.rs` — delete `Client::from_env`. Keep `Client::from_credentials` (used internally by `EnvCredentialSource` and `Client::from_source`).
  - Modify: `lib/crates/fabro-workflow/src/pipeline/pull_request.rs` — delete `install_mock_llm` helper and the three `install_mock_llm()` call sites. Tests construct clients directly via a mock source.
  - Modify: ~30 test sites in `lib/crates/fabro-llm/src/generate.rs` — use a helper `#[cfg(test)] fn test_params(model: &str) -> GenerateParams` that constructs a mock client.
  - Test: new negative test — attempting to build `GenerateParams` without a client is a compile error (trybuild or inspection).

  **Approach:**
  - Introduce the test helper first, then swap sites mechanically.
  - `GenerateParams` spread patterns (`GenerateParams { response_format: ..., ..params }`) continue to work since `client` is a field with a value being propagated.
  - `MockProvider` in `pull_request.rs` keeps its `name` field (added by PR #168) — useful for the regression test where one mock is registered under the "openai" provider name.

  **Test scenarios:**
  - Compile-time enforcement: `GenerateParams::new(model)` without a client arg fails to compile (by the struct shape).
  - All existing `fabro-llm` generate tests pass with `test_params(model)` swap.
  - Regression (pipeline-level): workflow with vault-only `openai_codex` runs to completion and generates a PR body using vault-resolved credentials. (This is the class-bug fix from PR #168, now type-enforced.)

  **Verification:**
  - `cargo nextest run --workspace` passes.
  - `rg 'DEFAULT_CLIENT|set_default_client|get_default_client|install_mock_llm|Client::from_env' lib/crates/` — zero hits.
  - `rg 'Option<Arc<Client>>' lib/crates/` — zero hits in production code.

### Phase 2 — `RunServices` split + PR #168 `Option<Client>` unwind

**Prerequisite:** PR #168 must have merged to main.

- [x] **Unit 2.1: Introduce `RunServices` + `EngineServices` split**

  **Goal:** Today's `EngineServices` (13 fields, mixed lifetimes) partitions into cross-phase `RunServices` + execute-only `EngineServices`. Handlers read cross-phase state as `services.run.xxx`.

  **Requirements:** R4.

  **Dependencies:** Phase 1 complete.

  **Files:**
  - Create: `lib/crates/fabro-workflow/src/services.rs` (both structs + `RunServices::for_test`)
  - Modify: `lib/crates/fabro-workflow/src/handler/mod.rs` — delete old `EngineServices`; re-export from `services`.
  - Modify: `lib/crates/fabro-workflow/src/handler/{parallel,manager_loop,prompt,agent,command,fan_in,human}.rs` — mechanical rewrite: `services.run_store` → `services.run.run_store`, etc.
  - Modify: `lib/crates/fabro-workflow/src/node_handler.rs` — same rewrites.
  - Modify: `lib/crates/fabro-workflow/src/pipeline/initialize.rs` — `build_registry` now returns `Arc<EngineServices>` (holding `Arc<RunServices>` with `llm_source`). Dry-run still produces a valid services pair.
  - Modify: `lib/crates/fabro-workflow/src/pipeline/execute.rs` — use `Arc<EngineServices>` directly instead of reconstructing it.
  - Modify: `lib/crates/fabro-workflow/src/test_support.rs` — test fixture adapts.
  - Test: handler unit tests and `fabro-workflow/tests/it/integration.rs` pipeline tests.

  **Approach:**

  | Field | Lives on |
  |---|---|
  | `run_store` | `RunServices` |
  | `emitter` | `RunServices` |
  | `sandbox` | `RunServices` |
  | `hook_runner` | `RunServices` |
  | `cancel_requested` | `RunServices` |
  | `provider` | `RunServices` |
  | `llm_source` | `RunServices` (**new**) |
  | `registry` | `EngineServices` |
  | `inputs` | `EngineServices` |
  | `workflow_bundle` | `EngineServices` |
  | `workflow_path` | `EngineServices` |
  | `dry_run` | `EngineServices` |
  | `env` | `EngineServices` |
  | `git_state` | `EngineServices` |

  - `RunServices::for_test() -> Arc<Self>` builds a minimal services bundle with a stub source. Used by handler and pipeline tests.
  - No delegation methods on `EngineServices`; handlers read through `services.run.xxx` explicitly. More lines but clearer boundary.

  **Patterns to follow:**
  - Existing `Arc<EngineServices>` construction at `pipeline/execute.rs:84`.

  **Test scenarios:**
  - Happy path: `RunServices::for_test()` constructs cleanly and supports all handler unit tests.
  - Happy path: end-to-end pipeline test runs without regression.
  - Type-level: a retro-phase test asserting `services.run.emitter` compiles but `services.registry` does not (via a compile-fail test or a comment marker).
  - Edge case: `services.run_hooks(&HookContext)` method lives on `RunServices` since `hook_runner` moved there.
  - Edge case: `services.git_state()` / `set_git_state(...)` stays on `EngineServices` (execute-only).

  **Verification:**
  - `cargo nextest run -p fabro-workflow` passes.
  - `cargo clippy -p fabro-workflow --all-targets -- -D warnings` clean.
  - `rg 'services\.run_store' lib/crates/fabro-workflow/src/handler/` — zero hits (all now `services.run.run_store`).

- [x] **Unit 2.2: Shrink phase structs; unwind PR #168 `Option<Client>`**

  **Goal:** `Initialized`/`Executed` carry `Arc<EngineServices>`; `Retroed`/`Concluded` carry `Arc<RunServices>`. Delete `Option<Client>` threading introduced by PR #168.

  **Requirements:** R4, R5.

  **Dependencies:** Unit 2.1 + PR #168 merged.

  **Files:**
  - Modify: `lib/crates/fabro-workflow/src/pipeline/types.rs` — shrink phase structs (below), shrink Options structs.
  - Modify: `lib/crates/fabro-workflow/src/pipeline/execute.rs` — destructure updates (carry `engine`).
  - Modify: `lib/crates/fabro-workflow/src/pipeline/retro.rs` — read `executed.engine.run`; drop `options.llm_client`.
  - Modify: `lib/crates/fabro-workflow/src/pipeline/finalize.rs` — destructure updates.
  - Modify: `lib/crates/fabro-workflow/src/pipeline/pull_request.rs` — drop `Option<Client>` parameter from `build_pr_body`/`maybe_open_pull_request`/`pull_request`; build via `Client::from_source(&services.run.llm_source)`.
  - Modify: `lib/crates/fabro-workflow/src/operations/start.rs` — `RunSession::run` threads services Arc through phase calls (no longer cloning `executed.llm_client`).
  - Modify: `lib/crates/fabro-cli/src/commands/pr/create.rs` — build `RunServices::for_cli(source, run_store, ...)` and pass to `maybe_open_pull_request`.
  - Test: `lib/crates/fabro-workflow/tests/it/integration.rs` pipeline-level regression; update phase-level tests.

  **Approach — phase structs after the split:**

  - `Initialized { graph, source, inputs, run_options, workflow_path, workflow_bundle, checkpoint, seed_context, engine: Arc<EngineServices>, model }`
  - `Executed { graph, outcome, run_options, duration_ms, final_context, engine: Arc<EngineServices>, model }`
  - `Retroed { graph, outcome, run_options, duration_ms, retro, services: Arc<RunServices> }`
  - `Concluded { run_id, outcome, conclusion, pushed_branch, graph, run_options, services: Arc<RunServices> }`
  - `Finalized { run_id, outcome, conclusion, pushed_branch, pr_url }` (unchanged, terminal)
  - `RetroOptions` drops `llm_client`, `provider`, `sandbox`, `run_store`, `emitter` (read from services); keeps `run_id`, `workflow_name`, `goal`, `run_dir`, `failed`, `run_duration_ms`, `enabled`, `model`.
  - `PullRequestOptions` drops `run_store`; keeps `run_dir`, `pr_config`, `github_app`, `origin_url`, `model`.
  - `build_pr_body(services: &RunServices, diff, goal, model, conclusion) -> Result<String, String>` — builds client via `Client::from_source(&services.llm_source)`.

  All phase structs retain `#[non_exhaustive]` (already present).

  **Patterns to follow:**
  - Unit 2.1's `Arc<EngineServices>` shape.

  **Test scenarios:**
  - Regression (the R5 pipeline-level test): workflow run with vault-configured `openai_codex`, no `OPENAI_API_KEY` env, generates a PR body successfully. Previously failed with "Provider 'openai' not registered."
  - Happy path: CLI `fabro pr create` resolves vault, builds source, builds `RunServices::for_cli`, calls `maybe_open_pull_request`.
  - Edge case: retro when disabled still no-ops (doesn't call `from_source`).
  - Edge case: workflow with no LLM stages and no vault initializes cleanly (source built from vault; `Client::from_source` is called lazily by consumers, not eagerly).

  **Verification:**
  - `cargo nextest run --workspace` passes.
  - `rg 'llm_client:' lib/crates/fabro-workflow/` — zero hits in non-test code.
  - `rg 'Option<Client>' lib/crates/` — zero production hits.

## System-Wide Impact

- **Interaction graph:** Every `generate`/`generate_object`/`stream_generate` consumer is affected — `fabro-workflow`, `fabro-hooks`, `fabro-agent`, `fabro-cli`, `fabro-server`. Each now builds its client from a source it already holds.
- **Error propagation:** Source-resolution errors (vault lookup failure, OAuth refresh failure) surface at `Client::from_source` call sites, which is the same point they surface today at `build_llm_client` call sites. Error semantics preserved.
- **State lifecycle risks:** `Client::from_source` builds a fresh client per call. For most contexts this runs once per long-running operation (per session, per PR body, per hook invocation). The cost is identical to today's `build_llm_client(resolver)` pattern — unchanged.
- **API surface parity:** `fabro_llm::set_default_client` re-export deletes. `GenerateParams::new` signature changes. `Client::from_env` deletes. Internal-only API shape changes; no external consumers in this monorepo.
- **Integration coverage:** R5's pipeline-level regression test (Unit 2.2) is the critical insurance — proves the class bug is closed at the pipeline shape level, not just at one unit.
- **Credential trust boundary:** `RunServices.llm_source` is shared across all phases by design. Workflow stages run against the same vault as retro/PR body — which matches today's behavior. Callers deploying for multi-tenant workflows must not mix trust levels in a single run. Per-phase credential scoping is out of scope.
- **Unchanged invariants:** `Vault`, `CredentialResolver`, `ApiCredential`, `Provider::ALL`, `ResolvedCredential` enum, phase ordering, `#[non_exhaustive]` on all phase structs, OAuth refresh behavior.

## Risks & Dependencies

| Risk | Mitigation |
|---|---|
| PR #168 lands with `Option<Client>` threading that Phase 2 needs to unwind. If PR #168 stalls, Phase 2 blocks. | Phase 1 is independent of PR #168 and can land first. Phase 2 starts when PR #168 merges. |
| 30+ `GenerateParams::new("mock-model")` test sites in `fabro-llm/src/generate.rs`. | Introduce `test_params(model)` helper before migrating sites; one-line edits thereafter (Unit 1.3). |
| Phase struct destructure patterns in `pipeline/execute.rs`, `pipeline/retro.rs`, `pipeline/finalize.rs` break when struct shape changes. | Unit 2.2 updates them explicitly. `#[non_exhaustive]` protects external crates; in-crate destructures adjust. |
| Server `ProviderCredentials` owns two capabilities (`build_llm_client` + `configured_providers`). Consolidation must preserve both. | Unit 1.2 parity tests on both paths: `resolve()` vs today's `build_llm_client()` (credentials + auth_issues), and `configured_providers()` vs today's `configured_providers()`, against the same fixture vault + env policy. Reconcile any divergence before deletion. |
| `SandboxReady` hooks today have no client; tomorrow they call `Client::from_source(&source)` on demand. Construction cost per hook. | Source already holds the resolver; `source.resolve()` is the same cost as today's `build_llm_client` call that would happen anyway if a hook wanted to generate. No net regression. |
| `AgentApiBackend` per-session client rebuild is load-bearing for OAuth refresh. | Preserved verbatim: backend holds source, calls `Client::from_source(&source)` per session. `source.resolve()` → `resolver.resolve()` → OAuth refresh. Identical behavior. |

## Documentation / Operational Notes

- Add a short strategy doc at `docs-internal/llm-client-resolution.md` describing the source-on-context rule: `fabro-auth::CredentialSource` is the authority, `Client` is always derived via `Client::from_source`, `generate()` requires an explicit client. Reference `docs-internal/server-secrets-strategy.md` for the adjacent "no `from_env` in production paths" enforcement model.
- A future follow-up should create a `docs/solutions/` entry capturing "pipeline boundary drop" as a pattern learning. Out of scope for this plan.

## Sources & References

- **Related PR:** #168 (`fix(workflow): reuse resolved llm client for auto-pr`) — the point fix this refactor replaces.
- **Related plans:**
  - `docs/plans/2026-04-20-002-refactor-extract-fabro-client-crate-plan.md` — `CredentialFallback` named-trait precedent.
  - `docs/plans/2026-04-05-server-canonical-secrets-doctor-repo-plan.md` — `from_env()` smell pre-flagged.
  - `docs/plans/2026-04-22-003-refactor-lock-down-server-secrets-plan.md` — credential taxonomy.
  - `docs/plans/2026-04-08-cli-services-command-context-refactor-plan.md`, `docs/plans/2026-04-23-001-refactor-command-context-alignment-plan.md` — "no peer wrapper" precedent (addressed in Key Technical Decisions).
- **Strategy docs:**
  - `docs-internal/server-secrets-strategy.md` — the `set_var`/`remove_var` enforcement model is precedent for enforcing "no `from_env` in production paths."
- **Target files:** `lib/crates/fabro-llm/`, `lib/crates/fabro-auth/`, `lib/crates/fabro-workflow/`, `lib/crates/fabro-cli/`, `lib/crates/fabro-server/`, `lib/crates/fabro-hooks/`, `lib/crates/fabro-agent/`.

## Unresolved questions

- **`docs/solutions/` seeding** — out of scope for this plan; flagged as a follow-up.
- **Observability** — should `VaultCredentialSource::resolve` emit a tracing event on successful resolution? Low-value, but worth deciding while implementing.
