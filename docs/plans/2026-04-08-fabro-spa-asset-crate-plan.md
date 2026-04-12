# Fabro SPA Asset Crate Plan

## Summary
Create a new internal crate, `fabro-spa`, as the runtime source of truth for the production web bundle. The crate will contain committed built assets and expose a narrow asset API backed by `rust-embed` with compression and include/exclude filtering. This removes Bun/Node from Cargo builds, release builds, and runtime, while preserving the current low-complexity local workflow: `fabro server start` plus `cd apps/fabro-web && bun run dev`, with browser refreshes picking up rebuilt files.

## Key Decisions
- Commit the production bundle in `lib/crates/fabro-spa/assets`.
  - This is the explicit tradeoff for keeping Bun out of Cargo builds, release builds, and runtime. A CI-only asset build would keep the release path coupled to Bun, which is the dependency boundary this plan is trying to remove.
  - Repo growth is accepted in exchange for a fully self-contained Rust release artifact. Mitigations in this plan are: no sourcemaps, generated-asset `.gitattributes`, a `15 MB` committed-asset size budget, and a separate `5 MB` embedded-payload budget.
- Keep Bun as an authoring/build-time tool only.
  - Do not add Bun to Rust CI or release builds.
  - Do not add HMR or a separate browser dev server in this pass.
- `fabro-spa` returns plain file bytes, not content-encoded payloads.
  - `rust-embed` compression is an implementation detail inside the crate.
  - The `fabro-server` caller should continue to receive normal file bytes for MIME detection and response body construction.
- Define refresh-script idempotency as: running the refresh script twice on unchanged frontend source produces no git diff under `lib/crates/fabro-spa/assets`.
- Keep debug disk fallback anchored to `env!("CARGO_MANIFEST_DIR")`, matching the current server behavior.
  - Do not make path resolution depend on cwd.
  - Do not add a new env var override in this iteration.

## Implementation Changes
### 1. Package the production SPA in `fabro-spa`
- Add `lib/crates/fabro-spa` with a committed `assets/` directory populated from `apps/fabro-web/dist`.
- In `fabro-spa`, use `rust-embed` with:
  - `compression`
  - `include-exclude`
  - `deterministic-timestamps` if needed to keep embedded metadata stable across identical rebuilds
- Exclude sourcemaps in two places:
  - the refresh script must not copy any `*.map` files into `fabro-spa/assets`
  - the embed definition must also exclude `*.map` and `**/*.map`
- Keep the crate API narrow and owned by `fabro-spa`.
  - Expose `get(path: &str) -> Option<AssetBytes>` where `AssetBytes` is a crate-local wrapper around the underlying bytes.
  - `AssetBytes` should provide access to the normal file bytes only; it must not expose `rust-embed` types or compression details.
  - Do not add `iter()` in v1. If tests need enumeration, add a dedicated test-only helper later.

### 2. Move `fabro-server` to consume `fabro-spa`
- Replace the direct `RustEmbed` usage in `lib/crates/fabro-server/src/static_files.rs` with calls into `fabro-spa`.
- Preserve existing server behavior:
  - SPA fallback to `index.html`
  - MIME detection in the server
  - current cache-control behavior for hashed assets vs entry/root assets
- Preserve low-friction local development by keeping a debug-only disk override.
  - In debug builds, first check `apps/fabro-web/dist/<path>` on disk using a path derived from `env!("CARGO_MANIFEST_DIR")`.
  - If the file exists, serve it directly.
  - Otherwise fall back to the embedded `fabro-spa` asset.
- Remove the current Bun-based release build hook in `lib/crates/fabro-server/build.rs`.
- Remove the direct `rust-embed` dependency from `fabro-server` once the embed lives entirely in `fabro-spa`.

### 3. Keep the current Bun authoring/build path
- Keep `apps/fabro-web` as the authoring app and keep the existing custom Bun production build script.
- Keep `bun run dev` as the watch-and-rebuild command, with no HMR or browser dev server added in this pass.
- The local workflow remains:
  - run `fabro server start`
  - run `cd apps/fabro-web && bun run dev`
  - refresh the browser manually after rebuilds
- Do not add a Rust-side dev proxy, a frontend HMR server, or a bundler migration in this iteration.

### 4. Add an explicit asset refresh workflow
- Add a repo-level refresh command, `scripts/refresh-fabro-spa.sh`.
- The script should:
  - run the existing web production build in `apps/fabro-web`
  - fully replace `lib/crates/fabro-spa/assets` rather than incrementally syncing it
  - copy only shippable files from `dist`
  - exclude all sourcemaps
- Treat git state as the source of idempotency truth.
  - Two consecutive runs on unchanged frontend source must leave no git diff under `lib/crates/fabro-spa/assets`.
  - Timestamps in the filesystem do not matter; only committed file content and paths matter.
- Make this script the only supported way to update committed SPA assets.

### 5. Add repo hygiene and CI guardrails
- Add `.gitattributes` entries for `lib/crates/fabro-spa/assets/**`.
  - Mark the directory as generated for repository tooling.
  - Suppress noisy diffs for minified hashed bundles.
  - Do not use Git LFS in v1 unless the committed asset size proves unmanageable.
- Add a size budget check for `lib/crates/fabro-spa/assets`.
  - Fail CI if total committed asset size exceeds `15 MB` without an explicit budget update.
  - This budget is intentionally above the current no-sourcemap bundle size, which is about `12.3 MB` in the current branch, so the initial implementation passes with modest headroom.
- Add a separate embedded-payload budget check for the release binary.
  - Fail CI or release verification if the estimated compressed `fabro-spa` asset payload exceeds `5 MB` without an explicit budget update.
  - This budget is intentionally above the current estimated compressed payload, which is about `2.8 MB` in the current branch, so the initial implementation passes with meaningful headroom.
  - Include a release-build smoke check so accidental asset over-inclusion is visible before merge.
- Keep drift verification in Bun-capable CI, not Rust CI.
  - Update the TypeScript workflow to run the refresh script and fail if it leaves a diff under `lib/crates/fabro-spa/assets`.
  - Expand that workflow's path coverage to include both `apps/fabro-web/**` and `lib/crates/fabro-spa/**`.
  - Treat this freshness job as the authoritative stale-asset check for frontend changes.
- Leave the Rust/release workflows Bun-free.

### 6. Update docs around the new split
- Update docs that currently say `bun run dev` serves the app on port `5173`; they should instead describe the watch-build plus manual-refresh flow against `fabro server start`.
- Update architecture/deployment docs that imply `fabro-server` builds frontend assets during release.
- Document the new refresh step for contributors who change `apps/fabro-web`.

## Interfaces And Workflow Changes
- New internal crate: `lib/crates/fabro-spa`
- Removed implicit build coupling: `cargo build` and release builds must no longer invoke Bun
- No HTTP API changes
- No new public CLI flags or env vars in this iteration
- New developer-maintained artifact boundary:
  - `apps/fabro-web` remains source code
  - `lib/crates/fabro-spa/assets` becomes committed runtime bundle output

## Test Plan
- Add server tests covering:
  - embedded asset serving when no disk bundle is present
  - debug disk override when `apps/fabro-web/dist` contains a rebuilt file
  - SPA fallback to `index.html` for unknown client routes
  - immutable cache headers for hashed assets
  - no-cache headers for entry/root assets
  - `.map` files are not served
- Add refresh-script verification covering:
  - running the refresh script twice without source changes produces no git diff
  - asset replacement strips sourcemaps reliably
  - the committed asset directory stays under the `15 MB` size budget
- Add release-build verification covering:
  - `cargo build --release` succeeds without Bun installed
  - the estimated compressed embedded asset payload stays under the `5 MB` budget
  - the resulting binary size is recorded during CI/manual verification so the measured end-to-end delta is visible
- Run:
  - `cd apps/fabro-web && bun run build`
  - the refresh script
  - `cargo nextest run -p fabro-server`
  - `cargo build --release`
- Manual smoke check:
  - start the Rust server
  - run `cd apps/fabro-web && bun run dev`
  - change a UI file
  - confirm the rebuilt asset is served after a browser refresh

## Assumptions
- The production bundle is intentionally committed to `lib/crates/fabro-spa/assets` to preserve Bun-free Cargo and release builds.
- Sourcemaps are not shipped in the binary and are not copied into the asset crate.
- Local web development accepts manual refresh; no HMR/dev server is part of this plan.
- Bun remains a frontend authoring/build-time dependency only; it is removed from runtime, Cargo build, and release requirements.
- A future bundler migration or HMR setup is explicitly deferred rather than partially introduced here.
