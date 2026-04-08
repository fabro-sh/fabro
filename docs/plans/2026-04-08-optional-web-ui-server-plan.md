# Optional Web UI for `fabro server` Plan

## Summary
Make the Fabro server able to run in two surfaces:

- API-only
- API + web UI

The always-on surface is `/health` plus the machine API under `/api/v1`. The web surface includes the embedded SPA fallback, browser auth routes under `/auth/*`, and the browser-session/setup/demo helper endpoints that currently live under `/api/v1` in `web_auth::api_routes()`:

- `/api/v1/auth/me`
- `/api/v1/setup/register`
- `/api/v1/setup/status`
- `/api/v1/demo/toggle`

When the web UI is disabled, all of those web-surface routes return `404`, even when they live under `/api/v1`. API-only mode is a routing change only; it does not remove the embedded SPA from the binary.

Operator control should use both config and CLI:

- Add `web.enabled` to server settings
- Add `--web` / `--no-web` startup overrides
- Precedence: CLI override > config > default
- Default: web UI enabled

## Key Changes
### Config and CLI surface
- Extend the existing `[web]` config section with `enabled`.
- Add `enabled: Option<bool>` to the config model and `enabled: bool` to resolved settings, defaulting to `true`.
- Add mutually exclusive `--web` and `--no-web` flags to server startup args.
- Define merge behavior explicitly: combine `WebConfig` sources first with last-non-`None` wins for `enabled`, then materialize resolved `WebSettings.enabled`, then apply the CLI override last.
- Apply the flags in the same resolved-settings pass that already handles other serve-time overrides.
- Update help text and docs to describe the new toggle and its precedence.

### Router composition
- Refactor server router construction so the machine API, web surface, and health surface are composed separately.
- Keep these always mounted:
  - machine API routes under `/api/v1`, excluding the routes currently provided by `web_auth::api_routes()`
  - `/health`
- Treat these as web-only routes, even when they live under `/api/v1`:
  - `/api/v1/auth/me`
  - `/api/v1/setup/register`
  - `/api/v1/setup/status`
  - `/api/v1/demo/toggle`
  - `/auth/*`
  - SPA/static fallback for non-API `GET`/`HEAD`
- The current SPA is served from the fallback closure, not a mounted router. In API-only mode, that fallback closure must return `404` for all non-API, non-health routes instead of serving the SPA.
- In API-only mode, disable the browser-oriented demo/session behavior as well:
  - `/api/v1/demo/toggle` returns `404`
  - cookie-driven and `X-Fabro-Demo` header demo dispatch do not route requests into the auth-disabled demo router
  - requests use only the normal machine API router plus `/health`
- When the web UI is enabled, preserve current browser session, setup, and demo-cookie behavior.

### Internal interface changes
- Thread a resolved `web_enabled` boolean into router construction.
- Prefer making this explicit in the server boundary, for example by extending `build_router(...)` with a surface/options argument, rather than re-reading settings inside the router.
- `build_router(state, auth_mode)` has a broad test blast radius. Minimize churn by introducing a small `RouterOptions`-style parameter or helper wrapper so tests that do not care about web surface toggling can keep using the default-enabled path.
- Keep the decision centralized in startup/resolution code so tests can build routers deterministically.

## Test Plan
- Router tests:
  - web enabled: `GET /` serves SPA
  - web enabled: `/auth/*` remains available
  - web enabled: `/api/v1/auth/me`, `/api/v1/setup/status`, and `/api/v1/demo/toggle` keep their current behavior
  - web disabled: `GET /` returns `404`
  - web disabled: client routes like `/runs/abc` return `404`
  - web disabled: `/auth/*` returns `404`
  - web disabled: `/api/v1/auth/me`, `/api/v1/setup/register`, `/api/v1/setup/status`, and `/api/v1/demo/toggle` return `404`
  - web disabled: representative machine API endpoints still work
  - web disabled: cookie-driven or `X-Fabro-Demo` requests do not enter auth-disabled demo dispatch
  - `/health` still works in both modes
- CLI/config tests:
  - default startup serves web UI
  - config `web.enabled = false` disables the UI
  - `--web` overrides config-disabled to enable
  - `--no-web` overrides config-enabled/default to disable
  - help snapshots include `--web` and `--no-web`
- Regression coverage:
  - existing source-map/static routing tests still pass in enabled mode
  - existing server lifecycle tests still pass with default behavior unchanged

## Docs and User-Facing Behavior
- Update CLI docs for `fabro server start` to describe `--web` / `--no-web`.
- Update server configuration docs to document `[web].enabled`.
- Update deploy/architecture docs so “server mode” no longer implies the web UI is always present.
- Call out that “web UI disabled” means:
  - no `/auth/*`
  - no web-session/setup/demo helper endpoints under `/api/v1`
  - no SPA fallback at `/` or client routes
  - machine API and `/health` only
  - embedded SPA assets are still compiled into the binary; this is not a build-time exclusion

## Assumptions and Defaults
- Default remains web UI enabled.
- CLI flags are `--web` and `--no-web`.
- CLI override precedence is standard: CLI > config > default.
- Disabling the web UI is strictly an HTTP-surface change; it does not disable workflow execution, machine API behavior, or server-owned background services.
- No OpenAPI/API schema changes are needed, since this only changes route availability and classification within the existing server.
