# Server Secrets Strategy

This document defines how Fabro handles server-level secrets.

## Core Rules

- `ServerSecrets` is the canonical server-secret reader.
- It reads from `process env` and `<storage>/server.env`.
- Resolution is snapshot-based: env and file are read once at construction, then treated as immutable for the life of the process.
- `process env` wins over `server.env` on conflicts.
- `fabro server start` never generates secrets. Missing required secrets are a startup error.
- `std::env::set_var` and `std::env::remove_var` are banned workspace-wide. Tests are not exempt. CI enforces this with `bin/dev/check-env-mutation.sh` so broad clippy suppressions cannot bypass it.

## Active Server Secrets

These values belong to the server runtime and are read via `state.server_secret(...)`:

| Secret | Used by |
|---|---|
| `SESSION_SECRET` | Cookie encryption and JWT signing derivation |
| `FABRO_DEV_TOKEN` | Dev-token auth for worker/server interactions |
| `GITHUB_APP_PRIVATE_KEY` | GitHub App credentials |
| `GITHUB_APP_WEBHOOK_SECRET` | GitHub webhook verification |
| `GITHUB_APP_CLIENT_SECRET` | GitHub OAuth login |

`FABRO_JWT_PRIVATE_KEY` and `FABRO_JWT_PUBLIC_KEY` are removed. `SESSION_SECRET` is the single auth root.

## Startup

- Foreground and daemon startup use the same validation path.
- Required-at-startup secrets are:
  - `SESSION_SECRET`
  - `FABRO_DEV_TOKEN` when dev-token auth is enabled
  - `GITHUB_APP_CLIENT_SECRET` when GitHub auth is enabled
- Other server secrets remain lazy/feature-specific rather than universal boot blockers.

## Provisioning

Server secrets come from one of two sources:

- Platform env for 12-factor deployments
- `server.env` written by install flows

There is no compatibility layer for removed secrets and no startup-time secret generation.

## Subprocess Boundaries

- Worker and render-graph subprocesses start from `env_clear()` and re-add only explicit allowlisted variables.
- Authority-bearing values are re-injected intentionally.
- The daemon child inherits the parent env unchanged except for output-format hygiene (`FABRO_JSON` removal).

## Tests

- In-process tests must inject server secrets with construction-time stubs (`EnvSource`, `StubEnv`) or by writing `server.env`.
- Subprocess tests must set child env with `Command::env`.
- Tests must not mutate the process-wide environment.

## Rotation

- Secret rotation requires restart.
- Live rotation is intentionally unsupported.

## Adding A New Server Secret

1. Provision it through platform env or install-written `server.env`.
2. Read it through `state.server_secret(...)`.
3. Decide explicitly whether startup should fail when it is absent.
4. If a worker or render subprocess needs it, re-inject it explicitly rather than broadening inheritance casually.
