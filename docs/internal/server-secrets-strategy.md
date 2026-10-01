# Server Secrets Strategy

This document defines how Fabro handles server-level secrets.

Fabro always runs as a server process plus one worker process per run. There is no CLI-local run
execution, so the operative question for any credential is **which process holds the value, and
when does it resolve** — see [Which process resolves what](#which-process-resolves-what).

## Core Rules

- `ServerSecrets` is the canonical reader for **bootstrap** server secrets only.
- It reads bootstrap secrets from `process env` and `<storage>/server.env`.
- Resolution is snapshot-based: env and file are read once at construction, then treated as immutable for the life of the process.
- `process env` wins over `server.env` on conflicts.
- Optional integration secrets are vault-only in the **server process**. Do not add optional server integrations to `ServerSecrets`, and do not add bespoke env fallback paths to it.
- Not every credential is a `ServerSecrets` or vault lookup. A third mechanism exists: **settings-declared credentials** in `InterpString` fields, resolved at consumption time from `{{ secrets.NAME }}`. See [Settings-declared credentials](#settings-declared-credentials).
- `fabro server start` never generates secrets. Missing required secrets are a startup error.
- `std::env::set_var` and `std::env::remove_var` are banned workspace-wide. Tests are not exempt. Enforced by clippy via `disallowed_methods` in `clippy.toml`; intentional exceptions must be annotated with a scoped `#[expect(clippy::disallowed_methods, reason = "...")]` at the call site.

## Bootstrap Server Secrets

These values may be read via `state.server_secret(...)` because the server can need them before optional integrations are available:

| Secret | Used by |
|---|---|
| `SESSION_SECRET` | Cookie encryption and JWT signing derivation |
| `FABRO_DEV_TOKEN` | Dev-token user auth when `server.auth.methods` includes `dev-token` |
| `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` / `AWS_SESSION_TOKEN` | Static S3 object-store credentials for server storage builders |

These optional integration secrets are **not** server bootstrap secrets. They are provisioned into
the vault:

- LLM provider API keys and OAuth credential records
- `GITHUB_TOKEN`
- `GITHUB_APP_PRIVATE_KEY`
- `GITHUB_APP_CLIENT_SECRET`
- `GITHUB_APP_WEBHOOK_SECRET`
- `FABRO_SLACK_APP_TOKEN`
- `FABRO_SLACK_BOT_TOKEN`
- `DAYTONA_API_KEY`
- `BRAVE_SEARCH_API_KEY`
- `VENICE_API_KEY`

`FABRO_JWT_PRIVATE_KEY` and `FABRO_JWT_PUBLIC_KEY` are removed. `SESSION_SECRET` is the single auth root.

Provisioning into the vault is not the same as the resolver being vault-only. `VaultCredentialSource`
(`lib/foundation/fabro-auth/src/vault_source.rs`) reads each secret name lithos-llm asks for from
the process environment first and the vault second, under the same conventional names. Which paths
that environment lookup is live on is a per-process question:

- **Server process** — inert. `lib/apps/fabro-server/src/server.rs:2453` builds
  `SqlVaultCredentialSource::vault_only(...)`, so the env lookup always returns `None`.
- **Worker process** — wired but effectively inert for provider keys. `build_llm_source`
  (`lib/components/fabro-workflow/src/pipeline/initialize.rs:275`) builds the run's credential
  source with `VaultCredentialSource::new`, which carries the process-env fallback; the
  provider-listing path does the same via `with_env_lookup(process_env_var)`
  (`operations/start.rs:529`). But the worker's env was cleared and repopulated from
  `WORKER_ENV_ALLOWLIST` (`lib/apps/fabro-server/src/spawn_env.rs:6`), which does not include
  provider API keys. Exporting a provider key in the server's shell therefore has no effect on runs.
- **`fabro exec` and direct `fabro-llm` SDK usage** — live. These have no vault and read process
  env deliberately.

## Which process resolves what

Both the server and per-run workers always exist. Name the resolving process and the timing rather
than saying "server runtime", which is ambiguous.

| Value | Resolved by | When |
|---|---|---|
| Bootstrap server secret | Server process, via `ServerSecrets` | Once at construction, then immutable |
| Optional integration secret | Server process or worker, via the vault | At use |
| `{{ vars.NAME }}` | Server process | When the run is created, from that run's variable snapshot |
| `{{ secrets.NAME }}` | The process that owns the value, against the server vault | At consumption time |

`docs/public/agents/mcp.mdx` documents the same split for MCP server configuration and is a good
worked example of the shape.

## Settings-declared credentials

Some credentials are declared in settings rather than looked up by name. Those fields are
`InterpString` (`lib/foundation/fabro-types/src/settings/interp.rs`), which supports narrow
`{{ namespace.NAME }}` tokens with no template logic. Two namespaces resolve: `secrets` (vault,
consumption time) and `vars` (non-sensitive run variables, substituted early at run creation).
`{{ env.NAME }}` tokens still parse but never resolve; they fail loudly with a migration message. A
token whose namespace is unavailable in the resolution context also fails loudly.

The reference implementation is LLM provider `default_headers`, whose `{{ secrets.* }}` values are
resolved against the vault in `lib/foundation/fabro-auth/src/vault_source.rs`
(`interpolated_headers`) and re-sent as credential headers:

```toml
[llm.providers.example.default_headers]
authorization = "Bearer {{ secrets.EXAMPLE_TOKEN }}"
```

Use this mechanism when the credential belongs to an operator-configured integration declared in
`settings.toml`, rather than being a fixed secret name the code looks up. It is not a `ServerSecrets`
field and is not covered by the bootstrap rules above.

When such a value is passed to a subprocess, treat it like any other authority-bearing value: see
[Subprocess Boundaries](#subprocess-boundaries).

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

There is no startup-time secret generation or import of optional integration secrets from process env or `server.env`. The compatibility migrations for those sources and pre-token/OAuth vault entries have been removed. The separate one-time import of current-format `secrets.json` entries into SQLite remains supported.

## Subprocess Boundaries

- Worker and render-graph subprocesses start from `env_clear()` and re-add only explicit allowlisted variables.
- Authority-bearing values are re-injected intentionally. For worker subprocesses this is `FABRO_WORKER_TOKEN`, plus any explicitly required internal value such as a vault-derived `GITHUB_APP_PRIVATE_KEY`; it is not user auth state such as `FABRO_DEV_TOKEN` or `auth.json`.
- The worker reads `FABRO_WORKER_TOKEN` from its env at startup (in `main()` before Tokio initializes) and immediately calls `std::env::remove_var` to scrub it. The token then flows through function arguments to `runner::execute`. Every descendant process (hooks, sandbox commands, MCP stdio, etc.) therefore inherits a worker env that no longer contains the bearer, so an unscrubbed spawn site cannot leak it.
- Worker tokens name the issuing server instance in their JWT audience. A new server instance rejects tokens from the previous instance before handling any request, even when `SESSION_SECRET` is unchanged. Startup recovery replaces in-flight workers; an old worker that survives the restart must not publish platform lifecycle records or reacquire a released Petri lease. This audience is a random, non-secret instance identifier, not a new bootstrap secret. User credentials keep their existing lifetime.
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

First pick the mechanism. These are the only three:

| Kind | Provisioned via | Read via |
|---|---|---|
| Bootstrap server secret | Platform env or install-written `server.env` | `state.server_secret(...)` |
| Optional integration secret | Vault (`fabro secret set`, `fabro install`) | `state.vault_secret(...)` |
| Settings-declared credential | `{{ secrets.* }}` in an `InterpString` settings field | Resolved at consumption time by the owning process |

Then:

1. For the first two kinds, classify it in `fabro-static` as `Bootstrap` or `OptionalVault`. Settings-declared credentials are not classified there — they have no fixed secret name.
2. For bootstrap secrets, provision through platform env or install-written `server.env`, then read through `state.server_secret(...)`.
3. For optional integration secrets, provision through the vault and read through `state.vault_secret(...)`.
4. For settings-declared credentials, follow [Settings-declared credentials](#settings-declared-credentials) and model the field on provider `extra_headers`.
5. Decide explicitly whether startup should fail when it is absent.
6. If a worker or render subprocess needs it, re-inject it explicitly rather than broadening inheritance casually. If the injected value is a credential, scrub it at worker startup the way `FABRO_WORKER_TOKEN` is scrubbed, so descendants do not inherit it.
