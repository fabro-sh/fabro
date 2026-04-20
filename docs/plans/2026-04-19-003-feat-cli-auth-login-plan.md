---
title: "feat: fabro auth login CLI command with GitHub OAuth + JWT"
type: feat
status: active
date: 2026-04-19
origin: docs/superpowers/specs/2026-04-19-cli-auth-login-design.md
---

# `fabro auth login` — CLI authentication via GitHub OAuth

## Overview

Adds a `fabro auth login` / `logout` / `status` command group that lets CLI users authenticate to a Fabro server without a shared dev-token. The flow is OAuth 2.0 Authorization Code with PKCE against a loopback listener, driven through the server's existing GitHub OAuth for web. The server issues a short-lived HS256 JWT access token (10 min) plus an opaque rotating refresh token (sliding 30 d) persisted in SlateDB. Dev-token stays unchanged and is preserved as a fallback.

## Problem Frame

Today the CLI authenticates only via a shared `fabro_dev_*` secret. Servers that enable GitHub OAuth but disable dev-token (production and shared environments) leave CLI users with no way in. The existing web OAuth flow mints an encrypted session cookie that is browser-only, and the file at `lib/crates/fabro-server/src/jwt_auth.rs` is misleadingly named — it does no JWT issuance or verification today.

We need per-user, per-device CLI credentials that are server-side revocable, short-lived on the wire, and forward-compatible with Google Workspace and GitHub Enterprise Server logins. See origin: `docs/superpowers/specs/2026-04-19-cli-auth-login-design.md`.

## Requirements Trace

- **R1.** CLI users can log in with `fabro auth login` against a server that accepts only GitHub OAuth (no dev-token). (origin §Problem, §CLI UX)
- **R2.** Access credential is a short-lived (10 min) HS256 JWT; refresh credential is an opaque 32-byte secret rotated on every use with 30 d sliding expiry. (origin §Token model)
- **R3.** Refresh-token rotation is atomic: two concurrent refreshes cannot both succeed. Reuse detection deletes the entire chain. (origin §Revocation semantics)
- **R4.** Identity is OIDC-style `(idp_issuer, idp_subject)` — not GitHub username — forward-compatible with Google/GHES. `login` is display-only. (origin §Identity model)
- **R5.** Browser flow runs on `server.web.url`, not on the `--server` target. CLI discovers the canonical origin via `GET /api/v1/auth/cli/config` preflight. (origin §Architecture overview, §Canonical origin)
- **R6.** `fabro auth login` succeeds on servers reachable only over HTTPS or Unix socket (browser hits web_url; token endpoints use the CLI transport). (origin §Canonical origin)
- **R7.** Dev-token flow is unchanged. Bearer-priority order: `FABRO_DEV_TOKEN` env → `AuthStore` JWT → dev-token file fallback. (origin §CLI UX, §Bearer priority)
- **R8.** Reactive auto-refresh triggers on 401 with `code == "access_token_expired"` in the `ApiError` envelope; refresh failures surface as "session expired — run `fabro auth login`". (origin §Bearer priority)
- **R9.** Server-side errors in the browser flow reach the CLI via OAuth-style redirect to loopback (`?error=&state=&error_description=`) when `redirect_uri`+`state` have validated; plain HTML page otherwise. (origin §Browser-to-CLI error handoff)
- **R10.** GitHub allowlist rejection at `/auth/callback/github` happens **before** a session is minted; `/auth/cli/resume` must forward the callback's `?error=` to the CLI loopback without checking session first. (origin §/auth/cli/resume algorithm)
- **R11.** `SESSION_SECRET` is reused as an HKDF master; cookie and JWT keys are domain-separated subkeys. No new env vars. (origin §Settings and secrets)
- **R12.** `server.auth.methods=[github]` combined with `web.enabled=false` fails at server startup with a clear config error. Other "CLI login unavailable" states (github not in methods) are running-server states reported via preflight. (origin §Web mode and config validation)
- **R13.** IP allowlist, when enabled, covers every new endpoint (no carve-outs). (origin §IP allowlist)
- **R14.** `fabro auth status` runs fully offline against local state (no server call); `--json` emits structured output; credentials and their expiry windows are shown per server. (origin §CLI UX)
- **R15.** Integration tests exercise the real `web_auth.rs` OAuth glue end-to-end against a black-box `twin-github` extended with OAuth endpoints, not via `#[cfg(test)]` session injection. (origin §Prerequisite: twin-github OAuth extension)

## Scope Boundaries

`fabro-server` is a single-node process. There is no multi-node deployment model to design against — the per-hash and per-code in-process mutexes in Units 9 and 10 provide the full atomicity guarantees R3 demands. No distributed-coordination design is needed.

Explicit non-goals for this plan (deferred to fast-follow work):

- **Windows support for `fabro auth login` writes.** `fabro auth login` returns a clear error on Windows; users work around via WSL or dev-token. Dropped to avoid DPAPI/ACL complexity that doesn't clearly improve on the 0600 file baseline for the same-user threat model. Note: `fabro auth status` and `fabro auth logout` DO work on Windows (see R14 — status must remain cross-platform for dev-token detection and logged-out-state reporting).
- **Idempotency grace on refresh.** Network failures during refresh-response delivery force re-login. A future grace window (if needed) must key by stable per-install identifier, not UA/source-IP.
- **`--browser-url` override for split-network topologies.** Browser always opens against `config.web_url` in v1.
- **Public `oauth_base_url` / `api_base_url` settings** for GHES / self-hosted GitHub. Test harness uses a test-only injection point; public settings land when GHES itself lands.
- Device flow (`--device`). Same token model, different exchange endpoint.
- OS keychain storage. Plain 0600 file is the v1 Unix storage baseline.
- Rate limiting. Out of scope; operators can add a reverse-proxy-level limiter.
- Per-device session UI (`fabro auth sessions`, revoke-by-device).
- Emergency per-user kick independent of allowlist removal.
- Configurable token TTLs via TOML.
- RS256/ES256 signing.
- Google Workspace or GHES IdPs (identity model supports them; plumbing is future work).
- Renames of `SESSION_SECRET` → `FABRO_AUTH_SECRET` or `ServerAuthMethod::Github` → IdP-agnostic name.
- Per-IdP allowlists (current `[server.auth.github].allowed_usernames` stays; note the GitHub login-reuse risk — see §Security properties).
- Audit log table for logins/logouts (`tracing::info!`/`warn!` only in v1).

## Context & Research

### Relevant Code and Patterns

- **`lib/crates/fabro-oauth/`** — already implements `PkceCodes`, `generate_pkce()`, `generate_state()`, `build_authorize_url()`, `start_callback_server()`, `run_browser_flow()`. CLI login reuses this rather than reimplementing PKCE/state/loopback.
- **`lib/crates/fabro-server/src/web_auth.rs`** — `SessionCookie` struct at `:24`; `login_github` at `:285`; `callback_github` at `:359` (allowlist check at `:522-526` — rejection returns `Redirect::to("/login?error=unauthorized")` without minting a session); `read_private_session` at `:127`; `parse_cookie_header` at `:113`; session cookie name `__fabro_session` at `:20`; cookie key derived from `SESSION_SECRET` via `state.session_key()`. **GitHub URLs are hardcoded** at `:337` (authorize), `:428` (token exchange), `:474` (/user), `:509` (/user/emails) — no config override today; the plan adds one (see Unit 2b).
- **`lib/crates/fabro-server/src/jwt_auth.rs`** — `AuthMode` at `:37`; `resolve_auth_mode_with_lookup` at `:46`; `AuthenticatedService` / `AuthenticatedSubject` extractors at `:181`/`:197`; existing session-cookie injection test pattern at `:430-460` (reusable in new tests). Misleadingly named — does no JWT work today.
- **`lib/crates/fabro-server/src/server.rs`** — router construction at `build_router_with_options` (`:904`); `real_routes()` at `:1087`; `demo_routes()` at `:1007`; `AppState` at `:535`; web-auth routes nested under `/auth` at `:917`. `.nest("/api/v1", api_common)` is always mounted; the `/auth/*` prefix only mounts when `options.web_enabled`.
- **`lib/crates/fabro-server/src/error.rs`** — `ApiError` with `IntoResponse` at `:121`; serializes to `{"errors":[{"status","title","detail"}]}`. Constructors: `::new`, `::bad_request`, `::unauthorized`, `::forbidden`, `::not_found`.
- **`lib/crates/fabro-store/src/keys.rs`** — `SlateKey::new(ns).with(seg)` builder, null-separated; `.into_prefix()` adds trailing null for prefix scans. **`SlateKey::new` and `.with` are `pub(crate)`** — fabro-server cannot construct keys directly. Values are serialized with `serde_json::to_vec` (not bincode as the spec said; plan corrects this).
- **`lib/crates/fabro-store/src/slate/mod.rs`** — `Database` wraps `slatedb::Db`; `db.scan_prefix(&prefix).await?` iterates keyed rows; new keyspaces live as sibling modules under `slate/`. **`Database` exposes only run-centric public methods** (`create_run`, `open_run`, `list_runs`, `runs()`, `get`, `find`, `list`) — no public raw `put`/`get`/`scan_prefix`. The refresh-token store lives inside `fabro-store` as a new sibling module (matching `run_store.rs` and `catalog.rs`), exposing typed methods to `fabro-server`.
- **`lib/crates/fabro-config/src/resolve/server.rs`** — `resolve_auth` at `:103`; pushes `ResolveError::Invalid { path, reason }` into an `errors` vec. This is the right layer for structural validation, but the cross-field check involving the env-var-driven `web.enabled` is in `jwt_auth::resolve_auth_mode_with_lookup` at `jwt_auth.rs:46-65`.
- **`lib/crates/fabro-cli/src/args.rs`** — `ProviderNamespace` namespace pattern at `:1437`: outer `Args` struct with `#[command(subcommand)]`, inner `Subcommand` enum. Top-level variant `Commands::Provider(ProviderNamespace)` at `:1025`. Dispatch in `main.rs:326`.
- **`lib/crates/fabro-cli/src/commands/provider/login.rs`** — canonical login-command shape: takes clap args + `CliSettings` + `CliLayer` + `Printer`, gets `ctx.server().await?`, calls generated `fabro_api` client.
- **`lib/crates/fabro-cli/src/server_client.rs`** — `ServerStoreClient` at `:33`; client construction in `connect_api_client_bundle` at `:132`; `apply_bearer_token_auth` at `:336` sets `Authorization` as a default header (one-shot at client build) — this is a real constraint: progenitor's generated `fabro_api::Client` holds the prebuilt `reqwest::Client` by value and its headers are baked in at construction. Dynamic-per-request auth means (a) a reqwest middleware reading an `Arc<Mutex<Token>>`, (b) rebuilding `fabro_api::Client` on refresh (drops nothing since it's a thin wrapper), or (c) a hand-written `AuthedApiClient` that wraps each operation. Plan chooses (b) — see Unit 21. `map_api_error` at `:1022-1047` collapses `progenitor_client::Error::ErrorResponse` to `anyhow::Error` by extracting only `errors[0].detail` — this is the specific gap auto-refresh must bridge.
- **`lib/crates/fabro-server/tests/it/helpers.rs`** — `test_app_with_scheduler()` (and `test_app_state_with_options`) builds in-process routers via `build_router(state, AuthMode::Disabled)`; tests drive via `tower::ServiceExt::oneshot(Request)`. No insta at this layer; CLI tests use insta (inline).
- **`test/twin/github/src/`** — handler registration in `handlers/mod.rs:15`; `SharedState = Arc<RwLock<AppState>>` (`server.rs:11`); state seeded in `state.rs`; fixtures in `fixtures.rs`. No OAuth endpoints today.
- **`lib/crates/fabro-util/src/dev_token.rs`** — file-write pattern with mode 0600 via `OpenOptionsExt` and atomic temp-then-rename. Reuse shape for `~/.fabro/auth.json`.
- **`docs/api-reference/fabro-api.yaml`** — OpenAPI source of truth. Build flow per CLAUDE.md §API workflow: edit yaml → `cargo build -p fabro-api` regenerates Rust client → `cd lib/packages/fabro-api-client && bun run generate` for TS.
- **`AGENTS.md`** (project root) — no glob imports; `fabro_test::test_http_client()` or `cli_http_client_builder().no_proxy()` in tests; `docs-internal/logging-strategy.md` for new `tracing` calls.

### Institutional Learnings

`docs/solutions/` does not exist in this repo. No prior OAuth/JWT/refresh-token solutions to draw from. Seed `docs/solutions/` during this work if we encounter novel problems worth compounding.

### External References

Design-time external work was consolidated in the spec (OAuth 2.0 RFC 6749 §4.1.2.1 for error redirects; OAuth 2.0 Security BCP for rotation-with-reuse-detection; comparisons to `gcloud`, `wrangler`, `gh auth login`, `aws sso login`). No new external research required at planning time.

## Key Technical Decisions

- **Reuse `fabro-oauth` crate for CLI-side PKCE + loopback, extending its callback server.** The existing `start_callback_server` swallows `error` params and hangs on state mismatch. The new `start_callback_server_with_errors` function fires the oneshot only when the state matches — with either `code` (success) or `error` (server-authored failure) payload. State-mismatch requests still return HTTP 400 without firing the oneshot (preserves the property that any local process can't abort an in-progress login by probing the callback port). The CLI relies on `--timeout` (default 5 min) for the state-mismatch-no-legitimate-callback case.
- **Piggyback on the existing web GitHub OAuth flow; do not fork it.** Add `return_to` query param to `/auth/login/github` (strict whitelist: `^/auth/cli/(start|resume)$`). `/auth/cli/resume` is the continuation point after web OAuth completes. `/auth/callback/github` preserves `return_to` on all error paths, not just success. Avoids duplicating ~200 lines of token exchange + user fetch + allowlist logic.
- **Reuse `SESSION_SECRET` as HKDF master, full Extract-and-Expand.** Two domain-separated subkeys derived via HKDF-SHA256 (`Extract` then `Expand`) with context labels `b"fabro-cookie-v1"` and `b"fabro-jwt-hs256-v1"`. HKDF-Extract applied first means operator-supplied `SESSION_SECRET` doesn't need to be uniformly random (it never is). Startup validation also requires `SESSION_SECRET` to be at least 32 bytes (64 hex chars) when `github` is in `auth.methods` — documented as "this secret now signs JWTs; a weak value allows offline forgery."
- **`SlateAuthTokenStore` is a concrete struct (no trait), held as `Arc<SlateAuthTokenStore>` in `AuthServices`.** Single instance per server; `Clone` yields another Arc handle to the same DashMap, not a duplicate. `DashMap::entry(...).or_insert_with(|| Arc::new(Mutex::new(())))` acquires the per-hash mutex atomically (not get-then-insert). Mutex-entry cleanup only occurs while holding the mutex and only when no other Arc strong references exist.
- **No idempotency grace in v1.** `consume_and_rotate` burns the old token immediately on success; any second presentation is treated as theft. Network failures during `/auth/cli/refresh` response delivery (server rotated, client disconnected) force the user to re-login on their next command — surfaced as "session ended during refresh. Run `fabro auth login`." Simpler semantics, smaller attack surface than the spoofable fingerprint-based grace that was previously proposed. If production telemetry shows frequent network-blip-triggered logouts, reintroduce a grace window keyed by a stable per-install identifier (stored in `auth.json`), NOT by UA/source-IP which are attacker-controlled.
- **Identity uses a type-enforced enum with serde validation.** `SessionCookie` carries `identity: Option<IdpIdentity>` where `IdpIdentity { issuer: String, subject: String }` validates non-empty fields via `#[serde(try_from = "IdpIdentityWire")]` — closes the serde-bypass gap where `#[derive(Deserialize)]` would otherwise reconstitute the type without invariant checks. The same `IdpIdentity` type is used by `AuthCode` and `RefreshToken` so the invariant flows through the entire auth lifecycle, not just at the cookie boundary. `None` = dev-token session; `Some(_)` = IdP-authenticated. Eligibility gate is a single `session.identity.as_ref()` pattern match. Cookie v1 → v2 bump; v1 cookies force re-login (acceptable given 30 d TTL).
- **Unix-only CLI login in v1.** Windows support deferred. `fabro auth login` on Windows exits with a clear message: "CLI OAuth login is not supported on Windows in this release. Use WSL, or use a dev-token server." Drops the DPAPI / NTFS-ACL machinery, the `windows` crate dependency, and Windows-specific tests. Unix continues to use mode 0600 atomic writes via `fs2` advisory locking.
- **Error shape sanitation.** Server emits only fixed `error_description` strings chosen from a closed enumeration per `error` code; inbound `error_description` is NEVER forwarded verbatim (would be a terminal-injection / log-injection vector). HTML error pages render only static server-authored text; query values are never echoed; `Content-Type: text/html; charset=utf-8`, `X-Content-Type-Options: nosniff`.
- **Cookie attributes explicit.** `fabro_cli_flow`: `HttpOnly; Secure` (auto-relaxed for `http://127.0.0.1` dev); `SameSite=Lax`; `Path=/auth`; 10 min max_age. `fabro_oauth_state`: existing attributes; max_age raised from 10 min to 30 min to cover slow GitHub authorize (MFA, org approvals).
- **Endpoint-appropriate error envelopes.** `/auth/cli/token`, `/refresh`, `/logout` return flat RFC 6749 `{error, error_description}`. `/auth/cli/start`, `/resume` return either a 302-redirect-with-error-param (when `redirect_uri`+`state` validated) or a plain HTML error page (otherwise). Protected endpoints continue to use the existing `ApiError` envelope, extended with an optional `code: Option<String>` field (backwards compatible via `skip_serializing_if = "Option::is_none"`). OpenAPI spec version bumped (minor) to document the new field.
- **SlateDB value encoding matches repo convention: `serde_json::to_vec`.** Fields are small fixed-size, the encoding cost is negligible, and JSON is easier to inspect in logs.
- **Refresh-token storage lives in `fabro-store` as a typed module** (`lib/crates/fabro-store/src/slate/auth_tokens.rs`), parallel to `run_store.rs` and `catalog.rs`. Exposes a typed `SlateAuthTokenStore` with domain methods (`insert_refresh_token`, `find_refresh_token`, `delete_chain`, `gc_expired`). Keeps `SlateKey` encapsulation intact; fabro-server consumes the typed API.
- **Authorization codes live in SlateDB**, keyspace `auth/code/<code>` (opaque 32-byte random code, base64url-encoded). 60 s TTL enforced at read time; reaper task every 30 s purges expired entries. `consume` is single-use via a per-code in-process mutex (fabro-server is single-node, so an in-process mutex is the correct primitive). PKCE provides a secondary defense at `/auth/cli/token` (Unit 15 step 5) as belt-and-suspenders against stolen codes without verifiers.
- **Reactive-only auto-refresh (no pre-flight).** CLI refreshes on 401 with `code == "access_token_expired"` and retries once. No clock-based pre-flight check. Avoids concurrent-refresh storms across parallel CLI invocations, removes clock-sync assumptions, and shrinks the refresh code path by half. Latency cost is at most one extra round-trip per ~10 min window of activity.
- **HTTPS-or-loopback-or-unix-socket-only for refresh traffic, checked by URL parsing, not string matching.** CLI parses the server target as `url::Url`. Accepts: `scheme == "https"`; OR `scheme == "http"` with `url.host()` parsing to an `IpAddr` that `is_loopback()` returns true (covers `127.0.0.0/8`, `::1`, and `::ffff:127.0.0.1`); OR the Unix-socket target type. Rejects literal `localhost` (DNS-overridable), any host containing dots after a loopback prefix (e.g., `127.0.0.1.evil.com`), any encoded form (decimal `2130706433`, hex `0x7f000001`, octal). Surfaces an actionable error pointing at the scheme or host. Prevents silent refresh-token leaks over plaintext.
- **`map_api_error_structured` wraps `progenitor_client::Error` into `ApiFailure { status, code, detail }`.** Existing `map_api_error` is refactored to call `map_api_error_structured` and discard `code`, reducing drift risk. Auto-refresh wrapper uses the structured helper.
- **OpenAPI-first for preflight.** `GET /api/v1/auth/cli/config` lands in `fabro-api.yaml` and regenerates the Rust+TS clients. Preflight response carries `reason` enum only (not `reason_description`) — the CLI renders user-facing text from the enum locally. The OAuth-style `/auth/cli/{token,refresh,logout}` endpoints are outside the canonical API surface (different envelope, different auth); they are **not** in the OpenAPI spec and are hand-wired.
- **Browser URL always equals `config.web_url` in v1.** Split-network topologies (dockerized dev, VPN mismatches, SSH-plus-browser-on-different-hosts) are deferred to a fast-follow `--browser-url` flag. For v1, users whose `server.web.url` is not reachable from their browser cannot complete login; the preflight handler returns the exact URL the CLI will open so the failure mode is at least visible.

## Open Questions

### Resolved During Planning

- **SlateDB value encoding.** Resolved: `serde_json::to_vec` per existing repo convention.
- **Where startup validation lives.** Resolved: `jwt_auth::resolve_auth_mode_with_lookup` at `jwt_auth.rs:46-65`, extended with the `github + !web.enabled` check. Matches the existing `SESSION_SECRET` and `GITHUB_APP_CLIENT_SECRET` checks at the same boundary.
- **Whether to extend `fabro-api.yaml` for all six new endpoints.** Resolved: only `GET /api/v1/auth/cli/config` goes in the OpenAPI spec. The five `/auth/cli/*` OAuth endpoints use a flat OAuth envelope (incompatible with `ApiError`) and live outside the progenitor-generated client — CLI calls them via a dedicated `fabro_http` helper.
- **How much to extend twin-github.** Resolved: happy path + explicit denial + wrong-client-secret. Not a fidelity clone.

### Deferred to Implementation

- **Exact `error_description` strings.** User-visible copy decided at implementation time per endpoint; not architecturally load-bearing.
- **GC reaper cadence tuning.** Initial values: AuthCode reaper every 30 s, refresh-token GC every 6 h. Tune if logs show pressure.
- **Whether `fabro auth login` reuses a single `AuthStore` file lock across preflight + token POST + write.** Implementation detail; spec requires lock around read-modify-write, not across network I/O.
- **Whether `VerifiedAuth` adds `idp_issuer`/`idp_subject` fields or exposes them via a separate method.** Shape decision during Unit 11; handlers largely treat `AuthenticatedService` as a unit marker.

## High-Level Technical Design

> *This illustrates the intended approach and is directional guidance for review, not implementation specification. The implementing agent should treat it as context, not code to reproduce.*

### Flow shape

```mermaid
sequenceDiagram
    autonumber
    participant CLI
    participant Browser
    participant API as API router<br/>(--server target)
    participant Web as Web router<br/>(server.web.url)
    participant GH as GitHub<br/>(or twin-github in tests)

    CLI->>API: GET /api/v1/auth/cli/config
    API-->>CLI: {enabled, web_url, methods}
    Note over CLI: Abort if enabled=false

    CLI->>CLI: generate PKCE + state<br/>bind loopback :PORT
    CLI->>Browser: open web_url/auth/cli/start?...
    Browser->>Web: GET /auth/cli/start
    alt eligible GitHub session exists
        Web-->>Browser: 302 redirect_uri?code=&state=
    else no eligible session
        Web-->>Browser: 302 /auth/login/github?return_to=/auth/cli/resume
        Browser->>Web: GET /auth/login/github
        Web->>GH: redirect to authorize
        GH-->>Browser: redirect to /auth/callback/github?code=&state=
        Browser->>Web: GET /auth/callback/github
        alt allowlist ok
            Web-->>Browser: set __fabro_session; 302 /auth/cli/resume
            Browser->>Web: GET /auth/cli/resume
            Web-->>Browser: 302 redirect_uri?code=&state=
        else allowlist rejects / GitHub error
            Web-->>Browser: 302 /auth/cli/resume?error=
            Browser->>Web: GET /auth/cli/resume?error=
            Web-->>Browser: 302 redirect_uri?error=&state=
        end
    end
    Browser->>CLI: GET loopback/callback?code=&state= | ?error=&state=
    CLI->>API: POST /auth/cli/token {code, verifier}
    API-->>CLI: {access_token, refresh_token, ...}
    CLI->>CLI: persist ~/.fabro/auth.json (0600, fs2-locked)
```

### Module tree

```
lib/crates/fabro-server/src/
├── auth/
│   ├── mod.rs              AuthServices bundle + re-exports
│   ├── cli_flow.rs         /auth/cli/* handlers (config, start, resume, token, refresh, logout)
│   ├── jwt.rs              HS256 encode/verify, claims, HKDF subkey derivation
│   ├── (refresh tokens live in fabro-store; fabro-server re-exports via auth/mod.rs)
│   └── (auth codes live in fabro-store; fabro-server re-exports via auth/mod.rs)
├── web_auth.rs             SessionCookie migration; return_to plumbing; error-path preservation
├── jwt_auth.rs             extractor extended; resolve_auth_mode gets github+!web.enabled check
└── server.rs               route registration + AuthServices wiring

lib/crates/fabro-cli/src/
├── commands/auth/
│   ├── mod.rs              AuthNamespace / AuthCommand dispatch
│   ├── login.rs            preflight + fabro-oauth browser flow + POST /token + persist
│   ├── logout.rs           POST /logout + AuthStore remove
│   └── status.rs           local-only status formatter (text + JSON)
├── auth_store.rs           AuthStore + AuthEntry + ServerTargetKey normalization + fs2 lock
└── server_client.rs        bearer priority + auto-refresh wrapper; map_api_error_structured
```

### `AuthMode` extension shape

Adds `jwt_key: Option<Arc<JwtSigningKey>>` alongside existing `dev_token` and `session_methods`. Construction goes through `resolve_auth_mode_with_lookup` (HKDF subkey derivation from `SESSION_SECRET` happens there).

## Implementation Units

Organized into five phases. Early phases stand alone (no behavior changes). Later phases deliver observable features.

### Phase 1 — Foundations (no user-visible behavior)

- [ ] **Unit 1: OpenAPI spec — preflight config endpoint**

**Goal:** Add `GET /api/v1/auth/cli/config` to the OpenAPI spec and regenerate Rust + TypeScript clients.

**Requirements:** R5, R6, R12

**Dependencies:** none

**Files:**
- Modify: `docs/api-reference/fabro-api.yaml` — add operation under `/auth/cli/config`; add `CliAuthConfig` schema with `enabled: bool`, `web_url: nullable string`, `methods: [string]`, `reason: nullable string`. **`reason` is an open string (NOT an OpenAPI `enum`)** so future server versions can add new values without breaking older CLI clients. The schema description enumerates the currently known values (`"github_not_enabled"`, `"web_not_enabled"`) for documentation, but the type is a plain string. This ensures the progenitor-generated client accepts any string value; forward-compat handling lives in the CLI (Unit 18), which pattern-matches known values and falls back to a generic "CLI login is not available on this server" message for anything unrecognized. No `reason_description` (CLI renders text for known values locally).
- Run: `cargo build -p fabro-api` (build.rs regenerates Rust types + client).
- Run: `cd lib/packages/fabro-api-client && bun run generate` (regenerates TS client).
- Test: rely on existing `tests/it/api/openapi_conformance.rs` once Unit 8 (the handler) lands.

**Approach:**
- Endpoint is `GET`, unauthenticated, no request body.
- `operationId: getCliAuthConfig` (camelCase to match existing ops).
- Response 200 schema carries the five fields above.
- `reason` is an open string (not a closed OpenAPI `enum`) — the schema description lists the currently-known values (`github_not_enabled`, `web_not_enabled`) but the generated Rust type is `Option<String>`, so an older CLI deserializing a future server's new `reason` value succeeds and falls through to Unit 18's generic-unknown-value branch.

**Patterns to follow:**
- Mirror existing unauthenticated ops in `fabro-api.yaml` (e.g., `/health` if present, or the anonymous route pattern).

**Test scenarios:**
- Test expectation: none — pure schema change; conformance is asserted transitively when Unit 8 registers the route.

**Verification:**
- Rust client exposes a `get_cli_auth_config()` builder method.
- TS client gets a matching method.
- `cargo build --workspace` succeeds.

---

- [ ] **Unit 2: Workspace dependency additions**

**Goal:** Add `hkdf` and `fs2` as workspace dependencies.

**Requirements:** R11, R14

**Dependencies:** none

**Files:**
- Modify: `Cargo.toml` (workspace root) — add `hkdf = "0.12"` and `fs2 = "0.4"` under `[workspace.dependencies]`.
- Modify: `lib/crates/fabro-server/Cargo.toml` — declare `hkdf = { workspace = true }`.
- Modify: `lib/crates/fabro-cli/Cargo.toml` — declare `fs2 = { workspace = true }`.

**Approach:**
- Pin to current minor versions.
- No crate-level code lands here — just Cargo files.

**Patterns to follow:**
- Existing workspace deps in `Cargo.toml` use `{ workspace = true }` at crate level.

**Test scenarios:**
- Test expectation: none — dependency addition only.

**Verification:**
- `cargo build --workspace` succeeds.

---

- [ ] **Unit 3: `ApiError` extended with optional `code` field**

**Goal:** Extend the `ApiError` envelope with an optional machine-readable `code` while preserving backwards compatibility.

**Requirements:** R8

**Dependencies:** none

**Files:**
- Modify: `lib/crates/fabro-server/src/error.rs` — add `code: Option<String>` to `ErrorEntry` with `#[serde(skip_serializing_if = "Option::is_none")]`; add `ApiError::with_code(status, detail, code)` constructor; add `unauthorized_with_code` and `forbidden_with_code` convenience constructors used by the JWT extractor.
- Modify: `docs/api-reference/fabro-api.yaml` — update the error-response schema component to document `code` as an optional string; document the values `access_token_expired`, `access_token_invalid`, `unauthorized` as the initial set.
- Test: `lib/crates/fabro-server/src/error.rs` (inline `#[cfg(test)] mod tests`).

**Approach:**
- `code` is purely additive: when `None`, the serialized JSON is identical to today's shape (verified by test).
- Regenerate clients after the yaml update so `fabro-api`'s generated `Error` type includes the field.

**Patterns to follow:**
- `#[serde(skip_serializing_if = "Option::is_none")]` on optional response fields is common elsewhere in `server.rs` DTOs.

**Test scenarios:**
- Happy path: `ApiError::with_code(StatusCode::UNAUTHORIZED, "token expired", "access_token_expired")` serializes to `{"errors":[{"status":"401","title":"Unauthorized","detail":"token expired","code":"access_token_expired"}]}`.
- Edge case: `ApiError::unauthorized()` (no code) serializes without a `code` key — byte-exact match with today's output (guard against accidental envelope breakage).

**Verification:**
- Round-trip test confirms `code` absent when `None`.
- `cargo build --workspace` succeeds with regenerated client.

---

- [ ] **Unit 4: HKDF subkey derivation helper (Extract-and-Expand, entropy-validated)**

**Goal:** Derive two domain-separated symmetric subkeys from `SESSION_SECRET` — one for the existing `cookie` crate, one for HS256 JWT signing — with full HKDF-Extract-and-Expand so operator-supplied secrets don't need to be uniformly random.

**Requirements:** R11

**Dependencies:** Unit 2

**Files:**
- Create: `lib/crates/fabro-server/src/auth/mod.rs` — module root (stub for now; later units add re-exports).
- Create: `lib/crates/fabro-server/src/auth/keys.rs` — `derive_cookie_key(master: &[u8]) -> Result<cookie::Key, KeyDeriveError>` and `derive_jwt_key(master: &[u8]) -> Result<JwtSigningKey, KeyDeriveError>`; HKDF context labels `b"fabro-cookie-v1"` and `b"fabro-jwt-hs256-v1"`. `KeyDeriveError` variants: `TooShort { got_bytes, min_bytes }`, `Empty`.
- Modify: `lib/crates/fabro-server/src/lib.rs` — declare `pub mod auth;`.
- Modify: `lib/crates/fabro-server/src/web_auth.rs` — existing `state.session_key()` callers now receive the derived subkey (internally — call site untouched; the derivation happens inside `state.session_key()` wiring or at `AuthMode` construction, matching Unit 5).
- Test: `lib/crates/fabro-server/src/auth/keys.rs` (inline `#[cfg(test)] mod tests`).

**Approach:**
- HKDF-Extract-and-Expand (two steps) with SHA-256: Extract with a zero-byte salt and the raw `SESSION_SECRET` as IKM to produce a 32-byte PRK; then Expand the PRK with `info = context label` to 64 bytes for cookie or 32 bytes for JWT. This is the correct primitive for operator-supplied master secrets; HKDF-Expand-only (what the spec originally said) requires the input to already be uniformly random, which `SESSION_SECRET` may not be.
- Minimum-entropy check: reject a `SESSION_SECRET` shorter than 32 bytes with `KeyDeriveError::TooShort`. Documented as: "`SESSION_SECRET` must be at least 32 bytes (64 hex characters). This secret now signs JWTs as well as session cookies — a weak value allows offline JWT forgery if any JWT is captured."
- `JwtSigningKey` is a thin newtype wrapping the raw 32 bytes + helper `encoding_key() / decoding_key()` methods that bridge to `jsonwebtoken::EncodingKey::from_secret`.
- Keep the helper private to `fabro-server` (no public re-export).

**Patterns to follow:**
- `cookie::Key::from(&[u8])` accepts a 64-byte derived key (the `cookie` crate itself derives internal sub-keys for encryption vs MAC from those 64 bytes).
- The `hkdf` crate exposes `Hkdf::<Sha256>::new(salt, ikm)` for Extract and `.expand(info, &mut okm)` for Expand.

**Test scenarios:**
- Happy path: same master + same label yields identical subkey bytes across two calls (determinism).
- Happy path: different labels yield different subkeys for the same master (domain separation).
- Happy path: 32-byte master + cookie label produces 64 bytes; + JWT label produces 32 bytes (shape).
- Error path: empty master → `Empty`.
- Error path: 31-byte master → `TooShort { got_bytes: 31, min_bytes: 32 }`.
- Integration (with the `cookie` crate): a key derived via Extract-and-Expand round-trips a private cookie (encrypt → decrypt) successfully.
- Edge case: a purposefully low-entropy 32-byte master (e.g., `[0x61; 32]`) derives distinct JWT and cookie subkeys — the test just confirms domain separation still holds; we do NOT attempt to detect weak secrets beyond the length check.

**Verification:**
- Tests pass in isolation.

---

- [ ] **Unit 5: Startup validation — `github + !web.enabled` fails at boot**

**Goal:** Reject the incoherent config combination `server.auth.methods=[..., github, ...]` with `server.web.enabled=false` at startup. Also enforce the `SESSION_SECRET` ≥32-byte floor required for HKDF key derivation when github auth is enabled.

**Requirements:** R12

**Dependencies:** Unit 4

**Files:**
- Modify: `lib/crates/fabro-server/src/jwt_auth.rs` — extend `resolve_auth_mode_with_lookup` (`:46`) with the cross-field check; produce a `ResolveError::Invalid { path: "server.auth.methods", reason: "GitHub auth is enabled but server.web.enabled is false" }`.
- Modify: `lib/crates/fabro-server/src/jwt_auth.rs` — store the HKDF-derived JWT key on `AuthMode::Enabled` (field `jwt_key: Option<JwtSigningKey>`); derive it in the same resolver using Unit 4's helper when `SESSION_SECRET` is present and github is enabled.
- Test: `lib/crates/fabro-server/src/jwt_auth.rs` (existing `#[cfg(test)] mod tests`).

**Approach:**
- Add three checks alongside the existing `SESSION_SECRET` and `GITHUB_APP_CLIENT_SECRET` validations at `:60-65`:
  1. `github in methods` with `web.enabled=false` → `ResolveError::Invalid { path: "server.auth.methods", reason: "GitHub auth requires server.web.enabled = true" }`.
  2. `github in methods` but `SESSION_SECRET` missing → (existing behavior, unchanged).
  3. `github in methods` with `SESSION_SECRET` present but shorter than 32 bytes → `ResolveError::Invalid { path: "SESSION_SECRET", reason: "SESSION_SECRET must be at least 32 bytes (64 hex characters) when github auth is enabled — it now signs JWTs as well as session cookies. Current length: {got} bytes." }`. Uses the `KeyDeriveError::TooShort` from Unit 4's helper.
- `jwt_key` is `None` when github is not enabled — keeps non-github deployments clean.

**Patterns to follow:**
- Existing `ResolveError::Invalid` pushes in `fabro-config/src/resolve/server.rs:103`.

**Test scenarios:**
- Happy path: `methods=[dev-token, github]` + `web.enabled=true` + 32-byte `SESSION_SECRET` → resolves successfully with `jwt_key = Some(_)`.
- Error path: `methods=[github]` + `web.enabled=false` → returns `ResolveError::Invalid` with the web-enabled reason text.
- Error path: `methods=[github]` + `web.enabled=true` + 31-byte `SESSION_SECRET` → returns `ResolveError::Invalid` with a path-of-`SESSION_SECRET` and a reason mentioning minimum length.
- Edge case: `methods=[dev-token]` + `web.enabled=false` + weak `SESSION_SECRET` → resolves successfully (no github, no JWT key, entropy rule doesn't apply) with `jwt_key = None`.
- Edge case: `methods=[github]` + `web.enabled=true` but no `SESSION_SECRET` → existing error wins (no regression).

**Verification:**
- New tests pass; existing `jwt_auth` tests still pass.

---

- [ ] **Unit 6: `SessionCookie` migration — type-safe `Option<IdpIdentity>` representation**

**Goal:** Migrate `SessionCookie` from `provider_id: Option<i64>` to a type-enforced IdP identity representation. Eligibility decisions become exhaustive pattern matches instead of empty-string checks, making it impossible to accidentally bootstrap a non-IdP session as CLI-eligible.

**Requirements:** R4

**Dependencies:** none

**Files:**
- Create: `lib/crates/fabro-types/src/auth.rs` — `IdpIdentity` type; `IdpIdentityWire` DTO for serde; `IdpIdentityError` enum. **Placed in `fabro-types` (not `fabro-server`) because `fabro-store` also needs to use the type for `RefreshToken` (Unit 10), and `fabro-store` already depends on `fabro-types` but not on `fabro-server` — the reverse direction would create a dependency cycle.** Both `fabro-server` and `fabro-store` import from `fabro-types`.
- Modify: `lib/crates/fabro-types/src/lib.rs` — declare `pub mod auth;` and re-export `IdpIdentity` at the crate root.
- Modify: `lib/crates/fabro-server/src/web_auth.rs`:
  - `SessionCookie` struct (`:24`): bump `v: u8` sentinel from 1 → 2; drop `provider_id: Option<i64>`; add `identity: Option<IdpIdentity>`.
  - Callback GitHub-login minting at `:533-545`: `identity = Some(IdpIdentity::new("https://github.com", profile.id.to_string())?)`.
  - Dev-token minting at `:246-257`: `identity = None` (dev sessions are non-IdP — semantic rather than stringly).
- Modify: `lib/crates/fabro-server/src/jwt_auth.rs` — `read_private_session` and `AuthenticatedSubject` extraction: decoded cookies with `v != 2` are treated as absent (force re-login).
- Modify: `lib/crates/fabro-server/src/web_auth.rs` — `SessionUser` response DTO (`:67`) and `/api/v1/auth/me`: flatten `identity` into `idp_issuer: Option<String>` and `idp_subject: Option<String>` in the serialized shape (keeping wire compatibility with anyone consuming the API). `login` remains the display field.
- Test: `lib/crates/fabro-types/src/auth.rs` (inline `#[cfg(test)] mod tests`) — invariant + serde-bypass tests live next to the type.
- Test: `lib/crates/fabro-server/src/web_auth.rs` (inline `#[cfg(test)] mod tests`) — cookie round-trip with the new `identity` field.

**Approach:**
- `IdpIdentity` shape (declared in `fabro-types/src/auth.rs`):
  ```
  #[derive(Debug, Clone, PartialEq, Eq, Serialize)]
  #[serde(try_from = "IdpIdentityWire", into = "IdpIdentityWire")]
  pub struct IdpIdentity {
      issuer: String,   // private field — inaccessible outside this module
      subject: String,  // private field
  }
  impl IdpIdentity {
      pub fn new(issuer: impl Into<String>, subject: impl Into<String>) -> Result<Self, IdpIdentityError>;
      pub fn issuer(&self) -> &str;
      pub fn subject(&self) -> &str;
  }

  #[derive(Deserialize, Serialize)]
  struct IdpIdentityWire { issuer: String, subject: String }  // public fields, no invariants
  ```
- The `#[serde(try_from = "IdpIdentityWire")]` attribute forces deserialization to go through `IdpIdentity::try_from(wire)` which validates non-empty. This closes the serde-bypass gap: direct `Deserialize` would otherwise set the private fields blindly; `try_from` applies the constructor. `into = "IdpIdentityWire"` keeps the serialized form identical to today (flat `{issuer, subject}`).
- One-way migration: v1 cookies are dropped, users re-login. Acceptable given 30 d TTL and this lands before CLI login GA.
- The eligibility gate used in Units 14 and 15 becomes `matches!(session.identity, Some(_))` — compiler-enforced exhaustiveness. The **runtime 403-on-empty-strings check disappears** (the type makes it impossible). The **unreachability assertion** (returning 500 if somehow an invalid shape is reached — Unit 15 step 7) stays as a defensive guard against future bugs; it should be `debug_assert!(false, "unreachable: IdpIdentity invariant violated")` in debug builds and a 500 in release.
- Dev-token sessions: `identity = None` means "no IdP authentication." `/auth/cli/*` eligibility gate rejects these by pattern-match.
- Type invariant: `IdpIdentity::new` and `IdpIdentity::try_from(wire)` both reject empty `issuer` or `subject`; no public API can construct an invalid instance. The fields are `String` (not `pub`), the struct body has no public fields, so external crates can only construct via `new` or deserialize via `try_from`.
- **Visibility:** `pub struct IdpIdentity`, `pub fn new`, `pub fn issuer`, `pub fn subject`, `pub struct IdpIdentityError` — required for `fabro-server` and `fabro-store` to both use the type from `fabro-types`. Invariants are enforced by private fields + validating constructor/`try_from`, NOT by `pub(crate)` visibility.

**Patterns to follow:**
- Existing `CookieJar::private_mut(&key).add(...)` cookie minting shape in callback at `web_auth.rs:549`.
- Rust newtype-with-validation pattern paired with `#[serde(try_from = "...")]` is the standard way to enforce invariants across serde boundaries.

**Test scenarios:**
- Happy path: v2 GitHub cookie encodes with `identity: Some(IdpIdentity { issuer: "https://github.com", subject: "12345" })`, decodes round-trip.
- Happy path: v2 dev-token cookie encodes with `identity: None`, decodes cleanly.
- Error path: `IdpIdentity::new("", "12345")` → error.
- Error path: `IdpIdentity::new("https://github.com", "")` → error.
- **Error path — serde bypass guard:** deserialize a crafted JSON `{"issuer": "", "subject": "12345"}` directly into `IdpIdentity` → returns a serde error (NOT a successfully-constructed empty-issuer instance). This is the critical regression test — it verifies `#[serde(try_from)]` is wired correctly.
- **Error path — serde bypass via SessionCookie:** mint a v2 session cookie with a tampered JSON payload carrying an empty `issuer` field directly (bypassing `IdpIdentity::new`) → `read_private_session` returns `None` or a deserialization error, NOT a successfully-decoded cookie with an invalid `IdpIdentity`.
- Edge case: v1 cookie presented → `read_private_session` returns `None` (treated as absent).
- Integration: session-cookie-injection test (existing pattern at `jwt_auth.rs:430-460`) updated to mint v2 cookies — no regressions in existing protected-endpoint tests.

**Verification:**
- Full `cargo nextest run -p fabro-server` green; the invariant test explicitly rejects empty strings.

---

- [ ] **Unit 6b: GitHub base-URL override via test-only Axum extension (prerequisite for twin-github integration)**

**Goal:** Make the four hardcoded `github.com` / `api.github.com` URLs in `web_auth.rs` redirectable to `twin-github` in integration tests, **without** introducing a public settings surface. Closes the test-integration gap without adding a production attack vector (a misconfigured public `oauth_base_url` would leak client-secret to an attacker-controlled host).

**Requirements:** R15

**Dependencies:** none

**Files:**
- Create: `lib/crates/fabro-server/src/auth/github_endpoints.rs` — `GithubEndpoints { oauth_base: Url, api_base: Url }` with a `production_defaults()` constructor and a `with_bases(oauth: Url, api: Url)` test-only constructor (the test-only one is exposed via `pub(crate)` and only called from `#[cfg(test)]` code or the dedicated test harness in `fabro-server/tests/it/helpers.rs`).
- Modify: `lib/crates/fabro-server/src/server.rs` — inject `Arc<GithubEndpoints>` as an Axum `Extension` layer at router build time. Production router uses `GithubEndpoints::production_defaults()`; test harness replaces it.
- Modify: `lib/crates/fabro-server/src/web_auth.rs`:
  - `:337` authorize URL: read from `Extension<Arc<GithubEndpoints>>`, use `endpoints.oauth_base.join("login/oauth/authorize")`.
  - `:428` token exchange: `endpoints.oauth_base.join("login/oauth/access_token")`.
  - `:474` user fetch: `endpoints.api_base.join("user")`.
  - `:509` emails fetch: `endpoints.api_base.join("user/emails")`.
- Modify: `lib/crates/fabro-server/tests/it/helpers.rs` — test-app builders accept an optional `GithubEndpoints` override; default (when not overridden) is production.
- Test: `lib/crates/fabro-server/src/auth/github_endpoints.rs` (inline tests).

**Approach:**
- **No public config field.** The settings schema is unchanged. Operators cannot override these URLs from `settings.toml` or env. The only way to override is via the test-only constructor in test code.
- `GithubEndpoints` is an Axum Extension, which means handlers pull it from the request extensions rather than from settings. Clean separation of concerns: production code path is unconditional; tests configure the stack at the router-build boundary.
- When GHES / Google IdPs land (deferred scope), a proper settings surface for this override can be added with validation (HTTPS-only, allowlist, etc). For now, production has exactly one set of GitHub URLs — as hardcoded today — with no surface for an attacker to misdirect.
- Use `Url::join()` not string concatenation — avoids trailing-slash footguns and validates shape.

**Patterns to follow:**
- Existing Axum Extension injection patterns in `build_router_with_options` at `server.rs:904`.
- Existing `helpers.rs::test_app_state_with_options` for accepting per-test config overrides.

**Test scenarios:**
- Happy path: `GithubEndpoints::production_defaults()` yields the two URLs equal to what's currently hardcoded.
- Happy path: `GithubEndpoints::with_bases("http://127.0.0.1:12345".parse()?, "http://127.0.0.1:12345/api".parse()?)` yields those URLs.
- Edge case: `Url::join` with trailing-slash base produces a correctly single-slashed URL (regression guard for concatenation bugs).
- Integration: `web_auth::login_github` in test mode with an injected `GithubEndpoints` redirects to the injected oauth base, not to github.com. Confirm by inspecting the 302 `Location` header.

**Verification:**
- All existing `web_auth.rs` tests pass (default behavior unchanged).
- Integration tests in Phase 5 successfully redirect Fabro's OAuth calls to twin-github.

---

- [ ] **Unit 7: `twin-github` OAuth + user endpoints (prerequisite for integration tests)**

**Goal:** Extend `test/twin/github` with the four endpoints Fabro's `web_auth.rs` calls during the OAuth flow, so integration tests exercise the real glue end-to-end instead of stubbing session cookies.

**Requirements:** R15

**Dependencies:** none

**Files:**
- Create: `test/twin/github/src/handlers/oauth.rs` — `GET /login/oauth/authorize` (auto-approve or configurable denial → redirect to `redirect_uri?code=<fake>&state=<state>`), `POST /login/oauth/access_token` (validate `client_id`/`client_secret`/`code`, return `{access_token, token_type, scope}`).
- Create: `test/twin/github/src/handlers/users.rs` — `GET /user` and `GET /user/emails`, returning fixture user/emails from state.
- Modify: `test/twin/github/src/handlers/mod.rs` — register the four new routes.
- Modify: `test/twin/github/src/state.rs` — extend `AppState` with `oauth_codes: HashMap<String, OauthCode>`, `oauth_tokens: HashMap<String, OauthSubject>`, `seeded_user: GithubUser`, `seeded_emails: Vec<GithubEmail>`, `allow_authorize: bool` (for denial simulation).
- Modify: `test/twin/github/src/fixtures.rs` — default seeded user + emails.
- Modify: `test/twin/github/src/test_support.rs` — builder method `.with_oauth_user(user)` to seed before the test runs.
- Test: `test/twin/github/src/handlers/oauth.rs` (inline `#[cfg(test)] mod tests`); cross-crate integration tests land in Phase 5.

**Approach:**
- Codes are single-use; exchanging invalidates the entry.
- No token refresh, no scope validation. GitHub's OAuth doesn't rotate by default anyway.
- Denial path: when `allow_authorize=false`, redirect to `redirect_uri?error=access_denied&state=<state>`.

**Patterns to follow:**
- Existing handler shape in `test/twin/github/src/handlers/*.rs` — `State(state): State<SharedState>`, `state.read().await` / `state.write().await`.

**Test scenarios:**
- Happy path: authorize returns a code; exchanging the code with the correct client_secret returns an access token; `/user` with that token returns the seeded fixture.
- Error path: wrong client_secret on token exchange → 400 `invalid_client`.
- Edge case: replay an already-exchanged code → 400 `invalid_grant`.
- Error path: `allow_authorize=false` → authorize redirects with `error=access_denied`.
- Integration (within twin): full round-trip `authorize → exchange → /user → /user/emails` using a single seeded subject.

**Verification:**
- Twin-github's own test suite passes; twin is ready to be consumed by Fabro integration tests in Phase 5.

### Phase 2 — Auth internals (store + JWT)

- [ ] **Unit 8: JWT issue + verify (`auth/jwt.rs`)**

**Goal:** HS256 JWT encode and verify with strict algorithm pinning, `iss`/`aud`/`exp`/`iat` validation, and 5 s clock-skew tolerance.

**Requirements:** R2, R4, R11

**Dependencies:** Unit 4, Unit 5

**Files:**
- Create: `lib/crates/fabro-server/src/auth/jwt.rs` — `Claims` struct (all fields in origin §Token model — `iss`, `aud`, `sub`, `exp`, `iat`, `jti`, `idp_issuer`, `idp_subject`, `login`, `name`, `email`, `auth_method`); `issue(key, subject_snapshot, ttl) -> String`; `verify(key, expected_iss, token) -> Result<Claims, JwtError>`; `JwtError` enum with variants `AccessTokenExpired`, `AccessTokenInvalid`.
- Modify: `lib/crates/fabro-server/src/auth/mod.rs` — re-export `Claims`, `issue`, `verify`, `JwtError`.
- Test: `lib/crates/fabro-server/src/auth/jwt.rs` (inline `#[cfg(test)] mod tests`).

**Approach:**
- `Header` explicitly pinned to `HS256`; `Validation` constructed with `algorithms = vec![HS256]`.
- Parse header before signature verify; reject any other `alg` (including `alg: none`).
- **Clock-skew tolerance window is 5 seconds** (`Validation::leeway = 5`). Revised down from 30 seconds — fabro-server is single-node with NTP, not federated multi-region. A 5-second window accommodates normal clock drift without extending stolen-token replay windows unnecessarily.
- `aud = "fabro-cli"` hardcoded constant. `iss` passed in by caller (the server's public URL).
- **Reject any token with a `kid` header field** for now (no multi-key support in v1); forward-closes algorithm-confusion and key-selection abuse until we explicitly design multi-key rotation.

**Patterns to follow:**
- `jsonwebtoken` crate's `encode()` + `decode::<Claims>()` shape. Existing usage in `fabro-github` shows the crate's ergonomics for this repo.

**Test scenarios:**
- Happy path: round-trip encode → verify yields the original claims.
- Error path: `alg: none` header → `verify` returns `AccessTokenInvalid` before any signature comparison (guard against algorithm-confusion attack).
- Error path: `alg: RS256` → rejected.
- Error path: `exp` 10 s past now → `AccessTokenExpired`.
- Happy path: `iat` 3 s in the future → accepted (within 5 s skew).
- Error path: `iat` 10 s in the future → `AccessTokenInvalid`.
- Error path: header contains `kid` → `AccessTokenInvalid`.
- Error path: `iss` mismatch → `AccessTokenInvalid`.
- Error path: `aud` mismatch → `AccessTokenInvalid`.
- Edge case: signature tampered (valid header, valid claims, bad MAC) → `AccessTokenInvalid`.
- Integration (with Unit 4): key derived from a given master produces verifiable tokens; changing the HKDF context label invalidates all tokens.

**Verification:**
- All tests pass. `jti` is a valid UUIDv4 in emitted tokens.

---

- [ ] **Unit 9: `AuthCode` SlateDB store + reaper**

**Goal:** Store authorization codes in SlateDB with 60 s TTL so they survive server restart. `consume` is single-use via a per-code in-process mutex (fabro-server is single-node — the in-process mutex is the right primitive). Reaper task prunes expired entries.

**Requirements:** R1, R10

**Dependencies:** Unit 6 (for `IdpIdentity`)

**Files:**
- Create: `lib/crates/fabro-store/src/slate/auth_codes.rs` — typed SlateDB module parallel to `auth_tokens.rs` (Unit 10):
  - `AuthCode { identity: IdpIdentity, login: String, name: String, email: String, code_challenge: String, redirect_uri: String, expires_at: DateTime<Utc> }` (uses the `IdpIdentity` type from `fabro-types` — no bare `idp_issuer`/`idp_subject` strings).
  - `SlateAuthCodeStore` struct wrapping `Arc<slatedb::Db>` + `DashMap<String, Arc<tokio::sync::Mutex<()>>>` (per-code in-process mutex map, mirroring Unit 10's pattern for refresh tokens).
  - Public methods: `insert(&self, code: &str, entry: AuthCode) -> Result<()>`, `consume(&self, code: &str) -> Result<Option<AuthCode>>` (single-use via per-code mutex — see Approach), `gc_expired(&self, cutoff: DateTime<Utc>) -> Result<u64>`.
- Modify: `lib/crates/fabro-store/src/slate/mod.rs` — declare the new module; expose `auth_codes()` accessor from `Database` returning `Arc<SlateAuthCodeStore>`.
- Modify: `lib/crates/fabro-store/src/keys.rs` — add `SlateKey::auth_code(code)` constructor and `SlateKey::auth_code_prefix()` for GC scans.
- Modify: `lib/crates/fabro-server/src/auth/mod.rs` — re-export `AuthCode` via a thin façade.
- Modify: `lib/crates/fabro-server/src/serve.rs` — spawn the reaper task alongside `auth_tokens` GC; reaper invokes `auth_codes.gc_expired(now)` every 30 s; cancel on graceful shutdown.
- Test: `lib/crates/fabro-store/src/slate/auth_codes.rs` (inline `#[cfg(test)] mod tests`) — backed by `object_store::memory::InMemory`.

**Approach:**
- Code value is `base64url(32 bytes from OsRng)` — opaque server-internal identifier; the CLI treats it as a string.
- **`consume` is single-use.** Implementation, mirroring Unit 10's per-hash mutex pattern:
  ```
  let mutex = mutex_map.entry(code.to_string()).or_insert_with(|| Arc::new(Mutex::new(()))).value().clone();
  let _guard = mutex.lock().await;
  // Critical section (holding the per-code mutex):
  let row = slatedb.get(auth_code_key(code)).await?;
  let entry = row.map(decode)?;
  if let Some(ref entry) = entry {
      if entry.expires_at <= now { return Ok(None); }  // expired
      slatedb.delete(auth_code_key(code)).await?;
  }
  return Ok(entry);
  ```
  Second caller within the same node blocks on the mutex, reads `None` after the first caller's delete, returns `None`. **Mutex cleanup:** same `Arc::strong_count == 2` check as Unit 10, never remove under the mutex.
- Reaper scans `SlateKey::auth_code_prefix()` every 30 s and deletes rows with `expires_at <= now`.
- Values encoded as `serde_json::to_vec` to match existing repo convention.

**Patterns to follow:**
- `lib/crates/fabro-store/src/slate/auth_tokens.rs` (Unit 10) — parallel pattern for refresh tokens; `auth_codes` mirrors the shape.
- `lib/crates/fabro-store/src/slate/run_store.rs` for SlateDB CRUD.

**Test scenarios:**
- Happy path: insert → consume returns the entry; second consume returns `None`.
- Edge case: insert → wait 61 s (via mocked time) → consume returns `None`.
- **Integration — concurrent consume:** `tokio::task::JoinSet` with N=16 tasks all calling `consume(code)` on the same code → exactly one task receives `Some(entry)`; all others receive `None`. Asserts the per-code mutex + delete-under-mutex pattern holds.
- Happy path: reaper run removes expired entries but preserves unexpired ones (table-driven).
- Integration (with Unit 13): reaper is cancelled cleanly on shutdown — no leaked task.

**Verification:**
- Tests pass; no clippy warnings on the new module.

---

- [ ] **Unit 10: `RefreshToken` + `SlateAuthTokenStore` with atomic `consume_and_rotate`**

**Goal:** SlateDB-backed refresh-token persistence with `consume_and_rotate` as the only rotation primitive. Atomic via an in-process per-hash mutex (fabro-server is single-node — the in-process mutex is the correct primitive). No idempotency grace — network failures during response delivery force re-login (see Key Technical Decisions).

**Requirements:** R2, R3

**Dependencies:** Unit 2 (workspace deps), Unit 6 (`IdpIdentity` type)

**Files:**
- Modify: `Cargo.toml` (workspace root) — add `dashmap = "6"` to `[workspace.dependencies]` (for the per-hash mutex map inside the store; extends Unit 2's scope).
- Create: `lib/crates/fabro-store/src/slate/auth_tokens.rs` — typed module parallel to `run_store.rs` and `catalog.rs`:
  - `RefreshToken` struct with fields: `token_hash: [u8;32]`, `chain_id: Uuid`, `identity: IdpIdentity`, `login: String`, `name: String`, `email: String`, `issued_at: DateTime<Utc>`, `expires_at: DateTime<Utc>`, `last_used_at: DateTime<Utc>`, `used: bool`, `user_agent: String`. `identity` uses the typed wrapper from Unit 6; the non-empty invariant flows through serde via `#[serde(try_from = ...)]`.
  - `SlateAuthTokenStore` struct wrapping `Arc<slatedb::Db>` + `DashMap<[u8;32], Arc<tokio::sync::Mutex<()>>>` for per-hash critical sections. No `recently_rotated` map (grace dropped for v1).
  - `ConsumeOutcome { Rotated(RefreshToken /* old */, RefreshToken /* new */), Reused(RefreshToken /* old */), Expired, NotFound }`.
  - Public methods: `consume_and_rotate(&self, presented_hash, new_token, now) -> Result<ConsumeOutcome>`, `insert_refresh_token(&self, token) -> Result<()>` (primary insertion at first login), `find_refresh_token(&self, token_hash) -> Result<Option<RefreshToken>>`, `delete_chain(&self, chain_id) -> Result<u64>`, `gc_expired(&self, cutoff) -> Result<u64>`.
- Modify: `lib/crates/fabro-store/src/slate/mod.rs` — declare the new module; expose `auth_tokens()` accessor from `Database` (returns `Arc<SlateAuthTokenStore>` — match existing `runs()` pattern).
- Modify: `lib/crates/fabro-store/src/keys.rs` — add `SlateKey::auth_refresh(token_hash)` constructor and `SlateKey::auth_refresh_prefix()` for scans (keeps `SlateKey` encapsulated inside `fabro-store`).
- Modify: `lib/crates/fabro-server/src/auth/mod.rs` — re-export `RefreshToken`, `ConsumeOutcome` via a thin façade so handlers don't need to import from `fabro-store` directly.
- Modify: `lib/crates/fabro-server/src/serve.rs` — obtain `Arc<SlateAuthTokenStore>` from `Database` at startup; spawn the GC reaper every 6 h with a 7-day grace window.
- Test: `lib/crates/fabro-store/src/slate/auth_tokens.rs` (inline `#[cfg(test)] mod tests`) — backed by `object_store::memory::InMemory` (existing pattern in `fabro-store`).

**Approach:**
- **Concrete struct, not a trait.** `SlateAuthTokenStore` has no abstraction over the backing store.
- **Singleton ownership model:** `AuthServices { refresh_tokens: Arc<SlateAuthTokenStore>, ... }`. Every caller obtains the store via `Arc::clone(&services.refresh_tokens)` — sharing, not duplication. The DashMap lives inside the inner type, so every `Arc::clone` handle sees the same concurrency state.
- **Per-hash mutex acquisition uses DashMap's atomic entry API:**
  ```
  let mutex = mutex_map.entry(hash).or_insert_with(|| Arc::new(Mutex::new(()))).value().clone();
  let _guard = mutex.lock().await;
  ```
  (Conceptual — actual code uses the dashmap entry ref + value().clone() pattern; the plan names it so implementers don't accidentally write the racy get-then-insert variant.)
- **Mutex cleanup:** after the critical section completes, attempt to remove the entry only if `Arc::strong_count == 2` (one from the map + one from the local `mutex` binding). Never remove under the mutex (implementation detail; documented inline).
- **Critical section (holding the per-hash mutex):**
  1. `find_refresh_token(presented_hash)` → if `None` → `NotFound`.
  2. If `now >= expires_at` → `Expired`.
  3. If `used == true` → `Reused(old)`. Do NOT mutate. The caller invokes `delete_chain(old.chain_id)` — which prefix-scans for every row sharing the chain, including this used row and every subsequent rotation. This is the theft-detection path; the row must remain queryable by `find_refresh_token` for it to fire, which is why step 4 does NOT delete the old row.
  4. Else perform the rotation: **mark old `used = true, last_used_at = now`; insert new row.** The old row stays in SlateDB (with `used=true`) so that a subsequent replay presentation still hits step 3 and triggers theft detection. GC (`gc_expired`) purges used rows after `expires_at + 7d`. Return `Rotated(old, new)`.
- **Multi-key write atomicity.** SlateDB exposes a `write(WriteBatch)` API for atomic multi-key commits. The rotation is now two writes (mark old `used=true` + insert new row); a single `WriteBatch` commits them atomically, eliminating the narrow crash window between them. If the implementer finds `WriteBatch` doesn't fit, fall back to sequential ops — a crash between mark-old and insert-new leaves `used=true` with no successor, which fails-closed on next presentation (replay is treated as theft; the one-time legitimate retry also forces re-login). Acceptable fail-mode.
- Values encoded as `serde_json::to_vec` to match existing repo convention.
- `delete_chain(chain_id)` is a prefix scan + filter (no secondary index in v1).

**Patterns to follow:**
- `lib/crates/fabro-store/src/slate/run_store.rs` for SlateDB CRUD shape.
- `lib/crates/fabro-store/src/slate/mod.rs` for prefix-scan iteration and `Database::runs()` accessor pattern.
- `dashmap::DashMap::entry().or_insert_with()` + `.value().clone()` on an `Arc<Mutex<()>>` is the standard atomic-upsert idiom.

**Test scenarios:**
- Happy path: `insert_refresh_token` then `find_refresh_token` round-trips.
- Happy path: `consume_and_rotate` with a valid token returns `Rotated(old, new)`; the new token is findable; **the old token is still findable via `find_refresh_token` but now has `used=true`** (critical: required for theft detection on replay).
- Error path: `consume_and_rotate` with unknown hash → `NotFound`.
- Error path: `consume_and_rotate` with expired token → `Expired`.
- Error path: `consume_and_rotate` with `used==true` token → `Reused(old)`; not mutated further (caller calls `delete_chain`).
- **Error path — replay after successful rotation (the theft-detection invariant):** rotate once; re-call `consume_and_rotate` with the SAME `presented_hash` → returns `Reused(old)` with the old row's `chain_id`. This is the critical test — before the fix, this path returned `NotFound` because the old row had been deleted, silently collapsing theft detection to look-like-expiry.
- **Integration — concurrent rotation (critical):** `tokio::task::JoinSet` with N=32 tasks all calling `consume_and_rotate` on the same presented token → exactly one sees `Rotated(old, new)`; all other 31 see `Reused(old)`. Chain deletion happens once (driven by the handler; see Unit 15).
- **Integration — singleton sharing:** build `AuthServices`; clone the `Arc<SlateAuthTokenStore>`; assert two tasks using different clone handles but contending on the same hash still see exactly one `Rotated`. Explicit regression guard against accidental per-clone DashMaps.
- **Serde invariant guard (flows from Unit 6):** deserialize a crafted `RefreshToken` JSON with `identity.issuer = ""` → serde error, NOT a successfully-reconstituted invalid row. Validates that the `#[serde(try_from)]` invariant on `IdpIdentity` flows into `RefreshToken` via standard derive.
- Happy path: `delete_chain(chain_id)` removes all rows sharing that chain and returns their count.
- Happy path: `gc_expired(now - 7d)` removes rows older than the cutoff; preserves newer.

**Verification:**
- All tests pass, including the concurrent-rotation integration test (run with `cargo nextest run -p fabro-server --test-threads=8` to maximize contention).

---

- [ ] **Unit 11: Extend `jwt_auth` extractor with JWT bearer path**

**Goal:** Extend `AuthenticatedService` / `AuthenticatedSubject` to accept an HS256 JWT bearer alongside the existing dev-token bearer and session cookie.

**Requirements:** R2, R4, R8

**Dependencies:** Unit 5, Unit 6, Unit 8

**Files:**
- Modify: `lib/crates/fabro-server/src/jwt_auth.rs` — extend bearer dispatch with the following decision chain (order matters):
  1. If bearer starts with `fabro_dev_` → dev-token validation (existing path, unchanged).
  2. Else if bearer starts with `fabro_refresh_` → log `INFO` with the route path ("refresh token presented at protected endpoint") as a diagnostic signal for CLI bugs; return `ApiError::unauthorized_with_code(..., "unauthorized")`. Do NOT attempt JWT parsing.
  3. Else attempt JWT parse via Unit 8's `verify()`. On success: produce `VerifiedAuth { idp_issuer, idp_subject, login, auth_method: Github, credential_source: JwtAccessToken }`. On `JwtError::AccessTokenExpired`: `ApiError::unauthorized_with_code(..., "access_token_expired")`. On `JwtError::AccessTokenInvalid`: `ApiError::unauthorized_with_code(..., "access_token_invalid")`. On parse failure (not a valid JWT structure): `ApiError::unauthorized_with_code(..., "unauthorized")`.
- Modify: `lib/crates/fabro-server/src/jwt_auth.rs` — extend `CredentialSource` enum with `JwtAccessToken`.
- Test: `lib/crates/fabro-server/src/jwt_auth.rs` (inline `#[cfg(test)] mod tests`).

**Approach:**
- `iss` compared against the server's resolved public URL (pulled from `AppState`).
- Bearer dispatch uses explicit prefix checks for known-shaped tokens and JWT parse fallthrough for everything else — more robust than sniffing the base64 header prefix `eyJ` (which is format-dependent and future-fragile).
- The `fabro_refresh_` branch emits an explicit diagnostic log so CLI bugs that pass the wrong bearer become visible (otherwise they'd look like an opaque 401 to the user).

**Patterns to follow:**
- Existing dev-token bearer validation at `jwt_auth.rs:99-145` (constant-time compare).

**Test scenarios:**
- Happy path: valid JWT bearer → extractor produces `VerifiedAuth` with correct `idp_issuer`/`idp_subject`.
- Happy path: dev-token bearer still works (regression guard).
- Happy path: session cookie still works (regression guard).
- Error path: expired JWT → 401 `ApiError` with `code="access_token_expired"`.
- Error path: JWT with bad signature → 401 `access_token_invalid`.
- Error path: JWT with `alg: none` header → 401 `access_token_invalid` (no crash on crafted input).
- Error path: refresh-token-shaped bearer (`fabro_refresh_...`) on a protected endpoint → 401 `unauthorized` AND an `INFO` log entry with the path (diagnostic for CLI bugs).
- Error path: garbage bearer that happens to start with `eyJ` but is not a valid JWT → 401 `unauthorized` (not `access_token_invalid` — we distinguish malformed from tampered).
- Error path: empty bearer → 401 `unauthorized`.
- Integration: JWT issued by Unit 8 with key from Unit 4 verifies successfully end-to-end.

**Verification:**
- All existing protected-endpoint tests still pass; new JWT-bearer tests pass.

### Phase 3 — Server endpoints

- [ ] **Unit 12: `/auth/login/github` and `/auth/callback/github` accept `return_to` and preserve it on error paths**

**Goal:** Thread a strictly-whitelisted `return_to` query param through the existing GitHub OAuth flow, including on callback error paths (allowlist rejection, token-exchange failure, user-fetch failure).

**Requirements:** R9, R10

**Dependencies:** none

**Files:**
- Modify: `lib/crates/fabro-server/src/web_auth.rs` — `login_github` (`:285`) accepts `?return_to=<path>`; validate against regex `^/auth/cli/(start|resume)$` (reject anything else with `WARN` log of the redacted value, treat as absent). Store `return_to` in the existing `fabro_oauth_state` cookie alongside the state token. **Extend the `fabro_oauth_state` cookie `max_age` from 10 min to 30 min** to cover slow GitHub authorize flows (MFA, org approval, user delay).
- Modify: `lib/crates/fabro-server/src/web_auth.rs` — `callback_github` (`:359`), error-path matrix:
  - **State-cookie missing/expired** (`:377-380`): the plan cannot trust `return_to` (we have no signed state to read it from). Render a plain HTML error page "Your login took too long or was tampered with. Please start again." and log WARN with the request-id. Do NOT redirect anywhere derived from query params.
  - **Config errors** (`session_key`/`web_url`/`client_id` resolution): existing JSON 409s stay (these are admin-facing, never CLI-facing).
  - **Success**: read `return_to` from state cookie, redirect there (default `/runs` when absent).
  - **Allowlist rejection** (`:522-526`): if state cookie carries CLI `return_to`, redirect to `<return_to>?error=unauthorized&error_description=Login%20not%20permitted&state=<preserved>`; no session minted. If no CLI `return_to`, preserve existing `/login?error=unauthorized`.
  - **GitHub token-exchange or user-fetch failure**: same shape, `error=server_error&error_description=Could%20not%20complete%20GitHub%20sign-in`.
  - **User denial at GitHub** (`?error=access_denied` in callback query string): forward as `<return_to>?error=access_denied&error_description=Authorization%20denied`.
  - Every `error_description` value is a **closed enumeration of server-authored strings**, never a forwarded-from-upstream value (defense against log/terminal injection — see Key Technical Decisions).
- Modify: `lib/crates/fabro-server/src/web_auth.rs` — extend `OauthStateCookie` struct (look near `add_oauth_state_cookie`) to carry `return_to: Option<String>`.
- Test: `lib/crates/fabro-server/tests/it/api/` — add `cli_auth_return_to.rs` for end-to-end checks via `tower::ServiceExt::oneshot`.

**Approach:**
- `return_to` whitelist is strict: must be an absolute path starting with `/`, must match the exact regex. Any other value is ignored and logged at WARN (with redaction of the raw value to avoid log pollution).
- Error-path redirect only occurs when a CLI `return_to` is present on the state cookie — existing non-CLI behavior is preserved.
- Every CLI-visible error is mapped from the server-side cause to one of a closed set of `{error, error_description}` pairs. Inbound attacker-controlled query strings never flow through verbatim.

**Patterns to follow:**
- `add_oauth_state_cookie` / `remove_oauth_state_cookie` around `web_auth.rs:348-353` show the cookie-signing pattern.

**Test scenarios:**
- Happy path: `login_github?return_to=/auth/cli/resume` → state cookie carries the return_to; success callback redirects to `/auth/cli/resume`.
- Error path: `return_to=http://evil.com` → treated as absent, WARN logged, redirect goes to default `/runs`.
- Error path: `return_to=/` (not a CLI path) → treated as absent.
- Error path: allowlist rejection with CLI return_to → 302 to `<return_to>?error=unauthorized&error_description=Login+not+permitted&state=...`, no session minted.
- Error path: simulated GitHub token-exchange failure (via Unit 6b's override pointing at a failing twin-github) → 302 to `<return_to>?error=server_error&...`.
- Error path: callback query carries `?error=access_denied` (user clicked Cancel at GitHub) → forwarded to CLI via `<return_to>?error=access_denied&...`.
- **Error path — state cookie missing**: browser hits `/auth/callback/github` with valid GitHub code+state but no `fabro_oauth_state` cookie → plain HTML error page, CLI loopback times out (documented behavior — not a new redirect target).
- **Error path — state cookie expired**: extend max_age to 30 min means this is less common; test verifies a 35-min-delayed callback still fails cleanly (plain HTML; CLI timeout).
- **Error path — inbound error_description from attacker**: an attacker crafts `/auth/callback/github?error=access_denied&error_description=<script>...</script>...&state=<valid>` → server returns `error=access_denied&error_description=Authorization%20denied` (the server-authored string), NOT the attacker string. Sanitization test.
- Regression: non-CLI login (no `return_to`) — existing happy path and rejection paths unchanged.

**Verification:**
- All tests pass; existing GitHub OAuth tests unchanged.

---

- [ ] **Unit 13: `/api/v1/auth/cli/config` preflight handler**

**Goal:** Implement the unauthenticated preflight endpoint that tells the CLI whether OAuth login is available and, if so, which origin to open in the browser.

**Requirements:** R5, R6, R12

**Dependencies:** Unit 1, Unit 5

**Files:**
- Create: `lib/crates/fabro-server/src/auth/cli_flow.rs` — `config()` handler; registers under `/api/v1/auth/cli/config`. Always mounted (regardless of `web.enabled`), as it lives on the API router.
- Modify: `lib/crates/fabro-server/src/server.rs` — add route registration in `api_common` (the API sub-router defined at `server.rs:902`).
- Modify: `lib/crates/fabro-server/src/auth/mod.rs` — re-export the handler.
- Test: `lib/crates/fabro-server/tests/it/api/cli_auth_config.rs` (new).

**Approach:**
- Read `AuthMode` and `server.web` settings from `AppState`.
- `enabled = web.enabled && auth.methods.contains(Github)`.
- When `enabled`: `{enabled: true, web_url: server.web.url, methods: [...]}` with `reason` absent.
- When `!enabled`: `web_url: null`, `reason ∈ {"github_not_enabled", "web_not_enabled"}`. The CLI renders the user-visible description from the enum value locally — no `reason_description` on the wire.
- **Mount-site split** (implementation footgun): `config()` registers in `api_common` (unauthenticated preflight, always mounted). Units 14 and 15's `/auth/cli/*` handlers register in the `/auth` nest (only when `web.enabled`). The `auth/cli_flow.rs` module exports two route-set functions: `api_routes()` for Unit 13's preflight, and `web_routes()` for Units 14/15's browser + token endpoints. `server.rs` calls each at its correct mount point. This keeps the file unified while making the mount asymmetry explicit.

**Patterns to follow:**
- Existing unauthenticated-handler shape in `web_auth.rs:auth_config` (`:278`).
- Existing `api_routes()` / `routes()` split in `web_auth.rs:97`, `:105`.

**Test scenarios:**
- Happy path: `web.enabled=true`, `methods=[github]` → `{enabled: true, web_url: "http://...", methods: ["github"]}` (no `reason` key).
- Error path: `methods=[dev-token]` → `{enabled: false, web_url: null, methods: ["dev-token"], reason: "github_not_enabled"}`.
- Error path: `web.enabled=false`, `methods=[dev-token]` (no startup conflict since github is absent) → `{enabled: false, web_url: null, methods: ["dev-token"], reason: "web_not_enabled"}`.
- Edge case: IP allowlist enabled + request from a non-allowlisted IP → 403 (allowlist runs before this handler; regression guard).
- **Integration — mount split**: boot with `web.enabled=false` (no github); verify `GET /api/v1/auth/cli/config` returns 200 with `enabled=false`, while `GET /auth/cli/start` returns 404 (not mounted).
- Integration: OpenAPI conformance test confirms the serialized shape matches the spec added in Unit 1.

**Verification:**
- Tests pass; conformance test passes.

---

- [ ] **Unit 14: `/auth/cli/start` and `/auth/cli/resume` browser-flow handlers**

**Goal:** Implement the two browser-consumed endpoints of the CLI OAuth flow, including strict `redirect_uri` validation, session eligibility gate, error passthrough on `/resume`, and `github_auth_not_configured` HTML short-circuit.

**Requirements:** R5, R9, R10, R12, R13

**Dependencies:** Unit 4, Unit 9, Unit 12, Unit 13

**Files:**
- Modify: `lib/crates/fabro-server/src/auth/cli_flow.rs` — add `start()` and `resume()` handlers; single static HTML helper `static_error_page(title: &'static str, body: &'static str) -> Response` that emits server-authored text only (no query-value echoing) with `Content-Type: text/html; charset=utf-8`, `X-Content-Type-Options: nosniff`, `Cache-Control: no-store`. Callers pass `&'static str` pairs — the `'static` lifetime bound is load-bearing: it makes echoing untrusted runtime data into the HTML a compile-time error. Constants for the five cases: `GITHUB_NOT_CONFIGURED`, `INVALID_REDIRECT_URI`, `INVALID_OR_MISSING_STATE`, `MISSING_FLOW_COOKIE`, `LOGIN_SUCCESSFUL`.
- Modify: `lib/crates/fabro-server/src/server.rs` — register both routes under the existing `/auth` nest (only mounted when `web.enabled`).
- Modify: `lib/crates/fabro-server/src/auth/cli_flow.rs` — private (authenticated-encryption) `fabro_cli_flow` cookie helpers via `cookie::PrivateJar` with the cookie key from Unit 4, matching the existing `__fabro_session` pattern. Cookie attributes: `HttpOnly; SameSite=Lax; Path=/auth; MaxAge=10min; Secure` (Secure relaxed only for `http://127.0.0.1` dev deployments, detected via `server.web.url.starts_with("https://")` — same pattern as existing `session_cookie_secure` helper at `web_auth.rs:215`).
- Test: `lib/crates/fabro-server/tests/it/api/cli_auth_browser.rs` (new).

**Approach:**
- **Session eligibility gate** (used by `/start` and `/resume`, exhaustively pattern-matched on Unit 6's typed `Option<IdpIdentity>`):
  ```
  match session.and_then(|s| s.identity.as_ref()) {
      Some(idp) => eligible,
      None => ineligible,
  }
  ```
  No empty-string checks; the type system enforces the invariant.
- `/auth/cli/start` query validation:
  1. If `github` not in `methods` → static HTML error (template `GithubAuthNotConfigured`). NOT a flat JSON envelope.
  2. `redirect_uri` must match `http://127.0.0.1:<port>/callback` or `http://[::1]:<port>/callback`. Else → static HTML error (`InvalidRedirectUri`) — the redirect target cannot be trusted, so no redirect.
  3. `state` is 16–512 URL-safe chars. Missing/malformed → static HTML error (`InvalidOrMissingState`).
  4. `code_challenge` present; `code_challenge_method` = `S256`. Missing/wrong → 302 to `redirect_uri?error=invalid_request&error_description=<server-authored>&state=<state>`.
- Eligible session: mint `AuthCode` (Unit 9), 302 to `redirect_uri?code=<code>&state=<state>`.
- Ineligible or absent session: set `fabro_cli_flow` cookie with `{redirect_uri, state, code_challenge}` (10 min TTL, attributes above); 302 to `/auth/login/github?return_to=/auth/cli/resume`.

- `/auth/cli/resume` algorithm (order matters — enforced by tests):
  1. If `github` not in `methods` → static HTML error (`GithubAuthNotConfigured`).
  2. Read `fabro_cli_flow` cookie. If missing/expired → static HTML error (`MissingFlowCookie`).
  3. **Error passthrough** (before any session check): if query carries `?error=<code>`, map the inbound `error` value to a closed enum (valid values: `unauthorized`, `server_error`, `access_denied`; any other → `server_error`). Clear `fabro_cli_flow`, 302 to `redirect_uri?error=<mapped_error>&error_description=<server-authored fixed string for that code>&state=<state from cookie>`. Do NOT inspect `__fabro_session`. **Never forward the inbound `error_description` verbatim** — the server authors the user-visible text (closed set).
  4. Apply session eligibility gate. If ineligible: clear `fabro_cli_flow`, 302 to `redirect_uri?error=github_session_required&error_description=<server-authored>&state=<state>`.
  5. Mint `AuthCode` keyed to the session's `(identity.issuer, identity.subject, login, name, email, code_challenge, redirect_uri)`; clear `fabro_cli_flow`; 302 to `redirect_uri?code=<code>&state=<state>`.

**Patterns to follow:**
- `remove_cookie` + `add_cookie` pair at `web_auth.rs:549-561`.
- `session_cookie_secure` helper at `web_auth.rs:215` (reuse for Secure-flag detection).

**Test scenarios:**
- Happy path (/start): eligible session → 302 to `redirect_uri?code=&state=`; code is a fresh entry in the `AuthCodeStore`.
- Happy path (/start): no session → `fabro_cli_flow` cookie set with the exact attributes above; 302 to `/auth/login/github?return_to=/auth/cli/resume`.
- Happy path (/resume): after a valid GitHub callback, session is present + eligible → 302 to `redirect_uri?code=&state=`.
- Error path (/start): invalid `redirect_uri` host → static HTML error (status 400, HTML content-type, `X-Content-Type-Options: nosniff` set).
- Error path (/start): missing `state` → static HTML error.
- Error path (/start): missing `code_challenge` → 302 with `error=invalid_request`.
- Error path (/start): `github` not in methods → static HTML error, not JSON.
- Error path (/resume): inbound `?error=unauthorized` (from allowlist rejection per Unit 12) → 302 to `redirect_uri?error=unauthorized&error_description=<server-authored>&state=...` regardless of session state. **Assert ordering explicitly** — dev-token session must NOT mask this as `github_session_required`.
- Error path (/resume): inbound `?error=<unknown_code>` → mapped to `server_error`; inbound `?error_description=<script>` → NOT echoed (server-authored description used instead).
- Error path (/resume): dev-token session (identity=None) → 302 to `redirect_uri?error=github_session_required&...`.
- Error path (/resume): `fabro_cli_flow` cookie missing → static HTML error.
- **Cookie attribute test**: on HTTPS deployment, `fabro_cli_flow` Set-Cookie includes `HttpOnly`, `Secure`, `SameSite=Lax`, `Path=/auth`, `Max-Age=600`. On HTTP-loopback dev, `Secure` is relaxed.
- **HTML error injection test**: submit crafted `redirect_uri=http://127.0.0.1/%3Cscript%3E…` → static error page contains no echoed query values; response headers include `X-Content-Type-Options: nosniff`.
- Integration (IP allowlist): allowlist enabled + non-allowlisted IP → 403 on both `/start` and `/resume` (no carve-out).

**Verification:**
- All tests pass; `session-cookie-injection` helper extended (or inline pattern) covers the v2 cookie shape.

---

- [ ] **Unit 15: `/auth/cli/token`, `/auth/cli/refresh`, `/auth/cli/logout` JSON handlers**

**Goal:** Implement the three JSON endpoints that the CLI calls over its normal transport (HTTP/HTTPS/Unix socket), using the flat RFC 6749 error envelope.

**Requirements:** R2, R3, R6, R8, R12, R13

**Dependencies:** Unit 8, Unit 9, Unit 10, Unit 11, Unit 13 (creates `auth/cli_flow.rs`)

**Files:**
- Modify: `lib/crates/fabro-server/src/auth/cli_flow.rs` — `token()`, `refresh()`, `logout()` handlers; shared flat-error helper `oauth_error(status, code, description) -> Response`.
- Modify: `lib/crates/fabro-server/src/server.rs` — register POST routes under `/auth/cli/{token,refresh,logout}` (only mounted when `web.enabled`).
- Modify: `lib/crates/fabro-server/src/auth/mod.rs` — `AuthServices { jwt_key, refresh_tokens: Arc<SlateAuthTokenStore>, auth_codes: Arc<SlateAuthCodeStore>, settings }` bundle; inject as Axum `Extension`.
- Test: `lib/crates/fabro-server/tests/it/api/cli_auth_token.rs` (new).

**Approach:**
- **`token()`** (POST):
  1. `github_auth_not_configured` pre-handler short-circuit → 403 flat OAuth `github_auth_not_configured`.
  2. Parse JSON body `{grant_type, code, code_verifier, redirect_uri}`. Anything missing → 400 `invalid_request`.
  3. `grant_type != "authorization_code"` → 400 `invalid_request`.
  4. `auth_codes.consume(code)` → `None` → 400 `invalid_code` (covers missing, expired, already-used).
  5. `SHA256(code_verifier) != stored.code_challenge` → 400 `pkce_verification_failed`.
  6. `stored.redirect_uri != presented redirect_uri` → 400 `redirect_uri_mismatch`.
  7. `stored.identity` is typed `IdpIdentity` from Unit 6 — invariant is compile-time + serde-validated; no runtime empty-string check needed. Behind a `debug_assert!(false, "unreachable")`, release builds return 500 `server_error` as defense-in-depth.
  8. Allowlist re-check on `stored.login` → 403 `unauthorized` if removed. **Known limitation: allowlist is login-keyed (`allowed_usernames`), so GitHub login reuse — user deletes account, attacker registers freed username — could rematch under a new identity. Documented in §Security properties as an accepted v1 risk.**
  9. Generate 32-byte refresh secret + `chain_id = Uuid::new_v4()`; build `RefreshToken` row with `user_agent = <sanitized Request User-Agent header>`; `auth_tokens.insert_refresh_token(row)`.
  10. Issue JWT (Unit 8); return 200 with:
      ```json
      {
        "access_token": "eyJ...",
        "access_token_expires_at": "2026-04-19T14:30:00Z",
        "refresh_token": "fabro_refresh_...",
        "refresh_token_expires_at": "2026-05-19T14:20:00Z",
        "subject": {
          "idp_issuer": "https://github.com",
          "idp_subject": "12345",
          "login": "bhelmkamp",
          "name": "Bryan Helmkamp",
          "email": "bryan@qlty.ai"
        }
      }
      ```
      `subject` is a structured object (not a string) carrying the full identity snapshot the CLI persists and displays. `login`, `name`, `email` come from the `AuthCode` row (which captured them at `/resume` time from the session cookie). The CLI uses these fields verbatim in `AuthEntry.subject` (Unit 16) and in the `fabro auth login` success message (Unit 18).
- **`refresh()`** (POST):
  1. `github_auth_not_configured` → 403 flat OAuth.
  2. Bearer in `Authorization: Bearer fabro_refresh_...`; strip prefix; SHA-256 the remainder.
  3. Pre-build the new `RefreshToken` row (same `chain_id`, `expires_at = now + 30d`, `user_agent` from request headers, sanitized).
  4. `auth_tokens.consume_and_rotate(hash, new_row, now)` → branch on outcome:
     - `NotFound | Expired` → 401 `refresh_token_expired`.
     - `Reused(old)` → `delete_chain(old.chain_id)`, `WARN` log with `chain_id`, `idp_subject`, `user_agent_fingerprint` (never the raw UA — see Logging invariant), 401 `refresh_token_revoked`.
     - `Rotated(old, new)` → continue.
  5. Allowlist re-check against `old.login` → if removed, `delete_chain(old.chain_id)`, 403 `unauthorized`.
  6. Issue new JWT for `new`; return 200 with same shape as `/token`.
- **`logout()`** (POST):
  1. `github_auth_not_configured` → 403 flat OAuth `github_auth_not_configured` (pre-handler short-circuit).
  2. Parse bearer; hash; `auth_tokens.find_refresh_token(hash)`; if found, `delete_chain(row.chain_id)`.
  3. Return 204 regardless (no oracle for the presented-token-validity question).

**Logging invariant (key technical decision):**
- Never log raw bearer values, raw auth codes, or `code_verifier`.
- Never log raw `user_agent` strings — they're attacker-controlled and can carry ANSI escape sequences (terminal injection for operators tail-ing logs) or newlines (log-splitting). Log a `user_agent_fingerprint = hex(sha256(user_agent)[..8])` instead — stable for correlation, inert as payload. SHA-256 is already in the workspace; no new dependency. If the raw UA is needed for support debugging, route it through `fabro_util::redact` which strips control characters.
- User email is logged at INFO only on `/token` (login event) and `/logout` (explicit exit event); not on `/refresh` rotations.
- Enforced by review at implementation time and documented in the module's top-level doc comment.

**Patterns to follow:**
- Existing handler signature patterns for state/extension extraction in `server.rs:2771` (`list_runs`) and `web_auth.rs:285` (`login_github`).
- Generate 32-byte secrets via `OsRng::fill_bytes`, encode with `base64::engine::general_purpose::URL_SAFE_NO_PAD`.
- Existing `fabro_util::redact` patterns for any case where bearer-adjacent data must appear in logs.

**Test scenarios:**
- Happy path (/token): valid code + verifier → 200 with access+refresh; refresh row exists in store.
- Error path (/token): wrong `code_verifier` → 400 `pkce_verification_failed`; code is burned (retry with correct verifier still fails).
- **Security property — PKCE is the secondary defense for stolen codes:** a caller presenting a valid code with a WRONG `code_verifier` → 400 `pkce_verification_failed`. Belt-and-suspenders alongside the single-use consume primitive.
- Error path (/token): replay same code twice → second attempt → 400 `invalid_code`.
- Error path (/token): code expired (manipulate clock) → 400 `invalid_code`.
- Error path (/token): allowlist removed between `/resume` and `/token` → 403 `unauthorized`.
- Happy path (/refresh): valid refresh → 200 with rotated pair; old token no longer valid.
- **Error path — /refresh replay after successful rotation:** valid refresh, then replay the OLD token (simulates a CLI that retries due to network failure after the server already rotated) → 401 `refresh_token_revoked`; chain deleted; WARN logged. The CLI's user-facing error message instructs running `fabro auth login` again.
- **Integration — concurrent refresh:** N=32 simultaneous `/refresh` calls with the same token → exactly one 200; the other 31 see 401 `refresh_token_revoked`; chain deleted. (No idempotency grace in v1 — parallel CLI invocations sharing the same token accept one-wins-all-others-kicked-out as expected behavior.)
- Error path (/refresh): deallowlisted user → 403 `unauthorized`, chain deleted.
- Edge case (/logout): valid refresh → 204, chain deleted.
- Edge case (/logout): unknown refresh → 204 (no oracle).
- Error path (all three): `github` not in methods → 403 flat OAuth `github_auth_not_configured` on each.
- Error path (/logout): specifically, the 403 short-circuit takes precedence over the "always 204" rule. Explicit test.
- **Logging regression test**: all three endpoints complete a full cycle including a reuse-detection WARN emission; assert `tracing` buffer contains no substring matching `fabro_refresh_`, `eyJ` (JWT prefix), or the raw `code_verifier`; also assert the WARN entry contains no ANSI escape bytes or newlines (even when the incoming request's `User-Agent` header contains them — the fingerprint-only policy strips them by construction).
- Integration: round-trip `/token` → protected API call with JWT → sleep past access TTL → `/refresh` → second protected call with new JWT → all succeed.

**Verification:**
- All tests pass. Flat-error envelope verified by-shape (not `ApiError` envelope).

### Phase 4 — CLI

- [ ] **Unit 16: CLI `AuthStore` + `ServerTargetKey` normalization (Unix-only v1, HTTP/HTTPS/Unix-socket targets)**

**Goal:** Local credential storage at `~/.fabro/auth.json` keyed by a canonical form of the CLI target. Supports all three transport variants the CLI already handles: HTTPS URL, loopback HTTP URL, Unix socket path. Unix: mode 0600 atomic writes + `fs2` advisory lock. Windows: unsupported in v1 — `fabro auth login` returns a clear error.

**Requirements:** R6, R7, R14

**Dependencies:** Unit 2

**Files:**
- Create: `lib/crates/fabro-cli/src/auth_store.rs`:
  - `AuthStore { path: PathBuf }` with methods `default()`, `get(&ServerTargetKey)`, `put(&ServerTargetKey, entry)`, `remove(&ServerTargetKey)`, `list()`.
  - `ServerTargetKey` newtype **wrapping the existing `fabro_cli::user_config::ServerTarget` enum** (`HttpUrl { api_url, tls }` | `UnixSocket(PathBuf)` at `user_config.rs:71-78`). Normalizes on construction:
    - `HttpUrl`: lowercase scheme + host, strip default port, strip trailing slash → canonical form `https://host:port` or `http://host:port`.
    - `UnixSocket(path)`: canonicalize via `std::fs::canonicalize` when the path exists, otherwise accept as-is; canonical form `unix://<absolute-path>`.
  - `ServerTargetKey::Display` emits the canonical string used as the JSON key in `auth.json`'s `servers` map.
  - `AuthEntry { access_token: String, access_token_expires_at: DateTime<Utc>, refresh_token: String, refresh_token_expires_at: DateTime<Utc>, subject: Subject, logged_in_at: DateTime<Utc> }`.
  - `Subject { idp_issuer: String, idp_subject: String, login: String, name: String, email: String }` — structured identity snapshot, mirrors the `/auth/cli/token` response body's `subject` object (Unit 15). Provides `login`/`name`/`email` to `fabro auth login` for the success message and `fabro auth status` for the per-server block.
- On non-Unix targets: `AuthStore::new()` constructs successfully but **write methods** (`put`, `remove`) return `Err(AuthStoreError::UnsupportedPlatform)`. **Read methods** (`get`, `list`) still work: on Windows there's no existing `auth.json` file to read so `list()` returns empty and `get()` returns `None` — which is exactly what `fabro auth status` needs to report "no OAuth logins on this server." The dev-token path continues to work cross-platform (dev-token file write is a separate existing code path, not changed by this unit). This preserves R14 (status works offline) while gating OAuth writes behind the Windows non-goal.
- Modify: `lib/crates/fabro-cli/src/lib.rs` or `main.rs` — declare the new module.
- Test: `lib/crates/fabro-cli/src/auth_store.rs` (inline `#[cfg(test)] mod tests`).

**Approach:**
- All mutating ops on Unix: acquire exclusive lock on `~/.fabro/auth.lock` via `fs2::FileExt::try_lock_exclusive` (with blocking fallback), read file, mutate, atomic temp+rename, release.
- Read ops: shared lock, brief.
- **Unix protection:** atomic temp+rename with mode 0600 at creation (matching `dev_token.rs`); trust the filesystem for pre-existing files.
- **Windows:** `AuthStore::new()` succeeds; `put`/`remove` return `UnsupportedPlatform`; `get`/`list` return empty results (there's no Unix-created `auth.json` on Windows to read). Preserves R14.
- **NFS detection (Unix only):** when `fs2::try_lock_exclusive` returns `EOPNOTSUPP` or `ENOLCK`, surface a typed error pointing at `FABRO_AUTH_FILE`. Do NOT silently fall back to unlocked writes.
- **Unix-socket key canonicalization** avoids the trap that the existing CLI uses `http://fabro` as a synthetic base URL for HTTP-over-Unix-socket (`server_client.rs:359`). That synthetic base must NOT leak into `auth.json` — two different socket paths would collapse to the same key. `ServerTargetKey` keys on the actual `ServerTarget` enum variant + its canonical contents, not on the synthetic base URL.
- **No top-level `version` field.** When a v2 schema is needed, adding any new key signals the migration.

**Patterns to follow:**
- `lib/crates/fabro-util/src/dev_token.rs:66-117` for atomic write + mode 0600.
- `lib/crates/fabro-cli/src/user_config.rs:71-148` for `ServerTarget` parsing; reuse the existing parser, don't duplicate.

**Test scenarios:**
- Happy path (Unix, HTTPS): `put` then `get` round-trips for `ServerTargetKey::from(ServerTarget::HttpUrl { api_url: "https://fabro.example.com", .. })`.
- Happy path (Unix, loopback HTTP): round-trip for `http://127.0.0.1:3000`.
- Happy path (Unix, Unix socket): round-trip for `ServerTarget::UnixSocket("/var/run/fabro.sock".into())`.
- Edge case: HTTPS normalization — `https://EXAMPLE.COM/`, `https://example.com:443`, `https://example.com` all collide on the same key.
- **Edge case: two distinct Unix-socket paths do NOT collide.** `UnixSocket("/a.sock")` and `UnixSocket("/b.sock")` produce distinct keys; regression guard against keying by synthetic `http://fabro` base URL.
- **Edge case: canonicalize symlinked socket paths.** `UnixSocket("/var/run/fabro.sock")` and a symlink to the same target collapse to one key (when both exist).
- Edge case: corrupt file (invalid JSON) → `AuthStore::get` returns a clear error with the file path; does not panic.
- Edge case: missing file → `list()` returns empty; `get` returns `None`.
- **Integration — concurrent puts (Unix):** two `AuthStore::put` calls from different threads against the same path; after both complete, the file contains one of the two values (not a corrupt merge). `tokio::task::JoinSet`-driven.
- **Unix: file written with mode 0600.** Explicit stat-check after `put`.
- **NFS simulation (Unix):** inject a mocked filesystem that returns `EOPNOTSUPP` on `try_lock_exclusive` → `AuthStore::put` returns `LockError::FilesystemDoesNotSupportLocking`.
- **Windows platform gate:** on a Windows build target, `AuthStore::new()` succeeds; `list()` returns empty; `put(...)` returns `AuthStoreError::UnsupportedPlatform` with an actionable message.

**Verification:**
- All tests pass; all three transport variants keyed distinctly; Unix code path fully covered; Windows path is a typed error.

---

- [ ] **Unit 17: `fabro auth` namespace + clap dispatch**

**Goal:** Wire the `Auth` top-level command group into clap + dispatch.

**Requirements:** R1

**Dependencies:** Unit 16

**Files:**
- Modify: `lib/crates/fabro-cli/src/args.rs` — add `Commands::Auth(AuthNamespace)` variant; define `AuthNamespace { command: AuthCommand }` and `AuthCommand::{Login(AuthLoginArgs), Logout(AuthLogoutArgs), Status(AuthStatusArgs)}`; define the three args structs with their flags.
- Create: `lib/crates/fabro-cli/src/commands/auth/mod.rs` — `dispatch(ns, cli_settings, cli_layer, printer) -> Result<()>` matches the `AuthCommand` variants and calls the right subcommand.
- Create: stubs at `lib/crates/fabro-cli/src/commands/auth/login.rs`, `logout.rs`, `status.rs` (implemented in Units 18–20).
- Modify: `lib/crates/fabro-cli/src/main.rs` — add the `Commands::Auth(ns)` dispatch branch (near `Commands::Provider` at `:326`).
- Test: `lib/crates/fabro-cli/tests/` — snapshot-driven `--help` test to verify the command surface appears as expected.

**Approach:**
- Flags per origin §CLI UX: `auth login [--server] [--no-browser] [--timeout]`, `auth logout [--server] [--all]`, `auth status [--server] [--json]`.
- `AuthNamespace` mirrors `ProviderNamespace` at `args.rs:1437`.

**Patterns to follow:**
- `lib/crates/fabro-cli/src/commands/provider/mod.rs` for the dispatcher shape.

**Test scenarios:**
- Happy path: `fabro auth --help` shows `login`, `logout`, `status`.
- Happy path: `fabro auth login --help` lists the three flags.
- Happy path: `fabro auth logout --all` parses to `AuthLogoutArgs { server: None, all: true }`.
- Edge case: unknown subcommand → clap error exit code.
- Integration: insta snapshot the `--help` outputs for the namespace and each subcommand.

**Verification:**
- Snapshots stable; `cargo nextest run -p fabro-cli` green.

---

- [ ] **Unit 18: `fabro auth login` — preflight + browser flow + token exchange**

**Goal:** Implement the happy path of `fabro auth login`, reusing `fabro-oauth`'s PKCE/loopback/browser-open helpers and the CLI's normal transport for preflight and token exchange.

**Requirements:** R1, R5, R6, R9

**Dependencies:** Unit 1, Unit 13, Unit 14, Unit 15, Unit 16, Unit 17

**Files:**
- Modify: `lib/crates/fabro-cli/src/commands/auth/login.rs` — implement the happy path.
- **Modify: `lib/crates/fabro-oauth/src/lib.rs` — add `start_callback_server_with_errors(expected_state, port, callback_path) -> (CallbackHandle, oneshot::Receiver<CallbackResult>)`** where `CallbackResult = Ok(CallbackSuccess { code })` or `Err(CallbackFailure { error_code, error_description })`. The new function fires the oneshot **only when the state parameter matches `expected_state`** — either `code` (success) or server-emitted `error`+`error_description` (failure). State-mismatch requests return HTTP 400 with a plain-HTML error to the browser and do NOT fire the oneshot (preserves the existing robustness property that stray local probes can't abort an in-progress login). The CLI-side `--timeout` handles the case where no legitimate callback ever arrives.
- Modify: `lib/crates/fabro-oauth/src/lib.rs` inline tests — cover: normal success (state match + code), normal error forwarding (state match + error), state mismatch (400 HTML, oneshot still alive after the stray request), concurrent race (legitimate callback arrives after a bad-state probe — legitimate one still wins).
- Create: `lib/crates/fabro-cli/src/auth_store/loopback_target.rs` — `fn is_loopback_or_unix_socket(target: &ServerTarget) -> Result<LoopbackClassification, TargetSchemeError>`. Uses `url::Url::host()` + `std::net::IpAddr::is_loopback()`, NOT string matching. Rejects literal `localhost` (DNS-overridable), decimal/hex/octal encodings, any host with dots after a loopback prefix. Unix socket targets return the third classification variant.
- Test: `lib/crates/fabro-cli/src/commands/auth/login.rs` (inline tests for URL construction, PKCE pair verification, loopback-check semantics, HTTPS-or-loopback-or-unix enforcement).

**Approach:**
1. Resolve target server URL (flag > `FABRO_SERVER` env > `settings.toml`, existing resolution).
2. **Platform gate:** on Windows, return `"CLI OAuth login is not supported on Windows in this release. Use WSL, or use a dev-token server."` and exit non-zero. This is the only Windows-specific line in the login flow (see Unit 16).
3. Preflight: `GET {target}/api/v1/auth/cli/config` via `cli_http_client_builder().no_proxy()` (mandatory per `AGENTS.md`). If `enabled=false`, render the local message for the `reason` enum value (see Unit 13 for the mapping) and exit non-zero.
4. Browser URL origin: always `config.web_url` from the preflight response. No `--browser-url` flag in v1 (deferred; see Scope Boundaries).
5. Generate PKCE pair (verifier + S256 challenge) and CSRF state via `fabro_oauth::generate_pkce()` + `generate_state()`.
6. Bind loopback listener via the new `fabro_oauth::start_callback_server_with_errors(expected_state, port, "/callback")`.
7. Build browser URL using `config.web_url` + `/auth/cli/start` path + query params.
8. Unless `--no-browser`: `open::that(url)`. Otherwise print the URL (headless devs still need to complete it in some browser).
9. Wait for loopback callback (with `--timeout`, default 5 min). On callback:
   - `CallbackFailure { error_code, error_description }`: render in browser "Login failed: <locally-rendered message for error_code>" page, shut down listener, exit non-zero.
   - `CallbackSuccess { code }`: render "Logged in. You can close this tab." page; shut down listener.
10. **HTTPS-or-loopback-or-unix-socket enforcement for the token POST.** Before `POST {target}/auth/cli/token`, call `is_loopback_or_unix_socket(target)`:
    - `Https` → proceed.
    - `LoopbackHttp` → proceed.
    - `UnixSocket` → proceed.
    - `Rejected` → refuse with `"Refusing to send refresh-token credentials over plaintext HTTP to a non-loopback host ({target}). Use HTTPS, or bind the server to 127.0.0.1 / ::1."` Exit non-zero. This check rejects `http://127.0.0.1.evil.com`, `http://localhost.attacker.com`, decimal/hex encoded loopback, and literal `localhost` (see §Key Technical Decisions for the precise rule).
11. `POST {target}/auth/cli/token` with `{grant_type, code, code_verifier, redirect_uri}`. Parse the flat OAuth response.
12. On success: `AuthStore::put` the new entry; print `✓ Logged in to <server> as <login> (<name> <email>)`.

**Execution note:** Start with a failing integration test that drives the full loopback flow against twin-github.

**Patterns to follow:**
- `lib/crates/fabro-cli/src/commands/provider/login.rs` for the command signature.
- `cli_http_client_builder().no_proxy()` per `AGENTS.md` testing-strategy.
- `fabro-oauth`'s existing `run_browser_flow` for the browser-open + listener pattern; use `start_callback_server_with_errors` for the listener.

**Test scenarios:**
- Happy path (unit): PKCE verification — `SHA256(verifier) == code_challenge` (base64url no-pad).
- Happy path (unit): URL construction — given `web_url` and flow params, produce a canonical URL string (insta snapshot).
- Happy path (unit): loopback callback delivers success → token POST proceeds.
- Error path (unit): loopback callback delivers `error=github_session_required` → command exits non-zero with the locally-rendered message for that error code.
- **Regression (unit): state-mismatch probe does NOT abort login.** Start a listener with `expected_state = "abc"`; make a stray GET to the loopback with `state=wrong&error=evil` — oneshot does not fire, returns 400 HTML. Then make a legitimate callback with `state=abc&code=ok` — oneshot fires with `CallbackSuccess`. Command proceeds.
- Error path (unit): preflight returns `{enabled: false, reason: "github_not_enabled"}` → command exits 1; stderr carries the locally-rendered message for that enum value.
- Error path (unit): preflight returns `{enabled: false, reason: "web_not_enabled"}` → command exits 1 with the appropriate message.
- Error path (unit): preflight returns an unknown `reason` value (future server forward-compat) → command exits 1 with a generic "CLI login is not available on this server" message (don't crash on unknown enum values).
- Error path (unit): `--timeout 2s` elapsed with no callback → "login did not complete within 2s" error.
- **Error path (unit) — HTTPS-or-loopback-or-unix enforcement matrix:** table-driven.
  - `https://fabro.example.com` → accept.
  - `http://127.0.0.1:3000` → accept (loopback).
  - `http://[::1]:3000` → accept.
  - `http://[::ffff:127.0.0.1]:3000` → accept (IPv4-mapped loopback).
  - `unix:///run/fabro.sock` → accept.
  - `http://fabro.example.com` → reject.
  - `http://127.0.0.1.evil.com` → reject (parses as public DNS name).
  - `http://localhost` → reject (DNS-overridable).
  - `http://localhost.evil.com` → reject.
  - `http://2130706433` → reject (decimal encoding).
  - `http://0x7f000001` → reject (hex encoding).

**Verification:**
- Unit tests pass. Integration covered in Unit 22.

---

- [ ] **Unit 19: `fabro auth logout` — remote revocation + local clear**

**Goal:** Revoke the refresh-token chain server-side and clear the local credential entry. `--all` clears every server.

**Requirements:** R1

**Dependencies:** Unit 15, Unit 16, Unit 17

**Files:**
- Modify: `lib/crates/fabro-cli/src/commands/auth/logout.rs` — implement.
- Test: `lib/crates/fabro-cli/src/commands/auth/logout.rs` (inline tests).

**Approach:**
1. If `--all`: iterate `AuthStore::list()`, call `/auth/cli/logout` for each, then remove each entry. Collect errors; fail-local-open (remove local entries even if remote POST fails).
2. Else (default): resolve target server; `AuthStore::get(server)` → if entry exists, POST `/auth/cli/logout` with bearer; remove from `AuthStore`. If remote POST fails, still remove locally and print a WARN that remote revocation didn't succeed and the refresh token may remain valid until its natural expiry.
3. If no entry exists: print "not logged in to <server>", exit 0 (not an error).
4. **Cross-platform:** on Windows, `AuthStore::list()` returns empty and `get()` returns `None` (see Unit 16), so `--all` is a no-op and the default path falls through to "not logged in." No Windows-specific error — logout is effectively always a no-op on Windows.

**Patterns to follow:**
- Same `cli_http_client_builder().no_proxy()` pattern.
- `fabro_http::HttpClient` POST shape (existing usage in `fabro-oauth/src/lib.rs`).

**Test scenarios:**
- Happy path: `fabro auth logout` → remote 204 + local entry removed.
- Happy path (`--all`): two entries logged out → both removed.
- Error path: no entry for target server → prints "not logged in", exit 0.
- Error path: remote POST fails (network error mocked) → local entry still removed; stderr carries WARN.
- Edge case: server returns 403 `github_auth_not_configured` (shouldn't happen if we successfully logged in earlier, but defensive) → local entry still removed.
- Integration (cross-crate, deferred to Unit 21): end-to-end with twin-github.

**Verification:**
- Unit tests pass.

---

- [ ] **Unit 20: `fabro auth status` — local-only formatted status**

**Goal:** Display per-server login state, running entirely against local files (no server calls).

**Requirements:** R14

**Dependencies:** Unit 16, Unit 17

**Files:**
- Modify: `lib/crates/fabro-cli/src/commands/auth/status.rs` — implement.
- Test: `lib/crates/fabro-cli/src/commands/auth/status.rs` — insta snapshot for text and JSON outputs.

**Approach:**
- Read `AuthStore::list()`. Also probe for dev-token presence (existing CLI search order: env > `<storage_dir>/server.dev-token` > `~/.fabro/dev-token`) for display completeness.
- State per entry:
  - `active` if `access_token_expires_at > now`.
  - `expired (refreshable)` if access expired but `refresh_token_expires_at > now`.
  - `expired` if both expired.
  - `revoked (re-login required)` — we can't detect this locally; reserved for a future when the server can annotate.
- Text output mirrors origin §CLI UX.
- `--json`: structured per-server payload.
- `--server <url>`: filter to one.
- **Cross-platform:** Works on Windows. `AuthStore::list()` on Windows returns empty (no Unix-created auth.json); dev-token detection works cross-platform. Typical Windows output: "No OAuth logins (Windows). Dev-token: <active|not set>." Preserves R14.

**Patterns to follow:**
- Existing `printer` usage in `lib/crates/fabro-cli/src/commands/provider/mod.rs`.

**Test scenarios:**
- Happy path: multi-server AuthStore → text output renders blocks per origin §CLI UX (insta snapshot).
- Happy path: `--json` → stable-order JSON (insta snapshot).
- Happy path: `--server` filter → only matching server shown.
- Edge case: empty AuthStore, no dev-token → "not logged in to any servers" message.
- Edge case: dev-token present but no JWT for the same server → shows dev-token block.
- Edge case: access expired but refresh still valid → state is "expired (refreshable)".
- Edge case: both expired → state is "expired".
- **Edge case (Windows):** `AuthStore::list()` returns empty (Unit 16 platform gate) → status output shows dev-token state if present, plus a note that OAuth login is unavailable on this platform. Command exits 0. Regression guard for R14's cross-platform requirement.

**Verification:**
- Snapshots stable.

---

- [ ] **Unit 21: `server_client.rs` — bearer priority + auto-refresh**

**Goal:** Pick the right bearer per request, and transparently refresh the JWT on `access_token_expired` 401s.

**Requirements:** R7, R8

**Dependencies:** Unit 3, Unit 15, Unit 16

**Files:**
- Modify: `lib/crates/fabro-cli/src/server_client.rs` — bearer selection in a new helper `resolve_bearer(server_target, env, auth_store) -> Option<Bearer>`: env `FABRO_DEV_TOKEN` → `AuthStore` JWT (if not refresh-expired) → dev-token file fallback → `None`. Held as `Arc<RwLock<Option<Bearer>>>` inside `AuthedApiClient` so auto-refresh can update it atomically.
- Modify: `lib/crates/fabro-cli/src/server_client.rs` — **bearer-injection approach is rebuild-client-on-rotation** (not per-call, not middleware). Rationale: progenitor's generated `fabro_api::Client::new_with_client` takes a concrete `reqwest::Client` by value (see `fabro-http/src/lib.rs:16`, `pub type HttpClient = reqwest::Client`). `reqwest_middleware::ClientWithMiddleware` is a distinct type that does NOT implement `Into<reqwest::Client>` — middleware-based injection would require changing the progenitor client generation boundary, which is out of scope. Per-call reconstruction (rebuild `fabro_api::Client` before every operation) thrashes the connection pool. The middle ground: `AuthedApiClient` holds `Arc<RwLock<Arc<fabro_api::Client>>>`. The inner Arc is replaced atomically only when the bearer rotates. During normal calls there's no reconstruction; only refresh triggers a swap. In-flight calls captured the old Arc before the swap and keep using it (their 401 response triggers another refresh which is no-ops past the rotation; bounded retry prevents loops).
- Modify: `lib/crates/fabro-cli/src/server_client.rs` — `AuthedApiClient { inner: Arc<RwLock<Arc<fabro_api::Client>>>, bearer: Arc<RwLock<Option<Bearer>>>, auth_store: Arc<AuthStore>, target: ServerTarget }`. Method shim for each wrapped operation:
  1. Load current client Arc: `let client = self.inner.read().await.clone()`.
  2. Call `fabro_api::Client::<op>(&*client, ...)`.
  3. On `ApiFailure { status: 401, code: Some("access_token_expired"), .. }`:
     a. Acquire bearer write lock (so only one refresh runs at a time).
     b. Double-check: if the bearer held under the lock is DIFFERENT from the one this call captured, another concurrent refresh already happened — skip refresh, just rebuild the local `client` and retry.
     c. Else call `refresh_access_token` → update bearer → rebuild `fabro_api::Client` with new default-header → swap `self.inner` → retry once.
  4. Retry is bounded at 1.
- Modify: `lib/crates/fabro-cli/src/server_client.rs` — add `map_api_error_structured<E>(err: progenitor_client::Error<E>) -> ApiFailure` returning `ApiFailure { status, code: Option<String>, detail: String }`. **Refactor existing `map_api_error` to delegate**: `map_api_error(err) -> anyhow::Error { let f = map_api_error_structured(err); anyhow!(f.detail) }`. One parser, two consumers; no drift risk.
- Modify: `lib/crates/fabro-cli/src/server_client.rs` — dedicated refresh helper `refresh_access_token(target: &ServerTarget, refresh_token: &str, auth_store: &AuthStore) -> Result<AuthEntry, RefreshError>`. Takes `ServerTarget` (not `server_url`/`&Url`) so the HTTP-URL and Unix-socket transport variants both work — the helper dispatches to an HTTP `fabro_http::HttpClient` for `HttpUrl` targets, and to the existing Unix-socket `fabro_http::HttpClient` builder for `UnixSocket` targets (same pattern as the rest of `server_client.rs`). POSTs to `/auth/cli/refresh` directly (not via progenitor — flat OAuth envelope isn't in the spec); parses the flat `{access_token, refresh_token, ...}` shape.
- `RefreshError` variants: `Expired`, `Revoked`, `NonHttpsTarget`, `Network(anyhow::Error)`. On `Expired`/`Revoked`, the helper also removes the local entry from `AuthStore` — keyed by `ServerTargetKey::from(target)` (Unit 16's canonical form) so removal hits the right entry regardless of how the target was specified.
- Modify: `lib/crates/fabro-cli/src/server_client.rs` — loopback/unix-socket check reuses the same `is_loopback_or_unix_socket` helper introduced in Unit 18 (no duplication). Takes `&ServerTarget`. Refuses `ServerTarget::HttpUrl` with non-HTTPS, non-loopback host; accepts `ServerTarget::UnixSocket` unconditionally.
- Test: `lib/crates/fabro-cli/src/server_client.rs` (inline tests).

**Approach:**
- **Reactive-only auto-refresh, no pre-flight.** The CLI refreshes on 401 `access_token_expired` and retries once. Previous plan proposed clock-based pre-flight; dropping it removes clock-sync assumptions and halves the refresh paths.
- Refresh failures (`Expired` / `Revoked`) produce a stable stringy `anyhow::Error` with `session expired. Run 'fabro auth login'.` so existing `main.rs` error-formatting paths continue to work.
- Retry is bounded at 1 — no infinite loops.
- All `fabro_http::HttpClient` creation uses `.no_proxy()`.

**Patterns to follow:**
- `apply_bearer_token_auth` at `:336` for the `reqwest::Client::builder().default_headers(...)` pattern. The new wrapper invokes this helper each time it rebuilds the inner `fabro_api::Client` on bearer rotation.
- `fabro-oauth`'s refresh-token-parsing shape for the flat OAuth envelope.

**Test scenarios:**
- Happy path (unit): bearer priority — env > JWT > dev-token > none, table-driven.
- Happy path (unit): `map_api_error_structured` extracts `code` when present; returns `None` when absent.
- Happy path (unit): `map_api_error` is a thin wrapper — identical `anyhow::Error` output for a given input before and after the refactor (regression guard).
- Happy path (integration, mocked): 401 with `code="access_token_expired"` → refresh called, retry succeeds.
- **Invariant test — "current bearer wins":** set up `AuthedApiClient` with an initial bearer `"A"`. Make a request; assert the outgoing request carries `Authorization: Bearer A`. Simulate a token rotation (update the `RwLock` to `"B"`). Make another request; assert the outgoing request carries `Authorization: Bearer B`. This is implementation-agnostic — catches any regression whether we pick middleware or client-reconstruction, and specifically catches the `default_headers`-baked-in bug where the stale bearer would persist.
- Edge case: 401 without `code` (non-JWT path, e.g. raw `unauthorized` from a protected endpoint) → no refresh attempt, error surfaces directly.
- Error path: refresh endpoint returns `refresh_token_expired` → caller sees the canonical `session expired` error; `AuthStore` entry for this server is removed.
- Error path: refresh endpoint returns `refresh_token_revoked` → same canonical message; entry removed.
- **Error path — loopback-rule enforcement for /refresh:** same matrix as Unit 18. `http://fabro.example.com` refused with `RefreshError::NonHttpsTarget`. `http://127.0.0.1.evil.com` refused. `https://fabro.example.com` and `http://127.0.0.1:3000` and `unix:///path` accepted.
- Integration: retry is bounded — if the first retry also returns 401, the error surfaces (no infinite loop).

**Verification:**
- All tests pass. `cargo clippy --workspace --all-targets -- -D warnings` clean.

### Phase 5 — End-to-end integration

- [ ] **Unit 22: End-to-end CLI + server + twin-github integration test**

**Goal:** Exercise the full flow from `fabro auth login` through `fabro run` with a JWT bearer and `fabro auth logout`, against a real `fabro-server` connected to `twin-github`.

**Requirements:** R1, R2, R3, R7, R8, R15

**Dependencies:** all prior units

**Files:**
- Create: `lib/crates/fabro-cli/tests/it/auth.rs` — end-to-end test using a real in-process `fabro-server` + `twin-github`, driving the browser callback via a test HTTP client in place of a real browser.
- Modify: `lib/crates/fabro-cli/tests/it/mod.rs` — declare the new module.
- Create or modify: `lib/crates/fabro-cli/tests/it/support/harness.rs` — helpers to boot the combined stack, seed twin-github's OAuth user, expose the CLI subprocess.

**Approach:**
- Spin up `twin-github` in-process.
- Spin up `fabro-server` with its `GithubEndpoints` Axum Extension overridden (via the test harness, per Unit 6b) to point at the twin's URLs.
- Run `fabro auth login --server <addr>` as a subprocess; from the parent test, intercept the browser-open URL and drive the request-response chain: `GET /auth/cli/start` → GitHub-login redirect → twin-github authorize → `GET /auth/callback/github` → `GET /auth/cli/resume` → loopback callback hit.
- Assert `auth.json` entry exists with non-empty JWT + refresh.
- Make a `fabro run` call against the server using the JWT bearer — assert success.
- Wait past access-token TTL (configure test TTL to 2 s via a test-only override, or mock clock). Make another call — assert auto-refresh happened (refresh endpoint hit exactly once).
- Run `fabro auth logout --server <addr>`; assert `auth.json` entry gone and subsequent `fabro run` fails with "session expired".

**Patterns to follow:**
- `lib/crates/fabro-cli/tests/it/` structure (check existing tests for subprocess-driving helpers and in-process server hosting).

**Test scenarios:**
- Happy path: full login → authenticated API call → auto-refresh → authenticated API call → logout → unauthenticated call fails.
- Integration: login with `twin-github.allow_authorize=false` → CLI exits non-zero with `access_denied` (user-denial path).
- Integration: canonical origin — `--server http://127.0.0.1:API_PORT` with `server.web.url = http://127.0.0.1:WEB_PORT` (different origins); assert browser URL is on WEB_PORT, not API_PORT. Token-exchange POST is on API_PORT.
- Integration: dev-token session cannot bootstrap — mint a dev-token session cookie, point the browser at `/auth/cli/start`, observe Case B routing (redirect to GitHub login), NOT Case A. Exact assertion on the 302 `Location` header.
- Integration: re-login to the same server. Server-side refresh row for the previous session continues to work until explicitly used or expired; local entry is overwritten. Origin-confirmed behavior.

**Verification:**
- Integration test green across the full flow.

**Execution note:** This unit is where the prerequisite (Unit 7, twin-github OAuth) pays off. If twin-github isn't complete, Unit 22 blocks.

## System-Wide Impact

- **Interaction graph:**
  - `jwt_auth.rs` extractor is called by every protected API handler; adding the JWT path is a hot-path change. Must preserve existing dev-token and session-cookie behavior.
  - `web_auth.rs::callback_github` is the sole minting site for `__fabro_session`; Unit 6's `SessionCookie` migration ripples to every reader.
  - `server_client.rs::apply_bearer_token_auth` is called by every CLI command that talks to the server; bearer-priority changes affect every command.
  - `AppState` gains `auth_services: Arc<AuthServices>` — construction code in `serve.rs` extends accordingly.
- **Error propagation:**
  - Protected routes: `VerifiedAuth` extraction failure → `ApiError` with optional `code` → CLI's `map_api_error_structured` interprets `code` → auto-refresh or surface error.
  - OAuth routes: errors are flat `{error, error_description}` and do not pass through `ApiError`; the CLI parses them directly.
  - Browser-flow errors: `?error=&state=` query-param handoff through the loopback.
- **State lifecycle risks:**
  - Mid-rotation crash: Unit 10 keeps the old row with `used=true` after rotation (it is NOT deleted — deletion would break theft detection on replay). The rotation is two writes: mark old `used=true` + insert new row. These are committed as a single `SlateDB::WriteBatch` where the API supports it; otherwise sequential writes with fail-closed semantics — a crash between mark-old and insert-new leaves the old row marked `used=true` with no successor, which fails-closed on next presentation (replay treated as theft; legitimate retry also forces re-login). GC purges used rows after `expires_at + 7d`.
  - Client-disconnected-after-rotate-success: the user re-logs in. Accepted UX cost in v1. See §Key Technical Decisions.
  - Auth codes live in SlateDB (Unit 9) and survive server restart. Codes are short-lived (60 s TTL); a server restart between `/resume` minting and `/token` consuming is tolerated as long as the restart completes within 60 s.
  - `AuthStore` file-lock: concurrent `fabro run` processes sharing the same store must not corrupt the JSON. Exclusive `fs2` lock around read-modify-write closes the race. NFS detection surfaces a typed error instead of silent last-writer-wins.
- **API surface parity:**
  - `fabro-api.yaml` gains the preflight endpoint; regenerated Rust + TS clients pick it up automatically.
  - OpenAPI spec does NOT add the five OAuth endpoints — they live outside the canonical API surface.
- **Integration coverage:**
  - Concurrent-refresh test (Unit 10 + Unit 15) proves the atomicity claim.
  - Canonical-origin test (Unit 22) proves the `--server` ≠ `web_url` case works.
  - Dev-token-session-bootstrap-blocked test (Unit 14 + Unit 22) proves the identity gate.
- **Unchanged invariants:**
  - Dev-token bearer auth continues to work exactly as today when configured.
  - Existing web session cookie (now v2) continues to authenticate browser API calls.
  - `/api/v1/*` existing endpoints are not re-authenticated — only the bearer dispatch grows.
  - Non-CLI callers of `/auth/login/github` (default web login) see no behavior change.

## Risks & Dependencies

| Risk | Mitigation |
|------|------------|
| Concurrent refresh race masked by test flakiness | Unit 10's integration test uses N=32 tasks on one `SlateAuthTokenStore`, asserts the outcome histogram (exactly one `Rotated`, 31 `Reused`), and exercises a two-Arc-clone scenario to guard against accidental per-clone DashMaps. |
| `SessionCookie` v1 → v2 migration + cookie-key derivation change forces global re-login | Acceptable; documented in plan; lands before CLI login GA. In-flight GitHub authorize flows at cutover also lose the `fabro_oauth_state` cookie and must restart — acceptable. |
| Browser-to-CLI error handoff depends on `return_to` preservation on the existing GitHub callback error path | Unit 12 explicitly covers error-path `return_to` preservation; integration test in Unit 22 exercises the allowlist-rejection path end-to-end. |
| Browser cannot reach `web_url` (dockerized dev stack, VPN/Tailscale mismatch, SSH-plus-browser-on-different-hosts) | v1 has no override. Preflight exposes the `web_url` so the CLI prints the exact URL it will open, making the failure mode at least visible. `--browser-url` deferred to a fast-follow (see Scope Boundaries). |
| Network failure between rotation success and response delivery forces user re-login | **Accepted UX cost in v1.** No idempotency grace — simpler semantics, smaller attack surface (the UA-based fingerprint that was previously proposed was spoofable and broke under NAT/LB/fabro-upgrade). If production telemetry shows frequent spurious re-logins, revisit with a stable per-install identifier instead of headers. |
| IdP-identity invariant bypassed via direct serde deserialization | Unit 6 uses `#[serde(try_from = "IdpIdentityWire")]` so `Deserialize` goes through the validating constructor. Regression tests explicitly deserialize crafted JSON with empty fields and assert the serde error path fires. Same attribute applies to `AuthCode` and `RefreshToken` via the shared type. |
| `AuthStore` file lock fails on NFS / sshfs | Unit 16 detects `EOPNOTSUPP`/`ENOLCK` and surfaces a typed error pointing at `FABRO_AUTH_FILE`. Does NOT silently fall back to unlocked writes. |
| Windows users cannot use `fabro auth login` | Documented non-goal in Scope Boundaries. CLI returns a clear message directing users to WSL or dev-token. Dropped DPAPI/ACL complexity that didn't clearly improve over plain 0600 for the same-user threat model. |
| `progenitor` client surface changes break `map_api_error_structured` | Parser is dependency-light (JSON `Value` traversal, not typed shape); existing `map_api_error` is refactored to delegate, removing drift risk. |
| Auto-refresh stale-bearer regression | Unit 21 pins the rebuild-on-rotation approach (`Arc<RwLock<Arc<fabro_api::Client>>>`) so the inner client swaps only when the bearer actually rotates; the double-check-under-lock pattern serializes concurrent refreshes. Adds an implementation-agnostic "current bearer wins" invariant test. |
| `twin-github` OAuth fidelity gap causes false-positive integration-test greens | Scope is explicitly limited (happy + denial + wrong-client-secret). Known limitation: no scope validation. Manual verification checklist covers real github.com. |
| `FABRO_SERVER` env var points at a stale server while `AuthStore` has an entry for a different server | `ServerTargetKey` normalization ensures the stored key matches lookups across all three transport variants (HTTPS URL, loopback HTTP URL, Unix-socket path); a mismatch falls through to the dev-token fallback (existing behavior). |
| Startup validation change introduces a break for existing deployments running `github + !web.enabled` | Survey current deployments before merging. If any real setups rely on this combination, introduce a deprecation warning first, then the hard error one release later. |
| 32-byte `SESSION_SECRET` minimum trips existing operators using shorter values | Startup error is clear and actionable; affected operators need to regenerate. Document cross-impact on both cookies and JWTs. |
| HTTPS-or-loopback-or-unix-socket refresh enforcement breaks a user running the server over HTTP on a remote host | Clear error message with scheme guidance. Documented as intentional — plaintext HTTP for refresh tokens is a silent-leak path. Check uses URL parsing + `IpAddr::is_loopback()`, not string matching — covers `127.0.0.1.evil.com`, decimal/hex encodings, DNS-shadowed `localhost`. |
| Refresh-token GC reaper never runs under sustained idle, leaving expired rows on disk | Reaper scheduled with `tokio::spawn` + `tokio::time::sleep`; resilient to idle. Cap disk use via 7-day grace window + eventual purge. |
| Inbound `error_description` injection from attacker-controlled OAuth redirects | Unit 12 + Unit 14 forward only server-authored strings (closed enumeration). Inbound `error_description` is never echoed. Terminal/log injection test in Unit 14. |
| Reflected XSS via `/auth/cli/start` HTML error page | Unit 14 renders only static server-authored text with `X-Content-Type-Options: nosniff`. Echoing of query values is prohibited and enforced at the type level via `&'static str` bound on template parameters. |
| Log/terminal injection via UA header on reuse-detection WARN | Unit 15 logs `user_agent_fingerprint = sha256(ua)[..8]` instead of raw UA. Regression test asserts no ANSI/newline bytes in log output even when request UA contains them. |
| Test-only GitHub URL override misused in production | `GithubEndpoints` is an Axum Extension, not a public config field. No path for an operator to override via `settings.toml` or env. When GHES support lands, a properly-validated public surface replaces the test-only injection. |
| `/auth/cli/start`/`/resume` state-mismatch probe from a stray local process aborts login | Unit 18's `start_callback_server_with_errors` fires the oneshot ONLY when the state parameter matches `expected_state`. Stray probes get 400 HTML; the real callback still arrives and wins. CLI's `--timeout` handles the case where no real callback ever arrives. |
| GitHub login reuse — user deletes account, attacker registers freed username — matches the allowlist under a new identity | **Accepted v1 risk.** The allowlist is login-keyed (`allowed_usernames`); a future (idp_issuer, idp_subject)-keyed allowlist would close this gap but requires a separate schema change. Document in admin docs: treat `allowed_usernames` as "identities that may currently log in," not "identities that have historically logged in." |

## Documentation Plan

- **`docs/authentication/`** (new if absent) — user-facing guide for `fabro auth login`: preflight explanation, per-server credentials, dev-token vs JWT coexistence, logout semantics.
- **`docs/administration/server-configuration.mdx`** — document the startup-validation preconditions for GitHub auth (web enabled, `SESSION_SECRET` ≥32 bytes), the preflight endpoint, and the `return_to` whitelist behavior.
- **`docs-internal/logging-strategy.md`** — review for any changes needed (new `WARN` log on reuse detection, new `INFO` on login/rotate/logout). Likely no doc change; follow the existing conventions.
- **`fabro-api.yaml`** — Unit 1 and Unit 3 update this directly.
- **`CHANGELOG.md` / release notes** — "CLI now supports OAuth login on GitHub-auth-only servers" + "v1 session cookies invalidated — users will need to log in again on web."
- **Inline module-level docs** — `lib/crates/fabro-server/src/auth/mod.rs` gets an opening doc comment summarizing the three credential kinds and the three envelope shapes.

## Operational / Rollout Notes

- **Feature flag:** not required. The new endpoints are only reachable when `github` is in `server.auth.methods`; existing deployments without github in methods see zero behavior change (beyond the v2 session cookie invalidation for web users).
- **Rollout sequence:**
  1. Deploy server with Phase 1–3 units. Web sessions force re-login (v2 cookie bump). No CLI-side changes yet.
  2. Ship CLI with Phase 4 units. `fabro auth login` becomes available.
  3. Run manual verification checklist against a real dev server.
- **Monitoring:**
  - `WARN` logs on refresh reuse detection — alertable signal of token theft or network-failure-induced replays. In v1 we cannot distinguish these two causes; investigate each WARN.
  - `INFO` logs on login/logout — observable usage; `/refresh` does NOT emit INFO-per-rotation (would be noisy; reuse WARN is the signal that matters).
  - Add a metric (or log-derivable count) for `/auth/cli/token` success rate, reuse-detection events, and `fabro_refresh_...` bearer presentations at protected endpoints (CLI-bug diagnostic).
- **Operator docs update required:** `SESSION_SECRET` now also signs JWTs. Rotating it invalidates all live access tokens and all web sessions simultaneously. Document this cross-impact; recommend a minimum length of 32 bytes (enforced at startup); note that independent rotation of cookie vs JWT keys is not supported in v1.
- **Rollback:** reverting the server is safe (no persistent schema changes that can't be ignored — SlateDB keyspace is additive). Reverting after users have `auth.json` entries means those entries are ignored and the CLI falls back to dev-token; no data loss.
- **Migration:** no data migration. `auth/refresh/` keyspace is new and empty at cutover. Old `__fabro_session` v1 cookies are simply not decodable as v2 and treated as absent. **Cookie-key migration note:** because Unit 4 switches the cookie derivation (from `Key::derive_from(raw)` to HKDF-Extract-and-Expand), even structurally-decodable v1 cookies would fail MAC verification — the v1 → v2 bump is doubly enforced. Any in-flight GitHub authorize flow at cutover loses its `fabro_oauth_state` cookie; affected users retry.

## Phased Delivery

### Phase 1 — Foundations (Units 1–7, plus Unit 6b)

OpenAPI spec, workspace deps (+ `dashmap`), `ApiError` extension, HKDF helper with Extract-and-Expand + entropy validation, startup validation, `SessionCookie` → `Option<IdpIdentity>` migration with serde validation, GitHub base-URL override via test-only Axum extension (Unit 6b), `twin-github` OAuth. No user-visible behavior changes; web users will need to re-login once. Lands as a single PR or a small stack.

### Phase 2 — Auth internals (Units 8–11)

JWT encode/verify, code store, refresh store with atomic rotation, JWT bearer extractor. Server can now issue and validate JWTs; no public endpoints yet. Lands independently.

### Phase 3 — Server endpoints (Units 12–15)

`return_to` plumbing (Unit 12) + six new HTTP routes (Units 13–15). Server is fully ready for CLI consumption but no CLI changes yet. Can be smoke-tested with `curl` against a dev server.

### Phase 4 — CLI (Units 16–21)

`AuthStore`, `fabro auth {login,logout,status}`, bearer priority + auto-refresh. Feature becomes user-visible end-to-end.

### Phase 5 — Integration (Unit 22)

Full round-trip test against twin-github. Blocks shipping until green.

## Sources & References

- **Origin document:** [docs/superpowers/specs/2026-04-19-cli-auth-login-design.md](../superpowers/specs/2026-04-19-cli-auth-login-design.md)
- Existing OAuth helper: `lib/crates/fabro-oauth/src/lib.rs`
- Current auth extractor: `lib/crates/fabro-server/src/jwt_auth.rs`
- GitHub web OAuth: `lib/crates/fabro-server/src/web_auth.rs`
- `ApiError` envelope: `lib/crates/fabro-server/src/error.rs`
- SlateDB key + store patterns: `lib/crates/fabro-store/src/keys.rs`, `lib/crates/fabro-store/src/slate/`
- CLI clap pattern: `lib/crates/fabro-cli/src/args.rs`, `lib/crates/fabro-cli/src/commands/provider/login.rs`
- CLI server client: `lib/crates/fabro-cli/src/server_client.rs`
- Twin GitHub: `test/twin/github/src/`
- Dev-token file pattern: `lib/crates/fabro-util/src/dev_token.rs`
- AGENTS.md: project root, `no_proxy()` + import-style + OpenAPI-first guidance
- RFC 6749 §4.1.2.1 (referenced in origin) — OAuth 2.0 Authorization Code error redirects
- OAuth 2.0 Security BCP (referenced in origin) — refresh-token rotation with reuse detection
