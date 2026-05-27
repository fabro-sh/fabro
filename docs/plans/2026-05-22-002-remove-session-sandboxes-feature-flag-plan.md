# Remove `features.session_sandboxes`

## Summary

Remove the `session_sandboxes` feature flag and the now-empty `[features]` settings namespace entirely. Behavior should be as if `session_sandboxes = true` was always set: Ask Fabro is never disabled by a feature flag, and UI controls previously hidden behind the flag are always shown.

## Key Changes

- Remove the settings namespace from config:
  - Delete `FeaturesNamespace`, `FeaturesLayer`, `resolve_features`, and `[features]` defaults.
  - Remove `features` from resolved `ServerSettings` and `UserSettings`.
  - Remove `features` from the top-level settings parser allow-list, so old `[features]` config is rejected as unknown.
- Remove the runtime gate:
  - Simplify Ask Fabro readiness to check only sandbox presence/runtime and LLM configuration.
  - Remove `AskFabroUnavailableReason::FeatureDisabled` and the "Ask Fabro is disabled" tooltip.
- Update frontend behavior:
  - Run detail page no longer handles `FEATURE_DISABLED`.
  - Start page always renders the project/branch controls and no longer fetches system info just for this flag.
- Remove public API surfaces:
  - `/api/v1/settings` `ServerSettings` no longer includes `features`.
  - `/api/v1/system/info` no longer includes `features`.
  - OpenAPI removes `FeaturesNamespace`, `SystemFeatures`, `ServerSettings.features`, `SystemInfoResponse.features`, and `feature_disabled`.
  - Regenerate Rust API types and TypeScript Axios client.
- Update current docs:
  - Remove `[features]` from active configuration docs, generated options docs, API docs, and unknown-key guidance.
  - Do not touch unrelated meanings of "features" such as Cargo features or LLM model features.

## Test Plan

- Update or remove tests that assert `features.session_sandboxes` in config, settings, system info, and Ask Fabro readiness.
- Add or adjust coverage for:
  - Ask Fabro unavailable reasons are only `no_sandbox`, `sandbox_not_ready`, or `llm_unconfigured`.
  - Settings parsing rejects top-level `[features]`.
  - `/api/v1/settings` response contains only `server` at the top level.
  - `/api/v1/system/info` has no `features` field.
  - Start page renders project/branch controls without consulting `SystemInfo.features`.
- Run:
  - `cargo build -p fabro-api`
  - `cd lib/packages/fabro-api-client && bun run generate`
  - `cargo dev docs refresh && cargo dev docs check`
  - `cargo nextest run -p fabro-config -p fabro-api -p fabro-server -p fabro-cli`
  - `cd apps/fabro-web && bun test && bun run typecheck`
  - `cargo +nightly-2026-04-14 fmt --check --all`
  - `cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings`

## Acceptance Checks

- `rg -n "session_sandboxes|FeaturesNamespace|SystemFeatures|feature_disabled|Ask Fabro is disabled" lib apps docs/public` returns no relevant matches.
- `rg -n "\\[features\\]" docs/public lib/crates/fabro-config/src lib/crates/fabro-types/src lib/crates/fabro-server/src apps/fabro-web/app` returns no settings-namespace matches.
- Existing sandbox runtime behavior remains unchanged; only the feature flag and schema surface are removed.

## Assumptions

- This is intentionally a breaking config/API cleanup: existing user config containing `[features]` should fail validation until removed.
- Historical internal plans may still contain old text unless they are part of active public docs; implementation should prioritize product code, generated clients, and current docs.
