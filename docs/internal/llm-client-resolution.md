# LLM Client Resolution

This document defines how Fabro resolves LLM credentials and constructs `fabro-llm` clients.

## Core Rules

- `fabro_auth::CredentialSource` is the credential authority.
- Long-lived runtime contexts store `Arc<dyn CredentialSource>` and `Arc<Catalog>`, not `Client`.
- Call `fabro_llm::client::Client::from_source_with_catalog(&source, catalog).await?` at the point of use when runtime catalog settings are available.
- `Client::from_source(&source).await?` is the built-in-catalog fallback for setup, tests, and standalone contexts that do not have resolved runtime catalog settings.
- `GenerateParams::new(model, client)` always receives an explicit `Arc<Client>`.
- When a caller needs diagnostics in runtime request-serving paths, call `source.resolve_for_catalog(catalog)` directly and consume both `credentials` and `auth_issues`.
- Use `source.resolve()` only in built-in-catalog fallback contexts.
- `EnvCredentialSource` is the env-backed source for env-only or no-vault contexts.
- `VaultCredentialSource` is the normal source for vault-backed runtime contexts.

## Why

- Rebuilding a client from the source at point of use preserves OAuth refresh behavior on long-running processes.
- Holding the source on contexts avoids process-global installs and cross-context leakage.
- Threading the catalog into credential resolution keeps custom providers, aliases, header-only providers, and API model IDs consistent across auth, client registration, and request translation.
- Requiring an explicit client on `GenerateParams` makes the old silent fallback bug unrepresentable.

## Application

- Workflow state lives on `RunServices.llm_source` and `RunServices.catalog`.
- Server state lives on `AppState.llm_source` and `AppState.catalog()`.
- Hooks and other long-lived executors receive a source plus catalog and derive clients when they actually generate.
- One-shot CLI commands may resolve a source locally, then derive a client once for that operation. Use the built-in-catalog path only when those commands do not load runtime catalog settings.

## Enforcement

- Do not add new `Client::from_env`-style shortcuts in production paths.
- Do not cache a long-lived `Client` where OAuth refresh or storage-dir rebinding matters.
- Do not route runtime request-serving paths through `Client::from_source` or `CredentialSource::resolve()` if they have an `Arc<Catalog>`.
- Mirror [server-secrets-strategy.md](server-secrets-strategy.md): production credential resolution should be explicit about where secrets come from and how they flow into subprocesses.
