# Server Secrets Strategy

This document defines how Fabro handles server-level secrets.

## Core Rules

- `ServerSecrets` is the canonical reader for **bootstrap** server secrets only.
- It reads bootstrap secrets from `process env` and `<storage>/server.env`.
- Resolution is snapshot-based: env and file are read once at construction, then treated as immutable for the life of the process.
- `process env` wins over `server.env` on conflicts.
- Optional integration secrets are vault-only in server runtime. Do not add optional server integrations to `ServerSecrets` or add new runtime env fallback paths.
- `fabro server start` never generates secrets. Missing required secrets are a startup error.
- `std::env::set_var` and `std::env::remove_var` are banned workspace-wide. Tests are not exempt. Enforced by clippy via `disallowed_methods` in `clippy.toml`; intentional exceptions must be annotated with a scoped `#[expect(clippy::disallowed_methods, reason = "...")]` at the call site.

## Bootstrap Server Secrets

These values may be read via `state.server_secret(...)` because the server can need them before optional integrations are available:

| Secret | Used by |
|---|---|
| `SESSION_SECRET` | Cookie encryption and JWT signing derivation |
| `FABRO_DEV_TOKEN` | Dev-token user auth when `server.auth.methods` includes `dev-token` |
| `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` / `AWS_SESSION_TOKEN` | Static S3 object-store credentials for server storage builders |

These optional integration secrets are **not** server bootstrap secrets. They are read from the vault only:

- LLM provider API keys and OAuth credential records
- `GITHUB_TOKEN`
- `GITHUB_APP_PRIVATE_KEY`
- `GITHUB_APP_CLIENT_SECRET`
- `GITHUB_APP_WEBHOOK_SECRET`
- `FABRO_SLACK_APP_TOKEN`
- `FABRO_SLACK_BOT_TOKEN`
- `DAYTONA_API_KEY`
- `BRAVE_SEARCH_API_KEY`

`FABRO_JWT_PRIVATE_KEY` and `FABRO_JWT_PUBLIC_KEY` are removed. `SESSION_SECRET` is the single auth root.

## Startup

- Foreground and daemon startup use the same validation path.
- Required-at-startup secrets are:
  - `SESSION_SECRET`
  - `FABRO_DEV_TOKEN` when dev-token auth is enabled
  - `GITHUB_APP_CLIENT_SECRET` from the vault when GitHub auth is enabled
- Requiredness is independent from source. GitHub auth can require a vault secret at startup even though it is not a bootstrap `ServerSecrets` value.
- Other optional integration secrets remain lazy/feature-specific rather than universal boot blockers.

## Provisioning

Bootstrap secrets come from one of two sources:

- Platform env for 12-factor deployments
- `server.env` written by install flows

Optional integration secrets are provisioned into the vault, usually with `fabro secret set` or `fabro install`.

There is no startup-time secret generation. A temporary startup migration moves recognized legacy optional secrets from process env or `server.env` into the vault, removes matching `server.env` entries after writing a backup, and logs conflicts by key name only. Runtime lookup remains vault-only after that migration step. See [migrations-strategy.md](migrations-strategy.md) for the migration pattern.

## Subprocess Boundaries

- Worker and render-graph subprocesses start from `env_clear()` and re-add only explicit allowlisted variables.
- Authority-bearing values are re-injected intentionally. For worker subprocesses this is `FABRO_WORKER_TOKEN`, plus any explicitly required internal value such as a vault-derived `GITHUB_APP_PRIVATE_KEY`; it is not user auth state such as `FABRO_DEV_TOKEN` or `auth.json`.
- The worker reads `FABRO_WORKER_TOKEN` from its env at startup (in `main()` before Tokio initializes) and immediately calls `std::env::remove_var` to scrub it. The token then flows through function arguments to `runner::execute`. Every descendant process (hooks, sandbox commands, MCP stdio, etc.) therefore inherits a worker env that no longer contains the bearer, so an unscrubbed spawn site cannot leak it.
- The daemon child inherits the parent env unchanged except for output-format hygiene (`FABRO_JSON` removal).

## Tests

- In-process tests must inject bootstrap server secrets with construction-time stubs (`EnvSource`, `StubEnv`) or by writing `server.env`.
- In-process tests for optional integrations must write the vault and must not rely on process env or `server.env`.
- Subprocess tests must set child env with `Command::env`.
- Tests must not mutate the process-wide environment.

## Rotation

- Secret rotation requires restart.
- Live rotation is intentionally unsupported.

## Adding A New Server Secret

1. Classify it in `fabro-static` as `Bootstrap` or `OptionalVault`.
2. For bootstrap secrets, provision through platform env or install-written `server.env`, then read through `state.server_secret(...)`.
3. For optional integration secrets, provision through the vault and read through `state.vault_secret(...)`.
4. Decide explicitly whether startup should fail when it is absent.
5. If a worker or render subprocess needs it, re-inject it explicitly rather than broadening inheritance casually.
