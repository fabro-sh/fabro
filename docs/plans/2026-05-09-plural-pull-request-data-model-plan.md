# Plural Pull Request Data Model And API

## Summary

- Make runtime pull request records plural now: a run has `pull_requests: []`, not `pull_request: null`.
- Absorb the breaking API change now and remove singular runtime PR API fields/routes.
- Keep end-user surfaces simple: CLI and web continue showing/acting on the first PR only, with no multi-PR UI or chooser.
- Do not change run configuration settings like `[run.pull_request]`; that still controls whether a workflow auto-opens a PR.

## Key Changes

- Update OpenAPI runtime schemas so `RunProjection`, `RunSummary`, and board `RunListItem` expose `pull_requests: PullRequestRecord[]` or `RunPullRequest[]`; remove their singular `pull_request` fields.
- Replace singular PR routes with plural routes:
  - `POST /api/v1/runs/{id}/pull_requests` creates and links a new GitHub PR.
  - `GET /api/v1/runs/{id}/pull_requests` lists stored PR records.
  - `POST /api/v1/runs/{id}/pull_requests/link` links an existing PR record without touching GitHub.
  - `POST /api/v1/runs/{id}/pull_requests/unlink` removes the association without closing the PR.
  - `GET/POST /api/v1/runs/{id}/pull_requests/{owner}/{repo}/{number}` style targeted detail/merge/close routes operate on a specific stored PR.
- Add schemas for `PullRequestKey`, `LinkRunPullRequestRequest`, and `UnlinkRunPullRequestRequest`. Link/create requests may include `primary: boolean`; the first PR is primary by default, and primary means "move to index 0."
- Replace projection events with action-oriented events:
  - `pull_request.linked` with `{ pull_request, source, primary }`
  - `pull_request.unlinked` with `{ pull_request, reason? }`
  - `pull_request.create_failed` with `{ error }`
- Remove `pull_request.created` / `pull_request.failed` from canonical event handling unless implementation needs temporary test fixture cleanup during the refactor.

## Implementation Changes

- In `fabro-types` and the store reducer, replace `RunProjection.pull_request: Option<PullRequestRecord>` and `RunSummary.pull_request` with `pull_requests: Vec<PullRequestRecord>`.
- Add helper behavior around PR keys: upsert linked records by `(owner, repo, number)`, move primary records to index `0`, and remove matching records on unlink.
- Update workflow PR creation to emit `pull_request.linked` with `source: "created"` after GitHub succeeds, and `pull_request.create_failed` on failure.
- Update server PR handlers to append link/unlink events rather than mutating state directly; remove the old "any PR exists" conflict because multiple PRs are now valid.
- Update CLI `fabro pr create/view/merge/close` to keep singular UX:
  - `create` calls the plural create route and prints the returned URL.
  - `view/merge/close` list PRs, select `pull_requests[0]`, then call the targeted plural route.
  - If the list is empty, preserve the current "No pull request found" style error.
- Update web data mapping in `apps/fabro-web/app/data/runs.ts` to read the first item from `pull_requests`; do not add any new UI controls or multiple-PR display.
- Regenerate Rust and TypeScript API clients after editing `docs/public/api-reference/fabro-api.yaml`.

## Test Plan

- Add/update store reducer tests for link, idempotent upsert, primary reordering, unlink, and summary projection.
- Add/update event conversion/name/pretty-output tests for `pull_request.linked`, `pull_request.unlinked`, and `pull_request.create_failed`.
- Update server tests for plural route paths, list/link/unlink, targeted detail/merge/close, and create persisting into `pull_requests`.
- Update CLI PR command tests to mock plural endpoints while keeping user-facing output singular.
- Update web data mapper tests to prove only `pull_requests[0]` is surfaced in current cards.
- Run:
  - `cargo build -p fabro-api`
  - `cd lib/packages/fabro-api-client && bun run generate`
  - `cargo nextest run -p fabro-store -p fabro-workflow -p fabro-server -p fabro-cli`
  - `cd apps/fabro-web && bun test && bun run typecheck`
  - `cargo +nightly-2026-04-14 fmt --check --all`
  - `cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings`

## Assumptions

- Breaking runtime API changes are acceptable; no singular runtime PR fields/routes are kept for compatibility.
- No production data migration is needed. Existing local/dev event logs using old PR events may be treated as stale.
- `pull_requests[0]` is the primary PR for current CLI/web behavior.
- Link/unlink only changes Fabro's association to a PR; merge/close are the only operations that mutate GitHub state.
