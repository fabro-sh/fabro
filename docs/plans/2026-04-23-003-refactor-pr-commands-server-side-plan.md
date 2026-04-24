---
title: Move fabro pr commands server-side
type: refactor
status: active
date: 2026-04-23
deepened: 2026-04-23
---

# Move fabro pr commands server-side

## Overview

`fabro pr create|view|list|merge|close` currently run GitHub operations from the client using credentials loaded from the client's local vault. This breaks when the fabro server runs on a remote host, which is now the common deployment shape. Move the work to new fabro HTTP API endpoints so the CLI becomes thin presentation. The server already loads its own GitHub credentials for the in-run `pull_request` pipeline stage; the new endpoints reuse that path.

## Problem Frame

Audit of `lib/crates/fabro-cli/src/commands/pr/` found all five commands reach past the API boundary:

- `load_github_credentials_required` (`pr/mod.rs:35`) reads `base_ctx.machine_settings()` and the client-local vault for every subcommand, then calls `fabro_github::*` directly — fails on remote server because the client needs its own GitHub App / `GITHUB_TOKEN`.
- `pr create` is the worst offender: it rebuilds server run state client-side via `rebuild_run_store` (`pr/create.rs:31`), runs `detect_repo_info(&cwd)` + `ensure_matching_repo_origin` (requires client to be in a matching git clone), generates the PR body with a client-side LLM call using client-loaded provider keys (`create.rs:101-127`), calls GitHub from the client, and never writes the resulting `PullRequestRecord` back to the server.
- All five carry `#[allow(deprecated, reason = "boundary-exempt(pr-api): remove with follow-up #1 when PR ops move server-side")]`.

The server already owns everything needed: `AppState::github_credentials(...)` loads creds; `run_manifest.rs:373-381` wires them into `PullRequestOptions`; `maybe_open_pull_request` in `fabro-workflow` contains the full create flow; `PullRequestCreated`/`PullRequestFailed` events already exist and already feed `RunProjection.pull_request`.

## Requirements Trace

### API Contract

- R1. `fabro pr {create,view,list,merge,close}` work correctly against a remote fabro server without the client holding any GitHub credentials.
- R5. Existing CLI surface is preserved: same command names, flags (`--force`, `--model`, `--all`, `--method`, `--json`), same stdout/stderr shape (URL on success, tables for list, etc.).

### Client-Side Cleanup

- R2. The CLI crate stops importing client-loaded GitHub creds for PR ops. `load_github_credentials_required` is deleted. `boundary-exempt(pr-api)` annotations are removed.
- R3. `pr create` stops rebuilding run state client-side (`rebuild_run_store`), stops reading the client's cwd (`detect_repo_info`/`ensure_matching_repo_origin`), and stops picking the LLM model from client-side `configured_providers_from_process_env`.
- R6. `rebuild_run_store` keeps working for its other callers (`run fork`, `run rewind`) — only `pr/create.rs` stops calling it. The helper itself is not deleted in this plan.

### State Correctness

- R4. The persisted `PullRequestRecord` on the server matches what `maybe_open_pull_request` returns, i.e. `state.pull_request` is populated after `fabro pr create` the same way it is after the in-run PR stage. The existing `PullRequestCreated` event is the mechanism.

## Scope Boundaries

This refactor moves GitHub operations from client to server. It intentionally does not expand into broader run-model cleanup, lifecycle bookkeeping, or multi-host support.

**The plan must be read in light of these invariants:**
- **Runs without git metadata remain valid.** `RunSpec.repo_origin_url` and `RunSpec.base_branch` stay `Option<String>`. PR operations simply don't apply to runs that lack the required git metadata — the endpoints return explicit 400s for those runs.
- **PR operations are available only for runs that have sufficient git metadata** (repo origin, base branch, run branch, non-empty diff on create).
- **github.com is the only supported host.** GitHub Enterprise is not in scope.
- **Fabro stores PR creation, not authoritative PR lifecycle state.** After `pr create`, the `PullRequestRecord` is persisted via `PullRequestCreated`. After `pr merge` or `pr close`, nothing is written to run state — GitHub is the source of truth for current PR status.
- **`pr view` and `pr list` depend on live GitHub availability** to surface current status; they are not offline operations.

**In scope:**
- Four new run-scoped fabro-api endpoints (view, merge, close, create) and their progenitor-regenerated clients.
- Five rewritten CLI commands (`pr list` is CLI-only composition).
- Reuse of the existing `PullRequestCreated` event to persist the record on `pr create` — no new event variants.
- Host check restricting outbound GitHub calls to github.com.
- Deletion of `load_github_credentials_required` and related client-side PR helpers.
- Updates to CLI integration tests.

**Out of scope:**
- Moving `run fork` and `run rewind` server-side (tracked separately — still uses `rebuild_run_store`).
- Deleting `rebuild_run_store` (still used by fork/rewind).
- UI changes in `apps/fabro-web`.
- Changing `PullRequestRecord`'s existing fields.
- New `PullRequestMerged` / `PullRequestClosed` event variants, reducer arms, or durable merged/closed state.
- GitHub Enterprise / multi-host routing.
- New server-side audit events or audit-event infrastructure.
- `--draft` / `--no-draft` CLI flag, `model_used` in the create response, or any other CLI contract change that isn't needed to complete the move.
- LLM `body.model` allowlist, per-subject cost ceiling, or any AI-safety infrastructure.
- A global `GET /pull_requests` endpoint — `pr list` is CLI-side composition.

## Context & Research

### Relevant Code and Patterns

- Server handler template — run-scoped POST with JSON body: `archive_run` (`lib/crates/fabro-server/src/server.rs:6448`), `cancel_run` (`server.rs:6085`), `append_run_event` (`server.rs:4990`). All use `AuthorizeRunScoped` or `Path(id)` + `parse_run_id_path`, `state.store.open_run(&id)`, and return `Json(...)`.
- Server-side GitHub creds helper: `AppState::github_credentials(&self, settings)` at `lib/crates/fabro-server/src/server.rs:695`. Already used by `run_manifest.rs:373-381` when building `PullRequestOptions` for the in-run pipeline stage. This is the canonical path — the new handlers call the same helper.
- OpenAPI path template: `appendRunEvent` at `docs/api-reference/fabro-api.yaml:940-971` — operationId in camelCase, `$ref: "#/components/parameters/RunId"` for the run ID, body and response schemas as `$ref` components.
- Existing OpenAPI PR type: `RunPullRequest` (`fabro-api.yaml:4227-4247`) is a presentation summary (`number, additions, deletions, comments, checks`), not the full record. It stays as-is. This plan adds two new distinct schemas: `PullRequestRecord` (the full persisted record, aligned with `lib/crates/fabro-types/src/pull_request.rs:5` and reused via `fabro-api/build.rs` `with_replacement(...)` per `CLAUDE.md` API type ownership policy) and `PullRequestDetail` (stored record + live GitHub fields, response of `pr view`). `pr list` has no HTTP response schema — CLI composes the list from `list_runs` + per-run view calls. Neither new schema replaces `RunPullRequest`.
- Server-secrets handling: follow `docs-internal/server-secrets-strategy.md`. The new handlers must not surface `GITHUB_APP_PRIVATE_KEY` PEM bytes or `GITHUB_TOKEN` values in HTTP response bodies, error messages, or tracing spans. Upstream GitHub errors pass through redaction before surfacing. Use `fabro_util` redaction helpers, never raw string interpolation of credentials.
- GitHub API call surface: `fabro_github::{create_pull_request, get_pull_request, merge_pull_request, close_pull_request, branch_exists, ssh_url_to_https, parse_github_owner_repo, github_api_base_url}`. All are already callable from server code; the worker crate uses them today.
- Core create-flow reuse: `fabro_workflow::pull_request::maybe_open_pull_request` at `lib/crates/fabro-workflow/src/pipeline/pull_request.rs:405`. Takes creds, origin URL, branches, goal, diff, model, `draft`, `AutoMergeOptions`, `run_store`, `conclusion`. Returns `Option<PullRequestRecord>` (None on empty diff). The server-side handler calls this directly — no new GitHub-call code.
- Event emission for create: `PullRequestCreated` variant at `lib/crates/fabro-workflow/src/event.rs:498-507`, emission site at `pipeline/pull_request.rs:545`. The new `POST /runs/{id}/pull_request` handler emits the same event via `state.store.open_run(&id).append_event(...)` (see `server.rs:5018-5025` for the pattern).
- Client method template: `Client::cancel_run` (`lib/crates/fabro-client/src/client.rs:717-723`) for simple POSTs; `Client::create_secret` (`client.rs:525-535`) for POST-with-body-returning-JSON.
- Precedent: `refactor(dump)` in commit `1481ecf2a` — same-shape boundary cleanup on a smaller surface.
- Existing CLI integration tests: `lib/crates/fabro-cli/tests/it/cmd/pr_{create,view,list,merge,close}.rs`. These currently exercise the client-side-GitHub path; they're rewritten to exercise the server path.

### Institutional Learnings

- `docs/solutions/` is empty. No prior PR-boundary learnings.
- `CLAUDE.md` API type ownership: when an OpenAPI schema has the same semantics as an existing Rust type, reuse the Rust type via `with_replacement(...)` in `fabro-api/build.rs` and add a `fabro-api` test proving JSON parity. `PullRequestRecord` qualifies.
- `CLAUDE.md` testing guidance: CLI integration tests should create state through public commands; writing run internals is disallowed. The existing `pr_*.rs` tests already follow this.

### External References

None. All grounding is in-repo.

## Key Technical Decisions

- **Four run-scoped endpoints; no global list.** Routes: `GET /api/v1/runs/{id}/pull_request` (view), `POST /api/v1/runs/{id}/pull_request` (create), `POST /api/v1/runs/{id}/pull_request/merge`, `POST /api/v1/runs/{id}/pull_request/close`. `pr list` has no endpoint — CLI composes it from existing operations. *Rationale:* run-scoped auth stays simple (`AuthorizeRunScoped`); avoids inventing a new auth model for a global endpoint.

- **Reuse `PullRequestRecord` via `with_replacement`.** Add the schema to OpenAPI for contract visibility, but map the progenitor-generated type back to `fabro_types::PullRequestRecord` so Rust server, Rust client, and CLI all speak one type. *Rationale:* CLAUDE.md policy; `PullRequestRecord` already has the right shape and is already in `RunProjection`.

- **Only `PullRequestCreated` is persisted.** `pr merge` and `pr close` call GitHub and return success responses; they do not append events, do not update `state.pull_request`, do not add new event variants. *Rationale:* GitHub is the source of truth for current PR state. Fabro records that *a PR was opened* (for later reference by view/list); it does not track the PR's full lifecycle. This keeps the refactor focused and avoids growing the event schema for state Fabro doesn't need to own.

- **`RunSpec.repo_origin_url` and `base_branch` stay `Option<String>`.** Runs without git metadata remain valid. PR operations on such runs return explicit 400s naming the missing field. *Rationale:* out of scope to change run-model invariants; the refactor is about moving GitHub ops, not tightening run creation.

- **Server does its own branch/diff/conclusion validation.** What `pr/create.rs` validates client-side (`conclusion.status`, `run_branch` present, `final_patch` non-empty, branch exists on GitHub) all moves to the server handler, reading from `state.store.open_run(&id)` directly. *Rationale:* server has authoritative state; avoids race conditions between client rebuild and server truth.

- **CLI request bodies preserve existing flags.** `pr create` body: `{ force: bool, model: Option<String> }` — same shape as today's CLI args. No `--draft` flag added (current behavior is draft PR creation; keep it). *Rationale:* keep the CLI contract stable; only change what the refactor requires.

- **Live GitHub state stays server-side on `view`.** `GET /runs/{id}/pull_request` requires a stored `PullRequestRecord`, calls GitHub live, returns `PullRequestDetail` (stored record + live fields: `state`, `draft`, `merged`, `title`, `html_url`, `head`/`base` ref, `user`, `additions`, `deletions`, `changed_files`, `body`). `merged` is distinct from `state` — GitHub's API returns `state: "open" | "closed"` + a separate `merged: bool`; Unit 4's list classifier depends on this separation. *Rationale:* client stays credential-free for display; GitHub is authoritative for current PR status.

- **`pr list` is CLI-side composition with cheap skip.** CLI calls `list_runs`, filters client-side to runs whose existing run state already contains a stored `pull_request` record (via `get_run_state` or an equivalent lightweight probe), then calls `get_run_pull_request` only for those runs via `buffer_unordered(10)`. Runs without a stored record are skipped without any GitHub probe. *Rationale:* the cheap path — most runs don't have PRs; don't pay for GitHub calls on those.

- **No `rebuild_run_store` from the server handlers.** Server reads `RunProjection` directly via `state.store.open_run(&id)` / in-memory live state. *Rationale:* the reason `rebuild_run_store` exists is that the CLI couldn't touch server state — the server obviously can.

- **Error model across the four endpoints:**
  - `404 no_stored_record` — run has no stored `PullRequestRecord`.
  - `409 conflict` — `pr create` called when a stored record already exists.
  - `400 bad_request` — run precondition failure: missing `repo_origin_url`, missing `base_branch`, missing `run_branch`, empty diff, unsupported host, or similar.
  - `502 github_not_found` — stored record exists but GitHub no longer has that PR.
  - `503 integration_unavailable` — server GitHub creds missing or disabled. Generic external body; detailed diagnostic in log only. Authorizer runs first.

- **Accept the TOCTOU race on concurrent `pr create`.** Two simultaneous creates can both pass the None-check and both call GitHub before either emits, producing duplicate GitHub PRs. *Rationale:* narrow collision window; recoverable symptom (user closes the duplicate); mutex/intent-event alternatives are disproportionate. Documented in Risks.

- **Host check: github.com only.** Before any outbound GitHub call, verify the origin host is `github.com`. Reject with 400 `unsupported_host` otherwise. The source of the host differs by endpoint: `pr create` parses it from `run_spec.repo_origin_url` (the record doesn't exist yet); `pr view` / `pr merge` / `pr close` parse it from `record.html_url` (always present on any stored record, no cross-reference to run spec needed). *Rationale:* simpler than an allowlist, aligns with current scope; defense in depth against SSRF. GitHub Enterprise support is not in scope. Two-source check is deliberate — each endpoint has exactly one authoritative value available at the time of check.

- **Capture the GitHub API base URL once, at `AppState` construction.** Today `fabro_github::github_api_base_url()` reads `GITHUB_BASE_URL` from env on every call (`fabro-github/src/lib.rs:8`), and each outbound request template is `{base_url}/repos/{owner}/{repo}/...` (`fabro-github/src/lib.rs:969, 1039, 1100`). The origin host check on `repo_origin_url` / `html_url` therefore only validates our *intent* to talk to github.com — the actual authenticated HTTP destination comes from the env read, which can drift between requests. Fix: read `GITHUB_BASE_URL` (or its default `https://api.github.com`) once during `AppState` construction, store it on the state struct, and have every new PR handler pass **that** value to the outbound GitHub functions. This means two kinds of changes: (1) direct calls like `branch_exists`, `get_pull_request`, `merge_pull_request`, `close_pull_request` already take `base_url` and simply get the captured value instead of a fresh env read; (2) the shared helper `fabro_workflow::pull_request::maybe_open_pull_request` at `pipeline/pull_request.rs:405` grows a `base_url: &str` parameter so it stops hardcoding `github_api_base_url()` internally at lines 440 and 457 — handlers that go through this helper (Unit 5) thread the captured value through. Runtime env mutations after server start have no effect. *Rationale:* closes the SSRF-via-env-mutation gap that the per-request host check alone doesn't cover; test infrastructure keeps its override path (tests construct `AppState` with a twin base URL at startup). The existing in-run pipeline caller of `maybe_open_pull_request` passes `&github_api_base_url()` explicitly to preserve today's behavior — fully hardening that path is out of scope for this plan but now trivially possible.

- **Stage-less event envelope on HTTP-originated `PullRequestCreated`; audit consumers.** The HTTP handler emits `PullRequestCreated` without a stage scope (envelope lacks `stage_id`/`node_id`). *Rationale:* honest — it wasn't emitted by a pipeline stage. Before landing Unit 5 (the only unit that emits), audit every consumer of the event that groups or filters by `stage_id` (run_progress UI, SSE replay, `RunProjection` hydration, analytics) and add integration tests proving each tolerates `stage_id = None`.

- **Sequencing: simplest first, `create` last.** Order is view → merge → close → list (CLI-only) → create → cleanup. *Rationale:* view establishes the pattern; merge and close are tiny endpoints with no event work; list is pure CLI; create carries the most logic and is done last when the pattern is proven.

## Open Questions

### Resolved During Planning

- *Where does the server get GitHub credentials?* → `AppState::github_credentials(settings)` at `server.rs:695`. Same path `run_manifest.rs:373` uses today.
- *Where does the server get LLM credentials for PR body generation?* → `fabro_auth::configured_providers_from_process_env(state.vault.as_ref())`, same helper `operations/start.rs:322` uses.
- *Does `pr merge`/`pr close` need new events or durable state updates?* → **No.** GitHub is the source of truth for current PR state. Fabro records PR creation only. `view` / `list` re-read from GitHub on every call.
- *Does `pr create` need `ensure_matching_repo_origin`?* → No. Server uses `run_spec.repo_origin_url` directly — there's no cwd on the server.
- *Should there be a global `GET /pull_requests` endpoint?* → No. CLI composes `pr list` by filtering `list_runs` to runs with a stored `pull_request` record, then calling `get_run_pull_request` for each via `buffer_unordered(10)`.
- *What happens if `state.pull_request` already exists when `POST /runs/{id}/pull_request` is called?* → Return 409 Conflict with the existing record.
- *What LLM model does `create` default to on the server?* → `Catalog::builtin().default_for_configured(&configured)` using the server's configured providers.
- *What about concurrent `pr create` on the same run?* → Race accepted. Two simultaneous calls may produce duplicate GitHub PRs. Documented in Risks.
- *What authorizer on the mutating endpoints?* → `AuthorizeRunScoped` (same as existing run-scoped mutators like `archive_run`, `cancel_run`). Blast-radius expansion noted in Risks.
- *What about SSRF via `repo_origin_url`?* → Host check: github.com only. Reject with 400 on any other host.
- *What about GitHub Enterprise?* → Not supported in this plan. github.com only.
- *HTTP status for missing GitHub creds?* → 503 Service Unavailable, generic external body (`integration_unavailable`); detailed diagnostic only in logs; authorizer runs first.
- *HTTP status for the two "not found" cases on view?* → 404 `no_stored_record` when `state.pull_request` is None; 502 `github_not_found` when stored record exists but GitHub returns 404.
- *Should `RunSpec.repo_origin_url` and `base_branch` be made required?* → **No.** They stay `Option<String>`. Runs without git metadata remain valid runs. PR operations on such runs return 400 naming the missing field.
- *Does this plan add audit-event machinery?* → No. Per-user GitHub attribution is lost (server acts under its App identity); noted as an accepted tradeoff in Risks. Revisit in a follow-up if needed.
- *Does this plan add a `--draft` flag or a `model_used` response field?* → No. CLI contract stays as close to current as possible.

### Deferred to Implementation

- Exact progenitor method names — they follow `operationId` but sometimes snake_case differs (e.g., `createPullRequestForRun` vs `create_pull_request_for_run`). Verified at build time.
- Whether `pr list`'s cheap-skip should probe `state.pull_request` via `get_run_state` per run (one extra round trip per candidate) or via a richer `list_runs` response that already carries that field. Decide during implementation based on whether `list_runs`'s current response shape already includes it.
- Whether to unbound the CLI-side concurrency cap in a follow-up if 10 turns out to be too conservative for typical `pr list` sizes.

## High-Level Technical Design

> *This illustrates the intended approach and is directional guidance for review, not implementation specification. The implementing agent should treat it as context, not code to reproduce.*

### Request flow (create, representative of the pattern)

```
CLI (fabro pr create RUN --force --model M)
   │
   ├─ parse args, build request body { force, model }
   │
   └─ HTTP POST /api/v1/runs/{id}/pull_request
         │
         ▼
      Server handler create_run_pull_request
         │
         ├─ resolve run id, load RunProjection
         ├─ validate repo_origin_url, base_branch, run_branch all present (else 400)
         ├─ validate host(run_spec.repo_origin_url) == github.com (else 400)
         ├─ validate conclusion.status (or --force)
         ├─ validate final_patch non-empty after trim (else 400)
         ├─ validate state.pull_request is None (else 409)
         ├─ load GitHub creds via AppState::github_credentials (else 503)
         ├─ load LLM providers via configured_providers_from_process_env
         ├─ pick default model if body.model is None
         ├─ call maybe_open_pull_request(...) (draft=true, matching current CLI)
         ├─ append PullRequestCreated event (stage-less envelope) via run_store
         └─ return Json(PullRequestRecord)
         │
         ▼
      CLI: print record.html_url (or --json)
```

### Endpoint summary

| Verb | Path | CLI | Request body | Response |
|---|---|---|---|---|
| POST | `/api/v1/runs/{id}/pull_request` | `pr create` | `{ force: bool, model: Option<String> }` | `PullRequestRecord` (400 on missing git metadata or empty diff; 409 if already created; 503 if creds missing). Emits `PullRequestCreated`. |
| GET | `/api/v1/runs/{id}/pull_request` | `pr view` | — | `PullRequestDetail` (stored record + live GitHub fields); 404 `no_stored_record` if no record; 502 `github_not_found` if GitHub can't find it; 503 if creds missing. |
| POST | `/api/v1/runs/{id}/pull_request/merge` | `pr merge` | `{ method: MergeMethod }` | `{ number, html_url, method }`. No event emitted. No state update. |
| POST | `/api/v1/runs/{id}/pull_request/close` | `pr close` | — | `{ number, html_url }`. No event emitted. No state update. |

`pr list` has no server endpoint — CLI filters `list_runs` to runs with a stored `pull_request`, then calls `GET /runs/{id}/pull_request` per matching run via `futures::StreamExt::buffer_unordered(10)`.

**CLI contract change note (intentional):** `pr view` and `pr list` responses pass through richer `PullRequestDetail` fields than the old CLI emitted (e.g., structured `head`/`base`, numeric `additions`/`deletions`). `--json` output therefore becomes richer than before. Human-readable output (table for list, formatted view) stays as close to current behavior as practical; any deliberate contract change is called out in the relevant unit.

### What the CLI files look like after this plan

Each `pr/*.rs` file collapses to: parse args → call `client.<method>()` → print. No `fabro_github::` import. No `load_github_credentials_required`. No `rebuild_run_store`. No `detect_repo_info`. Comparable shape to the post-refactor `dump.rs`.

## Implementation Units

- [ ] **Unit 1: Add `pr view` endpoint + rewrite CLI**

**Goal:** Simplest endpoint establishes the pattern. Server-side GitHub call replaces client-side GitHub call.

**Requirements:** R1, R2, R5

**Dependencies:** None.

**Files:**
- Modify: `docs/api-reference/fabro-api.yaml` — add `GET /api/v1/runs/{id}/pull_request` endpoint + schemas:
  - `PullRequestRecord` — the full record (reused by the create response in Unit 5; establishing it here).
  - `PullRequestDetail` — view response = stored record + live GitHub fields, including a `merged: bool` field distinct from `state`.
- Modify: `lib/crates/fabro-github/src/lib.rs` — extend the in-crate `PullRequestDetail` struct to capture GitHub's `merged: bool` and `merged_at: Option<String>` fields (currently missing at line 20, which is why today's CLI can't actually surface "merged" state — the `Color::Magenta` branch in `pr/list.rs:140` is dead code). Update the GitHub API deserializer accordingly.
- Modify: `lib/crates/fabro-api/build.rs` — add `with_replacement("PullRequestRecord", "fabro_types::PullRequestRecord")`.
- Modify: `lib/crates/fabro-server/src/server.rs` — (1) add a `github_api_base_url: String` field on `AppState` populated at construction by calling `fabro_github::github_api_base_url()` **once** (so a later env mutation can't redirect traffic); (2) add handler `get_run_pull_request`, register route in `build_router()`. The new handler passes `state.github_api_base_url.as_str()` to `fabro_github::get_pull_request` instead of calling `github_api_base_url()` at request time. This capture field is reused by Units 2, 3, 5.
- Modify: `lib/crates/fabro-client/src/client.rs` — add `Client::get_run_pull_request(&self, run_id: &RunId) -> Result<PullRequestDetail>`.
- Rewrite: `lib/crates/fabro-cli/src/commands/pr/view.rs` — call the new client method; remove `load_github_credentials_required` call + `fabro_github::get_pull_request` call.
- Test: `lib/crates/fabro-cli/tests/it/cmd/pr_view.rs` — update expectations; real server path; assert `merged` field round-trips correctly.
- Test: `lib/crates/fabro-api/tests/` — JSON round-trip parity test for `PullRequestRecord`. Use a shared fixture asserting that (a) deserializing it as the progenitor-generated schema and (b) deserializing it as `fabro_types::PullRequestRecord` both succeed and re-serialize to byte-identical JSON. This is a wire-format test, not a type-identity check. Cover every inclusion site as later units add them (create response in Unit 5, inside `PullRequestDetail` here).

**Approach:**
- Handler loads `RunProjection` via `state.store.open_run(&id)`, reads stored `PullRequestRecord`, calls `fabro_github::get_pull_request` with server creds, composes `PullRequestDetail` (record + live fields: state, draft, **merged**, title, html_url, head/base ref, user, additions, deletions, changed_files, body). `merged` is independent of `state` — GitHub's API returns `state: "open" | "closed"` plus a separate `merged: bool`; a merged PR has `state: "closed"` + `merged: true`, a closed-without-merging PR has `state: "closed"` + `merged: false`.
- Error shapes: return 404 with body `{"error": "no_stored_record", "message": "..."}` when `state.pull_request` is None; return 502 with body `{"error": "github_not_found", "message": "..."}` when stored record exists but GitHub returns 404. Return 503 with generic external body `{"error": "integration_unavailable"}` when server GitHub creds are missing (detailed diagnostic in log only; authorizer runs first).
- Host check: parse the host from `record.html_url` (always present, always the rendered GitHub host) and verify it equals `github.com`. Reject with 400 `unsupported_host` otherwise. Do **not** rely on a separate `origin_host` field on `PullRequestRecord` — no such field exists; `html_url` is the source of truth for existing records. (GitHub Enterprise is not in scope for this plan.)
- Follow `archive_run` handler pattern for path/auth/response shape.

**Patterns to follow:**
- `archive_run` at `server.rs:6448` — run-scoped action handler shape.
- `appendRunEvent` OpenAPI path at `fabro-api.yaml:940-971`.
- `Client::cancel_run` at `client.rs:717-723`.

**Test scenarios:**
- Happy path (open): stored record + GitHub returns `state: "open"`, `merged: false` → `PullRequestDetail.state == "open"`, `merged == false`; CLI prints `#N title / State: open / URL / Branch / Author / Changes / body`.
- Happy path (merged): GitHub returns `state: "closed"`, `merged: true` → `PullRequestDetail.merged == true`; CLI prints `State: merged` (distinct from closed).
- Happy path (closed-not-merged): GitHub returns `state: "closed"`, `merged: false` → CLI prints `State: closed`.
- Happy path (draft): GitHub returns `draft: true`, `merged: false` → CLI prints `State: draft`.
- Error path: run without `state.pull_request` → 404 with `{"error": "no_stored_record"}`; CLI prints "No pull request found for this run. Create one first with: fabro pr create …".
- Error path: stored record exists, GitHub API returns 404 → 502 with `{"error": "github_not_found"}`; CLI prints a distinct message ("PR #N was deleted on GitHub").
- Error path: server GitHub creds missing → 503 with `{"error": "integration_unavailable"}`; CLI prints "GitHub integration unavailable on server".
- Error path: `record.html_url` parses to a host other than `github.com` → 400 `unsupported_host`; CLI prints clear rejection.
- Integration: request reaches server over HTTP (not in-process), run is resolved by prefix, live fields match GitHub fixture (including `merged`).
- **SSRF defense: outbound URL comes from captured startup value, not per-request env lookup.** Test without mutating process env (per `docs-internal/server-secrets-strategy.md:12` — `std::env::set_var` / `remove_var` are banned workspace-wide, tests not exempt). Two acceptable shapes:
  - *Preferred:* harness-level `AppState` construction test that injects `github_api_base_url: "http://twin.local/api".to_string()` directly, makes a `pr view` request, and asserts via a request-capturing HTTP mock that the outbound call landed at `http://twin.local/api/...` — not at whatever `std::env::var("GITHUB_BASE_URL")` would return. No env mutation involved.
  - *Alternative:* subprocess test — spawn one fabro server with `GITHUB_BASE_URL=http://twin-a.local` in the spawn env, run a request, kill; spawn another with `GITHUB_BASE_URL=http://twin-b.local`, run, kill. Each subprocess has its own immutable env. Slower than the in-process variant but no in-process env mutation.
  - The in-process-env-mutation variant (set `GITHUB_BASE_URL` from the test body after server start) is **not acceptable** — it violates the workspace rule and will fail clippy.
- JSON mode: `--json` returns the full `PullRequestDetail` as structured JSON matching OpenAPI schema, including `merged`.

**Verification:**
- `cargo nextest run -p fabro-server <view-handler-test>` passes.
- `cargo nextest run -p fabro-cli --test it 'cmd::pr_view'` passes end-to-end.
- CLI no longer imports `fabro_github` or `super::load_github_credentials_required`.
- All three error paths covered by dedicated tests with distinct HTTP status assertions.

---

- [ ] **Unit 2: Add `pr merge` endpoint + rewrite CLI**

**Goal:** Same pattern as view, mutating. GitHub call only — no events, no state updates.

**Requirements:** R1, R2, R5.

**Dependencies:** Unit 1 (establishes `PullRequestRecord` schema in OpenAPI).

**Files:**
- Modify: `docs/api-reference/fabro-api.yaml` — add `POST /api/v1/runs/{id}/pull_request/merge` + request body + response + `MergeMethod` enum.
- Modify: `lib/crates/fabro-github/...` — add strum derives to `AutoMergeMethod` (`Display`, `EnumString`, `IntoStaticStr`, `#[strum(serialize_all = "snake_case")]`) per CLAUDE.md strum policy, keeping the variants identical to today's enum. Alias as `MergeMethod` for the OpenAPI surface if name clarity warrants.
- Modify: `lib/crates/fabro-server/src/server.rs` — add handler `merge_run_pull_request`, wire route.
- Modify: `lib/crates/fabro-client/src/client.rs` — add `Client::merge_run_pull_request(run_id, method) -> Result<MergeResponse>`.
- Rewrite: `lib/crates/fabro-cli/src/commands/pr/merge.rs` — call new client method.
- Test: `lib/crates/fabro-cli/tests/it/cmd/pr_merge.rs`.

**Approach:**
- Handler resolves run id, loads stored `PullRequestRecord` (404 `no_stored_record` if missing), parses the host from `record.html_url` and verifies it equals `github.com` (400 `unsupported_host` otherwise), loads GitHub creds (503 `integration_unavailable` if missing), calls `fabro_github::merge_pull_request` with the requested method, passing `state.github_api_base_url.as_str()` (captured at startup by Unit 1) as the base URL. Returns `{ number, html_url, method }`.
- **No event emitted. No change to `state.pull_request`.** GitHub is the source of truth for merged state; a subsequent `pr view` re-reads from GitHub.

**Patterns to follow:**
- `archive_run` for mutating run-scoped action shape.

**Test scenarios:**
- Happy path: valid method `Squash` → GitHub merge called server-side; CLI prints `Merged #N (URL)`.
- Edge case: default `--method squash` fills the request body client-side (clap default), server accepts.
- Error path: invalid method `--method foo` → clap rejects before request.
- Error path: run without stored PR → 404 `no_stored_record`.
- Error path: server GitHub creds missing → 503 `integration_unavailable`.
- Error path: `record.html_url` parses to a host other than `github.com` → 400 `unsupported_host`.
- JSON mode: `--json` returns `{ number, html_url, method }`.

**Verification:**
- `cargo nextest run -p fabro-cli --test it 'cmd::pr_merge'` passes.
- No new events added to `fabro_workflow::event::Event`.
- CLI no longer uses `fabro_github::merge_pull_request`.

---

- [ ] **Unit 3: Add `pr close` endpoint + rewrite CLI**

**Goal:** Mirrors Unit 2's shape. GitHub call only — no events, no state updates.

**Requirements:** R1, R2, R5.

**Dependencies:** Unit 1.

**Files:**
- Modify: `docs/api-reference/fabro-api.yaml` — add `POST /api/v1/runs/{id}/pull_request/close` + response schema.
- Modify: `lib/crates/fabro-server/src/server.rs` — add handler `close_run_pull_request`.
- Modify: `lib/crates/fabro-client/src/client.rs` — add `Client::close_run_pull_request(run_id)`.
- Rewrite: `lib/crates/fabro-cli/src/commands/pr/close.rs`.
- Test: `lib/crates/fabro-cli/tests/it/cmd/pr_close.rs`.

**Approach:**
- Handler resolves run id, loads stored `PullRequestRecord` (404 `no_stored_record` if missing), parses the host from `record.html_url` and verifies it equals `github.com` (400 `unsupported_host` otherwise), loads GitHub creds (503 if missing), calls `fabro_github::close_pull_request` with `state.github_api_base_url.as_str()`. Returns `{ number, html_url }`.
- **No event emitted. No change to `state.pull_request`.** GitHub is the source of truth for closed state.

**Patterns to follow:**
- Unit 2 shape minus the method body.

**Test scenarios:**
- Happy path: close existing PR → GitHub close called; CLI prints `Closed #N (URL)`.
- Error path: run without stored PR → 404 `no_stored_record`.
- Error path: PR already closed upstream → GitHub returns error; server surfaces it as 502; CLI exits non-zero.
- Error path: server GitHub creds missing → 503.
- Error path: `record.html_url` parses to a host other than `github.com` → 400 `unsupported_host`.
- JSON mode: `--json` returns `{ number, html_url }`.

**Verification:**
- `cargo nextest run -p fabro-cli --test it 'cmd::pr_close'` passes.
- No new events added to `fabro_workflow::event::Event`.
- CLI no longer uses `fabro_github::close_pull_request`.

---

- [ ] **Unit 4: Rewrite `pr list` CLI to compose from existing operations**

**Goal:** `pr list` becomes pure CLI composition — no new server endpoint. Takes the cheap path: only runs that already have a stored `pull_request` record trigger a live GitHub call. Runs without a stored record are skipped before any GitHub probe.

**Requirements:** R1, R2, R5.

**Dependencies:** Unit 1 (per-run view endpoint returning `PullRequestDetail`). No server work in this unit.

**Files:**
- Rewrite: `lib/crates/fabro-cli/src/commands/pr/list.rs` — fetch runs via the existing `list_runs` client method, filter locally to runs whose run state already indicates a stored `pull_request` record (probe via existing `get_run_state` or equivalent lightweight call), then `futures::stream::iter(candidates).map(|r| client.get_run_pull_request(&r.run_id)).buffer_unordered(10)` to fetch `PullRequestDetail` for each. Apply the existing `--all` vs open/draft/unknown filter client-side. Table/JSON rendering stays.
- Test: `lib/crates/fabro-cli/tests/it/cmd/pr_list.rs` — existing tests adjust to the new flow.

**Approach:**
- The filter step is the key: runs without a stored `pull_request` record skip the GitHub call entirely. Only candidates (runs with a stored record) hit GitHub. This keeps GitHub traffic proportional to PR count, not run count.
- The CLI iterates at most 10 candidates in flight. For typical deployments (a minority of runs have PRs), this gives a good throughput / fairness tradeoff.
- Each returned `PullRequestDetail` is classified locally into one of `open`, `draft`, `merged`, `closed`, or `unknown` using both `state` and `merged` fields:
  - `merged == true` → `merged` (regardless of `state`, though GitHub always has `state: "closed"` in this case).
  - `state == "open"` + `draft == true` → `draft`.
  - `state == "open"` + `draft == false` → `open`.
  - `state == "closed"` + `merged == false` → `closed`.
  - Anything else, or a 502 `github_not_found` from the view endpoint → `unknown`.
- Default filter: `open` + `draft` + `unknown`. `--all`: adds `merged` + `closed`.

**Patterns to follow:**
- `pr/list.rs:48-88` today — the existing join_all shape, swapped for `buffer_unordered(10)` and preceded by the stored-record filter.
- Existing `ServerSummaryLookup` usage for pulling runs + state — adapted to just the filter step.

**Test scenarios:**
- Happy path: multiple runs with stored PRs across all five states → default filter shows open + draft + unknown; `--all` adds merged + closed. Classification matches the rules above (uses `merged: bool`, not a magic "merged" value in `state`).
- Edge case: zero runs with stored PRs → CLI prints "No pull requests found." with **zero** `get_run_pull_request` calls (assert via mock counter).
- Edge case: only 3 runs with stored PRs → exactly 3 `get_run_pull_request` calls; concurrency cap not exceeded.
- Error path: one candidate's view returns 502 → that entry shows `state: "unknown"`; other entries still populated.
- Error path: server returns 503 for missing creds on any view → CLI surfaces a single top-level "GitHub integration unavailable" message.
- JSON mode: `--json` returns the assembled list with `merged: bool` on each row.

**Verification:**
- `cargo nextest run -p fabro-cli --test it 'cmd::pr_list'` passes.
- CLI `pr/list.rs` no longer calls `fabro_github::get_pull_request`.
- No new handler added to `server.rs`.
- Test asserting the skip behavior: runs without stored PRs are provably **not** probed against GitHub.

---

- [ ] **Unit 5: Add `pr create` endpoint + rewrite CLI**

**Goal:** The worst offender. Server owns the entire create flow.

**Requirements:** R1, R2, R3, R4, R5

**Dependencies:** Unit 1 (schemas reused).

**Files:**
- Modify: `docs/api-reference/fabro-api.yaml` — add `POST /api/v1/runs/{id}/pull_request` + request body `{ force: bool, model: Option<String> }` + response `PullRequestRecord`.
- Modify: `lib/crates/fabro-workflow/src/pipeline/pull_request.rs` — add a `base_url: &str` parameter to `maybe_open_pull_request(...)`. Thread it through to the two internal outbound calls: `github_app::create_pull_request(..., base_url)` at line 440 and `github_app::enable_auto_merge(..., base_url)` at line 457 (both currently hardcode `&github_app::github_api_base_url()`). Also thread through to `build_pr_body(...)` if it makes any GitHub call; review the helper for other `github_api_base_url()` uses while touching it. Update the existing in-pipeline caller (`pipeline::pull_request` function) to pass its current `&github_api_base_url()` value explicitly (preserving today's behavior for that path — hardening it is out of scope but now trivially possible).
- Modify: `lib/crates/fabro-server/src/server.rs` — add handler `create_run_pull_request`. Handler passes `state.github_api_base_url.as_str()` into `maybe_open_pull_request` **and** into any direct `fabro_github::*` call (e.g., `branch_exists`).
- Modify: `lib/crates/fabro-client/src/client.rs` — add `Client::create_run_pull_request(run_id, body)`.
- Rewrite: `lib/crates/fabro-cli/src/commands/pr/create.rs` — parse args, build request, call client, print.
- Test: `lib/crates/fabro-cli/tests/it/cmd/pr_create.rs`; stage-scope-consumer audit tests (see below).

**Pre-flight audit (part of this unit):**
- **Consumer audit for stage-less `PullRequestCreated` envelope.** Before landing this unit, enumerate every code path that reads `stage_id`/`node_id` off a stored `RunEvent` and branches or groups on it. Known candidates to inspect: run_progress UI (`lib/crates/fabro-cli/src/commands/run/run_progress/...`), SSE event replay (server streams), `RunProjection` hydration reducer arms, any analytics/summary code that buckets by stage. For each, either (a) verify it already tolerates `stage_id = None`, or (b) add explicit handling. Add at least one integration test per consumer that constructs a `PullRequestCreated` event with no stage scope and asserts the consumer's behavior.

**Approach:**
- Handler reads `RunProjection` from `state.store.open_run(&id)`. Validates, in order:
  - `run_spec.repo_origin_url` is present (else 400 `missing_repo_origin`).
  - `run_spec.base_branch` is present (else 400 `missing_base_branch`).
  - `start.run_branch` is present (else 400 `missing_run_branch`).
  - `state.final_patch` is non-empty after trimming whitespace — i.e., `!state.final_patch.trim().is_empty()` (else 400 `empty_diff`). Preserves current CLI behavior at `pr/create.rs:64`.
  - `conclusion.status ∈ {Success, PartialSuccess}` unless `body.force`.
  - `state.pull_request` is `None` (else 409 `conflict` with the existing record).
  - Host parsed from `run_spec.repo_origin_url` (after `ssh_url_to_https` normalization) equals `github.com` (else 400 `unsupported_host`). On create, `run_spec.repo_origin_url` is the source of truth because no `PullRequestRecord` exists yet. Once this endpoint persists the record, later view/merge/close calls parse the host from `record.html_url` instead.
- Load GitHub creds via `AppState::github_credentials(settings)`. If missing, return 503 `integration_unavailable` (generic external body; detailed diagnostic in log only; authorizer runs first).
- Every outbound GitHub call from this handler receives `state.github_api_base_url.as_str()` — the value captured at `AppState` construction. This includes both the direct `fabro_github::branch_exists(...)` call and the indirect calls made inside `maybe_open_pull_request(...)`, which is why that helper grows a `base_url: &str` parameter in this unit. Do not call `fabro_github::github_api_base_url()` at request time from either the handler or the helper.
- Call `fabro_github::branch_exists(...)`. If missing, return 400 with a clear message referencing `git push origin <run_branch>`.
- Load LLM provider catalog via `fabro_auth::configured_providers_from_process_env(state.vault.as_ref())` (the helper `operations/start.rs:322` uses). Pick model: `body.model.clone().unwrap_or_else(|| Catalog::builtin().default_for_configured(&configured).id)`. Server process env + server vault — not client env.
- Call `fabro_workflow::pull_request::maybe_open_pull_request(...)` with `draft = true` (matches current CLI behavior; no new flag) and `state.github_api_base_url.as_str()` as the new `base_url` parameter. `maybe_open_pull_request` does **not** emit events — the caller owns emission.
- On `maybe_open_pull_request` returning `Some(record)`:
  - Emit `PullRequestCreated` via `fabro_workflow::event::append_event(&run_store, &run_id, &Event::PullRequestCreated { ... })` (`event.rs:2628`). Stage-less envelope (no `emit_scoped`).
  - Return `Json(record)`.
- On `maybe_open_pull_request` returning `None` (shouldn't happen because empty-diff is rejected upstream, but defensively): return 500 — unexpected.
- CLI rewrite: delete `rebuild_run_store` call, `ensure_matching_repo_origin`, `detect_repo_info`, `configured_providers_from_process_env`, `Catalog::builtin()` model pick, `fabro_github::*` calls, `maybe_open_pull_request` call, `load_github_credentials_required` call. Build request body from `args.force` + `args.model`, call client, print `record.html_url` (text mode) or the record as JSON.

**Patterns to follow:**
- In-run PR creation in `lib/crates/fabro-workflow/src/pipeline/pull_request.rs:492-560` — the handler is a simplified version of `pipeline::pull_request` that runs on demand rather than as a pipeline stage.
- Event emission via `fabro_workflow::event::append_event` (`event.rs:2628`).
- 409 Conflict pattern — search server.rs for existing 409 uses and mirror.

**Test scenarios:**
- Happy path: completed dry-run with stored `final_patch` + pushed branch → endpoint creates PR via GitHub, emits `PullRequestCreated` (stage-less), returns record; CLI prints URL.
- Happy path + `--force`: run in `Failed` state → normally rejected; with `force: true` → proceeds.
- Happy path + `--model`: body.model set → server uses it verbatim.
- Edge case: empty `final_patch` → 400 `empty_diff`; CLI exits non-zero.
- Edge case: whitespace-only `final_patch` (e.g., `"\n\n   \n"`) → 400 `empty_diff`, matching today's CLI behavior (`diff.trim().is_empty()`).
- Edge case: run already has `state.pull_request` → 409 with existing record in body; CLI prints "PR already exists at URL".
- **Nongit run:** `run_spec.repo_origin_url` is None → 400 `missing_repo_origin`; same for missing `base_branch` (400 `missing_base_branch`) and missing `start.run_branch` (400 `missing_run_branch`). Proves nongit runs are still valid runs — they just can't have PRs.
- Error path: GitHub branch doesn't exist → 400 referencing `git push origin <run_branch>`.
- Error path: server GitHub creds missing → 503 `integration_unavailable` (generic body).
- Error path: host parsed from `run_spec.repo_origin_url` is not `github.com` → 400 `unsupported_host`; no outbound HTTP made.
- Integration: event appended to run store is readable via `GET /runs/{id}/state` → `state.pull_request` populated. Regression test against the current bug where client-side create never wrote back.
- Consumer-audit tests (see Pre-flight): each identified stage_id-grouping consumer tolerates a stage-less `PullRequestCreated`.
- JSON mode: `--json` on success returns the `PullRequestRecord`.

**Verification:**
- `cargo nextest run -p fabro-cli --test it 'cmd::pr_create'` passes.
- After a successful `fabro pr create`, a subsequent `fabro pr view` against the same run returns a populated `PullRequestRecord` from server state. (This was previously broken.)
- `pr/create.rs` has no `fabro_github::`, `fabro_workflow::`, `fabro_sandbox::`, or `fabro_store::` imports.
- Consumer-audit list is written into the PR description with each consumer marked verified.

---

- [ ] **Unit 6: Delete client-side PR infrastructure**

**Goal:** Remove the dead client-side helpers and the `boundary-exempt` annotations they carry.

**Requirements:** R2

**Dependencies:** Units 1-5 complete.

**Files:**
- Modify: `lib/crates/fabro-cli/src/commands/pr/mod.rs` — delete `load_github_credentials_required`, `load_pr_record`, `GITHUB_CREDENTIALS_REQUIRED` constant, and the `#[allow(deprecated, reason = "boundary-exempt(pr-api): …")]` annotations. `dispatch` stays.
- Modify: `lib/crates/fabro-cli/src/commands/pr/create.rs` — remove `#[allow(deprecated, …)]` if it survived Unit 5.
- Modify: `lib/crates/fabro-cli/Cargo.toml` — if `fabro-github` and `fabro-workflow` are no longer used anywhere in the PR command module (check other commands), don't remove from `[dependencies]` since other commands still use them. This is a no-op verification step — just confirm the crate dep graph is still correct.

**Approach:**
- Grep the CLI crate for remaining `fabro_github::` and `load_github_credentials` calls after Units 1-5. Anything left is a miss; go fix.
- Verify no `boundary-exempt(pr-api)` annotations remain: `grep -r "boundary-exempt(pr-api)" lib/crates/fabro-cli/`.

**Test scenarios:**
- Test expectation: none — pure deletion of dead code. Existing integration tests from Units 1-5 prove nothing regressed.

**Verification:**
- `grep -r "boundary-exempt(pr-api)" lib/crates/fabro-cli/` returns no results.
- `grep -r "load_github_credentials_required\|load_pr_record" lib/crates/fabro-cli/` returns no results.
- `cargo +nightly-2026-04-14 clippy -p fabro-cli --all-targets -- -D warnings` is clean (no unused imports, no dead code warnings).
- Full `cargo nextest run -p fabro-cli --test it 'cmd::pr'` passes.

---

## System-Wide Impact

- **Interaction graph:** Server handlers plug into the existing `AppState::github_credentials` + run-store event pipeline. `pr create` emits `PullRequestCreated` via `fabro_workflow::event::append_event` (stage-less envelope — pre-flight consumer audit in Unit 5). `pr merge` and `pr close` call GitHub and return — they do not write to run state.
- **Error propagation:** Errors from `fabro_github::*` now surface as HTTP status + JSON body instead of anyhow errors in the CLI. Status codes are structured: 400 for input errors (missing git metadata, empty diff, unsupported host), 404 for no stored record, 502 for GitHub-said-not-found, 503 for missing server creds, 409 for already-exists, 500 only for actual code bugs. CLI prints the server's error message.
- **State lifecycle risks:** Creating a PR now writes back to the authoritative server run store (fixes the existing bug where client-side create never persisted the record). **State beyond creation is not tracked.** After merge or close, `state.pull_request` is unchanged — Fabro does not mirror GitHub's merge/close lifecycle. `pr view` and `pr list` always re-read from GitHub for current status.
- **API surface parity:** Four new operations added to OpenAPI (view, create, merge, close); `pr list` has no new endpoint. **No changes to `RunSpec` schema.** TypeScript client regenerates automatically; run `bin/dev/docker-build.sh` + `scripts/refresh-fabro-spa.sh` once after OpenAPI changes land.
- **Integration coverage:** Cross-boundary test — after `fabro pr create`, a `fabro pr view` against the same run returns the populated record. Unit 5 test scenarios cover this.
- **Unchanged invariants:** `PullRequestRecord` field shape; `fabro_github::*` surface; in-run pipeline PR stage (still creates + emits `PullRequestCreated` with its stage scope when the pipeline is configured for PR creation); `RunSpec.repo_origin_url` + `RunSpec.base_branch` stay `Option<String>`; runs without git metadata remain valid.

## Risks & Dependencies

| Risk | Mitigation |
|------|------------|
| Server has no GitHub creds configured → all four endpoints 503. Users on self-hosted installs without GitHub App setup hit this first. | Error body is generic externally (`integration_unavailable`); detailed diagnostic in log only; authorizer runs first so probing is bounded. Document GitHub App setup in `docs/`. Consider a follow-up `fabro doctor` check. |
| Breaking the CLI UX — stdout/stderr shape drifting from the current snapshots. | Existing `pr_*.rs` integration tests use snapshots. Keep them passing; update only where the server legitimately produces different error text. Preserve the existing command/flag surface. |
| Progenitor generated `PullRequestRecord` doesn't match `fabro_types::PullRequestRecord` byte-for-byte → `with_replacement` silently generates a parallel type. | Unit 1 adds a fabro-api JSON round-trip parity test over a shared fixture, covering every inclusion site as later units add them. Per CLAUDE.md. |
| **Blast-radius expansion of service bearer token.** Any authenticated fabro service token now authorizes GitHub merge/close under the server's App identity on every App-installed repo. In the old model, a leaked token was one dev's problem; now it's org-wide. | Accepted as a consistent expansion of existing fabro auth semantics (`archive_run`, `cancel_run` are similar). Documented in Threat Model. Follow-up if fabro gains per-subject GitHub permission checks. |
| **Loss of per-user GitHub attribution.** Today PRs are authored/merged/closed under the user's GitHub identity. After this refactor all actions are the server's App identity. No new audit-event infrastructure is added in this refactor. | Accepted and documented. Future work may add a general audit-event surface; this plan does not. |
| **TOCTOU: duplicate PRs from concurrent `pr create` on same run.** Two simultaneous calls both see `state.pull_request = None`, both call GitHub, both create real PRs before either emits the event. | Accepted. Observable symptom: duplicate GitHub PRs; user closes the extra. Collision window is narrow (concurrent manual invocations on the same run). Documented here so it's not discovered at runtime. |
| **LLM billing / provider shift.** Today PR body generation spends the user's LLM quota; after this refactor it spends the server operator's quota on the server's configured providers. May also silently pick a different model than the user had locally. | Accepted and documented. This plan does not add cost-ceiling or model-allowlist infrastructure. Follow-up if needed. |
| **Live-GitHub dependency for display.** `pr view` and `pr list` now hit GitHub every time. If GitHub is down or the server's creds are revoked, display fails. Today's CLI has the same dependency but from the client side; the shift is operational, not functional. | Accepted. `pr list` skips runs without a stored record before any GitHub call, so most deployments won't stress this path. |
| `rebuild_run_store` still used by `fork`/`rewind` — not deleted in this plan. | Scope boundary explicitly documents it stays. Future `fork`/`rewind` plan handles removal. |
| Stage-less `PullRequestCreated` envelope differs from pipeline-stage-scoped emission. Downstream consumers that group by `stage_id` could regress. | Unit 5 includes a mandatory consumer-audit pre-flight listing every known consumer + integration tests asserting each tolerates `stage_id = None`. PR description must reference completed audit list. |
| **CLI `--json` output becomes richer for `pr view` / `pr list`** than the old CLI emitted (e.g., more fields from `PullRequestDetail`). | Intentional contract change — called out in the Endpoint summary note. Human-readable output stays as close to current as practical. |

### Threat Model

After this refactor the fabro server holds GitHub App credentials and performs GitHub writes on behalf of clients. Two plan-level threats:

- **Leaked service bearer token → unrestricted merge on every App-installed repo.** Old model: each user held their own token; blast radius = one dev. New model: any authenticated fabro client can call `POST /runs/{id}/pull_request/merge`. **Accepted** — see Risks row above. Consistent with how existing run-scoped mutating endpoints (`archive_run`, `cancel_run`) already work.
- **Attacker-controlled `repo_origin_url` → SSRF with GitHub Authorization headers.** A crafted origin URL could end up in an outbound HTTP request with the server's App JWT as Authorization, leaking the installation token. **Mitigated** by the required github.com-only host check in Units 1/2/3/5 before any outbound GitHub call; Authorization headers never attached to other hosts. Negative test required.

(The "prompt-injected LLM PR body" concern from earlier reviews was deferred — the "Generated by fabro" disclaimer was considered but is out of scope for this narrowly-focused refactor.)

## Documentation / Operational Notes

- Update `docs/api-reference/fabro-api.yaml` — this is both code and docs. The Mintlify docs pick it up automatically.
- No user-facing `docs/changelog/` entry needed; CLI UX is unchanged from the user's perspective (same commands, same output). Internal changelog / PR description covers the refactor.
- **Rollout order: upgrade server first, then CLIs.** New CLI against old server gets 404 on the new paths; add a CLI-side capability hint or clear error that directs users to upgrade the server. Old CLI against new server keeps working against the old-path behavior for now (client-side GitHub creds), though authorship will differ from server-issued PRs. Deprecate the client-side GitHub-creds path in the release after this plan lands; remove it in the release after that.

## Sources & References

- Audit source (this-conversation): `fabro pr *` boundary audit; `dump` refactor precedent.
- Precedent commit: `1481ecf2a refactor(dump): test the real server boundary, drop client-side storage fakes`.
- Related code:
  - CLI PR commands: `lib/crates/fabro-cli/src/commands/pr/{mod,create,view,list,merge,close}.rs`.
  - Shared helper to delete: `lib/crates/fabro-cli/src/commands/pr/mod.rs::load_github_credentials_required` (`pr/mod.rs:35`).
  - Server handler templates: `lib/crates/fabro-server/src/server.rs::{archive_run,cancel_run,append_run_event}` (`server.rs:6448, 6085, 4990`).
  - Server GitHub creds helper: `lib/crates/fabro-server/src/server.rs::AppState::github_credentials` (`server.rs:695`).
  - In-run PR stage: `lib/crates/fabro-workflow/src/pipeline/pull_request.rs::{maybe_open_pull_request, pull_request}` (`pull_request.rs:405, 492`).
  - PR event variants: `lib/crates/fabro-workflow/src/event.rs:498-510`.
  - OpenAPI path template: `docs/api-reference/fabro-api.yaml:940-971` (`appendRunEvent`).
  - OpenAPI PullRequest schema (existing): `docs/api-reference/fabro-api.yaml:4227-4247` (`RunPullRequest` — keep as-is, add new `PullRequestRecord` separately).
  - Client method templates: `lib/crates/fabro-client/src/client.rs::{cancel_run, create_secret}` (`client.rs:717, 525`).
  - Type definition: `lib/crates/fabro-types/src/pull_request.rs::PullRequestRecord`.
