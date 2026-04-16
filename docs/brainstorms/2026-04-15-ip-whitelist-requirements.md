---
date: 2026-04-15
topic: ip-whitelist
---

# IP Address Whitelisting

## Problem Frame

Some fabro users deploy without network-level controls (VPN, firewall). These users need fabro itself to restrict access by client IP address so that only trusted networks can reach the web UI and API.

## Requirements

**Access Control**
- R1. When an IP whitelist is configured, requests from non-whitelisted IPs receive 403 Forbidden before authentication is evaluated. This applies to all requests: API, web UI, and static assets
- R2. When no IP whitelist is configured, all IPs are allowed (current behavior, no breaking change)
- R3. Health check endpoints (e.g., `/health`) are exempt from IP filtering so load balancers and monitoring continue to work
- R10. Log rejected requests (client IP, request path, timestamp) at warn level for security auditing and debugging

**IP Matching**
- R4. Support individual IPv4 and IPv6 addresses (e.g., `1.2.3.4`, `::1`)
- R5. Support CIDR notation for ranges (e.g., `10.0.0.0/8`, `fd00::/8`)

**Reverse Proxy Support**
- R6. When deployed behind a reverse proxy, detect client IP from `X-Forwarded-For` or `X-Real-IP` headers
- R7. Proxy header trust must be explicitly configured (trusted proxy count or trusted proxy list) to prevent spoofing; without configuration, use the TCP remote address
- R11. When the server listens on a Unix socket and an IP whitelist is configured, proxy header trust must also be configured or the server refuses to start

**Configuration**
- R8. IP whitelist is configured via server settings (settings.toml / environment variables), not via the web UI
- R9. Changes require a server restart (IP whitelist does not participate in the existing settings hot-reload cycle)
- R12. Server must refuse to start if the IP whitelist contains invalid entries (malformed CIDR, unparseable addresses). Fail-closed prevents a false sense of security

## Success Criteria

- A fabro instance with an IP whitelist configured rejects requests from non-whitelisted IPs with 403
- Health checks remain accessible from any IP
- The feature has zero impact when unconfigured (default: allow all)
- IP detection works correctly behind common reverse proxies (nginx, AWS ALB) when trust is configured
- Invalid whitelist configuration prevents server startup with a clear error message
- Rejected requests appear in server logs with enough detail to diagnose misconfiguration

## Scope Boundaries

- No web UI for managing the whitelist
- No per-user or per-role IP rules
- No rate limiting (separate concern)
- No IP blocklist (deny-list) -- only allowlist
- No hot-reload of IP rules without restart
- No geo-IP or DNS-based filtering
- No implicit localhost/loopback exemption -- admins must explicitly whitelist 127.0.0.1/::1 if needed
- GitHub webhook listener (separate TcpListener) is out of scope -- it uses HMAC signature verification for its own access control

## Key Decisions

- **Config-only, no UI management**: Keeps the attack surface small -- an attacker who bypasses IP filtering can't weaken it through the UI
- **Block before auth**: Reduces exposure -- non-whitelisted IPs can't even probe auth endpoints
- **Health check exemption**: Practical necessity for load-balanced deployments
- **Explicit proxy trust**: Prevents IP spoofing via forged headers in direct-connection deployments
- **Fail-closed on invalid config**: Server refuses to start rather than silently degrading to allow-all
- **No implicit loopback exemption**: Keeps behavior strict and predictable; admin error is recoverable via config file edit
- **Unix socket requires proxy trust**: Since Unix sockets have no TCP remote address, proxy headers are the only way to determine client IP

## Outstanding Questions

### Deferred to Planning
- [Affects R6, R7][Needs research] What reverse proxy IP detection does Axum's ecosystem already provide (e.g., `axum-client-ip`, `ConnectInfo`) and what needs custom implementation? Note: the server currently does not use `into_make_service_with_connect_info`, and the TLS path does not propagate `remote_addr`
- [Affects R8][Technical] What is the best configuration shape in settings.toml? Note: `ServerLayer` uses `#[serde(deny_unknown_fields)]`, which constrains field additions
- [Affects R3][Technical] Which health check endpoints should be exempted? Currently `/health` and `/health/diagnostics` exist; the latter probes server internals and may not be safe to exempt
- [Affects R4, R5][Technical] How should IPv4-mapped IPv6 addresses (e.g., `::ffff:10.0.0.1`) be handled -- should they match plain IPv4 whitelist entries?
- [Affects R6][Technical] For X-Forwarded-For with multiple IPs, which position in the chain should be treated as the client IP? (rightmost-minus-N is the secure approach)

## Next Steps

-> `/ce:plan` for structured implementation planning
