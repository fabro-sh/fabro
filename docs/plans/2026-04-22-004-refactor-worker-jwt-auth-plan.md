---
title: "refactor: Server-issued per-run JWT for worker subprocess auth"
type: refactor
status: completed
date: 2026-04-22
deepened: 2026-04-22
---

# refactor: Server-issued per-run JWT for worker subprocess auth

## Overview

Server mints one per-run JWT (HS256, 72h, claims include `run_id`) at every worker subprocess spawn, injects it into the worker env as `FABRO_WORKER_TOKEN`, and worker-touched run-scoped routes accept it. Worker stops reading `~/.fabro/auth.json`. The artifact-upload-token mechanism is deleted entirely (greenfield — no external consumers, no shim required). End-user auth (dev-token / github) is now strictly orthogonal to worker auth.

## Problem Frame

Workers POST back to the server (events, state, blobs, stage artifacts). Today the worker only authenticates because it inherits the CLI user's OAuth session from `~/.fabro/auth.json` (`fabro-cli/src/server_client.rs:312` → `AuthStore::default()` at `fabro-client/src/auth_store.rs:107`). Side effects:

- (a) worker authenticates *as the user* — events emitted by the worker get user identity, not "system";
- (b) any deployment where the worker doesn't share a home dir with an authenticated CLI user (containerized server, `fabro` system user, remote worker, multi-tenant) silently fails;
- (c) worker auth is implicitly coupled to end-user auth strategy when conceptually independent.

GitHub-only install (`auth.methods = ["github"]`) writes no `FABRO_DEV_TOKEN` and `worker_command` (`server.rs:3740-3750`) injects nothing — the worker has no documented credential at all. Only the home-dir steal makes it work.

The artifact-upload-token mechanism (`server.rs:752`, `server.rs:846`) already proves the right pattern for one route. Generalize it to every worker-touched route.

## Requirements Trace

- R1. Worker subprocess authenticates to server with a credential the server explicitly issued, not one stolen from the user's home directory.
- R2. Credential is per-run (claim `run_id` must match path `run_id`); cross-run reuse rejected.
- R3. Credential survives server restart up to its natural 72h expiry (no operational events shortening the ceiling).
- R4. Both `start` and `resume` spawn paths re-mint a fresh 72h credential.
- R5. Worker auth works in any deployment topology, including GitHub-only installs with no `~/.fabro/auth.json` on the worker host.
- R6. Worker-emitted events stamped as a system principal (`system:worker`); originator user identity remains discoverable on the run record.
- R7. Worker-touched run-scoped routes still accept end-user JWTs for non-worker callers (CLI, web UI) — fall-through, not replacement.

## Scope Boundaries

- **Greenfield context:** no shipped deployments; no need to preserve in-flight workers across the change. Atomic swap. No backwards-compat shim. No deprecation cycle.
- Out: refresh tokens for workers (decided: hard 72h ceiling per spawn, fresh mint on resume).
- Out: in-memory or persisted revocation list. Per-run binding + 72h `exp` is the entire blast-radius bound.
- Out: multi-host / remote worker spawning (this plan makes it *possible*, doesn't deliver it).
- Out: changes to end-user auth methods (`ServerAuthMethod::DevToken | Github`).
- Out: any new `RunAuthMethod` variant — worker token bypasses `AuthenticatedSubject` entirely.
- Out: extending `RunSummary` with provenance — originator already on `RunSpec.provenance.subject` and that's enough.
- Out: SSE attach routes (`/runs/{id}/attach`, `/attach`). Worker is a producer, not a consumer; never calls them. Stay user-JWT-only.
- Out: lifecycle/admin/user-action routes (`/runs/{id}/cancel`, `/pause`, `/unpause`, `/archive`, `/unarchive`, `DELETE /runs/{id}`, `submit_answer`, `start_run`, `create_run`, list endpoints). Worker token explicitly rejected on these — they remain user-JWT-only.
- Out: any `Display` impl on `Credential::Worker` payload. Plan keeps redacted `Debug` only; do not add `Display`.
- Out: distinct exit codes for auth failure. Worker bails clearly at startup if env is missing; server 401/403 mid-run flows through normal client error handling.
- Out: blanket env scrubbing of trusted internal subprocesses (`gh auth token`, MCP servers, git). These may legitimately need credentials. Scrubbing scope: workflow/sandbox stage-execution chokepoint (`LocalSandbox::execute`) AND host-mode hooks (defense-in-depth; shell commands have no business reading the worker token).

## Threat Model

State the assumptions explicitly so reviewers and operators can challenge them.

- **Trust boundary:** the server process and any worker subprocess running under the same OS user are mutually trusted. Same-UID attackers (or a workflow stage that compromises the worker process) can read `FABRO_WORKER_TOKEN` from `/proc/<pid>/environ` on Linux. Multi-tenant deployments must isolate per-tenant via separate UIDs / containers / namespaces. Cross-run isolation between same-UID workers is NOT a property of this design.
- **`SESSION_SECRET` is the master key.** It signs both user JWTs and worker JWTs (via distinct HKDF context labels). Any leak vector — backup including `server.env`, env dump in logs, ECS task definition exposure, accidental commit, breadcrumb capture — gives the attacker the ability to mint tokens of either kind for any user / any run. Operational guidance: store `SESSION_SECRET` in a secrets manager, exclude from logs/Sentry, document a rotation procedure (rotation invalidates ALL outstanding worker tokens AND ALL user sessions — accept as the cost of compromise response).
- **Worker token compromise:** an attacker who exfiltrates a single worker token gains read/write on that one run's events/blobs/state for up to 72h. Run-id binding limits cross-run damage. There is no in-product revocation; rotating `SESSION_SECRET` is the only mechanism to invalidate outstanding worker tokens.

## Context & Research

### Relevant Code and Patterns

- `lib/crates/fabro-server/src/server.rs:287-289, 752-812, 813-854` — artifact-upload-token: claims struct, key generation (`OsRng` per boot), mint, "service token first, else user JWT" check (`authorize_artifact_upload`). Generalize the shape to all worker-touched routes; replace this mechanism wholesale.
- `lib/crates/fabro-server/src/server.rs:3701-3755` — `worker_command`: single spawn site for both `start` and `resume`. Already passes `--artifact-upload-token` via argv (deleted in Unit 3) and calls `apply_worker_env` at `server.rs:3739`. Add `cmd.env("FABRO_WORKER_TOKEN", token)`.
- `lib/crates/fabro-server/src/server.rs:4666` — `execute_run_subprocess` calls `worker_command` once per spawn; `RunExecutionMode` flows from `start_run` (`server.rs:4181`) and `create_run` (`server.rs:4011`). Single mint site covers both modes.
- `lib/crates/fabro-server/src/spawn_env.rs:18` — existing `apply_worker_env` does `env_clear` + 8-name allowlist (PATH, HOME, TMPDIR, USER, RUST_LOG, RUST_BACKTRACE, FABRO_HOME, FABRO_STORAGE_ROOT). `SESSION_SECRET`, `FABRO_JWT_*`, `GITHUB_APP_*` are all already excluded. Existing `worker_allowlist_is_fail_closed` test (`spawn_env.rs:64-99`) asserts `SESSION_SECRET` is stripped.
- `lib/crates/fabro-server/src/auth/keys.rs:41` — existing HKDF helper `derive_jwt_key` for the user-JWT key. Mirror for worker JWT with distinct context label `b"fabro-worker-jwt-v1"` so worker keys survive server restarts (R3).
- `lib/crates/fabro-cli/src/commands/run/runner.rs:55-127` — `__run-worker` entry, `HttpRunStore`, `HttpArtifactUploader`. Only seven server endpoints touched (catalogued below).
- `lib/crates/fabro-cli/src/server_client.rs:51-58, 133, 312` — `connect_server_target_direct` → `connect_target_api_client_bundle` → `resolve_target_credential` → `AuthStore::default()`. Sole worker caller is `runner.rs:67`. Replaced for the worker only via a sibling constructor.
- `lib/crates/fabro-client/src/credential.rs:6-30` — `Credential` enum. Add `Worker(String)` variant; `bearer_token()` returns the string.
- `lib/crates/fabro-types/src/run_event/mod.rs:29-81` — `ActorRef`/`ActorKind { User | Agent | System }`. No new variant needed — stamp worker events with `ActorKind::System`.
- `lib/crates/fabro-types/src/run.rs:34-49` — `RunProvenance`/`RunSubjectProvenance` already on `RunSpec`. Originator preserved at run-creation time; no schema change.
- `lib/crates/fabro-sandbox/src/local.rs:43-66, 221` — `LocalSandbox::execute` does `env_clear` + `should_filter_env_var` heuristic for stage commands. The `_token` suffix filter incidentally catches `FABRO_WORKER_TOKEN`; make it explicit (denylist entry).

### Worker → server endpoint surface

These are the **only** routes that gain worker-token acceptance. Lifecycle/admin/list endpoints stay user-JWT-only (see Scope Boundaries).

| Worker call | HTTP | Path | Server handler | Auth today |
|---|---|---|---|---|
| `client.get_run_state` | GET | `/runs/{id}/state` | `get_run_state` (`server.rs:5076`) | `AuthenticatedService` |
| `client.list_run_events` | GET | `/runs/{id}/events` | `list_run_events` (`server.rs:5146`) | `AuthenticatedService` |
| `client.append_run_event` | POST | `/runs/{id}/events` | `append_run_event` (`server.rs:5096`) | `AuthenticatedService` |
| `client.write_run_blob` | POST | `/runs/{id}/blobs` | `write_run_blob` (`server.rs:5352`) | `AuthenticatedService` |
| `client.read_run_blob` | GET | `/runs/{id}/blobs/{blobId}` | `read_run_blob` (`server.rs:5379`) | `AuthenticatedService` |
| `client.upload_stage_artifact_file` | POST | `/runs/{id}/stages/{stageId}/artifacts` (octet-stream) | `put_stage_artifact` (`server.rs:5838`) | `authorize_artifact_upload` |
| `client.upload_stage_artifact_batch` | POST | same path (multipart) | same handler | same |

### Coordination with concurrent plans

- `docs/plans/2026-04-22-003-refactor-lock-down-server-secrets-plan.md` partially landed: `apply_worker_env` exists at `lib/crates/fabro-server/src/spawn_env.rs:18` and is invoked from `worker_command` at `server.rs:3739`. The allowlist excludes server-only secrets, structurally preventing the worker from inheriting `SESSION_SECRET`. This plan adds the `FABRO_WORKER_TOKEN` re-injection alongside the existing `FABRO_DEV_TOKEN` re-injection (and ultimately replaces the latter).
- `docs/plans/2026-04-19-003-feat-cli-auth-login-plan.md` Unit 8 created `lib/crates/fabro-server/src/auth/jwt.rs` (`Claims`, `issue`, `verify`, `JwtError`) and `auth/keys.rs::derive_jwt_key`. Reuse the HKDF derivation pattern (distinct context label) and the `jsonwebtoken` primitives directly — do not route worker-token claims through user-`JwtSubject`.
- `docs/plans/2026-04-20-001-fix-cli-server-same-host-assumptions-plan.md` deliberately closed the "trust local files because same host" pattern. This plan preserves that closure — no new same-host exceptions; the worker uses an explicitly-passed token.

### Institutional Learnings

- No `docs/solutions/` directory exists. Prior decisions live in `docs/plans/`.
- Artifact-upload-token TTL precedent is 24h. New worker-token TTL is 72h — justified because worker tokens must survive long human-in-the-loop pauses with no in-process refresh.

## Key Technical Decisions

| Decision | Rationale |
|---|---|
| Replace artifact-upload-token entirely; one JWT per run covers all worker-touched run-scoped routes | Two parallel per-run JWTs is bookkeeping. Run-id binding gives the same blast-radius constraint without a separate scope. Greenfield → atomic delete, no shim. |
| Pass JWT to worker via env var `FABRO_WORKER_TOKEN`. **Env-only — never argv.** | Symmetric with existing `FABRO_DEV_TOKEN` re-injection model. Plays naturally with `env_clear` + explicit re-injection in `apply_worker_env`. Avoids token strings in `ps` output. |
| HS256, key derived from `SESSION_SECRET` via HKDF (context `b"fabro-worker-jwt-v1"`) | Workers must survive server restarts up to natural 72h expiry (R3). `OsRng`-per-boot defeats the long TTL. Distinct context label keeps it isolated from the user-JWT key. Operator rotation of `SESSION_SECRET` invalidates outstanding worker tokens — accepted (and is the only revocation mechanism). |
| 72h TTL, no refresh, no revocation list | Long-paused runs need a generous outer ceiling. Resume re-mints. Workers running > 72h continuously fail loudly — acceptable outer bound. Adding revocation requires persistent state and creates restart-window contradictions; not worth the complexity for the current threat model. |
| Per-run `run_id` claim, path-vs-claim check | Mirrors `maybe_authorize_artifact_upload_token`. Cross-run reuse → 403. |
| Add `Credential::Worker(String)` variant (not reuse `DevToken`) | Debug printing stays accurate. No `Display` impl — compile-time hardening prevents accidental `format!`-leaks. |
| New worker-only client constructor `connect_server_target_with_bearer(target, token)`, bypasses `AuthStore`/`OAuthSession` entirely | Worker should never read user OAuth. Surgical to fix at the worker callsite (one caller, `runner.rs:67`) rather than gating `resolve_target_credential` with a "are you a worker" flag. |
| Stamp `system:worker` actor in a worker-side sink wrapper inside `RunEventSink::fanout`, NOT in `to_run_event_at` | `to_run_event_at` and `stored_event_fields` are shared with the server (server flushes lifecycle events through `workflow_event::to_run_event` at `server.rs:6702`). Default-filling there mis-stamps server-emitted events. The wrapper is worker-local. |
| Route API is a set of typed `FromRequestParts` extractors (`AuthorizeRunScoped`, `AuthorizeRunBlob`, `AuthorizeStageArtifact`), NOT a bare helper function | Composes with existing `Json<...>` / `Bytes` body extractors (a `Parts`-taking helper would force full-`Request` extraction everywhere and break body handling). Each extractor returns the already-parsed run-id (and secondary path params) so handlers drop their own `Path<String>` + `parse_*` dance. Replaces `_auth: AuthenticatedService` and the existing `authorize_artifact_upload` inline call. One fall-through behavior (worker token first, else user JWT) shared across all three. `authorize_worker_token` remains a `pub(crate)` internal helper used by the extractors. |
| Env scrubbing at two sites: `LocalSandbox::execute` (stage execution) and host-mode hooks | Stage commands run user-supplied code → MUST NOT see `FABRO_WORKER_TOKEN`. `LocalSandbox::execute` filters both inherited env AND the explicit `env_vars` extras path (today's code appends extras AFTER the filter — defense-in-depth gap this plan closes). Host-mode hooks get a targeted `env_remove("FABRO_WORKER_TOKEN")` (shell commands have no business reading the worker token, even when operator-configured). Trusted internal subprocesses (`gh auth token`, MCP server stdio, git) are NOT scrubbed — they may legitimately need credentials, and they aren't user-attack surfaces. |
| `authorize_worker_token` lives in `worker_token.rs` and takes `&WorkerTokenKeys` directly (NOT `&AppState`) | Sibling modules can't access private `AppState` fields. Mirroring `maybe_authorize_artifact_upload_token`'s signature (which already takes the typed keys) keeps the helper testable without a fixture `AppState`. The thin `authorize_run_scoped(parts, state, run_id)` adapter lives where it can see `AppState` and pulls `&state.worker_tokens` into the call. |
| Missing/invalid `FABRO_WORKER_TOKEN` → worker errors at startup with a clear message; mid-run 401/403 flow through normal client error handling | No special exit codes. Distinct operational telemetry isn't worth the machinery for the current scale. |

### Worker-token vs artifact-upload-token (delta)

| Property | Artifact-upload-token (today) | Worker-token (new) |
|---|---|---|
| Coverage | One route (`/runs/{id}/stages/{stageId}/artifacts`) | All 7 worker-touched run-scoped routes |
| TTL | 24h | 72h |
| Signing key | `OsRng` at server boot, in-memory only | HKDF from `SESSION_SECRET`, context `b"fabro-worker-jwt-v1"` |
| Survives server restart | No | Yes (up to natural expiry) |
| Issuer string | `"fabro-server-artifact-upload"` | `"fabro-server-worker"` |
| Scope claim | `"stage_artifacts:upload"` | `"run:worker"` |
| Passed to worker | `--artifact-upload-token` argv | `FABRO_WORKER_TOKEN` env var |
| Worker uses it as | Per-call method arg on the client | Client's bearer for every server call |

## Open Questions

### Resolved During Planning

- TTL: 72h. Refresh: none in-process; fresh mint at every spawn (start AND resume).
- Key derivation: HKDF from `SESSION_SECRET` with context `b"fabro-worker-jwt-v1"`.
- Replace artifact-upload-token entirely vs. keep both: replace.
- Token transport: env var (`FABRO_WORKER_TOKEN`), not argv.
- Revocation: none. Per-run binding + 72h `exp` is the entire blast-radius bound.
- New `RunAuthMethod::Worker` variant: no — worker token bypasses `AuthenticatedSubject` entirely.
- Stamp worker events server-side vs. worker-side: worker-side, in a dedicated sink wrapper inside the worker's `RunEventSink::fanout` chain.
- Multi-token-per-run on rapid pause/resume: accept and document. Each prior token remains valid up to 72h `exp`. Bounded by run-id; out-of-scope to fix here.
- Env scrubbing scope: workflow stage-execution chokepoint at `LocalSandbox::execute` (inherited env + explicit `env_vars` extras) AND host-mode hooks at `fabro-hooks/src/executor.rs`. Trusted internal subprocesses (`gh auth token`, MCP stdio, git) are not scrubbed.
- Auth-failure exit codes: no — generic error handling.

### Deferred to Implementation

- Exact module name for new server-side worker-token machinery — likely `fabro-server/src/worker_token.rs`.
- Mechanism for the compile-time "no `Display` for `Credential::Worker`" guard — `static_assertions::assert_not_impl_any!` is the natural fit; choose at implementation time.
- Whether to enforce "worker module never imports `AuthStore`" structurally (clippy `disallowed_types` on the `commands::run` module). Nice-to-have; defer.
- Core dump disable (`setrlimit(RLIMIT_CORE, 0)`) on the worker process. Same-UID attacker assumption holds today; defer.

## High-Level Technical Design

> *This illustrates the intended approach and is directional guidance for review, not implementation specification. The implementing agent should treat it as context, not code to reproduce.*

```mermaid
sequenceDiagram
    participant Op as Operator
    participant Srv as fabro-server
    participant W as worker subprocess
    participant Child as sandbox stage child

    Op->>Srv: start (SESSION_SECRET in env)
    Note over Srv: HKDF-derive WorkerTokenKeys<br/>context "fabro-worker-jwt-v1"
    Srv->>Srv: spawn scheduled (start or resume)
    Note over Srv: issue_worker_token(run_id)<br/>HS256 + claims{run_id, scope, 72h}
    Srv->>W: spawn with apply_worker_env (env_clear + allowlist)<br/>+ FABRO_WORKER_TOKEN injected
    W->>W: read FABRO_WORKER_TOKEN from env<br/>build Client with Credential::Worker(token)<br/>(no AuthStore, no OAuthSession)
    W->>Srv: POST /runs/{id}/events (Bearer ...)
    Srv->>Srv: authorize_run_scoped:<br/>1) try worker token (run_id match + exp check)<br/>2) else fall through to user-JWT extractor
    Srv-->>W: 200 OK
    W->>Child: spawn stage command via LocalSandbox::execute<br/>(env_clear + safelist; FABRO_WORKER_TOKEN excluded)
    Child-->>W: result (no token in env)
    Note over W,Srv: ... run completes ...
    Note over Srv: server restart: HKDF re-derives same key,<br/>outstanding tokens still verify (up to natural exp)
```

## Implementation Units

- [x] **Unit 1: Worker JWT primitives (claims, keys, mint)**

**Goal:** Server can mint a per-run worker JWT signed with a key derived from `SESSION_SECRET`. No callers yet.

**Requirements:** R2, R3.

**Dependencies:** None.

**Files:**
- Create: `lib/crates/fabro-server/src/worker_token.rs`
- Modify: `lib/crates/fabro-server/src/auth/keys.rs` (add `derive_worker_jwt_key`)
- Modify: `lib/crates/fabro-server/src/lib.rs` (module declaration)
- Modify: `lib/crates/fabro-server/src/server.rs` (`AppState` field for `WorkerTokenKeys`, construction in `build_app_state`)
- Test: `lib/crates/fabro-server/src/worker_token.rs` (unit tests inline)

**Approach:**
- Constants: `WORKER_TOKEN_ISSUER = "fabro-server-worker"`, `WORKER_TOKEN_SCOPE = "run:worker"`, `WORKER_TOKEN_TTL_SECS = 72 * 60 * 60`.
- `WorkerTokenClaims { iss, iat, exp, run_id, scope, jti }` — `jti` (random 128-bit hex) enables audit correlation in logs without exposing the token.
- `WorkerTokenKeys { encoding, decoding, validation }` — built from a 32-byte HKDF output keyed by `SESSION_SECRET`, context `b"fabro-worker-jwt-v1"`.
- `pub fn issue_worker_token(keys: &WorkerTokenKeys, run_id: &RunId) -> Result<String, ApiError>` — `jsonwebtoken::encode` with HS256.
- `pub(crate) fn derive_worker_jwt_key(secret: &[u8]) -> Result<[u8; 32], KeyDeriveError>` in `auth/keys.rs` — same HKDF construction as `derive_jwt_key`, distinct `info` parameter (`b"fabro-worker-jwt-v1"`). Mirrors the existing helper's error shape so the `KeyDeriveError` cases (empty / too-short secret) propagate identically.
- **App-state construction wires it explicitly**: `build_app_state` resolves `SESSION_SECRET` (already required for the user-JWT key today), calls `derive_worker_jwt_key`, and bails with a clear startup error if it fails. Failure modes: missing `SESSION_SECRET`, secret too short. Add `worker_tokens: WorkerTokenKeys` field on `AppState` next to `artifact_upload_tokens` (the artifact field is deleted in Unit 3).
- **Test app-state builders** (`worker_command_test_state` at `server.rs:7868` and any other test fixture that constructs `AppState`) must supply a fixture `SESSION_SECRET`. The existing test secret used by user-JWT tests can be reused.

**Patterns to follow:**
- `server.rs:287-289, 319-326, 752-773, 798-812` (artifact-upload-token, end-to-end).
- `auth/keys.rs:41` (`derive_jwt_key` HKDF construction).

**Test scenarios:**
- Happy path: `issue_worker_token` produces a token; `jsonwebtoken::decode` with the same `WorkerTokenKeys` returns the expected `WorkerTokenClaims` (iss, scope, run_id, jti).
- Edge case: token issued with `WorkerTokenKeys` derived from secret S verifies under a *fresh* `WorkerTokenKeys` derived from the same S — proves restart survival (R3).
- Edge case: token issued under secret S1 fails to verify under keys derived from secret S2 (rotation invalidation).
- Edge case: derivation context label `b"fabro-worker-jwt-v1"` produces a key materially different from `derive_jwt_key(secret)` (no accidental cross-acceptance with user JWTs).
- Error path: `derive_worker_jwt_key(b"")` returns `Err(KeyDeriveError::Empty)`. Mirrors existing `derive_jwt_key` error shape.
- Error path: `derive_worker_jwt_key(short_secret)` returns `Err(KeyDeriveError::TooShort { .. })` for secrets below the minimum length.
- Startup: `build_app_state` with no `SESSION_SECRET` in env returns a startup error matching the existing user-JWT-key startup-error wording.

**Verification:**
- All worker-token unit tests pass.
- `cargo build -p fabro-server` succeeds.
- No production callers of new symbols yet — Unit 1 is purely additive infrastructure.

---

- [x] **Unit 2: Server `AuthorizeRunScoped` extractor family + client `Credential::Worker` variant**

**Goal:** Three typed `FromRequestParts` extractors (`AuthorizeRunScoped`, `AuthorizeRunBlob`, `AuthorizeStageArtifact`) accept worker token (run-id-bound) OR fall back to user JWT. Client crate gains a typed worker credential with no `Display` and redacted `Debug`.

**Requirements:** R2, R7.

**Dependencies:** Unit 1.

**Files:**
- Modify: `lib/crates/fabro-server/src/worker_token.rs` (add `pub(crate) fn authorize_worker_token` internal helper; add the three public extractors `AuthorizeRunScoped`, `AuthorizeRunBlob`, `AuthorizeStageArtifact` with `FromRequestParts` impls)
- Modify: `lib/crates/fabro-server/src/lib.rs` (re-export the three extractors if needed by handler modules)
- Modify: `lib/crates/fabro-client/src/credential.rs` (add `Worker(String)` variant — no `Display`)
- Test: `lib/crates/fabro-server/src/worker_token.rs` (authorize helper tests inline)
- Test: `lib/crates/fabro-client/src/credential.rs` (Debug + bearer_token tests inline)

**Approach:**
- **Module placement & visibility**: `worker_token.rs` is a sibling module to `server.rs`; sibling modules cannot read private `AppState` fields. Two options: (a) keep the helper inside `impl AppState` in `server.rs` like `issue_artifact_upload_token` does today, or (b) put the helper in `worker_token.rs` and pass `&WorkerTokenKeys` directly (NOT `&AppState`). **Choose (b)** — keeps `AppState` internals private, makes the helper trivially testable without an `AppState` fixture, mirrors how `maybe_authorize_artifact_upload_token` already takes `&ArtifactUploadTokenKeys` not `&AppState`. The thin glue in `authorize_run_scoped` then takes `&AppState` and pulls `&state.worker_tokens` into the call.
- `pub(crate) fn authorize_worker_token(parts: &Parts, run_id: &RunId, keys: &WorkerTokenKeys) -> Result<bool, ApiError>` — mirror `maybe_authorize_artifact_upload_token` (`server.rs:813-844`). Verification-first rule (no unverified claim peeking — `jsonwebtoken::decode` only returns claims after signature + expiry validation):
  - Bearer absent → `Ok(false)` silently.
  - `jsonwebtoken::decode` with `WorkerTokenKeys` returns `Err(_)` (any reason — bad signature, expired, malformed, alg mismatch) → `Ok(false)` silently. Could be a user JWT in fall-through, an expired worker token, or anything else; we don't know without verifying, and we don't peek at unverified payload bytes.
  - `decode` returns `Ok(claims)` AND `claims.scope != WORKER_TOKEN_SCOPE` → `Err(ApiError::forbidden())` + `tracing::warn!`. A token signed by us with a wrong scope is a misuse.
  - `decode` returns `Ok(claims)` AND `claims.run_id != run_id` → `Err(ApiError::forbidden())` + `tracing::warn!`. Cross-run reuse.
  - `decode` returns `Ok(claims)` AND scope + run_id match → `Ok(true)` + `tracing::info!`.
- **Extractor body (shared logic)**: each `FromRequestParts` impl runs its path-parse step, then calls a shared `pub(crate)` helper that mirrors `authorize_artifact_upload` (`server.rs:846-854`): try `authorize_worker_token(parts, run_id, &state.worker_tokens)?`; if `Ok(false)`, fall through to `authenticate_service_parts(parts)`. `worker_tokens` stays `pub(crate)` on `AppState` so the helper in the same crate can read it.
- `Credential::Worker(String)` — `bearer_token() -> &str` returns the string; `Debug` prints `Credential::Worker(<redacted>)`. **Do NOT implement `Display` on the `Credential` enum.** Compile-time hardening: assert `Credential: !Display` (variant-level assertions don't exist in Rust — the trait impl lives on the type).
- **Audit logging at authorize time** (driven entirely by the verified-only rule above — no log if `decode` itself fails):
  - On successful worker-token auth: `tracing::info!(target = "worker_auth", run_id = %run_id, jti = %claims.jti, "worker token accepted")`.
  - On `Ok(claims)` with scope or run_id mismatch: `tracing::warn!(target = "worker_auth", reason = ..., jti = %claims.jti, "worker token rejected")`.
  - On `Err(_)` from `decode` (no claims available): silent. The bearer might be a user JWT in fall-through, an expired worker token, or garbage — we can't tell without verifying, and we don't try.
  - Never log the token string itself — only `jti`.

**Patterns to follow:**
- `server.rs:813-854` (artifact-upload-token authorize pattern).
- `fabro-client/src/credential.rs:6-40` (existing `DevToken`/`OAuth` variants).

**Test scenarios:**
- Happy path (server, unit): valid worker token for path run_id → `AuthorizeRunScoped` extractor succeeds; handler receives the parsed `RunId`.
- Error path (server): worker token with `claims.run_id != path.run_id` → 403 + `worker_auth` warn with `jti`.
- Error path (server): worker token signed with worker key but wrong scope → 403 + `worker_auth` warn.
- Error path (server): expired worker token → `decode` returns `Err` → silent fall-through → user-JWT extractor rejects → 401. No `worker_auth` log.
- Error path (server): bad signature (e.g. token signed with different key) → `decode` returns `Err` → silent fall-through → 401. No `worker_auth` log.
- Error path (server): `alg=none` JWT → `decode` returns `Err` (validation requires HS256) → silent fall-through → 401.
- Integration (server): no `Authorization` header → falls through to user-JWT extractor → 401 (no implicit acceptance).
- Integration (server): valid user JWT, no worker token → user-JWT path accepts (R7).
- Audit (precision): successful worker-token auth emits `target = "worker_auth"` info span with `run_id` and `jti`. Decode-success-but-claims-mismatch emits `warn` with `reason` and `jti`. Decode failure (any reason) emits NO `worker_auth` log — verify by counting log events on a user-JWT request and on an expired worker token; both must produce zero `worker_auth` lines.
- Happy path (client): `Credential::Worker(s).bearer_token()` returns `s`.
- Edge case (client): `Debug` impl prints `Credential::Worker(<redacted>)` — token string never appears in debug output.
- Compile-time guard (client): assert `Credential: !Display` at the type level (e.g. via `static_assertions::assert_not_impl_any!(Credential: std::fmt::Display)`). Variants are not types; the trait impl lives on the enum.

**Verification:**
- All extractor + helper tests pass with both branches exercised.
- `cargo nextest run -p fabro-server -p fabro-client` succeeds.

---

- [x] **Unit 3: Wire worker-touched routes through the `AuthorizeRunScoped` extractor family; replace artifact-upload-token at the spawn**

**Goal:** Each of the 7 worker-touched routes accepts the worker token. Server spawn injects `FABRO_WORKER_TOKEN` env var. Old artifact-upload-token machinery deleted atomically. Lifecycle/admin/SSE routes are NOT touched and continue to require user JWT.

**Requirements:** R1, R2, R4, R7.

**Dependencies:** Unit 1, Unit 2.

**Files:**
- Modify: `lib/crates/fabro-server/src/server.rs`
  - `get_run_state` (5076), `list_run_events` (5146), `append_run_event` (5096), `write_run_blob` (5352): replace `_auth: AuthenticatedService, Path(id): Path<String>` with `AuthorizeRunScoped(id): AuthorizeRunScoped`. Body extractors stay unchanged.
  - `read_run_blob` (5379): replace `_auth: AuthenticatedService, Path((id, blob_id)): Path<(String, String)>` with `AuthorizeRunBlob(id, blob_id): AuthorizeRunBlob`.
  - `put_stage_artifact` (5838): replace `Path::<(String, String)>` + inline `authorize_artifact_upload(&parts, ...)` with `AuthorizeStageArtifact(id, stage_id): AuthorizeStageArtifact`. Keep `request: Request` for body extraction; continue to call `request.into_parts()` inside the handler for the header/body split.
  - `worker_command` (3701-3755): replace `state.issue_artifact_upload_token` → `state.issue_worker_token`. Drop the `--artifact-upload-token <jwt>` arg entirely. Set `cmd.env("FABRO_WORKER_TOKEN", token)` unconditionally (always, regardless of auth method). Also `cmd.env_remove("FABRO_WORKER_TOKEN")` before re-injection (defense against parent-env leakage). Delete the conditional `cmd.env("FABRO_DEV_TOKEN", token)` block (3740-3750) — `FABRO_WORKER_TOKEN` replaces it as the only authority-bearing re-injection.
  - **Lifecycle/admin/SSE routes left alone**: `cancel_run`, `pause_run`, `unpause_run`, `archive_run`, `unarchive_run`, `delete_run`, `submit_answer`, `start_run`, `create_run`, `attach_run_events` (`/runs/{id}/attach`), `attach_events` (`/attach`), and any list endpoint. Keep `_auth: AuthenticatedSubject` / `AuthenticatedService` directly. Add a one-line code comment at each of the 9 worker-rejecting handlers: `// Worker token intentionally not accepted; this is a user/admin action.`
  - Delete: `ARTIFACT_UPLOAD_TOKEN_*` constants (287-289), `ArtifactUploadClaims` (319-326), `ArtifactUploadTokenKeys` (313-317), `artifact_upload_token_keys` (798-812), `AppState::issue_artifact_upload_token` (752-773), `AppState::artifact_upload_tokens` field (565), `maybe_authorize_artifact_upload_token` (813-844), `authorize_artifact_upload` (846-854).
- Test: existing tests in `lib/crates/fabro-server/src/server.rs` test module (rename / replace `worker_command_injects_dev_token_only_when_enabled` at 7836).

**Approach:**
- The shape change is NOT trivially mechanical for JSON/body handlers like `append_run_event` (`Json<...>` body) or `write_run_blob` (`Bytes` body). Switching them to `Request::into_parts()` would require manually re-parsing the body. Instead, introduce a custom `FromRequestParts` extractor that composes naturally with body extractors.
- **Route API is the extractor, not the helper.** Route handlers should use the new extractor types as their route-facing auth contract. `authorize_worker_token` and the inner decode/fall-through logic remain `pub(crate)` helpers used only by the extractors.
- **Three extractor variants** (one per path shape the worker actually uses) in `worker_token.rs`:
  - `AuthorizeRunScoped(pub RunId)` — for `/runs/{id}/...` with a single run-id path param. Used by: `get_run_state`, `list_run_events`, `append_run_event`, `write_run_blob`.
  - `AuthorizeRunBlob(pub RunId, pub RunBlobId)` — for `/runs/{id}/blobs/{blobId}`. Used by: `read_run_blob`.
  - `AuthorizeStageArtifact(pub RunId, pub StageId)` — for `/runs/{id}/stages/{stageId}/artifacts`. Used by: `put_stage_artifact` (and `list_stage_artifacts`, `get_stage_artifact` if they ever join the worker-touched set; not today).
- Each `impl FromRequestParts` internally:
  - Extracts `Path::<(String, ...)>::from_request_parts` with the right tuple shape (1, 2, or 2 segments).
  - Parses each segment via the existing `parse_run_id_path`, `parse_run_blob_id_path`, `parse_stage_id_path` helpers.
  - Reads `AuthMode` from `parts.extensions` and `&AppState.worker_tokens` from axum state.
  - Calls `authorize_worker_token(parts, &run_id, &keys)?`; on `Ok(false)` falls through to `authenticate_service_parts(parts)`.
  - Returns the typed path params so handlers skip their own `Path<String>` + `parse_*` dance.
- Handlers change:
  - `get_run_state`, `list_run_events`, `append_run_event`, `write_run_blob`: `_auth: AuthenticatedService, Path(id): Path<String>` → `AuthorizeRunScoped(id): AuthorizeRunScoped`. Body extractors (`Json<...>`, `Bytes`) stay intact.
  - `read_run_blob`: `_auth: AuthenticatedService, Path((id, blob_id)): Path<(String, String)>` → `AuthorizeRunBlob(id, blob_id): AuthorizeRunBlob`.
  - `put_stage_artifact`: currently takes `Request` + `Path::<(String, String)>`. Swap to `AuthorizeStageArtifact(id, stage_id): AuthorizeStageArtifact` + `request: Request` (body extraction stays manual via `request.into_parts()`).
- Atomic swap, no transition period — cargo + tests catch any missed callsite.
- **Pre-implementation audit:** grep all `client.*` and `api.*` callsites under `lib/crates/fabro-cli/src/commands/run/` (and any helpers it transitively uses) to confirm each resolves to one of the 7 endpoints in the table. If any newly-discovered handler exists, add a row and wire it through `AuthorizeRunScoped`.
- **Structural rule:** `AuthorizeRunScoped` is used ONLY in handlers whose path contains `{id}` (`RunId`). Verify by grep: every usage must correspond to a `{id}` path segment.

**Patterns to follow:**
- `put_stage_artifact` handler (`server.rs:5838`) is the existing model: takes `parts: Parts`, calls `authorize_artifact_upload(&parts, &state, &id)?`.

**Test scenarios:**
- Happy path: each of the 5 newly-wired routes accepts a worker token whose `claims.run_id` matches the path `id` → expected response.
- Error path: each route with worker token whose `claims.run_id` ≠ path `id` → 403.
- Integration: each route with valid user JWT and no worker token → still works (R7 fall-through).
- Replacement test for `worker_command_injects_dev_token_only_when_enabled`: rename to `worker_command_always_sets_worker_token_env`. Build `worker_command` for both `methods=["github"]` and `methods=["dev-token"]` settings; assert `FABRO_WORKER_TOKEN` env is set to a valid token in BOTH cases. Assert `FABRO_DEV_TOKEN` env is NOT set in either case. Assert no `--artifact-upload-token` or `--worker-token` arg appears in argv (env-only).
- **Negative path (user-only route table):** for every route in the table below, assert presenting a valid worker token (with claims matching the run_id where applicable) is rejected. Tests MUST run under `AuthMode::Enabled` with a valid user-JWT key configured — under `AuthMode::Disabled`, `AuthenticatedService` accepts everything before any validation (`jwt_auth.rs:279`-style), so a "reject" assertion proves nothing. Model after the existing `jwt_auth.rs` tests that use `AuthMode::Enabled` with test secrets.

  | Route | Handler | Expected status with worker token |
  |---|---|---|
  | `GET /runs` | `list_runs` | 401/403 |
  | `POST /runs` | `create_run` | 401/403 |
  | `GET /runs/resolve` | `resolve_run` | 401/403 |
  | `POST /preflight` | `run_preflight` | 401/403 |
  | `POST /graph/render` | `render_graph_from_manifest` | 401/403 |
  | `GET /attach` | `attach_events` | 401/403 |
  | `GET /boards/runs` | `list_board_runs` | 401/403 |
  | `GET /runs/{id}` | `get_run_status` | 401/403 |
  | `DELETE /runs/{id}` | `delete_run` | 401/403 |
  | `GET /runs/{id}/questions` | `get_questions` | 401/403 |
  | `POST /runs/{id}/questions/{qid}/answer` | `submit_answer` | 401/403 |
  | `GET /runs/{id}/attach` | `attach_run_events` | 401/403 |
  | `GET /runs/{id}/checkpoint` | `get_checkpoint` | 401/403 |
  | `POST /runs/{id}/cancel` | `cancel_run` | 401/403 |
  | `POST /runs/{id}/start` | `start_run` | 401/403 |
  | `POST /runs/{id}/pause` | `pause_run` | 401/403 |
  | `POST /runs/{id}/unpause` | `unpause_run` | 401/403 |
  | `POST /runs/{id}/archive` | `archive_run` | 401/403 |
  | `POST /runs/{id}/unarchive` | `unarchive_run` | 401/403 |
  | `GET /runs/{id}/graph` | `get_graph` | 401/403 |
  | `GET /runs/{id}/stages` | `list_run_stages` | 401/403 |
  | `GET /runs/{id}/artifacts` | `list_run_artifacts` | 401/403 |
  | `GET /runs/{id}/files` | `list_run_files` | 401/403 |
  | `GET /runs/{id}/stages/{stageId}/artifacts` | `list_stage_artifacts` | 401/403 |
  | `GET /runs/{id}/stages/{stageId}/artifacts/download` | `get_stage_artifact` | 401/403 |
  | `GET /runs/{id}/billing` | `get_run_billing` | 401/403 |
  | `GET /runs/{id}/settings` | `get_run_settings` | 401/403 |
  | `POST /runs/{id}/preview` | `generate_preview_url` | 401/403 |
  | `POST /runs/{id}/ssh` | `create_ssh_access` | 401/403 |
  | `GET /runs/{id}/sandbox/files` | `list_sandbox_files` | 401/403 |
  | `GET /runs/{id}/sandbox/file`, `PUT /runs/{id}/sandbox/file` | `get_sandbox_file`, `put_sandbox_file` | 401/403 |

  Every route in the real-routes router (`server.rs:1162-1230`) except the 7 worker-touched routes is user-only. The acceptance criterion is a single test helper that iterates this explicit table and asserts rejection for each under `AuthMode::Enabled`. Routes that return `not_implemented` (turns, workflows, insights) are not included — they'll stay user-only automatically if ever implemented; flag in a follow-up if needed.
- **Positive path for `put_stage_artifact`** (auth semantics changed — the only worker-touched route that previously had its own auth helper): explicit tests for the artifact upload route.
  - Valid worker token with matching `run_id` → 200 (octet-stream variant; multipart variant if cheap to set up).
  - Worker token with `claims.run_id != path.run_id` → 403.
  - Valid user JWT (no worker token) → 200 (R7 fall-through preserved on this route).
  - No bearer at all → 401.
- Regression (env scrubbing): extend the existing `worker_allowlist_is_fail_closed` test (`spawn_env.rs:64-99`) to assert `FABRO_JWT_PRIVATE_KEY`, `FABRO_JWT_PUBLIC_KEY`, `GITHUB_APP_PRIVATE_KEY`, `GITHUB_APP_CLIENT_SECRET`, `GITHUB_APP_WEBHOOK_SECRET` don't leak (alongside the existing `SESSION_SECRET` assertion).
- Edge case: assert deleted symbols (`ArtifactUploadClaims`, `authorize_artifact_upload`, etc.) no longer exist — covered implicitly by `cargo build`.

**Verification:**
- `cargo build --workspace` succeeds (no references to deleted artifact-upload-token symbols).
- `cargo nextest run -p fabro-server` passes.

---

- [x] **Unit 4: Worker (CLI) — use injected worker token from env, stop reading `AuthStore`**

**Goal:** Worker subprocess reads its credential from `FABRO_WORKER_TOKEN` env at startup, uses it as its sole bearer for every server call, and never constructs `AuthStore::default()`. The artifact uploader holds the same token string in a per-call bearer field (the client's upload methods require a per-call bearer argument today).

**Requirements:** R1, R5.

**Dependencies:** Unit 2 (`Credential::Worker` variant), Unit 3 (server sets `FABRO_WORKER_TOKEN` env on the worker subprocess).

**Files:**
- Modify: `lib/crates/fabro-cli/src/args.rs:808` — DELETE the `artifact_upload_token: Option<String>` field on `RunWorkerArgs`. Do NOT add a replacement clap arg — the worker reads from env directly.
- Modify: `lib/crates/fabro-cli/src/commands/run/mod.rs:74-90` — drop the `artifact_upload_token` plumbing on the dispatch path.
- Modify: `lib/crates/fabro-cli/src/server_client.rs` — add `pub(crate) async fn connect_server_target_with_bearer(target: &ServerTarget, bearer: &str) -> Result<Client>`. Builds `Client` with `.credential(Credential::Worker(bearer.to_owned()))`, no `oauth_session`, no `resolve_target_credential` call, no `AuthStore` access.
- Modify: `lib/crates/fabro-cli/src/commands/run/runner.rs`
  - At top of `execute`: read `FABRO_WORKER_TOKEN` from env via `std::env::var`. Validate non-empty; bail with a clear error mentioning `FABRO_WORKER_TOKEN` if missing or empty (this is a server-bug indicator, not a user error). Drop `artifact_upload_token` from `execute`'s signature.
  - Line 67: replace `connect_server_target_direct(&server)` with `connect_server_target_with_bearer(&target, &worker_token)`.
  - Delete `MissingArtifactUploadTokenUploader` (300-317) and the `match artifact_upload_token { Some/None }` fork (~244-251); always construct `HttpArtifactUploader`.
  - `HttpArtifactUploader`: **keep** the per-call bearer field, rename `bearer_token: String` → `worker_token: String`. The client methods `upload_stage_artifact_file` and `upload_stage_artifact_batch` still require a per-call bearer parameter (no `Client::credential()` accessor exists today — see next bullet), so the uploader must hold the token and pass it per call. The token is the same string stored at client construction in `Credential::Worker`.
- Keep `lib/crates/fabro-client/src/client.rs:1043, 1081` — `upload_stage_artifact_*` continue to take a `bearer_token` parameter. There is no `Client::credential()` accessor today (`credential` at `client.rs:176` is a builder setter, not a getter), so threading the token per-call is the path of least resistance. The worker-side caller passes the same `FABRO_WORKER_TOKEN` string it built the client with. (If a `Client::credential()` accessor is added later as a separate concern, the per-call parameter can be dropped then.)

**Stage-execution env scrubbing (narrow scope):**

The spawn site that runs user-supplied workflow stage commands must NOT see `FABRO_WORKER_TOKEN`: `LocalSandbox::execute` (`lib/crates/fabro-sandbox/src/local.rs:221`). It already does `env_clear` + safelist (`local.rs:43-66`) with a `_token` suffix denylist that incidentally catches `FABRO_WORKER_TOKEN`.

- Modify: `lib/crates/fabro-sandbox/src/local.rs:43-66` — add `"FABRO_WORKER_TOKEN"` (and `SESSION_SECRET`, `FABRO_JWT_PRIVATE_KEY`, `FABRO_JWT_PUBLIC_KEY`, `GITHUB_APP_PRIVATE_KEY`, `GITHUB_APP_CLIENT_SECRET`, `GITHUB_APP_WEBHOOK_SECRET`) to an explicit denylist alongside the suffix heuristic. Comment why: future rename like `FABRO_WORKER_AUTH` would silently leak under the suffix heuristic alone.
- **Critical: filter both inherited env AND explicit `env_vars` extras.** `LocalSandbox::execute` (`local.rs:224`) appends caller-supplied `env_vars` after the inherited-env filter, so a stage config with `env_vars` containing `FABRO_WORKER_TOKEN` would still leak. Apply the same denylist to the `env_vars` extras path: drop any key in the denylist before calling `cmd.env(key, value)` on each extra.

**Hooks (host mode) — surgical scrub:**

`fabro-hooks/src/executor.rs:186` runs `sh -c <hook command>` for non-sandbox hooks and inherits the worker process env. Even though hooks are operator-configured (not random user input), they are still shell commands that have no business reading `FABRO_WORKER_TOKEN`.

- Modify: `lib/crates/fabro-hooks/src/executor.rs:186` — `cmd.env_remove("FABRO_WORKER_TOKEN")` (and the same six server-secret names listed above) on host-mode hook spawns. Targeted, defense-in-depth. Hooks remain operator-trusted; this just keeps the worker token out of their env.

**Out of scope for env scrubbing:** trusted internal subprocesses that run server-controlled code and may legitimately need credentials in their env: `gh auth token` (`fabro-github/src/lib.rs:129`), MCP server stdio (`fabro-mcp/src/client.rs:47`), git (`fabro-workflow/src/git.rs:35`). These are not user-attack surfaces. Do NOT scrub them.

**Approach:**
- `connect_server_target_with_bearer` is the smallest possible surface: it skips the `AuthStore`/`OAuthSession` machinery entirely. The user-facing `connect_server_target` and `connect_server_with_settings` are unchanged.
- The new constructor is the *only* path the worker takes; verify by grep that `commands::run::runner` is the only module importing it.

**Patterns to follow:**
- `server_client.rs:80-110` (`connect_managed_unix_socket_api_client_bundle`) for the client-builder shape; new constructor is a stripped-down version.

**Test scenarios:**
- Happy path: `connect_server_target_with_bearer` builds a `Client` whose outgoing requests carry `Authorization: Bearer <worker-token>`.
- Edge case: `connect_server_target_with_bearer` does NOT call `AuthStore::default()` — verify by injecting a `FABRO_AUTH_FILE=/nonexistent` env override and confirming construction succeeds (the helper must not even attempt to read the file).
- Edge case: `connect_server_target_with_bearer` does NOT install an `OAuthSession` (no refresh attempts on 401).
- Integration: `runner::execute` with `FABRO_WORKER_TOKEN=<jwt>` set in env POSTs an event using only the injected token; no fallback to `auth.json`.
- Error path: `runner::execute` invoked without `FABRO_WORKER_TOKEN` in env returns a clear error mentioning the variable name; does NOT silently fall back to user OAuth.
- Edge case: `RunWorkerArgs` no longer has `artifact_upload_token` field — `cargo build` confirms.
- Integration (env hygiene, sandbox path, **inherited**): worker env has `FABRO_WORKER_TOKEN=<jwt>` set; a workflow stage Bash command `env | grep -E "FABRO_WORKER_TOKEN|SESSION_SECRET|FABRO_JWT_PRIVATE_KEY"` prints empty output. Covers `LocalSandbox::execute` denylist correctness for inherited env.
- Integration (env hygiene, sandbox path, **explicit env_vars**): a workflow stage configured with `env_vars: { FABRO_WORKER_TOKEN: "leaked", MY_VAR: "ok" }` produces a child env where `FABRO_WORKER_TOKEN` is absent but `MY_VAR=ok` is present. Covers the explicit-extras filter path. **Without this test, the explicit-env_vars bypass is undetected.**
- Integration (env hygiene, hooks): a host-mode hook (`fabro-hooks/src/executor.rs:186`) spawned with `FABRO_WORKER_TOKEN` in the worker's env produces a child where `env | grep FABRO_WORKER_TOKEN` is empty.

**Verification:**
- `cargo build --workspace` succeeds.
- `cargo nextest run -p fabro-cli` passes.
- grep for `AuthStore` from `commands/run/` returns no hits.

---

- [x] **Unit 5: Stamp `system:worker` actor on worker-emitted events (worker-side sink wrapper)**

**Goal:** Events the worker emits without a typed actor get a `system:worker` stamp at the *worker-side sink layer*. User identity stays on the run record (`RunSpec.provenance.subject`), where it already lives.

**Requirements:** R6.

**Dependencies:** Should land WITH the auth changes (Units 1-4), not in isolation. Landing alone produces a wire-visible behavior change (worker events flip from `actor: None` to `actor: System(worker)`) without the auth context that justifies it.

**Files:**
- Modify: `lib/crates/fabro-workflow/src/event.rs` — add `RunEventSink::Map { transform, inner }` variant that applies `transform` to each event before forwarding to `inner`. Wire it into the existing dispatch logic so all events reach the inner sink already transformed.
- Modify: `lib/crates/fabro-cli/src/commands/run/runner.rs` — at the `RunEventSink::fanout([...])` construction site (`runner.rs:100`), wrap the fanout in `RunEventSink::Map { transform: stamp_system_worker, inner: ... }` so the stamp applies to all downstream sinks (backend HTTP, local callback, future).
- Test: `lib/crates/fabro-workflow/src/event.rs` (test the `Map` variant in isolation).
- Test: `lib/crates/fabro-cli/src/commands/run/runner.rs` (test the worker-local stamp wrapper end-to-end).

**Approach:**
- **Critical: do NOT modify `lib/crates/fabro-workflow/src/event.rs::to_run_event_at` or `stored_event_fields`.** Those helpers are shared with the server (e.g. `server.rs:6702` flushes lifecycle events through `workflow_event::to_run_event`). Default-filling there mis-stamps server-emitted events.
- Helper function (not `const` — `ActorRef::id`/`display` are `Option<String>`, heap-allocated): `fn system_worker_actor() -> ActorRef { ActorRef { kind: ActorKind::System, id: Some("worker".to_string()), display: Some("system:worker".to_string()) } }` lives in `runner.rs` (worker-local).
- **Stamping must apply to the whole fanout, not just one sink variant.** `RunEventSink` is an enum (`Backend | Callback | Composite | …`) in `fabro-workflow/src/event.rs`. Wrapping only the `Backend` variant means the local callback (today: `update_worker_title_from_event`) and any future sink see unstamped events.
- **Design: add `RunEventSink::Map { transform: Arc<dyn Fn(RunEvent) -> RunEvent + Send + Sync>, inner: Box<RunEventSink> }`** variant. Worker constructs `RunEventSink::Map { transform: stamp_system_worker, inner: Box::new(RunEventSink::fanout([backend, callback])) }`; stamp applies before the fanout splits.
- **Dispatch: non-recursive, iterative, owned per branch.** The existing `write_run_event` at `event.rs:2698` uses an iterative stack with a single shared `&RunEvent`. Naively translating `Map => inner.write_run_event(&mapped).await` would introduce recursive async (doesn't compile cleanly without boxing each recursive call, and obscures the shape).
  - Redesign the traversal to carry **`(sink, owned_event)` pairs** on the stack instead of `(&sink, &event)`. Each node owns the `RunEvent` value for its subtree.
  - `Map { transform, inner }`: apply `transform` to the owned event → push `(*inner, transformed_event)` onto the stack. The branch downstream sees the new event; the original is dropped when this stack frame unwinds.
  - `Composite { sinks }`: for each child sink, push `(child, event.clone())`. Each branch gets its own owned event. (Cloning `RunEvent` is cheap — it's a struct of owned data already serialized once; no deep-copy of large payloads.)
  - `Backend { ... }` / `Callback { ... }`: terminal — invoke with the owned event, no recursion.
  - Keep the existing async-loop shape; only the stack element type changes.
- The transform applies the value-based rule: **if `event.actor.is_none()`, fill with `system_worker_actor()`**. Agent events (`AssistantMessage`) keep `ActorKind::Agent` because `actor.is_some()`. Worker self-cancel events (`Event::RunCancelRequested { actor: None }`) correctly get the system actor.

**Patterns to follow:**
- `RunEventSink::fanout` composition (`fabro-workflow/src/event.rs` sink layer).
- Existing actor-stamping for lifecycle events at the server endpoints (`actor_from_subject` at `server.rs:6175`) — server-side stamping for user actions; worker-side wrapper for worker events.

**Test scenarios:**
- Happy path (`Map` variant): a `RunEventSink::Map { transform: |e| e.with_actor(ActorRef::user("alice")), inner: backend }` applied to an event with any actor → forwarded event has actor = `ActorRef::user("alice")` regardless. Confirms the variant works.
- Happy path (worker stamp): a stage-execution `RunEvent { actor: None, ... }` enters the wrapped fanout → BOTH the backend sink AND the local callback see `actor: Some(ActorRef { kind: System, id: Some("worker"), display: Some("system:worker") })`.
- Edge case: `RunEvent { actor: Some(user_actor), ... }` → both sinks retain the user actor (`actor.is_some()` → no-op).
- Edge case: agent message `RunEvent { actor: Some(ActorKind::Agent), ... }` → both sinks retain `ActorKind::Agent`.
- Edge case: worker self-cancel `RunEvent { actor: None, body: RunCancelRequested { ... } }` → both sinks get `system:worker`.
- Per-sink uniformity assertion: construct the worker's actual fanout (Backend + Callback), feed an `actor: None` event in, capture what each sink receives, assert both have `system:worker`. Without this test, only stamping the Backend variant could regress without detection.
- Regression: server-side event flush via `workflow_event::to_run_event` (`server.rs:6702`) is unchanged — `to_run_event_at` retains its passthrough semantics.
- Regression: `create_hydrates_provenance_into_store_state` (`fabro-workflow/src/operations/create.rs`) still passes — originator user identity still on `RunSpec.provenance.subject`.

**Verification:**
- `cargo nextest run -p fabro-cli` passes.
- New tests for the default-fill and the override-protections both pass.

---

- [x] **Unit 6: End-to-end regression — github-only worker run with no `~/.fabro/auth.json`**

**Goal:** Lock in the bug fix: a github-only deployment can spawn a worker that completes a run, with no user OAuth artifact present on the worker host. Worker authenticates using only `FABRO_WORKER_TOKEN`.

**Requirements:** R1, R5.

**Dependencies:** Units 1-5.

**Files:**
- Create: an integration test under `lib/crates/fabro-cli/tests/it/cmd/` (existing test scaffolding) — file name per local convention (e.g. `worker_auth.rs`).

**Approach:**
- **Test fixture:**
  - Server configured with `auth.methods = ["github"]` only; no `FABRO_DEV_TOKEN` in `server.env`.
  - `FABRO_HOME` redirected to a fresh tempdir on the *worker* side (no `auth.json`, no `dev-token` file).
  - Submitter path uses an authenticated test user JWT (minted directly via `auth/jwt.rs::issue` with the test `SESSION_SECRET`) to call `POST /runs` and start the run. **Do not** confuse this with the worker's auth — the user JWT is what authorizes run creation; the worker JWT (server-issued in response) is what the worker subprocess uses.
- Run a tiny workflow end-to-end via the daemon → worker spawn path.
- Assert the worker successfully POSTs at least one `RunEvent` and the run reaches a terminal status.
- Assert `~/.fabro/auth.json` is not touched (non-existence at the redirected `FABRO_HOME`).

**Patterns to follow:**
- Existing integration tests in `lib/crates/fabro-cli/tests/it/cmd/` (per `support.rs` helpers like `daemon.bind.to_target()`).
- CLAUDE.md note: tests must use `.no_proxy()` HTTP clients.

**Test scenarios:**
- Integration: github-only server + no `auth.json` on worker host + minimal workflow → run completes successfully; events visible via `GET /runs/{id}/events`.
- Integration: same setup but worker spawned with a deliberately-bogus `FABRO_WORKER_TOKEN` (e.g. valid HS256 but wrong `run_id` claim) → worker fails fast on first server call.

**Verification:**
- `cargo nextest run -p fabro-cli --test it` passes the new test.
- Test fails on `main` (pre-Units 1-5) — confirms it covers the regression.

## System-Wide Impact

- **Interaction graph:** Worker subprocess no longer reads `~/.fabro/auth.json`. CLI user-facing commands (`fabro run`, `fabro ps`, `fabro auth`, etc.) unchanged — they still go through `connect_server_target` / `connect_server_with_settings`. SSE attach endpoints unchanged: worker is a producer, not a consumer; user-JWT-only auth on those routes preserved.
- **Error propagation:** Worker token expiry mid-run → next server call returns 401, worker exits with a generic error, server marks run failed via existing pump-worker exit handling at `server.rs:4786`. No new error class.
- **State lifecycle:** Server restart with HKDF-derived key: outstanding worker tokens remain valid up to natural expiry. Server restart with `SESSION_SECRET` rotated: outstanding workers fail at next call (acceptable; matches user-session invalidation). No revocation set.
- **Event sink uniformity:** the worker wraps its `RunEventSink::fanout([Store(http), Callback])` (`runner.rs:100`) in `RunEventSink::Map { transform: stamp_system_worker, inner: fanout }`, so the stamp applies once *before* the fanout splits — both the HTTP backend and the local callback observe identical actor metadata. The shared `to_run_event_at` converter remains pure (passthrough). A test asserts per-sink uniformity directly.
- **API surface parity:** `RunEvent.actor` shape unchanged (`ActorRef` already has `ActorKind::System`); worker-emitted events newly carry `system:worker` instead of `None`. Web UI audit (`apps/fabro-web/app/`) found ZERO references to `actor` or `author` today — nothing to break.
- **Worker-process trust degradation:** A workflow stage that compromises the worker process (malicious shell, code injection) gains read access to `FABRO_WORKER_TOKEN` for the worker's lifetime + up to 72h until natural expiry. Blast radius bounded by run-id claim: only the compromised run's blobs/events/state are accessible. Not cross-run. `SESSION_SECRET` is NOT in the worker's env (`apply_worker_env` allowlist) so the worker cannot mint cross-run tokens.
- **Integration coverage:** Unit 6 covers github-only-no-auth-store regression; existing CLI integration tests cover dev-token deployments.
- **Unchanged invariants:** End-user auth (dev-token / github) unchanged. `RunAuthMethod` enum unchanged. `RunSpec.provenance` shape unchanged. Webhook auth unchanged. Lifecycle/admin/SSE/list endpoints continue to require user auth — worker token explicitly rejected on all of them. `OpenAPI` / `fabro-api-client` (TypeScript) DTOs unchanged.

## Risks & Dependencies

| Risk | Mitigation |
|------|------------|
| Worker process inherits `SESSION_SECRET` → can mint tokens for any run, defeating per-run binding | **Already structurally mitigated**: `apply_worker_env` at `spawn_env.rs:18` does `env_clear` + 8-name allowlist that excludes `SESSION_SECRET`. Existing `worker_allowlist_is_fail_closed` test asserts this. Unit 3 extends the test to also cover `FABRO_JWT_*` and `GITHUB_APP_*`. |
| Token leaks via `format!`/`Display`/`tracing` of `Credential::Worker` payload | `Credential::Worker` has redacted `Debug`, no `Display`. Compile-time guard test in Unit 2 asserts `!impl Display`. Audit logging in Unit 2 logs `jti` only, never the token. |
| Sentry / panic capture serializes the worker's env or backtrace locals | Sentry panic hook (`fabro-telemetry/src/panic.rs`) captures only panic message + stacktrace, not env or frame variables. Code review responsibility to keep token out of panic format strings. |
| 72h token compromised mid-run, attacker uses it from any host on network | Run-id binding limits blast radius to one run. No revocation; rotating `SESSION_SECRET` is the only invalidation mechanism (also invalidates user sessions). Acceptable for current threat model. |
| `FABRO_WORKER_TOKEN` readable via `/proc/<pid>/environ` to same-UID processes on Linux | Documented in Threat Model: env-var transport does not protect against same-UID reads. Multi-tenant deployments must isolate per-tenant via separate UIDs / containers. NOT a property of this design. |
| Workflow stage child processes (sandbox-executed Bash) inherit `FABRO_WORKER_TOKEN` via env or via explicitly-supplied `env_vars` extras | `LocalSandbox::execute` filters BOTH the inherited env (existing safelist + denylist) AND the `env_vars` extras path (new in Unit 4). Two regression tests prove both paths. |
| Host-mode hook commands inherit `FABRO_WORKER_TOKEN` | `fabro-hooks/src/executor.rs` does targeted `cmd.env_remove("FABRO_WORKER_TOKEN")` (and the same six server-secret names) on host-mode hook spawns. Hooks remain operator-trusted; this is defense-in-depth — shell commands have no business reading the worker token. |
| Trusted internal subprocesses (`gh`, MCP, git) inherit env including `FABRO_WORKER_TOKEN` | NOT scrubbed by design — these run server-controlled code, may legitimately need credentials, and are not user-attack surfaces. Documented in Unit 4. |
| `client.upload_stage_artifact_*` API requires a per-call bearer parameter | Per Unit 4: `HttpArtifactUploader` holds the token in a `worker_token: String` field (same string read from `FABRO_WORKER_TOKEN`) and threads it per call. No `Client::credential()` accessor today; per-call threading is the path of least churn. |
| Same-run concurrent worker spawn (scheduler race) → two valid tokens for one `run_id` racing on event/state appends | Scheduler's at-most-one-worker-per-run guarantee is assumed but not verified by this plan. If a race exists today, follow-up plan adds a server-side spawn lock or a per-spawn nonce. Out of scope here. |
| Rapid pause/resume cycles leave multiple valid tokens per run | Each prior worker token remains valid up to its 72h `exp`. Multiplicative compromise window bounded by run-id. Accepted; out of scope to fix here. |

## Documentation / Operational Notes

- `docs-internal/` — if any internal doc describes worker auth (search before landing), update to reflect: "worker → server auth uses a server-issued per-run JWT, independent of end-user auth method."
- No external user-facing doc impact (no public API change; CLI args on `__run-worker` are internal-only, hidden via `#[command(hide = true)]`).

### Deploy story (greenfield, atomic swap)

No shipped deployments to preserve. Atomic swap:
1. Deploy new binary; server restarts.
2. New runs spawn workers with `FABRO_WORKER_TOKEN` in env; worker uses it via `Credential::Worker`.
3. Old artifact-upload-token mechanism is gone from the codebase entirely.

No drain, no shim, no checklist beyond verifying `SESSION_SECRET` is set (HKDF key derives from it).

## Sources & References

- Worker auth surface inventory: `lib/crates/fabro-cli/src/commands/run/runner.rs:55-127`
- Artifact-upload-token model to replace: `lib/crates/fabro-server/src/server.rs:287-289, 752-854`
- Worker spawn site: `lib/crates/fabro-server/src/server.rs:3701-3755`
- Existing HKDF key derivation: `lib/crates/fabro-server/src/auth/keys.rs:41`
- Existing worker env allowlist: `lib/crates/fabro-server/src/spawn_env.rs:18`
- `Credential` variants: `lib/crates/fabro-client/src/credential.rs:6-30`
- `ActorRef` / `ActorKind`: `lib/crates/fabro-types/src/run_event/mod.rs:29-81`
- `RunProvenance`: `lib/crates/fabro-types/src/run.rs:34-49`
- Stage-execution chokepoint: `lib/crates/fabro-sandbox/src/local.rs:221, 43-66`
- Coordinated plans: `docs/plans/2026-04-22-003-refactor-lock-down-server-secrets-plan.md`, `docs/plans/2026-04-19-003-feat-cli-auth-login-plan.md`, `docs/plans/2026-04-20-001-fix-cli-server-same-host-assumptions-plan.md`
- Origin of artifact-upload-token pattern: `docs/plans/2026-04-06-object-backed-artifact-uploads.md:42-45`
- Worker subprocess history: `docs/plans/2026-04-06-subprocess-run-workers-signal-control-plan.md`, `docs/plans/2026-04-07-worker-http-only-run-store-migration-plan.md`
