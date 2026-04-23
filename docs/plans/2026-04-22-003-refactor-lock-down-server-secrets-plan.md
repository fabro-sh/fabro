---
title: "refactor: Lock down server-level secrets handling"
type: refactor
status: active
date: 2026-04-22
deepened: 2026-04-23
---

# Lock down server-level secrets handling

## Overview

Server-level secrets (SESSION_SECRET, FABRO_DEV_TOKEN, GitHub App credentials, JWT keypair) flow through three independent paths today:

- `ServerSecrets::get` reads process env *live* on every call, with on-disk envfile fallback.
- `load_or_create_local_session_secret` reads env first, then envfile, then auto-generates if missing.
- `execute_foreground` mutates parent process env via `std::env::set_var` so the in-process server's live env reads see the resolved value.

Worker subprocess scrubs `FABRO_DEV_TOKEN`; daemon spawn does not — accidental inconsistency.

This refactor:

- **Makes `ServerSecrets` snapshot-based.** Reads env and envfile *once* at construction, exposes the merged result. Both env and file remain valid sources (env wins on conflict — 12-factor convention). The bug was *live* env reads coupled with parent-process mutation, not env reads themselves.
- **Eliminates parent-process env mutation.** With snapshot semantics, the foreground `set_var` becomes unnecessary; the daemon `cmd.env(SESSION_SECRET)` becomes redundant.
- **CLI install and web install share a common orchestration path for `server.env` writes.** Coordinated install flows may update server-level secrets while a server is running, but they must also own restart/handoff (web install already does this via `/install/finish`).
- **Worker and render-graph subprocesses use `env_clear` + strict fail-closed allowlists.** Worker is the trust boundary — it dispatches user-supplied workflow stages via `Sandbox`. Daemon child inherits parent env unchanged (12-factor pattern works).
- **Foreground and daemon startup share one validation path.** Daemon preflight builds the same `ServerSecrets` snapshot and runs the same server-side auth/startup validation foreground does — no separate parent-CLI validator with drift risk.
- **Legacy `FABRO_JWT_PRIVATE_KEY` / `FABRO_JWT_PUBLIC_KEY` drift is removed.** SESSION_SECRET remains the sole auth master (HKDF source for cookie key + JWT signing key per `2026-04-19-003-feat-cli-auth-login-plan.md`); install stops generating those keys and actively removes them from `server.env` on subsequent runs.
- **`clippy::disallowed_methods` denies `std::env::set_var` / `remove_var`** workspace-wide *including tests*; narrow documented post-fork/pre-exec exceptions only.

## Problem Frame

- **Live env reads + parent-process mutation is unsound.** `std::env::set_var` in `execute_foreground` mutates global process state inside an async runtime. Rust 2024 marks this `unsafe` because it races concurrent readers. Workspace is on 2021; moving to 2024 forces the issue. The set_var is load-bearing because `ServerSecrets::get` reads env *live*, not at construction.
- **Worker subprocess env leakage.** Workers run user-supplied workflow stages (via `Sandbox`). Today they inherit the operator's full process env — any credential the operator exported leaks to user-controlled commands.
- **`SESSION_SECRET` autogeneration is the real startup bypass.** `load_or_create_local_session_secret` mints a value and writes the envfile if missing — a server can boot with a freshly-minted secret that bypassed the install flow's full setup. Same precedent already removed for `FABRO_DEV_TOKEN` (commit `7cb6c65d5`); SESSION_SECRET is the leftover.
- **`FABRO_JWT_*` is stale config drift.** The CLI auth migration (`2026-04-19-003-feat-cli-auth-login-plan.md`) consolidated cookie + JWT signing into a single HKDF chain rooted at SESSION_SECRET. The legacy `FABRO_JWT_PRIVATE_KEY` / `FABRO_JWT_PUBLIC_KEY` envfile entries no longer participate in the runtime auth model but install still generates them and diagnostics still inspects them — they obscure the actual auth model and risk operator confusion.
- **CLI install and web install have drifted into separate persistence/restart behaviors.** They write to `server.env` via parallel code paths today; the bug-surface of "what does install actually persist" is twice as large as it should be. They should reconverge on a shared orchestration with consistent contracts.

## Requirements Trace

- R1. `ServerSecrets` reads env and envfile *once* at boot and exposes the merged snapshot. No live env reads from `ServerSecrets::get` or its callers on the resolution path. Env wins on conflict.
- R2. `fabro server start` does not generate any server-level secret. Missing → fail fast pointing at the install flows (CLI or web) or direct env-set.
- R3. `execute_foreground` does not mutate parent process env.
- R4. `execute_daemon` does not pass server-level secrets via `cmd.env(...)` (no longer needed; daemon child reads env+file at its own boot).
- R5. Worker and render-graph subprocesses use `env_clear` + strict fail-closed allowlists. New inherited env vars require demonstrated need plus tests. Authority-bearing values re-injected explicitly.
- R6. Tests use construction-time env stubs (via a small `EnvSource` trait) or explicit child-process `Command::env`. No test mutates process-wide env.
- R7. Foreground and daemon startup use one shared validation path; required secrets mirror current auth rules (SESSION_SECRET always; FABRO_DEV_TOKEN when dev-token auth enabled; GITHUB_APP_CLIENT_SECRET when GitHub auth enabled).
- R8. CLI install and web install share orchestration for coordinated `server.env` writes and restart/handoff.
- R9. `FABRO_JWT_PRIVATE_KEY` / `FABRO_JWT_PUBLIC_KEY` are removed from install output, diagnostics, docs, and existing `server.env` files during install flows.
- R10. Behavior changes are documented in a strategy doc and enforced via workspace-wide `clippy::disallowed_methods` — including tests — with narrow documented exceptions only.

## Scope Boundaries

**In scope, active** — server-level secrets read via `state.server_secret(...)`, sourced from process env or `<storage>/server.env`:

| Secret | Consumer | Sources |
|---|---|---|
| `SESSION_SECRET` | `AppState::session_key` (`server.rs:713`); HKDF source for cookie key + JWT signing key | env (12-factor) or `server.env` (install) |
| `FABRO_DEV_TOKEN` | `worker_command` (`server.rs:3825`) | env or `server.env` |
| `GITHUB_APP_PRIVATE_KEY` | `AppState::github_credentials` (`server.rs:726`) | env or `server.env` |
| `GITHUB_APP_WEBHOOK_SECRET` | `github_webhook_routes` (`server.rs:983`) | env or `server.env` |
| `GITHUB_APP_CLIENT_SECRET` | `web_auth.rs:536` | env or `server.env` |

Both install flows (CLI `fabro install` and web install via `/install/finish`) write the envfile; neither mutates process env. Operators on 12-factor PaaS set secrets in platform env; operators on local/server installs use one of the install flows.

**In scope, removal targets:**

- `FABRO_JWT_PRIVATE_KEY`, `FABRO_JWT_PUBLIC_KEY` — legacy entries that no longer participate in the runtime auth model. Install stops generating them; diagnostics stops inspecting them; existing entries are actively removed from `server.env` on subsequent install flows.

**Startup-critical subset:** SESSION_SECRET (always), FABRO_DEV_TOKEN (when dev-token auth is enabled), and GITHUB_APP_CLIENT_SECRET (when GitHub auth is enabled). The other in-scope active secrets (GITHUB_APP_PRIVATE_KEY, GITHUB_APP_WEBHOOK_SECRET) are not boot blockers — they continue to surface where they did before (conditional route mounts, per-request errors). The plan does not promote every server secret to a startup gate.

**Out of scope:**

- **Vault + `ProviderCredentials`** (`secrets.json`, REST-managed, runtime-mutable). Different lifecycle.
- **`vault_or_env` for run-level credentials** (`GITHUB_TOKEN`, `DAYTONA_API_KEY`). Same env-as-source pattern, different track.
- **`{{ env.FOO }}` config interpolation.** Operator-supplied templating, different feature.
- **Workflow-stage env (Sandbox).** Stages run inside `Sandbox` (local/Docker/Daytona); their env is configured by the workflow definition + Sandbox config. Anything a workflow stage needs to execute (e.g. `git push` requiring `GITHUB_TOKEN`) routes via Vault → Sandbox, not subprocess inheritance.
- **`validate_api_key` `set_var` in `provider_auth.rs`.** Vault-side smell, deferred.
- **Tailscale and `bun --watch-web` spawns.** Different surfaces.

## Context & Research

### Relevant code

- `lib/crates/fabro-server/src/server_secrets.rs` — `ServerSecrets` definition; today's live `env_lookup` closure is the bug.
- `lib/crates/fabro-server/src/server.rs:697-699` — `state.server_secret(name)` wrapper.
- `lib/crates/fabro-server/src/server.rs:703,712-715` and `jwt_auth.rs:84-99` — `resolve_auth_mode_with_lookup`, currently routed through the live `env_lookup`.
- `lib/crates/fabro-server/src/server.rs:3776-3834` (`worker_command`) — worker spawn site.
- `lib/crates/fabro-server/src/server.rs:7232-7235` — `__render-graph` spawn site.
- `lib/crates/fabro-cli/src/commands/server/start.rs:229-274` — `load_or_create_local_session_secret` and `execute_foreground` `set_var` block.
- `lib/crates/fabro-cli/src/commands/server/start.rs:319-362` — `execute_daemon` spawn flow.
- `lib/crates/fabro-cli/src/commands/install.rs:1816,1856,1864` — install SESSION_SECRET generation and persistence.
- `lib/crates/fabro-config/src/envfile.rs:204-256` — `write_env_entries`, already atomic via tmp + fsync + rename.
- `Cargo.toml:111` and `clippy.toml` — existing `disallowed_methods` mechanism.

### Institutional learnings

- `docs/plans/2026-04-05-server-canonical-secrets-doctor-repo-plan.md` — architectural anchor: server is canonical, install provisions, request-time reads from store.
- `docs/plans/2026-04-19-003-feat-cli-auth-login-plan.md` — `SESSION_SECRET` is now HKDF master for JWT signing + cookie key. Higher stakes.
- `docs/plans/2026-04-18-001-feat-webhook-strategy-plan.md` — webhook secret already plumbed via `AppState` from the resolved store.
- Commit `7cb6c65d5` — "Remove dev-token minting from server start." SESSION_SECRET autogenerate is the leftover.

## Key Technical Decisions

- **`ServerSecrets` is a snapshot, not a live reader.** A small `EnvSource` trait exists *only at construction time* and immediately materializes into an owned `HashMap<String, String>`. No trait object or callback survives into runtime lookup. `get(name)` returns from the merged (env ∪ file) snapshot; env wins on conflict (12-factor). Role: the *resolved-secrets API*; the source mix is the operator's choice.
- **Production uses a real process-env `EnvSource`; tests use a map-backed stub.** No live env reads anywhere on the secret resolution path. No `EnvLookup` closure surviving into runtime.
- **`resolve_auth_mode_with_lookup` migrates to read from `server_secrets`, not raw env.** Closure passed in delegates to `self.server_secrets.get(...)`. Without this, auth-mode resolution remains a live env reader.
- **Worker and render-graph allowlists are strict fail-closed.** Worker is the trust boundary (dispatches user stages via `Sandbox`); render-graph is hygiene. New inherited env vars require demonstrated need (failing test) and intentional addition. Proxy/cert env vars are not auto-inherited.
  - Worker list: `PATH`, `HOME`, `TMPDIR`, `USER`, `RUST_LOG`, `RUST_BACKTRACE`, `FABRO_HOME`, `FABRO_STORAGE_ROOT`. Plus explicit `FABRO_DEV_TOKEN` re-injection when dev-token auth is enabled.
  - Render-graph list: `PATH`, `HOME`, `TMPDIR`. Plus explicit `FABRO_TELEMETRY=off`.
- **Daemon child spawn inherits parent env unchanged** (modulo existing `cmd.env_remove("FABRO_JSON")` for output-format hygiene). The daemon is fabro's own server code, not a security boundary against itself. 12-factor env vars (SESSION_SECRET, AWS_*, RAILWAY_*) flow through naturally to the daemon child's snapshot.
- **Daemon preflight reuses the same shared auth/startup validation as foreground startup.** Both modes construct the same `ServerSecrets` snapshot and call the same server-side validation logic (today's `jwt_auth::resolve_auth_mode_with_lookup` already encodes "what's required for this auth mode" — that's the shared path). No separate parent-CLI validator with drift risk. Required secrets remain exactly the current startup-critical set: SESSION_SECRET (always), FABRO_DEV_TOKEN (when dev-token auth enabled), GITHUB_APP_CLIENT_SECRET (when GitHub auth enabled). GITHUB_APP_PRIVATE_KEY and GITHUB_APP_WEBHOOK_SECRET remain in-scope server secrets but are not new universal boot blockers.
- **Install coordination policy lives in shared install orchestration, not in low-level envfile helpers.** Coordinated install flows (CLI and web) may write to `server.env` while a server is running — but only when they also own restart/handoff. Web install already does this via `/install/finish`. CLI install joins the same orchestration. Manual edits to `server.env` outside the orchestration still require operator restart discipline.
- **`pub(crate) server_secrets` field tightened to `pub(super)`.** No production code legitimately bypasses the wrapper.
- **Workspace-wide clippy ban including tests.** Add `std::env::set_var`/`remove_var` to `clippy.toml` `disallowed-methods` — applies to production AND test code. Tests must use construction-time `EnvSource` stubs or explicit child-process `Command::env`. Narrow documented exceptions only (`fabro-telemetry/src/spawn.rs:73-76` post-fork pre-execvp).
- **Legacy `FABRO_JWT_*` is removed, not preserved.** Install stops generating; diagnostics stops reading; existing envfile entries are actively cleaned up on subsequent install flows. SESSION_SECRET is the sole auth master.

## Open Questions

### Resolved during planning

- **`ServerSecrets` reads env AND file, snapshots, env wins.** Process env is a legitimate source (12-factor). The bug was *live* env reads + parent mutation, not env-as-source.
- **`EnvSource` exists only at construction time.** A small trait used to materialize an owned `HashMap` once; no callback or trait object survives into runtime lookup. Production uses a real process-env source; tests use a map-backed stub.
- **Daemon child inherits parent env; only worker/render-graph scrub.** Daemon is fabro's own server code and must see whatever the operator/platform set in env. Worker is the trust boundary (dispatches user stages via `Sandbox`).
- **No carve-out for AWS / cloud-platform credentials.** Daemon inherits parent env, so AWS env names reach the object-store layer naturally.
- **Active scope is 5 server secrets plus 2 legacy removal targets.** SESSION_SECRET, FABRO_DEV_TOKEN, GITHUB_APP_PRIVATE_KEY, GITHUB_APP_WEBHOOK_SECRET, GITHUB_APP_CLIENT_SECRET are active. FABRO_JWT_PRIVATE_KEY and FABRO_JWT_PUBLIC_KEY are legacy — install stops generating them; diagnostics stops reading them; existing entries are actively removed.
- **Workflow stage env is the Sandbox's job.** This plan does not address workflow `bash`/`git`/etc. stage env — that's per-workflow Sandbox config. Workflows requiring credentials route them through Vault.
- **Web install keeps its existing restart-handoff contract.** `/install/finish` still returns the restart URL and the SPA still waits for the server to come back. No new `restart_required` flag, no manual-restart message proposal, no API surface change.
- **CLI and web install share orchestration for coordinated writes while running.** Blanket "refuse-while-running" is out. The orchestration is responsible for restart/handoff; both flows call into it.
- **Test migration shape:** full migration in Unit 1. `ServerSecrets::with_env_lookup` is deleted; tests use either `ServerSecrets::load(path, env_source)` with a map-backed `EnvSource` stub, or `provision_server_secrets(env_path, &[(name,value)])` helper (file-based injection). No `#[cfg(test)]` backdoor.
- **Visibility tightening:** `pub(crate) server_secrets` at `server.rs:579` → `pub(super)` as part of Unit 1.
- **Install lock window for interactive flows:** install orchestration's job; `gather inputs → persist + restart-handoff`. Serialization window is sub-second regardless of OAuth duration.
- **Dev-loop friction:** no `--quick` flag. Contributors run `fabro install` or set secrets directly via env (`SESSION_SECRET=$(openssl rand -hex 32) cargo run -- server start` works under the env-as-source model).
- **Compliance-driven rotation:** deferred. Captured as a known limitation in the strategy doc.

## Implementation Units

- [ ] **Unit 1: `ServerSecrets` becomes a snapshot built from `EnvSource` (construction-time only)**

**Goal:** `ServerSecrets` reads env and envfile *once* at construction via a small `EnvSource` trait, materializes both into owned `HashMap`s, and exposes the merged snapshot. No trait object or closure survives into runtime lookup. Auth-mode resolution stops being a live env reader.

**Requirements:** R1, R6

**Dependencies:** None.

**Files:**
- Modify: `lib/crates/fabro-server/src/server_secrets.rs` — replace `env_lookup` closure field with `env_entries: HashMap<String, String>` (owned). Introduce a small `EnvSource` trait used *only* at construction:
  - `pub trait EnvSource { fn snapshot(&self) -> HashMap<String, String>; }`
  - `pub struct ProcessEnv;` impls `snapshot` via `std::env::vars().collect()`.
  - `pub struct StubEnv(pub HashMap<String, String>);` impls `snapshot` by cloning. (cfg-test or cfg(any(test, feature = "test-support")) — implementer picks based on existing patterns.)
  - `pub fn load(path: PathBuf, env: &dyn EnvSource) -> Result<Self, Error>` — calls `env.snapshot()` once, stores the result, never references `env` again.
  - `get(name)` returns `self.env_entries.get(name).cloned().or_else(|| self.file_entries.get(name).cloned())`.
- Modify: `lib/crates/fabro-server/src/server.rs` — `build_app_state` accepts pre-built `ServerSecrets` and `AuthMode` from the caller (no longer constructs `ServerSecrets` itself). The single production caller is `serve_command` (via `resolve_startup` from Unit 4); test helpers construct both in the same way. Tighten `pub(crate) server_secrets` field at `server.rs:579` to `pub(super)`. Migrate the `resolve_auth_mode_with_lookup` call (`server.rs:703`) to pass a closure delegating to `self.server_secrets.get(...)`.
- Modify: `lib/crates/fabro-server/src/serve.rs:512` and `install.rs:910,965,2108` — minimal API migration: `ServerSecrets::load(path, &ProcessEnv)` instead of `with_env_lookup` at any *non-startup* construction sites (e.g. install-time inspection, install_object_store_lookup test helper). The startup construction sites at `serve.rs:594-607` are NOT modified by Unit 1 — Unit 4 collapses them into a single `resolve_startup(...)` call. To keep the workspace compiling between Unit 1 and Unit 4, Unit 1 may leave a temporary `ServerSecrets::load(path, &ProcessEnv)` call at `serve.rs:594` if needed; Unit 4 deletes it. Sequencing is documented in Unit 4's Dependencies.
- Modify: `lib/crates/fabro-server/src/server.rs:7864-7885` — replace `server_secrets_resolve_process_env_before_server_env` with a snapshot test using `StubEnv` (assert env-wins-on-conflict from the snapshot).
- Modify: `lib/crates/fabro-server/tests/it/api/cli_auth_token.rs:34`, `routing.rs:21,35-36`, `tcp.rs:28,68,173-174`, `lib/crates/fabro-cli/tests/it/support/auth_harness.rs:39-86` — migrate from `env_lookup` injection (for *secret* values) to constructing the test's `ServerSecrets` with `&StubEnv(...)`, or writing to a temp `server.env` via the `provision_server_secrets(env_path, &[(name, value)])` helper. Tests that currently pass `env_lookup` only to feed secret values into `ServerSecrets` switch to the new mechanism.
- Modify: test helpers like `create_app_state_with_env_lookup` (`server.rs:2488` and similar) — once `build_app_state` (`server.rs:2631`) requires precomputed `ServerSecrets` and `AuthMode`, the helpers must produce both internally and pass them through. The `env_lookup` parameter on these helpers is *retained* (still consumed downstream by `resolve_canonical_origin` at `server.rs:708` and slack `{{ env.FOO }}` interpolation at `server.rs:2676`, both out of scope). New shape: helper accepts an additional `&dyn EnvSource` parameter (or constructs a `StubEnv`/`ProcessEnv` based on the call site's intent), internally calls `resolve_startup(...)` (or directly constructs `ServerSecrets` + computes `AuthMode` if the helper bypasses settings — auditor's choice per call site), and passes the resulting `ServerSecrets` and `AuthMode` into `build_app_state`. Tests that set up specific auth modes (e.g. dev-token enabled) now thread settings + `EnvSource` through the helper rather than relying on a single closure to drive both secret resolution and auth-mode computation.
- Test: existing files plus new snapshot-semantics tests in `server_secrets.rs`.

**Approach:**
- `EnvSource::snapshot()` is called exactly once during `ServerSecrets::load`. The trait reference is dropped immediately after; only the materialized `HashMap` is retained. No risk of live env reads via trait object dispatch.
- Object-store credential resolution at `serve.rs:340-401, 520-522, 540-542` uses `std::env::var(...)` directly (daemon child inherits parent env, so AWS names are present).

**Test scenarios:**
- Happy path: `ServerSecrets::load(path, &StubEnv([("SESSION_SECRET", "from-env")].into()))` returns "from-env" even when file has a different value.
- Edge: env empty, file has the value → file wins (single fallback path).
- Edge: neither env nor file has the value → `get` returns `None`.
- Edge: env has it, file does too, different values → env wins (12-factor).
- Edge: missing file path → empty `file_entries`, env-only resolution.
- Snapshot semantics: after construction, mutating the source `EnvSource` (or process env, if production source) does not affect `get` returns. The snapshot is owned and immutable.
- Auth-mode happy path: `resolve_auth_mode_with_lookup` resolves correctly when secrets come from env, file, or both.
- Migration: `cli_auth_token`, `routing`, `tcp`, `auth_harness` tests pass after migration to `EnvSource` stubs.

**Verification:**
- `cargo nextest run -p fabro-server -p fabro-cli` passes.
- `grep -rn 'std::env::var\|std::env::vars' lib/crates/fabro-server/src/ | grep -v 'serve.rs\|object_store\|spawn_env\|server_secrets.rs'` shows no live env reads on the server-secret resolution path (the only `std::env::vars()` is inside `ProcessEnv::snapshot`, called once at construction).

---

- [ ] **Unit 2: Drop foreground `set_var`; drop daemon `cmd.env(SESSION_SECRET)`**

**Goal:** Eliminate parent-process env mutation. Daemon parent stops passing SESSION_SECRET via `cmd.env`; daemon child reads env+file at its own boot.

**Requirements:** R3, R4

**Dependencies:** Unit 1.

**Files:**
- Modify: `lib/crates/fabro-cli/src/commands/server/start.rs:265-274` — delete the `set_var`/scopeguard block in `execute_foreground`.
- Modify: `lib/crates/fabro-cli/src/commands/server/start.rs:350,352` — `execute_daemon` removes `cmd.env("SESSION_SECRET", ...)`. Keep existing `cmd.env_remove("FABRO_JSON")` (output-format hygiene).
- Modify: `lib/crates/fabro-test/src/lib.rs:931` — drop `cmd.env("SESSION_SECRET", ...)` for spawned test servers; tests provision via `server.env` or by setting env on the spawned `Command` explicitly when the test simulates a 12-factor PaaS scenario.
- Test: existing `cmd/server_start.rs` tests cover both modes.

**Approach:**
- Unit 1's snapshot semantics make both `set_var` and `cmd.env(SESSION_SECRET)` unnecessary. The in-process foreground server's `ServerSecrets::load(path, &ProcessEnv)` snapshots whatever env the parent CLI had. The daemon child inherits parent env (today's behavior — unchanged) and snapshots it at its own boot.
- A `SESSION_SECRET` exported in the operator's parent shell does flow through to both modes — that's the 12-factor design intent.

**Test scenarios:**

Tests proving env behavior must use construction-time `EnvSource` stubs or explicit child-process `Command::env` — *not* `std::env::set_var`. Any old test using process-env mutation is migrated as part of this unit (or Unit 7's clippy enforcement will fail it).

- Happy path (foreground, env source): in-process server constructed with `&StubEnv([("SESSION_SECRET", "from-env")].into())`; running server uses that value.
- Happy path (foreground, file source): empty `EnvSource`, value in `server.env`; running server uses the file value.
- Happy path (foreground, env-wins): env and file have different values; env wins.
- Happy path (daemon, env source): test passes `SESSION_SECRET` to spawned daemon via explicit `Command::env`; daemon child inherits and snapshots it. NOT via `std::env::set_var` in the test process.
- Happy path (daemon, file source): no env on the spawned `Command`, value in `server.env`; daemon child uses the file value.

**Verification:**
- `grep -rn 'std::env::set_var\|remove_var' lib/crates/fabro-cli/src/` returns no hits in production code.
- `grep -rn 'cmd\.env."SESSION_SECRET"' lib/crates/fabro-cli/src/` returns no hits in production code (test code may still use `Command::env` for daemon spawn simulation; that's fine — it's setting child env, not parent env).

---

- [ ] **Unit 3: Worker and render-graph spawns use `env_clear` + strict fail-closed allowlists**

**Goal:** Worker and render-graph subprocesses inherit only an explicit allowlist. The lists are strict fail-closed — new env vars require demonstrated need (a failing test that proves a workflow execution path needs them) plus intentional addition. Authority-bearing values re-injected explicitly. Daemon child spawn is unchanged (inherits parent env per 12-factor).

Proxy and TLS env vars (`HTTPS_PROXY`, `SSL_CERT_FILE`, etc.) are *not* inherited unless usage-proven by a failing test and intentionally added — they were never part of the worker's documented contract.

**Requirements:** R5

**Dependencies:** None.

**Files:**
- Create: `lib/crates/fabro-server/src/spawn_env.rs` — defines two helpers:
  - `apply_worker_env(&mut tokio::process::Command)` — `env_clear` + worker list.
  - `apply_render_graph_env(&mut tokio::process::Command)` — `env_clear` + render-graph list.
- Modify: `lib/crates/fabro-server/src/server.rs:3776-3834` — `worker_command` calls `apply_worker_env(&mut cmd)` first, then existing `cmd.env("FABRO_DEV_TOKEN", token)` re-injection. Remove existing `env_remove("FABRO_JSON")` and `env_remove("FABRO_DEV_TOKEN")` (covered by `env_clear`).
- Modify: `lib/crates/fabro-server/src/server.rs:7232-7235` — `__render-graph` spawn calls `apply_render_graph_env(&mut cmd)`. Keep explicit `cmd.env("FABRO_TELEMETRY", "off")`.
- Test: new tests in `spawn_env.rs`.

**Approach:**

```text
WORKER list (each entry has // reason: ... in the source):
  PATH, HOME, TMPDIR, USER       // process essentials
  RUST_LOG, RUST_BACKTRACE       // diagnostics
  FABRO_HOME, FABRO_STORAGE_ROOT // worker reads its own state
+ explicit cmd.env("FABRO_DEV_TOKEN", token) when dev-token auth enabled

RENDER_GRAPH list:
  PATH, HOME, TMPDIR
+ explicit cmd.env("FABRO_TELEMETRY", "off")

DAEMON spawn: no helper. Inherits parent env. Keeps existing cmd.env_remove("FABRO_JSON").
```

The lists are constants in `spawn_env.rs`. Each entry is one named env var with a one-line `//` comment. A worker that needs a new env var must be amended in source — that's the entire mechanism.

**Test scenarios:**
- Happy path (worker): with allowlisted names in parent env, all reach the worker. Random names (`MY_API_KEY`, `NEW_RELIC_LICENSE_KEY`, `DATABASE_URL`, `SESSION_SECRET=leak`) do not.
- Happy path (worker, dev-token enabled): `FABRO_DEV_TOKEN` is set to the install-provisioned value via explicit re-injection.
- Negative (worker): parent's `FABRO_DEV_TOKEN=garbage` does not reach the worker; explicit re-injection sets the correct value.
- Happy path (render-graph): `PATH` and `HOME` reach the child; arbitrary parent vars do not.
- Integration (render-graph): with `FABRO_TELEMETRY=on` in parent env, child sees `off` (explicit override after `env_clear`).
- Integration: real worker subprocess executes a workflow end-to-end with leak probes (`MY_API_TOKEN=leak`, `NEW_RELIC_LICENSE_KEY=leak`) exported in parent env. Workflow completes; leak probes do not appear in worker logs or in stage env (Sandbox-configured).

**Verification:**
- `cargo nextest run -p fabro-server` passes.
- A test asserts the worker spawn sees *only* names in the allowlist plus the explicit `FABRO_DEV_TOKEN` — i.e. "no ambient inheritance except allowlisted names." Same for render-graph. The check operates by enumerating the child's actual env, not by greping `env_remove` calls.
- `grep -rn 'env_remove' lib/crates/fabro-server/src/` returns no hits for the worker (`server.rs:3776-3834`) or render-graph (`server.rs:7232-7235`) spawn sites — both routes are now via `env_clear` + the helpers.
- `grep -rn 'env_remove' lib/crates/fabro-cli/src/commands/server/start.rs` returns the single intentional `cmd.env_remove("FABRO_JSON")` on the daemon-spawn path (output-format hygiene; daemon child unchanged per Unit 2). No other `env_remove` calls in CLI server code.

---

- [ ] **Unit 4: Shared startup validation via snapshot-backed `ServerSecrets`**

**Goal:** Server start no longer auto-generates. Daemon preflight constructs the same `ServerSecrets` snapshot foreground startup uses and runs the *same* server-side auth/startup validation logic. No separate parent-CLI validator that could drift from the server-side rules.

**Requirements:** R1, R2, R7

**Dependencies:** Units 1-3.

**Files:**
- Create: `lib/crates/fabro-server/src/startup.rs` (or extend an existing module) — define the shared validation logic. Two entry points around it: a public preflight wrapper (CLI calls this; never sees `ServerSecrets`), and a crate-internal full-resolution function (`serve_command` calls this; consumes the snapshot directly). Both share a single internal implementation so there is no possibility of drift.

  ```text
  // Crate-internal: returns full state for in-process consumption.
  pub(crate) struct StartupResolution {
      pub(crate) auth_mode: AuthMode,
      pub(crate) server_secrets: ServerSecrets,
  }

  pub(crate) fn resolve_startup(
      env_path: &Path,
      env: &dyn EnvSource,
      settings: &ResolvedServerSettings,
  ) -> Result<StartupResolution, StartupValidationError>

  // Public: thin wrapper for CLI preflight. Calls resolve_startup, drops the
  // StartupResolution after validating, returns just the success/failure.
  pub fn validate_startup(
      env_path: &Path,
      env: &dyn EnvSource,
      settings: &ResolvedServerSettings,
  ) -> Result<(), StartupValidationError>
  ```

  Internally `resolve_startup` constructs `ServerSecrets::load(env_path, env)`, then runs the existing `jwt_auth::resolve_auth_mode_with_lookup` against a closure delegating to that snapshot, then returns both. `validate_startup` is `resolve_startup(...).map(|_| ())` — the literal sharing of code makes drift impossible.

  Function takes `&ResolvedServerSettings` (not just `&ServerAuthSettings`) because validation already depends on `server.web.enabled` and `server.integrations.github.client_id` (`jwt_auth.rs:63`) — narrowing would silently weaken the validation surface.

  `StartupValidationError` *wraps or re-uses the existing error type* returned by `jwt_auth::resolve_auth_mode_with_lookup` plus the secret-loading errors from `ServerSecrets::load`. It must cover the full surface that path rejects today: missing required secret (`SESSION_SECRET`, `FABRO_DEV_TOKEN` when dev-token auth enabled, `GITHUB_APP_CLIENT_SECRET` when GitHub auth enabled), invalid secret value (e.g., malformed `FABRO_DEV_TOKEN`), empty auth methods, GitHub auth configured with `server.web.enabled = false`, missing `server.integrations.github.client_id`. Implementer audits `jwt_auth.rs:67` to enumerate the full variant set; the plan does not invent a narrow new one.

  CLI never sees `ServerSecrets`. `ServerSecrets` and `resolve_startup` stay `pub(crate)`. Only `validate_startup` + `EnvSource` + `ProcessEnv` + `StartupValidationError` cross the crate boundary.
- Modify: `lib/crates/fabro-server/src/lib.rs` — re-export `startup::{validate_startup, EnvSource, ProcessEnv, StartupValidationError}` from the crate root. **Not** `resolve_startup` or `StartupResolution`.
- Modify: `lib/crates/fabro-cli/src/commands/server/start.rs:229-250` — delete `load_or_create_local_session_secret`. Daemon preflight calls `fabro_server::validate_startup(runtime_directory.env_path(), &fabro_server::ProcessEnv, &resolved_settings)`. On `Err`: surface the error to stderr exactly as returned (text comes from the shared error type's `Display`).
- Modify: `lib/crates/fabro-cli/src/commands/server/start.rs:264` — `execute_foreground` does not run a separate validator. The in-process server's `serve_command` calls `resolve_startup` internally and threads the returned `(ServerSecrets, AuthMode)` into `build_app_state` (replacing today's separate `ServerSecrets::with_env_lookup` + `resolve_auth_mode_with_lookup` calls at `serve.rs:594-607`). Single source of truth: foreground's resolution and daemon's preflight share the same internal `resolve_startup`; only the calling surface differs.
- Modify: `lib/crates/fabro-server/src/serve.rs:594-607` — replace today's ad-hoc two-step construction with a single `resolve_startup(...)` call. The returned `ServerSecrets` and `AuthMode` are passed into `build_app_state` (per Unit 1's revised signature).
- Modify: `lib/crates/fabro-cli/src/commands/server/start.rs:350` — `execute_daemon` runs `validate_startup` before spawning the child.
- Test: `tests/it/cmd/server_start.rs` adds missing-secret tests for each required secret across both modes, plus tests demonstrating env-source success, file-source success, and each non-missing-key rejection (empty auth methods, web-disabled with GitHub auth, missing client_id, invalid dev token). A unit test in `fabro-server` asserts `validate_startup` and `resolve_startup` return identical accept/reject decisions for the same inputs.

**Approach:**
- Required secrets remain *exactly* the current startup-critical set, encoded once in the shared validation: SESSION_SECRET (always), FABRO_DEV_TOKEN (when dev-token auth is enabled), GITHUB_APP_CLIENT_SECRET (when GitHub auth is enabled). GITHUB_APP_PRIVATE_KEY and GITHUB_APP_WEBHOOK_SECRET remain in-scope server secrets but are NOT new universal boot blockers — they continue to surface where they did before (conditional route mounts, per-request errors).
- Daemon preflight and foreground startup run the same validation function with the same `ServerSecrets` snapshot type; "drift" is impossible by construction.

**Test scenarios:**
- Happy path: required secrets in env → boots (both modes).
- Happy path: required secrets in file → boots (both modes).
- Happy path: env wins when both present.
- Error path (each required secret): missing in both → fail fast naming the secret and both sources (both modes get identical error text because they call the same function).
- Negative regression: post-Unit-1 snapshot is built once; mutating env after boot doesn't change the resolved value.

**Verification:**
- `fabro server start` against an empty env and uninstalled storage exits non-zero with the same error message in both `--foreground` and daemon modes.
- `fabro server start` with secrets in env (12-factor simulation) boots without any install flow having run.
- `grep -rn 'generate_session_secret' lib/crates/fabro-cli/src/commands/server/` returns no hits.
- `grep -rn 'ServerSecrets\|server_secrets::\|resolve_startup\|StartupResolution' lib/crates/fabro-cli/` returns no hits — CLI never sees `ServerSecrets` or the internal resolution. Only `validate_startup` is reachable from outside `fabro-server`.
- The public secret-related surface from `fabro-server`'s crate root is exactly: `validate_startup`, `EnvSource`, `ProcessEnv`, `StartupValidationError`. Nothing else.
- Foreground startup at `serve.rs:594-607` makes exactly one call to compute `(ServerSecrets, AuthMode)` — `resolve_startup(...)` — not a separate `ServerSecrets::with_env_lookup` followed by `resolve_auth_mode_with_lookup`. Both values are passed into `build_app_state`.
- `build_app_state` no longer constructs `ServerSecrets`; it accepts pre-built `ServerSecrets` and `AuthMode` from the caller. Verified by reading the signature.

---

- [ ] **Unit 5: Shared install orchestration for coordinated `server.env` writes and restart handoff**

**Goal:** CLI install and web install share a single orchestration layer for `server.env` persistence. Both flows take the same path through generation, persistence, removal of legacy entries, and restart handoff. Web install keeps its existing `/install/finish` restart-handoff contract; CLI install joins the same orchestration. No blanket refuse-while-running policy.

**Requirements:** R8

**Dependencies:** Pairs naturally with Unit 6 (legacy `FABRO_JWT_*` removal hooks into the same orchestration). Either order works.

**Files:**
- Modify: `lib/crates/fabro-install/src/lib.rs` — establish the shared orchestration entry points. Both CLI install and web install call into these. Orchestration owns:
  - input gathering (no `server.env` write, no serialization)
  - persistence (atomic `server.env` write via existing `envfile::merge_env_file` at `envfile.rs:204-256`)
  - legacy entry removal (Unit 6 hook for `FABRO_JWT_*`)
  - restart/handoff (web install via `/install/finish`'s existing restart URL contract; CLI install determines its own handoff — for the in-process case, exit cleanly; for daemon mode, respect today's `fabro server stop` + restart pattern)
- Modify: `lib/crates/fabro-cli/src/commands/install.rs:1864, 1213` — route `server.env` writes through the shared orchestration in `fabro-install`. CLI-specific UX (prompts, progress display) remains in `commands/install.rs`; persistence does not.
- Modify: `lib/crates/fabro-server/src/install.rs:1313, 1343-1353, 1380` — server-side `/install/finish` handlers route through the same orchestration. The existing `/install/finish` API contract (returns restart URL; SPA polls until server returns) is preserved exactly — no `restart_required` flag, no API surface change.
- Test: parity tests in `lib/crates/fabro-install/tests/` prove CLI install and web install take the same persistence/removal path with the same outputs given the same inputs.

**Approach:**
- The shared orchestration in `fabro-install` is the single chokepoint for `server.env` writes from install flows. Manual edits to `server.env` outside the orchestration still require operator restart discipline (documented).
- Coordinated install flows MAY write while a server is running — they own restart/handoff. Web install already has the handoff via `/install/finish`; CLI install gets the same primitives.
- Two-phase shape: `gather inputs (no serialization)` → `persist + restart-handoff (atomic)`. Serialization window is sub-second regardless of OAuth duration.
- No PID-comparison logic, no refuse-while-running predicate, no new chokepoint helper in `fabro-config`. The earlier `write_server_env_serialized` proposal is dropped.

**Test scenarios:**
- Parity: identical install inputs produce identical `server.env` contents and identical removed entries via CLI and web paths.
- Happy path (CLI): install on stopped server succeeds; `server.env` updated atomically.
- Happy path (web): install on running server completes via `/install/finish`; restart URL returned; SPA reconnects after restart.
- Legacy removal: install (CLI or web) on a `server.env` containing `FABRO_JWT_*` entries leaves the file without them (Unit 6 hook).
- Negative regression: install-then-start works in the common single-shell CLI sequence.

**Verification:**
- `cargo nextest run -p fabro-install -p fabro-cli` passes including parity tests.
- `grep -rn 'envfile::write_env_entries\|envfile::merge_env_file' lib/crates/fabro-cli/src lib/crates/fabro-server/src` shows `server.env` writes routed through `fabro-install` orchestration; no direct calls outside it.
- `/install/finish` API contract unchanged (existing OpenAPI schema and SPA reconnect tests pass without modification).

---

- [ ] **Unit 6: Remove legacy `FABRO_JWT_*` drift**

**Goal:** Stop generating `FABRO_JWT_PRIVATE_KEY` / `FABRO_JWT_PUBLIC_KEY`, stop reading them in diagnostics, remove operator/docs references that describe them as auth inputs, and actively remove existing entries from `server.env` during install flows. SESSION_SECRET is the sole auth master.

**Requirements:** R9

**Dependencies:** None for code shape; the active-removal hook plugs into Unit 5's shared install orchestration.

**Files:**
- Modify: `lib/crates/fabro-cli/src/commands/install.rs:1853-1855` — delete `generate_jwt_keypair()` call and the corresponding `("FABRO_JWT_PRIVATE_KEY", ...)` / `("FABRO_JWT_PUBLIC_KEY", ...)` entries from `generated_server_env_pairs`.
- Modify: `lib/crates/fabro-server/src/install.rs` (whichever lines mirror the CLI side, surfaced in research) — same deletion on the web install side.
- Modify: `lib/crates/fabro-install/src/lib.rs` (Unit 5's shared orchestration) — add `FABRO_JWT_PRIVATE_KEY` and `FABRO_JWT_PUBLIC_KEY` to a `legacy_keys_to_remove` set; the persistence step writes these as removals on every install run.
- Modify: `lib/crates/fabro-server/src/diagnostics.rs:540, 552` — remove the `state.server_secret("FABRO_JWT_PUBLIC_KEY")` and `state.server_secret("FABRO_JWT_PRIVATE_KEY")` checks. Adjust diagnostics output and tests accordingly.
- Modify: **all** operator-facing references to `FABRO_JWT_PRIVATE_KEY` / `FABRO_JWT_PUBLIC_KEY` across `docs/`, `apps/marketing/`, README, and any in-repo runbook. The criterion is "any mention," not "mention as auth input." Concrete known-stale targets:
  - `docs/administration/server-configuration.mdx:297` — remove the "future CLI login flows" reference and any surrounding prose treating these as install/runtime secrets.
  - `docs/administration/server-configuration.mdx:331` — remove the row(s) from the "Server authentication" table.
  - Implementer must also `grep -rn 'FABRO_JWT_PRIVATE_KEY\|FABRO_JWT_PUBLIC_KEY' docs/ apps/ README*` and either delete or rewrite every hit. Surviving mentions should appear only in (a) the new strategy doc's "removed" section or (b) historical plans under `docs/plans/`.
- Modify: install snapshot tests, server.env fixture files, and any insta snapshots that assert on `FABRO_JWT_*` lines — regenerate.
- Test: install run against a `server.env` pre-seeded with `FABRO_JWT_*` entries — assert they are absent after install completes.

**Approach:**
- Deletion is the entire change. No deprecation period; no compatibility shim. The keys haven't participated in runtime auth since the CLI auth login migration (`2026-04-19-003`).
- Operators upgrading run install once; legacy entries are silently cleaned up. The strategy doc records the cleanup for any operator who notices.

**Test scenarios:**
- Happy path: install on a fresh storage dir produces a `server.env` with no `FABRO_JWT_*` entries.
- Happy path (cleanup): install on a `server.env` containing `FABRO_JWT_PRIVATE_KEY=...` and `FABRO_JWT_PUBLIC_KEY=...` produces an updated `server.env` without those keys, with other entries preserved.
- Diagnostics: `fabro doctor` (or whichever command surfaces diagnostics) does not mention `FABRO_JWT_*`.

**Verification:**
- `grep -rn 'FABRO_JWT_PRIVATE_KEY\|FABRO_JWT_PUBLIC_KEY\|generate_jwt_keypair' lib/crates apps/ docs/ README*` returns hits *only* in (a) the new strategy doc's "removed" section, (b) historical plan files under `docs/plans/`. No hits in operator-facing docs (`docs/administration/`, `docs/quickstart/`, marketing) or production crate code.
- `cargo nextest run -p fabro-cli -p fabro-server` passes with regenerated snapshots.
- Mintlify docs build succeeds with the removed entries (no broken anchors/links from other pages that referenced the JWT-key sections).

---

- [ ] **Unit 7: Strategy doc + workspace-wide clippy enforcement**

**Goal:** Document the design and encode the rules as compile-time enforcement applied to *all* code including tests.

**Requirements:** R10

**Dependencies:** Units 1-6.

**Files:**
- Create: `docs-internal/server-secrets-strategy.md`.
- Modify: `clippy.toml` — add `std::env::set_var` and `std::env::remove_var` to `disallowed-methods` with reason text pointing at the strategy doc. The ban is workspace-wide and applies to tests as well as production code.
- Modify: `lib/crates/fabro-telemetry/src/spawn.rs:73-76` — add `#[expect(clippy::disallowed_methods, reason = "post-fork pre-execvp env mutation; safe because grandchild is single-threaded and about to be replaced via exec")]`.
- Modify: any other production caller surfaced during implementation (the count is small per Unit 1's grep) — same `#[expect]` pattern with documented reason.
- Modify: `CLAUDE.md` (and `AGENTS.md` if separate) — add "Strategy docs" entry pointing at the new doc.

**Approach:**

Strategy doc covers:
- **`ServerSecrets` is the resolved-secrets API.** Sources are process env (12-factor) and `<storage>/server.env` (install/local convenience). Snapshotted at boot via `EnvSource` trait that materializes immediately into owned `HashMap`. Env wins on conflict.
- **The five active server-level secrets** and their consumers (table from this plan). `FABRO_JWT_*` is removed (Unit 6); SESSION_SECRET is the sole auth master via HKDF.
- **Provisioning paths:** CLI install, web install (shared orchestration in `fabro-install`), or platform env. Server start does not auto-generate.
- **Tests must not mutate process env.** Enforced workspace-wide by clippy. Tests inject via construction-time `EnvSource` stubs (e.g. `StubEnv`) for in-process resolution, or via explicit child-process `Command::env` for subprocess simulation.
- **Worker and render-graph env:** `env_clear` + strict fail-closed allowlist. Daemon child inherits parent env (12-factor pattern). New worker env entries require demonstrated need + intentional addition.
- **Install while running:** allowed only through the shared install orchestration which owns restart/handoff. Manual `server.env` edits still require restart discipline.
- **Rotation:** restart required. Live rotation intentionally not supported. Compliance-driven N+1 rotation (overlap windows) is a known limitation tracked as follow-up.
- **Out of scope (with reasons):** Vault + `ProviderCredentials`, `vault_or_env` for run-level credentials (different track), `{{ env.FOO }}` config interpolation, workflow-stage env (Sandbox's job), `validate_api_key` `set_var` smell, Tailscale spawns, `bun --watch-web`.
- **Adding a new server-level secret:** (1) provision via the install orchestration or platform env; (2) consume via `state.server_secret(...)`; (3) do not touch env in any other layer; (4) decide if it joins the startup-critical set (most don't).
- **Adding a new worker env var:** add to the worker list in `spawn_env.rs` with a one-line reason and a failing-without test that proves need.

**Verification:**
- `cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings` passes.
- Adding a new `std::env::set_var(...)` in any production OR test file produces a clippy denial that names the strategy doc.

## System-Wide Impact

- **Interaction graph:** all active server-level secrets (the five remaining after Unit 6) converge through `state.server_secret(name)` after Unit 1, which returns from the env+file snapshot. `resolve_auth_mode_with_lookup` migrates in the same unit. Legacy `FABRO_JWT_*` is removed across install, diagnostics, and docs (Unit 6).
- **Error propagation:** daemon preflight (`validate_startup`) and foreground startup (`resolve_startup`, called from `serve_command`) both delegate to the same shared validation logic introduced in Unit 4 — the public preflight wrapper is literally `resolve_startup(...).map(|_| ())`, so error messages and accept/reject decisions are identical by construction. Unit 1 provides only the snapshot machinery (`ServerSecrets` + `EnvSource`) that Unit 4's validation consumes. Object-store credential failures still surface from the AWS SDK / object_store crate (unchanged — daemon inherits AWS env).
- **State lifecycle:** `ServerSecrets` snapshot built once at boot from env+file; both immutable for process lifetime. Rotating any in-scope secret requires restart. Install flows MAY update `server.env` while a server is running when they own restart/handoff via the shared install orchestration (Unit 5).
- **Subprocess env:** workers and render-graph processes inherit only an explicit fail-closed allowlist. Daemon child inherits parent env — that's how 12-factor SESSION_SECRET reaches the daemon.
- **API surface:** no public API change. `state.server_secret(...)` signature unchanged. `ServerSecrets::with_env_lookup` removed; replaced by `load(path, &dyn EnvSource)`. `/install/finish` API contract unchanged (no `restart_required` flag, no new fields).
- **Unchanged invariants:** `ProviderCredentials`, Vault REST API, `vault_or_env` for run-level credentials, `{{ env.FOO }}` interpolation, workflow stages (configured by `Sandbox`) all behave exactly as before.

## Risks & Dependencies

| Risk | Mitigation |
|---|---|
| Snapshot semantics surprise: operator changes env after boot, expects the running server to pick it up | Documented behavior. Live rotation is intentionally not supported; restart required. Strategy doc is explicit. |
| Worker list is missing something a workflow stage runner inside Sandbox needs | Worker process itself only does HTTP callbacks — Sandbox handles stage env. Real workflow integration test in Unit 3 verification catches false negatives. |
| Existing deployments without `server.env` AND without env-set secrets fail to start | Intended behavior; error message names both sources. Per repo policy, accept the breakage. |
| Test migration: replacing `std::env::set_var` calls with `EnvSource` stubs touches many files | `EnvSource` + `StubEnv` pattern is mechanical; one helper per pattern. Unit 7's clippy enforcement catches stragglers at compile time so nothing slips through. |
| Shared install orchestration regression breaks CLI/web parity | Parity tests in Unit 5 explicitly assert identical persistence behavior across CLI and web paths. Both flows route through the same `fabro-install` entry points. |
| Automatic `FABRO_JWT_*` removal surprises operators who believed those keys were authoritative | Strategy doc names the cleanup explicitly. Keys haven't participated in runtime auth since `2026-04-19-003`; cleanup is overdue, not novel. Operators upgrading run install once and the cleanup happens silently. |
| Rust 2024 edition migration is a separate effort | Removing `set_var` is a prerequisite; this work doesn't gate on the edition migration. Workspace-wide clippy enforcement (Unit 7) prevents reintroduction including in tests. |

## Documentation / Operational Notes

- **Operator-facing change:** `fabro server start` against an empty env AND uninstalled storage now fails fast naming both sources. Document in install/quickstart.
- **12-factor PaaS deployments (Railway, Heroku, Fly):** unchanged ergonomics — set secrets in platform env, run `fabro server start`. Works without `fabro install` having run on the platform.
- **Container/k8s deployments:** if secrets are mounted as files (e.g. via projected volume into `<storage>/server.env`) or set as env vars (k8s Secret → env), both work.
- **Cloud object-store (S3 / IRSA / ECS):** unchanged — daemon inherits AWS env from parent, object-store layer reads ambient credentials.
- **Install while server running:** install flows may update `server.env` on a running server only when they coordinate restart/handoff via the shared install orchestration (CLI install or web install via `/install/finish`). Manual edits to `server.env` outside the orchestration still require restart discipline.
- **`FABRO_JWT_*` removal:** `FABRO_JWT_PRIVATE_KEY` / `FABRO_JWT_PUBLIC_KEY` are removed from the runtime auth model; SESSION_SECRET is the sole auth master for cookie and JWT derivation. Existing legacy entries are cleaned up automatically on subsequent install flows.
- **Rotation:** edit env (and restart) or edit `server.env` (and restart). Live rotation not supported.
- **Logging:** the fail-fast error appears in operator log aggregators. Pair human-readable message with a structured error code (e.g., `error_code=missing_session_secret`) so log searches match without depending on the exact string.

## Sources & References

- Related plans:
  - `docs/plans/2026-04-05-server-canonical-secrets-doctor-repo-plan.md` — architectural anchor
  - `docs/plans/2026-04-19-003-feat-cli-auth-login-plan.md` — SESSION_SECRET as HKDF master
  - `docs/plans/2026-04-18-001-feat-webhook-strategy-plan.md` — webhook secret precedent
  - `docs/plans/2026-04-02-001-feat-server-daemon-management-plan.md` — origin of daemon/foreground split
- Related commits:
  - `7cb6c65d5` — "Remove dev-token minting from server start" (precedent for Unit 4)
- Strategy doc precedent: `docs-internal/logging-strategy.md`, `docs-internal/events-strategy.md`
