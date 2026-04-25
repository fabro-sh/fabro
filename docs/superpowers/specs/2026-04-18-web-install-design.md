# Web-based install — design

**Status:** approved spec, pre-implementation
**Date:** 2026-04-18
**Owner:** bryan@qlty.ai

## Summary

Add a browser-based installation wizard as an alternative to `fabro install`. When the operator runs `fabro server start` or `fabro server restart` (no `--config` / `FABRO_CONFIG`) on a machine with no `~/.fabro/settings.toml`, the server enters **install mode**: prints a one-time install token, mounts an install-only HTTP router, and serves a wizard from the existing `fabro-web` bundle. On successful completion, the server persists the same on-disk state the CLI install produces and exits cleanly.

In supervised deployments (Docker `restart: unless-stopped`, Railway, systemd) the supervisor restarts the process and it boots into normal mode automatically. On a local laptop without a supervisor, the wizard's completion screen tells the operator to re-run `fabro server start` themselves.

This unblocks remote-first deployments (Docker, Railway, VPS) where the operator has no terminal-time access to run `fabro install`, and gives local users a browser alternative if they prefer it. The CLI `fabro install` wizard remains fully supported.

Other CLI commands that auto-start the local server (`fabro run attach`, etc.) do **not** trigger install mode — they fail with a clear "configure first" message. Only the explicit `fabro server start` and `fabro server restart` commands enter install mode.

## Goals

- Enable first-run configuration of a Fabro server entirely from a browser.
- Work for both local (laptop) and remote (PaaS / VPS) deployments under a single mechanism.
- Reach the same on-disk end state as `fabro install`, sharing the TOML-merging and env-file primitives. Vault persistence uses a different code path in install mode (direct-to-disk rather than via the API client) but produces the same file at the same location with the same schema. See *`persist_install_outputs` cannot be reused as-is in install mode* below for why.
- Keep the install surface transient and isolated: zero install code paths reachable on a configured server.

## Non-goals (v1)

- OAuth-based LLM provider auth. API-key providers (Anthropic API key, OpenAI, Gemini, OpenAI-compatible) only in v1; Codex (OAuth-only via the local `codex` binary) and Anthropic OAuth are explicitly deferred. The CLI `fabro install` continues to support these for users who need them today.
- Resume of a partially-completed install across server restarts.
- Replacing the CLI `fabro install` command — additive, not a replacement.
- Multi-operator coordination during install (two browsers using the same token).
- Automatic deletion of GitHub Apps created on github.com if local persistence then fails.

## Decisions log

The following decisions were made during brainstorming and are load-bearing for the rest of the design:

| # | Decision | Why |
|---|---|---|
| 1 | Support both local and remote deployments | Remote is the use case that motivates this; local is trivial once remote works |
| 2 | Trust = one-time install token, printed to stdout/log on startup | Operator must already have log access; matches Jupyter / Gitea / Vaultwarden prior art |
| 3 | Friendly landing page when token is missing/invalid (not 401) | First-time users will hit `/` before they read the logs; explain instead of error |
| 4 | Regular `fabro server` self-detects unconfigured state and enters install mode | Single mental model: "I started the app, it told me how to finish setup." Required for Docker/Railway. |
| 5 | On wizard completion: persist, respond, exit cleanly with code 0 | Avoids in-process mode transition. In supervised deployments the supervisor restarts into normal mode; on local laptops the operator re-runs `fabro server start`. |
| 6 | Update orchestration configs (Docker Compose, Railway) with restart policies | Required for (5) to work end-to-end |
| 7 | Full parity with CLI install (long-term) | The CLI is the source of truth for "configured." A web flow that diverges is half a feature. |
| 8 | v1 ships API-key LLM providers only | Defers OAuth-based provider flows to v2 |
| 9 | Server config screen detects canonical URL from forwarded headers, prefills, user confirms | Works for any reverse proxy; no per-PaaS detection in the prefill path |
| 10 | Per-step API calls with in-memory server-side install session | Live validation; GitHub App OAuth needs server-side state regardless |
| 11 | Install endpoints in main `fabro-api.yaml`, tagged for exclusion from Mintlify | Reuses progenitor + openapi-generator pipelines and the conformance test |
| 12 | Same bundle (`fabro-web`) hosts install wizard, with a server-injected mode flag in `index.html` controlling which router tree mounts at boot | One codebase, one design system; the existing route loaders (`redirect-home.tsx:6`, `auth-login.tsx:7`, `app-shell.tsx:26`) call `/api/v1/auth/*` which 404s in install mode, so install mode must mount a different router tree without invoking those loaders |
| 13 | After install, `/install/*` routes return 404 (not 401) | Routes are permanently absent in normal mode, not auth-gated |
| 14 | Detection trigger: absence of `~/.fabro/settings.toml`. Parse errors fail loudly | Matches `install.rs:1647`; one-line check; no ambiguity |
| 15 | Refactor shared persistence into a new `fabro-install` crate | Avoids circular deps; both `fabro-cli` and `fabro-server` depend on it |
| 16 | Wizard order: welcome → LLM → **Server config** → GitHub → Review (Server moved before GitHub) | The GitHub App manifest bakes `<canonical_url>` into `redirect_url` and `callback_urls`; if the user creates the App with a wrong URL, the App exists on github.com and we cannot unmake it. URL must be confirmed before manifest creation. |
| 17 | Install mode persists vault secrets directly to disk (`Vault::load(...).set(...)`), not via the API client | The CLI helper goes through `connect_api_client(storage_dir)` which would call back into the install-mode server itself, hitting `/api/v1/secrets` which is not mounted. |
| 18 | Install mode triggers only when no explicit `--config` / `FABRO_CONFIG` was provided AND the default `~/.fabro/settings.toml` is absent | A typo in `--config /typo` must error, not silently install on top of the wrong target. Matches the asymmetry the existing config loader already enforces. |
| 19 | v1 inherits the existing helper's partial-state semantics on `/install/finish` failure (settings.toml rolled back, server.env not). Atomic rollback deferred. | Real atomic rollback requires temp-dir + rename refactor; partial state is acceptable because env keys are deterministic and idempotent on retry. |
| 20 | Only the explicit `fabro server start` (and `restart`) command enters install mode. Auto-start callers (`run attach`, `server runs`, etc.) fail with a "configure first" message. | Auto-start callers spawn-and-block; an install-mode server has no `/api/v1/*` to connect to, so they would hang waiting for a route that doesn't exist. |
| 21 | Local laptops without a supervisor: the wizard completion screen shows a "run `fabro server start` to launch the configured server" message after a 30s polling timeout. Built-in supervisor for local is a follow-up. | Install mode runs in foreground (decision #22), so the install process IS the operator's `fabro server start` invocation. When it exits cleanly, nothing brings it back. v1 surfaces the next step on the completion screen; one extra command is acceptable for local users who could have used `fabro install` instead. |
| 22 | Install mode forces foreground; never daemonizes. `--foreground` is implicit. | Daemon mode hides the install token in `server.log` (`start.rs:347, :397`); foreground puts it on the operator's terminal where they can act on it. |
| 23 | `--no-web` is ignored during install with a stderr warning; respected on next start. | Install requires the web UI. Rejecting the flag would force supervised-deployment operators to drop it or `docker exec` to run the CLI wizard — defeats the point. Warning makes the override visible. |
| 24 | Container image v1 changes: drop the baked `/etc/fabro/settings.toml` and the `FABRO_CONFIG` env, set `FABRO_STORAGE_DIR=/storage`, and persist `~/.fabro/` across restarts (recommendation: move `FABRO_HOME` into a subdirectory of the existing `/storage` volume). | Without these changes, the published container image (`Dockerfile:32, :35-36`) sets `FABRO_CONFIG` to a baked file, which makes the install-mode trigger rules treat it as explicit-config and never enter install mode — defeating the entire remote-first deployment use case the design exists to enable. |

## Architecture

### Process model

The detection fork happens at the **dispatch layer**, in `commands::server::dispatch` (`lib/crates/fabro-cli/src/commands/server/mod.rs:31`). It is not strictly required there for technical reasons — the existing config loader handles the missing-default case gracefully (`fabro-config/src/user.rs:77` returns defaults; `fabro-server/src/serve.rs:820-824` falls back to a default Unix socket; `tests/it/cmd/server_start.rs:111` covers `fabro server start` with no config). Today, `fabro server start` on an unconfigured machine cheerfully boots into a non-functional state (no LLM credentials, no integrations, the `/api/v1/auth/me` route returns 401). That non-functional default boot is exactly what install mode displaces. Forking at dispatch lets us mount the install router *instead of* the normal one, rather than letting the normal one come up empty.

The fork is also explicitly scoped to the `Start` (and equivalent `Restart`) variant of `ServerCommand`. Other commands that auto-start the server (see *Auto-start callers* below) do not trigger install mode.

The fork:

1. **Determine whether install mode is even eligible.** Install mode is *only* triggered when no explicit config path was given (no `--config` flag, no `FABRO_CONFIG` env var) AND the default `~/.fabro/settings.toml` is absent. The current loader (`fabro-config/src/user.rs:81-112`) treats explicit-path-missing as an error and default-path-missing as "fall back to defaults" — install mode follows the same asymmetry. A typo in `--config` or `FABRO_CONFIG` must error out, not silently install on top of the wrong location.
2. **If config is explicit (--config or FABRO_CONFIG):** existing path. Load settings (errors loudly if the file is missing or malformed), resolve storage_dir / bind, continue into `start::execute` / `serve::execute` as today. Never enters install mode regardless of file presence — operators using explicit config paths are doing something deliberate and the safe default is "fail loud, don't auto-install."
3. **If config is implicit (default path) AND `~/.fabro/settings.toml` exists:** existing path, same as today. Parse errors fail loudly.
4. **If config is implicit AND `~/.fabro/settings.toml` is absent:** install bootstrap path:
   - Resolve `storage_dir` from `--storage-dir` / `FABRO_STORAGE_DIR` / `legacy_default_storage_root().join("storage")` (the same fallback the CLI install uses at `install.rs:1661`). Do not require settings to derive it.
   - Resolve a bind address from `--bind` / env / default (`0.0.0.0:32276` in container env-detected scenarios, `127.0.0.1:32276` otherwise — picking a sensible default without a settings file is a small new helper).
   - Generate a one-time install token (`ring::rand::SystemRandom`, 32 bytes, base64url, no padding).
   - Print the token + a fully formed install URL to stderr (visible in `journalctl`, `docker logs`, Railway logs).
   - Create an in-memory `InstallSession` registry holding a single `PendingInstall`.
   - Build an install-only router (`build_install_router`) mounting: `/install/*` API endpoints (install-token middleware, except the OAuth callback — see below), the static-asset routes serving the existing `fabro-web` bundle but with a **server-injected mode flag** (see *Frontend mode-aware boot* below), `GET /health` returning `{ "mode": "install" }`, and a catch-all that returns the install-mode `index.html`. Normal `/api/v1/*` routes are not mounted.
   - **Skip** the eager `load_or_create_local_dev_token` / `load_or_create_local_session_secret` calls that `start.rs:247-248, 389-390` perform — the install router authenticates via its own middleware and doesn't use the JWT/session machinery.
   - **Force foreground.** Install mode never daemonizes, regardless of how `fabro server start` was invoked. The current daemon path (`start.rs:318` `execute_daemon`) redirects the child's stdout/stderr to `server.log` (`start.rs:347, :397`) and the parent only prints its own summary; the install token would be hidden in the log file. Foreground install puts the token directly on the operator's terminal. The daemon path is fine for normal mode; install mode bypasses it. The boot stderr explicitly says "`--foreground` is implicit during install" so operators who passed daemon-implying flags know what happened.
   - **`--no-web` is ignored** with a stderr warning. Install mode requires the web UI; the flag will be respected on the next start (after the supervisor restart, or after the operator re-runs `fabro server start`). The boot stderr emits: `Warning: --no-web is ignored during install; will be respected on next start.`

When `POST /install/finish` succeeds, the handler persists outputs to disk, returns 202, then schedules a clean process exit (~500ms after responding). What happens next depends on whether the process has a supervisor:

- **Supervised deployments** (Docker `restart: unless-stopped`, systemd, Railway, fly.io). The supervisor restarts the process; the new process finds `settings.toml` present and boots into normal mode (which now does its own eager dev-token/session-secret creation as today). The wizard's completion screen polls `/health` and redirects to the configured normal-mode URL when the new server is up — typically a few seconds.
- **Local laptop without supervisor.** Install mode forces foreground, so `fabro server start` IS the install-mode process — there is no daemon child. When the foreground process exits cleanly after `/install/finish`, nothing brings it back; the operator's terminal returns to a shell prompt. The wizard's completion screen still polls `/health`, but after a ~30s timeout it shows the operator a clear instruction: **"Install complete. Run `fabro server start` to launch your configured server."** The polling does not hang forever because the operator may be away from the terminal at that moment.

A possible follow-up is to add a built-in supervisor for the local case — `fabro server start` could re-exec itself in normal mode after the install-mode pass exits with a "needs restart" exit code. This would make the local UX seamless without changing the supervised-deployment path. **Not v1.** v1 keeps the local flow as a manual re-run.

**No mode-transition inside a running process.** Avoiding hot-swap means we don't reason about half-mounted routers, in-flight install requests during shutdown, or partial state after a failed finalize. Process exit is the boundary.

### Auto-start callers

Several CLI commands auto-start a local server through `connect_server` → `connect_api_client_bundle` → `start::ensure_server_running_for_storage` (`lib/crates/fabro-cli/src/server_client.rs:88, :117-121`). Examples: `fabro run attach` (`commands/run/attach.rs:50`), `fabro server runs` (`server_runs.rs:20`).

These callers must **not** auto-trigger install mode. If they did, the spawned install-mode server would print an install token to its log file (which the user is not watching), the auto-start helper would block waiting for an `/api/v1/*` route that doesn't exist, and the experience would be inscrutable.

The fix is at `ensure_server_running_for_storage`: before spawning, check the same condition the dispatch fork uses (no explicit config + default settings.toml absent). If it matches, fail with a clear message:

```
Cannot reach Fabro server: no settings.toml configured.

Run one of:
  fabro server start    # browser-based wizard
  fabro install         # terminal wizard
```

This change is small but load-bearing — without it, `fabro run attach` on a fresh machine silently boots an install daemon and then hangs.

### Frontend mode-aware boot

The existing `fabro-web` SPA root + login + app-shell loaders all call `/api/v1/auth/*` (`apps/fabro-web/app/routes/redirect-home.tsx:6`, `auth-login.tsx:7`, `app-shell.tsx:26`). In install mode `/api/v1/*` returns 404, so any of these loaders running would throw before the install UI could render. The install mode therefore needs a different React entry path, in the same bundle.

The cleanest approach: the install-mode static handler serves a slightly different `index.html` that exposes a global flag the React entry reads at boot:

```html
<script>window.__FABRO_MODE__ = "install";</script>
```

The React entry (`apps/fabro-web/app/entry.client.tsx` or equivalent) checks `window.__FABRO_MODE__`. If `"install"`, it mounts an install-only React Router tree (a separate `createBrowserRouter` rooted at the install routes) and the normal router never instantiates. Existing loaders never fire.

In normal mode, the flag is absent (or `"normal"`) and the existing router boots as today.

The two router trees live side-by-side in the bundle. Install mode pays for normal-mode code in download size (negligible — the install wizard is the larger of the two). Normal mode pays for install-mode code (also negligible).

### Crate placement

A new `fabro-install` crate holds:
- `PendingInstall` and `InstallSession` types
- TOML-merging primitives currently in `install.rs:164–299` (`merge_server_settings`, `write_token_settings`, `write_github_app_settings`, etc.)
- `persist_install_outputs` and supporting helpers (currently `install.rs:1335`)
- `setup_github_app` refactored to be transport-agnostic (no embedded callback server)
- Generation of secrets (session secret, JWT keypair, dev token)

`fabro-cli` and `fabro-server` both depend on `fabro-install`. The CLI's `commands::install` becomes thin glue around the new crate; `fabro-server::install` adds the install-mode router and handlers that drive the same primitives.

## Trust model

### Token generation and surfacing

On entering install mode, the server generates a single 32-byte token, base64url-encoded. Stored only in memory; never persisted.

At startup, the server prints to stderr (which the operator sees directly because install mode forces foreground — see *Process model*):

```
  ⚒️  Fabro server is unconfigured — install mode active.

  Open this URL in your browser to finish setup:
    https://fabro.up.railway.app/install?token=8H_K2…

  Or visit the root path for the install token instructions.
```

For local foreground use this lands on the operator's terminal directly. For supervised deployments (Docker, Railway, systemd), stderr is captured by `docker logs`, journalctl, or the platform's log viewer; operators read it there.

The startup-log URL is best-effort: it uses env-var detection (Railway's `RAILWAY_PUBLIC_DOMAIN`, etc.) falling back to the bind address. The operator may already know their public hostname out-of-band; this is just a hint.

### Landing page

`GET /` (or any non-`/install/*`, non-asset path) with no/invalid token serves the SPA shell. The React install router renders an "Unconfigured server — find your token" page that explains:

- where to find the token (Docker logs, Railway logs, terminal output)
- example commands (`docker logs <container> | grep "Open this URL"`)
- a textarea to paste the token if the operator prefers not to use the URL

No 401, no error — a friendly explainer.

### Token validation

Every `/install/*` API call requires the token. Three places it can come from, in order:

1. `Authorization: Bearer <token>` header (used by the frontend after first load)
2. `?token=` query param (used for the initial human paste-and-go)
3. `X-Install-Token` header

A middleware checks the token against the in-memory session registry; mismatch returns 401 with a generic "invalid install token" body.

### Token lifecycle

**Session-lifetime, not strictly single-use.** The token is valid for any `/install/*` call until either (a) `/install/finish` succeeds and the process exits, or (b) the process restarts (which generates a fresh token). Strict single-use would force a re-auth handshake after every step; the token is already a high-entropy secret behind a transient process.

### CSRF and URL hygiene

- Install API endpoints don't accept cookies; they require `Authorization` header. Cross-origin form POSTs cannot set this. No CSRF token needed.
- The query-param token will appear in browser history and access logs. Acceptable: (i) the same token is already in server logs so the observability surface is symmetric; (ii) the token dies the moment install completes; (iii) URL-fragment alternatives add complexity for marginal benefit.
- The frontend strips the token from the URL after first load (`history.replaceState`) so screenshots / back-button don't expose it.

### Canonical URL detection

The server-config wizard step uses live request headers (the user is connected at that point):

1. `X-Forwarded-Proto` + `X-Forwarded-Host` if present (Railway, Caddy, nginx, fly-proxy)
2. Else `Host` header + request scheme

Basic syntactic validation (valid hostname[:port]). The result is prefilled into an editable form field; the operator confirms.

## Wizard flow

Linear flow with a sidebar showing progress. Users can back up to any prior completed step. All state lives in the server-side session, so refresh / back-button / network blip recover via `GET /install/session`.

### Screens

| # | Path | Purpose |
|---|---|---|
| 1 | `/install` | Landing / token entry. If query-param token validates → redirect to (2). Else: explainer page with paste-token textarea. |
| 2 | `/install/welcome` | One-paragraph "here's what we'll do" + Next. |
| 3 | `/install/llm` | Multi-select Anthropic / OpenAI / Gemini / OpenAI-compatible. Per-provider sub-form for API key. Each key live-validated against the provider's `/models` endpoint before advancing. |
| 4 | `/install/server` | Single screen with prefilled canonical URL (from forwarded headers) and confirm/edit field. Listen address is read-only display. **This screen runs before GitHub** because the canonical URL is baked into the GitHub App manifest's `redirect_url` and `callback_urls`; if it's wrong, the App is created on github.com with bad URLs and cannot be unmade by us. |
| 5 | `/install/github` | Choose **App** or **Token**. Token path: paste PAT, validate via `GET /user`, capture username. App path: collect owner + display name → server builds the GitHub App manifest using the canonical URL the user just confirmed in step 4, with `redirect_url` set to `<canonical_url>/install/github/app/redirect?state=<random>` and `callback_urls` set to `<canonical_url>/auth/callback/github` (the latter is for runtime OAuth login, not the install handoff) → user is redirected to `https://github.com/settings/apps/new` (or org equivalent) with the manifest as a form POST → after the user creates the App, GitHub redirects their browser back to `/install/github/app/redirect?code=…&state=…`. The endpoint validates `state` against the install session, exchanges the `code` via `POST https://api.github.com/app-manifests/{code}/conversions` for the App credentials (id, slug, client_id, client_secret, webhook_secret, pem), stores them in the session, then redirects the browser to `/install/github/done`. |
| 6 | `/install/review` | Read-only summary of every choice. "Install" button → `POST /install/finish`. |
| 7 | `/install/finishing` | Server: writes settings.toml, server.env, vault, dev token, then schedules process exit ~500ms after responding. Client renders the dev token (returned in the `/install/finish` response body) for the operator's records, then polls `GET /health` until the response no longer reports `"mode": "install"`, then redirects to the confirmed canonical URL. After a 30s timeout (no supervisor restarted the process — typically a local laptop), the screen pivots to: **"Install complete. Run `fabro server start` to launch your configured server. Your dev token: ___"**. The polling does not retry forever; it surrenders gracefully so the operator isn't stuck. |

### Navigation rules

- Session tracks which steps are complete.
- Sidebar lets users re-enter any completed step to edit.
- "Next" submits to the per-step endpoint and advances on 200; errors render inline.
- No "Save and exit" — only ways out are finish or process termination.

### Generated secrets

Session secret, JWT keypair, and dev token are generated server-side at `/install/finish` time and persisted to the appropriate locations (`server.env`, `server.dev-token`). The session secret and JWT keys are never exposed to the client — they exist only on disk on the server.

The dev token is a special case: the operator typically needs it to authenticate the `fabro` CLI against the new server, and forcing them to SSH/`docker exec` in to read `~/.fabro/storage/server.dev-token` defeats some of the value of the web wizard. So the dev token is **returned in the `/install/finish` response body** and rendered on the completion screen with a copy-to-clipboard control. The install token already authenticates the operator at that moment, so handing them the dev token over the same channel is no privilege escalation. No other secret leaves the server.

## API surface

Install endpoints live in `docs/api-reference/fabro-api.yaml` under a dedicated `install` tag. Mintlify exclusion via the existing tag-filter mechanism.

All endpoints under `/install`. Most require the install-token middleware; the one exception is `GET /install/github/app/redirect` (GitHub's manifest-conversion target — see row below for why it can't carry the install token, and how it's authorized by `state` instead).

| Method & path | Purpose | Notable request/response |
|---|---|---|
| `GET /install/session` | Returns the current `PendingInstall` snapshot — completed steps, recorded values (with secrets redacted), prefill data (canonical URL from forwarded headers, etc.). Frontend calls on mount to rehydrate. | Resp: `InstallSession` |
| `POST /install/llm/test` | Validates an LLM API key against the provider. Does not persist. | Req: `{ provider, api_key }`, Resp: `{ ok, error? }` |
| `PUT /install/llm` | Records the chosen providers + keys into the session. | Req: `LlmProvidersInput` |
| `POST /install/github/token/test` | Validates a GitHub PAT via `GET /user`, returns username. | Req: `{ token }`, Resp: `{ username }` |
| `PUT /install/github/token` | Records the PAT + username. | Req: `GithubTokenInput` |
| `POST /install/github/app/manifest` | Builds the GitHub App manifest. Returns the manifest JSON and the GitHub form-action URL the client should auto-submit the manifest to (`https://github.com/settings/apps/new` or `/organizations/<org>/settings/apps/new`). Stores the expected `state` in the install session for callback validation. The manifest's `redirect_url` is set to `<canonical_url>/install/github/app/redirect?state=<state>` (where the install token cannot travel because GitHub strips Authorization headers across redirects); `callback_urls` is set to `<canonical_url>/auth/callback/github` for later runtime OAuth login (matches what the CLI install does at `install.rs:1023`). | Req: `{ owner, app_name }`, Resp: `{ manifest, github_form_action }` |
| `GET /install/github/app/redirect` | GitHub manifest-conversion redirect target. **Not protected by install-token middleware** — GitHub strips Authorization headers, so this endpoint can only be authorized by validating the `state` query param against the install session. The handler validates `state`, then exchanges `code` via `POST https://api.github.com/app-manifests/{code}/conversions` to obtain the App's `id`, `slug`, `client_id`, `client_secret`, `webhook_secret`, and `pem`. Stores them in the session. Responds with a 302 to `/install/github/done?token=<install_token>` so the SPA picks back up with the token re-attached to the URL. | Query: `code`, `state` |
| `PUT /install/server` | Records canonical URL confirmation. | Req: `ServerConfigInput` |
| `POST /install/finish` | Validates session is complete, persists outputs, schedules clean exit, returns 202. The response carries the dev token so the operator can copy it from the completion screen without SSH-ing in. | Resp: `{ status: "completing", restart_url, dev_token }` |

### Why per-step `PUT` plus separate `test`

The validation calls (`POST /…/test`) make outbound network requests to the LLM provider / GitHub and shouldn't be conflated with state mutation. Lets the UI test multiple keys, fix typos, then commit. Aligns with how the CLI install today separates `authenticate_provider` from the recording step.

### Routing wiring

`build_install_router()` in `fabro_server::install::router` returns an `axum::Router` that mounts the table above. Install-token middleware is applied to all routes except `GET /install/github/app/redirect` (which is `state`-validated instead). The dispatch-layer fork (see *Process model* above) calls `build_install_router()` (install mode) or the existing `build_router()` (normal mode); never both.

## Persistence parity

The web wizard produces **the same on-disk state** as the CLI install. The TOML-merging primitives are reused as-is via the new `fabro-install` crate; vault writes go through a different code path (direct-to-disk, see previous section) but produce the same file at the same location with the same schema.

Files written:

- `~/.fabro/settings.toml` — server config, auth methods, GitHub integration strategy.
- `<storage_dir>/server.env` — `SESSION_SECRET`, `FABRO_DEV_TOKEN`, plus GitHub App env pairs (`GITHUB_APP_PRIVATE_KEY`, `GITHUB_APP_CLIENT_SECRET`, `GITHUB_APP_WEBHOOK_SECRET`) if the App strategy was chosen.
- `<storage_dir>/vaults/default/secrets.json` — vault entries for LLM API key credentials and (if Token strategy) `GITHUB_TOKEN`. Path matches `Storage::secrets_path()` at `lib/crates/fabro-config/src/storage.rs:38`.
- `<storage_dir>/server.dev-token` — the per-storage dev token, written via `Storage::server_state().dev_token_path()` at `storage.rs:103`. The CLI install also writes a home-level mirror at `Home::from_env().dev_token_path()` (`install.rs:1994-1999`); the web flow does the same to keep parity, since the home-level file is what tooling outside the storage dir expects to find.
- Artifact store metadata stamped with `FABRO_VERSION` via `write_artifact_store_metadata` (`install.rs:1458`).

The `/install/finish` handler is essentially the back half of `run_install_inner` (`install.rs:1925–:2061`) with two substitutions: the input source is replaced by the in-memory `PendingInstall`, and the vault-persist call goes through the new direct-to-disk path instead of the API-client path.

### `setup_github_app` refactor

The CLI version (`install.rs:1064`) spins up its own callback HTTP server because no other server is running at install time. The web version doesn't need that — the install-mode server is already running and exposes the manifest-conversion redirect target at `/install/github/app/redirect`. The refactor extracts the manifest-building, code-exchange, and credential-recording logic into transport-agnostic functions; the CLI keeps its embedded callback server, the web flow uses the install router.

### `persist_install_outputs` cannot be reused as-is in install mode

The current helper (`install.rs:1335`, `install.rs:1486`) persists vault secrets via `connect_api_client(storage_dir)`, which calls `start::ensure_server_running_for_storage` (`start.rs:73`). That function reuses the active server record and returns the bind of any server already running. In install mode the running server *is* the install-mode server, which does not mount `/api/v1/*` (it would 404 on `POST /api/v1/secrets`). So calling the helper unchanged from `/install/finish` would fail.

Decision: install mode persists vault secrets **directly to disk**, bypassing the API client. The implementation pattern is the one `persist_github_install_changes` already uses at `install.rs:1410-1421` — `Vault::load(storage.secrets_path()).set(name, value, type, description)` per secret, with rollback by snapshotting the prior file contents and restoring on error. The TOML-merging helpers (`merge_server_settings`, `write_*_settings`) and env-file writing (`persist_server_env_secrets` at `install.rs:1324`) are pure disk operations and reusable as-is.

Concretely: extract a `persist_install_outputs_direct` function in `fabro-install` that writes server.env, settings.toml, and vault directly (no API client). The CLI keeps using the API-client variant for `fabro install github` against a running server; the web `/install/finish` handler uses the direct variant. Both paths share the TOML-merging primitives.

## Error handling and edge cases

### Per-step validation errors

Bad API key, GitHub PAT lacks scopes, invalid hostname, etc. → 422 with structured error body the UI renders inline next to the offending field. Session unchanged.

### `/install/finish` failure

Load-bearing error case. v1 inherits the **partial-state semantics of the existing CLI helper** rather than refactoring for full atomicity.

Concretely, the persistence sequence is:

1. Write `server.env` (`install.rs:1479`).
2. Write `settings.toml` (`install.rs:1481-1484`).
3. Write vault secrets — in install mode, directly to `<storage_dir>/vaults/default/secrets.json` (see *`persist_install_outputs` cannot be reused as-is in install mode* in the previous section).

On error in step 3, the existing helper rolls back **only** `settings.toml`; `server.env` is left in place (verified by the test at `install.rs:2910`). The web flow inherits this behavior. The vault file is rolled back if step 3 fails partway (using the snapshot-and-restore pattern at `install.rs:1391-1444`).

On any persistence error:
- `settings.toml` restored to its pre-install contents (or removed if it didn't exist).
- `server.env` left as-written (partial-state — keys created in this attempt remain).
- Vault file restored to its pre-step-3 snapshot.
- Handler returns 500 with the error message + a list of which keys were left in `server.env` so the operator can clean up if they want.
- **Process does NOT exit.** Install mode stays up so the user can retry without losing the in-memory session. A retry will overwrite the same `server.env` keys.

This is good enough for v1 because (a) `server.env` keys are deterministic and idempotent — a retry produces the same content, (b) the only sensitive key (`SESSION_SECRET`) is regenerated each finalize so a leftover from a failed attempt is replaced atomically on retry, and (c) the operator can always wipe `server.env` and retry from a clean state. **Atomic rollback is a deliberate follow-up**, not a v1 requirement.

The dangerous edge: the GitHub App may already exist on github.com (real external side effect that local rollback can't undo). The error surface tells the user this and gives a "delete the App and retry" path. v1 does not auto-delete — GitHub's API for that requires the very credentials we just failed to persist.

### GitHub App callback timeout

User starts the App-creation flow on github.com, closes the tab, never returns. Session holds `pending_github_app: { state, expires_at }` for ~10 min. After expiry, the UI lets them retry (generates a fresh `state`). A late-arriving callback hits an unknown-`state` 400.

### OAuth `state` CSRF protection

`POST /install/github/app/manifest` generates a 32-byte `state` value, stores it in the install session, and embeds it in the manifest's `redirect_url` query string. `/install/github/app/redirect` validates `state` matches before exchanging the `code` with GitHub. This is the **only** authorization on that endpoint — the install token cannot be used because GitHub's manifest-conversion flow uses a 302 redirect to the user's browser and the `Authorization` header does not survive cross-origin redirects. Treating `state` as a single-use bearer-equivalent (consumed on first use, regenerated on retry) closes the CSRF / replay angle.

### Process restart mid-install

In-memory session is gone; new install token printed. User starts over. Acceptable because (a) install is short, (b) restarts shouldn't happen often, (c) supporting resume requires persisting partial install state — the surface we're explicitly keeping transient.

### Two operators with the same token

Both can load the wizard. They share the session; whoever submits a step last wins. The server logs a warning when a request's `User-Agent` or remote IP differs from the session's first-seen values but doesn't block. v1 acceptable.

### Health endpoint during install

`GET /health` returns 200 with `{ "mode": "install" }`. Required so supervisors (Docker, Railway) know the container is healthy and shouldn't restart it. Once normal mode boots, the same path returns the existing health response.

### `/api/v1/*` access during install

Install-mode router does not mount it. Any `/api/v1/*` request returns 404. Prevents partial-state queries against an unconfigured server.

### Logging

Per `docs-internal/logging-strategy.md` (re-read before implementing):

- `info!`: entering install mode (URL with token redacted in structured log fields, full URL only in the human-readable boot stderr message), each step completion (no values), finish success, scheduled exit.
- `warn!`: validation failures, suspected concurrent-operator activity.
- `error!`: persistence failures.

### Telemetry

Install-mode failures report to Sentry via `fabro-telemetry` with the existing anonymous-ID conventions. Successful installs emit a "first-run completed" telemetry event.

## Testing strategy

Per `docs-internal/testing-strategy.md` (re-read before implementing).

### Unit tests (`fabro-install` crate)

- `merge_*` / `write_*_settings` TOML transforms (port from existing `install.rs` tests).
- `PendingInstall` state machine: which steps are required/optional, what counts as complete.
- Install-token middleware: header / query / `X-Install-Token` precedence; reject on mismatch; reject when no session exists.
- OAuth `state` generation and validation.

### Integration tests (`fabro-server/tests/it/install/`)

- **Spec/router conformance, expanded.** The existing test at `lib/crates/fabro-server/tests/it/openapi_conformance.rs:62` only instantiates `build_router(...)`. With install endpoints in the spec but only mounted in `build_install_router`, those paths would hit nothing in `build_router` → 404 (not 405), which the test treats as passing. So the test would silently miss install drift. The fix: split spec iteration by tag — paths tagged `install` are sent to `build_install_router`, all other paths to `build_router`. Add a second pass that asserts install paths *do not* resolve in `build_router` and normal paths *do not* resolve in `build_install_router`, so cross-mounting is caught too.
- **End-to-end happy path with stub providers.** Boot a server with temp empty `~/.fabro`, follow the per-step API: test LLM key → record → test GitHub PAT → record → finish. Assert resulting `settings.toml`, `server.env`, vault contents match snapshot. Use `httpmock` for upstream calls.
- **Mode detection at dispatch.** A direct unit test of the `commands::server::dispatch` fork: with empty home directory → install bootstrap path is selected (no settings load attempted). With valid `settings.toml` → existing path. With malformed `settings.toml` → parse error (does not fall back to install mode).
- **Install router behavior.** Boot the install router directly: install endpoints respond (with valid token), `/api/v1/*` returns 404, `/health` returns `{"mode":"install"}`, the SPA shell HTML includes `window.__FABRO_MODE__ = "install"`.
- **Normal router behavior.** Boot the normal router directly: install endpoints return 404, `/api/v1/*` works, the SPA shell HTML does not include the install mode flag.
- **Token rejection.** All install endpoints (except `GET /install/github/app/redirect`) called without token → 401. Wrong token → 401. Valid token → 200/422.
- **GitHub App `state` validation.** `GET /install/github/app/redirect` called with mismatched or missing `state` → 400, session unchanged, no GitHub API call attempted.
- **GitHub App happy-path roundtrip.** With install token in session: `POST /install/github/app/manifest` returns a manifest containing the canonical URL the operator confirmed in the Server step + a fresh `state`. Then `GET /install/github/app/redirect?code=stub-code&state=<that-state>` is called with `httpmock` standing in for `https://api.github.com/app-manifests/stub-code/conversions` (returning a fixture body with `id`, `slug`, `client_id`, `client_secret`, `webhook_secret`, `pem`). Assert: the install session now holds `pending_github_app: { app_id, slug, client_id, ... }` populated from the fixture, the response is a 302 to `/install/github/done?token=<install_token>`, the manifest's `redirect_url` and `callback_urls` were built from the canonical URL (not the install-mode bind address). This is the riskiest new path in the design — it covers code-exchange wiring, session population, redirect-with-token handling, and that the canonical-URL ordering decision actually flows through to the manifest.
- **Finish failure partial-state semantics.** Force a vault write to fail; assert `settings.toml` is restored to its prior state, `server.env` keys written this attempt are *left in place* (matches the existing test at `install.rs:2910`), the vault file is restored to its pre-step-3 snapshot, the response carries the list of leftover env keys, and the process does not exit.
- **Forwarded-host detection.** Request with `X-Forwarded-Host: foo.com` + `X-Forwarded-Proto: https` → `GET /install/session` returns prefilled canonical URL `https://foo.com`.
- **Force-foreground in install mode.** Invoke `fabro server start` (no `--foreground`) on a tempdir with no settings → assert the parent process does NOT spawn a `__serve` daemon child, runs install in-process, prints the install token + URL to its own stderr (capture and grep). Assert that `--foreground` would have produced the same behavior.
- **`--no-web` ignored during install.** Invoke `fabro server start --no-web` on a tempdir with no settings → assert the install router still mounts the SPA static routes, `GET /install` returns the install HTML, and a stderr warning was emitted: `Warning: --no-web is ignored during install; will be respected on next start.`. After `/install/finish` and a fresh `fabro server start --no-web`, assert the normal-mode router does NOT mount the SPA static routes.

### Frontend tests (`apps/fabro-web`, Bun test)

- Install router renders the correct screen for each session state.
- Token-from-URL extraction + `history.replaceState`.
- Per-step form validation.
- Polling logic for the post-finish "waiting for restart" screen.

### Manual smoke test (documented in implementation plan)

Process-exit behavior is too OS-flaky for CI. Two manual tests:

1. **Local (no supervisor).** With an empty home: `fabro server start` → install mode prints token directly to the terminal (foreground) → complete the wizard → assert the foreground process exits with code 0 and the operator's shell prompt returns → assert the wizard completion screen shows the manual-restart message after the polling timeout → run `fabro server start` again → assert it boots into normal mode and the configured app is reachable.
2. **Supervised (Docker).** `docker compose up` against an empty named volume → install mode prints token in `docker logs` → complete the wizard → assert the container restarts via the compose `restart: unless-stopped` policy → assert the wizard completion screen redirects to the configured canonical URL within ~10s of finish.

### E2E live tests

None for v1. Real GitHub App creation against github.com is out of scope for automated CI; the `setup_github_app` extraction means the manifest-building logic is unit-testable.

### Snapshot tests for wizard screens

Deferred. Design isn't stable enough to lock in screenshots.

## Orchestration config updates

### Container packaging (load-bearing for v1)

The published container image today bakes `/etc/fabro/settings.toml` (one line: `server.storage.root = "/storage"`) and sets `ENV FABRO_CONFIG=/etc/fabro/settings.toml` (`Dockerfile:32, :35-36`). Under the install-mode trigger rules (explicit-config carveout), this means **the container would never enter install mode** — defeating the entire remote-first deployment use case the design exists to enable.

v1 must change the container packaging:

1. **Drop `COPY docker/settings.toml /etc/fabro/settings.toml`** from the Dockerfile. No baked settings file.
2. **Drop `FABRO_CONFIG=/etc/fabro/settings.toml`** from the `ENV` directive. Let the default `~/.fabro/settings.toml` resolution apply.
3. **Convey the storage root via `FABRO_STORAGE_DIR=/storage`** in the Dockerfile `ENV`. The install bootstrap and the post-install `start::execute` both honor `FABRO_STORAGE_DIR`, so the volume mount continues to work without a settings file.
4. **Persist `~/.fabro` across container restarts.** The install writes `~/.fabro/settings.toml` (which is `/var/fabro/.fabro/settings.toml` given `FABRO_HOME=/var/fabro`). Today `/var/fabro` is *not* a volume, so a container restart would wipe the settings and re-trigger install on every boot. Two options:
   - **A) Add `VOLUME ["/var/fabro/.fabro"]`** to the Dockerfile. Compose / Railway users get a separate named volume for config.
   - **B) Change `FABRO_HOME` to live inside the existing `/storage` volume** (e.g., `FABRO_HOME=/storage/.home`). One volume, one mount point, settings live alongside storage data.
   - Recommendation: **B**. One volume is simpler operationally (a single `volume_mounts: [/storage]` declaration covers both data and config) and matches the typical "one volume per service" convention. Update `docker/entrypoint.sh` to ensure `/storage/.home` exists with the right ownership at boot.

These changes are part of v1 implementation, not a follow-up.

### Restart policy

For clean-exit + supervisor restart to work end-to-end:

- **`compose.yml`:** add `restart: unless-stopped` to the fabro service (verify it's present; add if missing).
- **Railway:** verify the service restart policy. Railway restarts crashed processes by default; confirm exit code 0 also triggers a restart (may require setting a restart policy explicitly).
- **fly.toml** (if applicable): same — verify restart on exit-0.

### Boot-stderr environment detection

The install-mode boot stderr should tailor its post-finish guidance to the deployment context:

- For supervised deployments: "the server will restart automatically."
- For local use: "after install you'll be prompted to re-run `fabro server start`."

Detect supervised vs. local heuristically (`RAILWAY_*` env vars, `/.dockerenv`, `/run/.containerenv`, `KUBERNETES_*`) and emit the appropriate variant.

## Open questions

None at spec time. All design questions resolved during brainstorming.
