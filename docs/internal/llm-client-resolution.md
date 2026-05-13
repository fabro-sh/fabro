# LLM Client Resolution

This document defines how Fabro resolves LLM credentials and constructs `fabro-llm` clients.

## Core Rules

- `fabro_auth::CredentialSource` is the credential authority.
- Long-lived runtime contexts store `Arc<dyn CredentialSource>` and `Arc<Catalog>`, not `Client`.
- Call `fabro_llm::client::Client::from_source(&source, catalog).await?` at the point of use.
- Standalone setup and tests that use default settings build a default `Arc<Catalog>` locally, then pass it explicitly.
- `GenerateParams::new(model, client)` always receives an explicit `Arc<Client>`.
- When a caller needs diagnostics in runtime request-serving paths, call `source.resolve(catalog)` directly and consume both `credentials` and `auth_issues`.
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
- One-shot CLI commands may resolve a source locally, build a default settings catalog if they do not load runtime catalog settings, then derive a client once for that operation.

## Enforcement

- Do not add new `Client::from_env`-style shortcuts in production paths.
- Do not cache a long-lived `Client` where OAuth refresh or storage-dir rebinding matters.
- Do not construct LLM clients or resolve credentials without an explicit `Arc<Catalog>` or `&Catalog`.
- Mirror [server-secrets-strategy.md](server-secrets-strategy.md): production credential resolution should be explicit about where secrets come from and how they flow into subprocesses.
