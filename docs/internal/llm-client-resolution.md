# LLM Client Resolution

This document defines how Fabro resolves LLM credentials and constructs `fabro-llm` clients.

## Core Rules

- `fabro_auth::CredentialSource` is the credential authority.
- Long-lived runtime contexts store `Arc<dyn CredentialSource>`, not `Client`.
- Call `fabro_llm::client::Client::from_source(&source).await?` at the point of use.
- `GenerateParams::new(model, client)` always receives an explicit `Arc<Client>`.
- When a caller needs diagnostics, call `source.resolve()` directly and consume both `credentials` and `auth_issues`.
- `EnvCredentialSource` is the env-backed source for env-only or no-vault contexts.
- `VaultCredentialSource` is the normal source for vault-backed runtime contexts.

## Why

- Rebuilding a client from the source at point of use preserves OAuth refresh behavior on long-running processes.
- Holding the source on contexts avoids process-global installs and cross-context leakage.
- Requiring an explicit client on `GenerateParams` makes the old silent fallback bug unrepresentable.

## Application

- Workflow state lives on `RunServices.llm_source`.
- Server state lives on `AppState.llm_source`.
- Hooks and other long-lived executors receive a source and derive clients when they actually generate.
- One-shot CLI commands may resolve a source locally, then derive a client once for that operation.

## Enforcement

- Do not add new `Client::from_env`-style shortcuts in production paths.
- Do not cache a long-lived `Client` where OAuth refresh or storage-dir rebinding matters.
- Mirror [server-secrets-strategy.md](/Users/bhelmkamp/p/fabro-sh/fabro-6/docs-internal/server-secrets-strategy.md): production credential resolution should be explicit about where secrets come from and how they flow into subprocesses.
