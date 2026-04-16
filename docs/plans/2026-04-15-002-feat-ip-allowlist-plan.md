---
title: "feat: Add IP address allowlist to server"
type: feat
status: active
date: 2026-04-15
origin: docs/brainstorms/2026-04-15-ip-whitelist-requirements.md
deepened: 2026-04-15
---

# feat: Add IP address allowlist to server

## Overview

Add configurable IP address allowlisting to the fabro server so users without network-level controls (VPN, firewall) can restrict access by client IP. The allowlist is configured via `[server.ip_allowlist]` in settings.toml, validated at startup (fail-closed), and enforced as request middleware after tracing but before auth.

## Problem Frame

Some fabro users deploy on infrastructure without firewalls or VPNs. They need fabro itself to restrict which IP addresses can reach the web UI and API. (see origin: docs/brainstorms/2026-04-15-ip-whitelist-requirements.md)

## Requirements Trace

- R1. Non-allowlisted IPs receive 403 before auth, for all requests (API, web UI, static assets)
- R2. No allowlist configured = all IPs allowed (current behavior)
- R3. Health check endpoint `/health` is exempt from IP filtering
- R4. Support individual IPv4 and IPv6 addresses
- R5. Support CIDR notation for ranges
- R6. Detect client IP from X-Forwarded-For behind reverse proxies (X-Real-IP is not used — no positional trust model)
- R7. Proxy header trust must be explicitly configured; default uses TCP remote address
- R8. Configured via settings.toml / environment variables only
- R9. No hot-reload; read once at startup
- R10. Log rejected requests at warn level with client IP and path
- R11. Unix socket + allowlist requires trusted_proxy_count > 0 or server refuses to start
- R12. Invalid entries (malformed CIDR) cause startup failure (fail-closed)

## Scope Boundaries

- No web UI management
- No per-user/per-role IP rules
- No blocklist (deny-list)
- No hot-reload of IP rules
- No implicit loopback exemption
- GitHub webhook listener out of scope (has HMAC verification)
- `/health/diagnostics` is NOT exempt (requires auth, probes server internals)

## Context & Research

### Relevant Code and Patterns

- **Config layer/resolved pattern**: `ServerLayer` (sparse, `deny_unknown_fields`) -> `resolve_server()` -> `ServerSettings` (fully populated) in `lib/crates/fabro-types/src/settings/server.rs` and `lib/crates/fabro-config/src/resolve/server.rs`
- **Middleware pattern**: `middleware::from_fn_with_state` used for `cookie_and_demo_middleware` at `lib/crates/fabro-server/src/server.rs:971`
- **Router structure**: `/health` registered on outer router (line 948), dispatch service_fn routes to demo/real sub-routers (line 914)
- **Serve paths**: Three listener branches in `lib/crates/fabro-server/src/serve.rs:492` — Unix, TCP+TLS, TCP plain. None currently use `ConnectInfo`
- **TLS accept loop**: `lib/crates/fabro-server/src/tls.rs:61` captures `remote_addr` but does not inject it into requests
- **Auth mode resolution**: `auth_mode` is resolved once at startup (serve.rs:315) and passed to the router, not hot-reloaded — same pattern for IP allowlist
- **Test pattern**: `tower::ServiceExt::oneshot()` with `Request::builder()` in server.rs test module (line 6561+)

### External References

- `ipnet` crate (v2.11.0 already transitive dep) — `IpNet::contains(&IpAddr)` for CIDR matching
- Axum `ConnectInfo<SocketAddr>` via `into_make_service_with_connect_info::<SocketAddr>()`
- X-Forwarded-For rightmost-minus-N: the secure approach is to count N trusted proxies from the right of the comma-separated list

## Key Technical Decisions

- **Custom proxy IP extraction over `axum-client-ip`**: The crate's `RightmostXForwardedFor` takes only the single rightmost value, not rightmost-minus-N. Since R7 specifies trusted proxy count support, a simple custom extractor is more appropriate than adding a dependency that doesn't cover the requirement
- **No X-Real-IP fallback**: X-Real-IP has no positional trust model — any client can forge it. When `trusted_proxy_count > 0`, only X-Forwarded-For is used for IP extraction. If the header is absent or has too few entries, the request is rejected (fail-closed)
- **Fail-closed on short X-Forwarded-For chain**: If `X-Forwarded-For` has fewer entries than `trusted_proxy_count`, the request is rejected (403) rather than falling back to a potentially spoofable entry
- **Generic 403 response body**: Use `"Access denied."` (matching existing `ApiError::forbidden()`) rather than a message that reveals the filtering mechanism
- **TLS ConnectInfo must use Axum newtype**: The TLS accept loop must inject `axum::extract::ConnectInfo<SocketAddr>` (the wrapper type), not a bare `SocketAddr`, or the middleware extractor will silently fail
- **`ipnet` as direct dependency**: Present in Cargo.lock as a transitive dependency (v2.11.0); added as a direct dependency to fabro-types, fabro-config, and fabro-server for CIDR parsing and matching
- **`[server.ip_allowlist]` config section**: Follows the existing subdomain pattern (`[server.auth]`, `[server.listen]`, etc.). Contains `entries` (list of IP/CIDR strings) and `trusted_proxy_count` (u32)
- **Middleware after TraceLayer, before cookie middleware**: Rejected requests still get trace spans (including 403 status), but no cookie/demo processing for blocked IPs
- **Health exemption inside middleware**: The middleware checks the request path for `/health` rather than selectively applying to route sets, keeping router structure simple
- **IPv4-mapped IPv6 normalization**: Convert `::ffff:x.x.x.x` to IPv4 equivalent before matching, so `10.0.0.0/8` matches regardless of how the OS reports the address

## Open Questions

### Resolved During Planning

- **Which Axum ecosystem tools for IP detection?** Custom implementation. `axum-client-ip` doesn't support trusted-proxy-count. `ConnectInfo<SocketAddr>` for TCP remote address, manual X-Forwarded-For parsing for proxy support
- **Config shape?** `[server.ip_allowlist]` with `entries` (string list) and `trusted_proxy_count` (u32, default 0). Fits the existing subdomain pattern and `deny_unknown_fields` constraint
- **Which health endpoints to exempt?** Only `/health`. Not `/health/diagnostics` (requires auth, exposes server internals)
- **IPv4-mapped IPv6 handling?** Normalize to IPv4 before matching
- **X-Forwarded-For position?** Rightmost-minus-N where N = `trusted_proxy_count`. If N=0, use TCP remote address (ConnectInfo)

### Deferred to Implementation

- **Cross-domain config validation sequencing**: The Unix socket + allowlist check requires both `listen` and `ip_allowlist` to be resolved. Perform this check in `resolve_server()` after both `resolve_listen()` and `resolve_ip_allowlist()` return, not inside the IP allowlist resolver itself
- **Whether `ipnet` version needs bumping**: Currently 2.11.0 as transitive; verify `contains()` API availability at implementation time

## High-Level Technical Design

> *This illustrates the intended approach and is directional guidance for review, not implementation specification. The implementing agent should treat it as context, not code to reproduce.*

```
Request flow with IP allowlist:

  Client -> [TCP/TLS Accept] -> ConnectInfo<SocketAddr> injected
         -> [TraceLayer]     -> trace span created
         -> [IP Allowlist]   -> if path == "/health": pass through
                                else: extract client IP
                                  (ConnectInfo or X-Forwarded-For rightmost-minus-N)
                                  normalize IPv4-mapped IPv6
                                  check against Vec<IpNet>
                                  if miss: warn!(...), return 403
         -> [Cookie/Demo]    -> parse cookies, demo header
         -> [Router]         -> dispatch to demo/real sub-router
         -> [Auth extractor] -> per-handler auth check

Startup validation:
  settings.toml -> parse [server.ip_allowlist]
    -> validate all entries parse as IpNet (fail-closed on error)
    -> if Unix socket && allowlist present && trusted_proxy_count == 0: error
    -> resolve to IpAllowlistConfig (Arc, read-once)
    -> pass to build_router_with_options
```

## Implementation Units

- [ ] **Unit 1: Config types and resolution**

**Goal:** Define the settings.toml schema for `[server.ip_allowlist]` and wire up parsing/validation.

**Requirements:** R4, R5, R8, R11, R12

**Dependencies:** None

**Files:**
- Modify: `lib/crates/fabro-types/src/settings/server.rs`
- Modify: `lib/crates/fabro-config/src/resolve/server.rs`
- Modify: `lib/crates/fabro-config/Cargo.toml` (add `ipnet` direct dependency)
- Modify: `lib/crates/fabro-types/Cargo.toml` (add `ipnet` for the resolved type)
- Test: `lib/crates/fabro-config/src/resolve/server.rs` (inline tests)

**Approach:**
- Add `ServerIpAllowlistLayer` struct: `entries: Option<Vec<String>>`, `trusted_proxy_count: Option<u32>`. Apply `#[serde(deny_unknown_fields)]`
- Add `ip_allowlist: Option<ServerIpAllowlistLayer>` to `ServerLayer`
- Add `ServerIpAllowlistSettings` resolved struct: `entries: Vec<IpNet>`, `trusted_proxy_count: u32`
- Default when absent: empty `entries` vec, `trusted_proxy_count: 0`
- Add `resolve_ip_allowlist()`: parse each entry string as `IpNet`, push `ResolveError` for invalid entries (fail-closed)
- Add `ip_allowlist` field to `ServerSettings`, call resolver from `resolve_server()`
- Perform cross-domain validation in `resolve_server()` after both `resolve_listen()` and `resolve_ip_allowlist()` return: check Unix socket + non-empty entries + `trusted_proxy_count == 0` → push error. This avoids passing listen type into the IP allowlist resolver

**Patterns to follow:**
- `ServerAuthLayer` / `ServerAuthSettings` in same file for layer/resolved struct pattern
- `resolve_auth()` in `lib/crates/fabro-config/src/resolve/server.rs` for validation with `ResolveError`

**Test scenarios:**
- Happy path: valid IPv4, IPv6, and CIDR entries parse correctly into `Vec<IpNet>`
- Happy path: absent `[server.ip_allowlist]` section resolves to empty entries (R2)
- Happy path: `trusted_proxy_count` defaults to 0 when not specified
- Error path: malformed CIDR string (e.g., `10.0.0.0/33`) produces ResolveError
- Error path: unparseable address (e.g., `not-an-ip`) produces ResolveError
- Error path: Unix socket listener + non-empty allowlist + `trusted_proxy_count: 0` produces ResolveError
- Edge case: empty `entries` list (present but empty) resolves to empty vec (equivalent to no filtering)

**Verification:**
- `cargo nextest run -p fabro-config` passes
- `cargo nextest run -p fabro-types` passes
- Invalid config entries cause resolution errors, not panics

- [ ] **Unit 2: IP matching and client IP extraction**

**Goal:** Create the core IP matching logic and client IP extraction (from ConnectInfo or proxy headers).

**Requirements:** R4, R5, R6, R7

**Dependencies:** Unit 1 (needs `ServerIpAllowlistSettings`)

**Files:**
- Create: `lib/crates/fabro-server/src/ip_allowlist.rs`
- Modify: `lib/crates/fabro-server/src/lib.rs` (add module declaration)
- Modify: `lib/crates/fabro-server/Cargo.toml` (add `ipnet` direct dependency)
- Test: `lib/crates/fabro-server/src/ip_allowlist.rs` (inline test module)

**Approach:**
- `IpAllowlist` struct wrapping `Vec<IpNet>` with a `contains(&IpAddr) -> bool` method
- Before matching, normalize IPv4-mapped IPv6 (`::ffff:x.x.x.x` → IPv4 equivalent) using `to_canonical()` or manual mapping
- `extract_client_ip(req, trusted_proxy_count) -> Option<IpAddr>` function:
  - If `trusted_proxy_count > 0`: parse `X-Forwarded-For` header, split by comma, take the entry at zero-indexed position `len - 1 - trusted_proxy_count` (skipping N trusted proxy entries from the right to reach the client IP). If `X-Forwarded-For` is absent or has fewer entries than `trusted_proxy_count + 1`, return `None` (fail-closed)
  - If `trusted_proxy_count == 0`: extract `ConnectInfo<SocketAddr>` from request extensions, return `addr.ip()`
- `IpAllowlist::is_empty()` method to efficiently skip checking when no allowlist is configured (R2)
- `IpAllowlistConfig` struct containing `IpAllowlist` + `trusted_proxy_count: u32`, constructed from `ServerIpAllowlistSettings`. This is the type used as middleware state (`Arc<IpAllowlistConfig>`) in Units 3 and 4

**Patterns to follow:**
- `lib/crates/fabro-server/src/jwt_auth.rs` for module structure and inline test organization

**Test scenarios:**
- Happy path: IPv4 address matches an individual IPv4 entry
- Happy path: IPv4 address matches a CIDR range (e.g., `10.1.2.3` matches `10.0.0.0/8`)
- Happy path: IPv6 address matches an IPv6 CIDR range
- Happy path: `extract_client_ip` with `trusted_proxy_count: 0` returns ConnectInfo IP
- Happy path: `extract_client_ip` with `trusted_proxy_count: 1` and header `client, proxy1` returns `client` (second-from-right, skipping 1 trusted proxy)
- Happy path: `extract_client_ip` with `trusted_proxy_count: 2` and header `client, proxy1, proxy2` returns `client` (third-from-right, skipping 2 trusted proxies)
- Edge case: IPv4-mapped IPv6 (`::ffff:10.1.2.3`) matches plain IPv4 entry `10.0.0.0/8`
- Edge case: X-Forwarded-For with fewer entries than `trusted_proxy_count` — returns None (fail-closed)
- Edge case: X-Forwarded-For entries with whitespace around commas are trimmed
- Edge case: empty allowlist `is_empty()` returns true
- Edge case: single host IP as `/32` CIDR matches exactly
- Error path: missing ConnectInfo extension when `trusted_proxy_count: 0` — returns None
- Error path: malformed IP in X-Forwarded-For — returns None (fail-closed)
- Error path: X-Forwarded-For absent when `trusted_proxy_count > 0` — returns None (no X-Real-IP fallback)

**Verification:**
- `cargo nextest run -p fabro-server -- ip_allowlist` passes
- All matching and extraction edge cases covered

- [ ] **Unit 3: IP filter middleware**

**Goal:** Create the Axum middleware function that enforces the IP allowlist on incoming requests.

**Requirements:** R1, R2, R3, R10

**Dependencies:** Unit 2 (needs `IpAllowlist` and `extract_client_ip`)

**Files:**
- Modify: `lib/crates/fabro-server/src/ip_allowlist.rs` (add middleware function)
- Test: `lib/crates/fabro-server/src/ip_allowlist.rs` (middleware integration tests in inline module)

**Approach:**
- `ip_allowlist_middleware` async function with signature compatible with `middleware::from_fn_with_state`:
  - State: `Arc<IpAllowlistConfig>` containing the `IpAllowlist` and `trusted_proxy_count`
  - If allowlist is empty: call `next.run(req).await` immediately (R2 — zero overhead when unconfigured)
  - If request path is `/health`: pass through regardless (R3)
  - Extract client IP using `extract_client_ip()`
  - If IP is in allowlist: pass through
  - If IP is not in allowlist or extraction failed: `warn!(client_ip = %ip, path = %path, "request rejected: IP not in allowlist")`, return 403 response. Log only the parsed `IpAddr`, not raw header values, to prevent log injection
- Return `ApiError::forbidden().into_response()` (generic "Access denied." — does not reveal IP filtering as the mechanism; `.into_response()` needed since `from_fn_with_state` middleware must return `Response`)

**Patterns to follow:**
- `cookie_and_demo_middleware` at `lib/crates/fabro-server/src/server.rs:1994` for `from_fn_with_state` middleware signature and request inspection
- Logging strategy: `warn!` with structured fields per `docs-internal/logging-strategy.md`

**Test scenarios:**
- Happy path: request from allowlisted IP passes through to handler
- Happy path: request to `/health` from any IP passes through (R3)
- Happy path: empty allowlist allows all requests (R2)
- Error path: request from non-allowlisted IP returns 403 with appropriate body
- Error path: request with no extractable IP (missing ConnectInfo, no proxy headers) returns 403
- Integration: middleware runs before auth — a blocked IP never reaches auth extractor
- Edge case: request to `/health/diagnostics` from non-whitelisted IP returns 403 (not exempt)
- Edge case: request to `/health/` (trailing slash) from non-whitelisted IP returns 403 (not exempt — exact path match only)
- Edge case: static asset request from non-whitelisted IP returns 403

**Verification:**
- `cargo nextest run -p fabro-server -- ip_allowlist` passes
- Rejected requests logged at warn level with structured fields

- [ ] **Unit 4: Router and serve integration**

**Goal:** Wire the IP allowlist middleware into the router and enable ConnectInfo extraction in all serve paths.

**Requirements:** R1, R7, R9

**Dependencies:** Unit 3 (needs middleware function), Unit 1 (needs resolved config)

**Files:**
- Modify: `lib/crates/fabro-server/src/server.rs` (add middleware layer in `build_router_with_options`, update `build_router()` convenience wrapper to pass default empty allowlist)
- Modify: `lib/crates/fabro-server/src/serve.rs` (read allowlist config at startup, pass to router, update TCP serve call to use `into_make_service_with_connect_info`)
- Modify: `lib/crates/fabro-server/src/tls.rs` (inject `ConnectInfo<SocketAddr>` into request extensions in TLS accept loop)
- Test: `lib/crates/fabro-server/src/server.rs` (integration tests in existing test module)

**Approach:**
- In `serve.rs` `serve_command()`: resolve the IP allowlist config from `resolved_server_settings` once at startup (like `auth_mode` at line 315). Wrap in `Arc<IpAllowlistConfig>` and pass to `build_router_with_options` as a separate parameter (not in `RouterOptions`, to keep it lightweight and `Copy`-compatible — mirrors how `auth_mode` is passed separately)
- In `server.rs` `build_router_with_options()`: apply `middleware::from_fn_with_state(ip_allowlist_config, ip_allowlist_middleware)` as a layer AFTER `trace_layer` but BEFORE `cookie_and_demo_middleware`. Layer ordering (outermost to innermost): TraceLayer → IP allowlist → cookie/demo → router
- In `serve.rs` TCP plain path (line 519): change `axum::serve(listener, router)` to `axum::serve(listener, router.into_make_service_with_connect_info::<SocketAddr>())`
- In `tls.rs` accept loop (line 78): inject `axum::extract::ConnectInfo<SocketAddr>` (the Axum newtype wrapper, NOT a bare `SocketAddr`) as a request extension before calling the router. If the wrong type is inserted, `extract_client_ip` will silently fail and all requests will be rejected
- In `serve.rs` Unix socket path (line 498): no ConnectInfo change needed — the startup validation (Unit 1) ensures `trusted_proxy_count > 0` when using Unix socket with an allowlist, so the middleware will use proxy headers
- IP allowlist config is NOT added to `AppState` or `shared_settings` — it is captured by the middleware closure at construction time, ensuring R9 (no hot-reload)

**Patterns to follow:**
- `auth_mode` resolution at `serve.rs:315` for resolve-once-at-startup pattern
- `cookie_and_demo_middleware` layer application at `server.rs:971` for middleware wiring
- `auth_mode` parameter passing for how `build_router_with_options` accepts separate config

**Test scenarios:**
- Happy path: full router with allowlist configured blocks non-allowlisted IP, allows allowlisted IP
- Happy path: full router without allowlist configured allows all IPs
- Happy path: `/health` accessible from any IP when allowlist is configured
- Integration: ConnectInfo extension is available in request when using TCP serve
- Integration: IP filtering works with demo mode — blocked IPs cannot access demo routes
- Integration: existing tests with non-empty allowlist must inject `ConnectInfo<SocketAddr>` into request extensions via `Request::builder().extension(ConnectInfo(...))`, or fail-closed will reject all requests
- Edge case: TLS path injects ConnectInfo correctly so IP filtering works over HTTPS

**Verification:**
- `cargo nextest run -p fabro-server` passes (all existing tests + new integration tests)
- `cargo clippy --workspace -- -D warnings` passes
- `cargo +nightly fmt --check --all` passes

## System-Wide Impact

- **Interaction graph:** The middleware sits between TraceLayer and cookie/demo middleware. It reads `ConnectInfo` extensions (set by `into_make_service_with_connect_info` or manually in TLS path). It does not interact with auth, storage, or any other server subsystem
- **Error propagation:** 403 responses from the middleware short-circuit the request pipeline. They appear in TraceLayer output as normal 403 responses. The middleware also emits its own `warn!` log
- **State lifecycle risks:** None — the allowlist is immutable after startup (Arc, no hot-reload). No shared mutable state
- **API surface parity:** The middleware applies uniformly to all routes (except `/health`). No API changes needed
- **Integration coverage:** Key scenario: full request through TCP listener → ConnectInfo injection → IP filter → handler. This must be tested via `oneshot()` with manually injected `ConnectInfo` extensions
- **Unchanged invariants:** Auth middleware (extractors), demo mode dispatch, SSE streaming, API routes, and the webhook listener are all unchanged. The only change to existing code is adding the middleware layer and updating serve paths for ConnectInfo

## Risks & Dependencies

| Risk | Mitigation |
|------|------------|
| `into_make_service_with_connect_info` changes the `axum::serve` return type | Straightforward migration; Axum docs show the pattern. Test all serve paths |
| TLS path ConnectInfo injection uses wrong type (bare `SocketAddr` instead of `ConnectInfo<SocketAddr>`) | Explicitly documented constraint in Unit 4 approach; dedicated test verifies extraction works through TLS path |
| IPv4-mapped IPv6 normalization bugs | Comprehensive test coverage in Unit 2 with explicit mapped-address test cases |
| Existing tests may break if they rely on the serve signature | Tests use `oneshot()` against the router, not `axum::serve`, so they are unaffected |

## Sources & References

- **Origin document:** [docs/brainstorms/2026-04-15-ip-whitelist-requirements.md](docs/brainstorms/2026-04-15-ip-whitelist-requirements.md)
- Related code: `lib/crates/fabro-server/src/server.rs` (router, middleware)
- Related code: `lib/crates/fabro-server/src/serve.rs` (serve paths)
- Related code: `lib/crates/fabro-server/src/tls.rs` (TLS accept loop)
- Related code: `lib/crates/fabro-types/src/settings/server.rs` (config types)
- Related code: `lib/crates/fabro-config/src/resolve/server.rs` (config resolution)
- External: [ipnet crate](https://docs.rs/ipnet/latest/ipnet/) for CIDR matching
- External: [Axum ConnectInfo](https://docs.rs/axum/0.8/axum/extract/struct.ConnectInfo.html)
