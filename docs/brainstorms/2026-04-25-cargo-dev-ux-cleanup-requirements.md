---
date: 2026-04-25
topic: cargo-dev-ux-cleanup
---

# `cargo dev` UX Cleanup

## Problem Frame

`cargo dev` was created in April 2026 to consolidate five shell scripts into one Rust binary, modeled after `uv`'s ergonomics. Six commands shipped:

`docker-build`, `refresh-spa`, `check-spa-budgets`, `generate-cli-reference`, `generate-options-reference`, `release`.

A few rough edges have surfaced now that the surface is in regular use by humans, agents, and CI:

- `refresh-spa` is mandatory before any TypeScript commit, but CI enforces it via a separate `git diff --exit-code` step rather than a first-class check mode. The dirty-check lives in CI YAML, not in the dev tool.
- `check-spa-budgets` is a sibling command instead of part of the same SPA verification pass, so contributors need to remember two commands to "verify the SPA is shippable."
- Word order is mixed — `docker-build` is noun-verb, `refresh-spa` and `check-spa-budgets` are verb-noun, the two `generate-*-reference` commands are verb-noun-noun. There is no rule a contributor can apply to guess a command name.
- `release [nightly]` uses a positional with a single accepted value (`nightly`), reading awkwardly compared with `--nightly`.

This brainstorm captures the rename + consolidation pass to fix word order, fold SPA verification into one command, and switch the release prerelease to a flag.

## Surface Comparison

| Today | After |
|---|---|
| `cargo dev docker-build` | `cargo dev docker-build` (unchanged) |
| `cargo dev refresh-spa` | `cargo dev spa refresh` |
| `cargo dev check-spa-budgets` + CI `git diff --exit-code` | `cargo dev spa check` (folds both) |
| `cargo dev generate-cli-reference` | `cargo dev docs refresh` (regenerates both artifacts) |
| `cargo dev generate-cli-reference --check` | `cargo dev docs check` (verifies both artifacts) |
| `cargo dev generate-options-reference` | `cargo dev docs refresh` |
| `cargo dev generate-options-reference --check` | `cargo dev docs check` |
| `cargo dev release` | `cargo dev release` (unchanged) |
| `cargo dev release nightly` | `cargo dev release --nightly` |
| — | `cargo dev spa` prints SPA command help |
| — | `cargo dev docs` prints docs command help |

## Requirements

### Naming convention

- R1. Adopt noun-verb word order (uv style): the group/subject comes first, the action second. `docker-build` is grandfathered as a single hyphenated noun-verb name because it has no siblings; `release` stays as a single verb because it has no group.
- R2. Group-only invocation is non-mutating help: `cargo dev spa` prints the SPA subcommand help and exits successfully; `cargo dev docs` prints the docs subcommand help and exits successfully. Mutating commands always require an explicit verb (`refresh`).

### SPA group

- R3. `cargo dev spa refresh` rebuilds the SPA via `bun run build`, verifies the built `dist/` output against the asset budget gate, and only then replaces `lib/crates/fabro-spa/assets/`. On budget violation, it exits non-zero and leaves the committed asset directory untouched. This extends today's `refresh-spa` behavior with the budget gate so contributors can't ship an oversized bundle by accident without creating a dirty tracked tree on failed refresh.
- R4. `cargo dev spa check` is non-mutating with respect to tracked files: it must not rewrite `lib/crates/fabro-spa/assets/`, and it must leave the tracked working tree unchanged whether it passes or fails. It may use ignored temporary output, but it must clean up any temporary directory it creates. It must fail (non-zero exit) when **either** (a) the SPA bundle in `lib/crates/fabro-spa/assets/` does not match what `bun run build` would produce now (given the current `bun.lock` and source tree), **or** (b) the bundle exceeds asset budgets. This subsumes both today's `check-spa-budgets` and the CI `git diff --exit-code -- lib/crates/fabro-spa/assets` step. The bit-exact comparison relies on `bun run build` being reproducible from a frozen lockfile; CI is the authoritative environment because `bun install --frozen-lockfile` runs there. If a contributor sees a `spa check` failure locally that they can't explain, the resolution is to run `cargo dev spa refresh` and commit the result — CI's `spa check` then arbitrates.
- R5. Both `.github/workflows/typescript.yml` (Build job) and `.github/workflows/release.yml` (`verify-spa` job, lines 29–31) call `cargo dev spa check` instead of `cargo dev refresh-spa` + `git diff --exit-code -- lib/crates/fabro-spa/assets`. The dirty-check no longer lives in workflow YAML. Both CI workflows must run `bun install --frozen-lockfile` before `cargo dev spa check`; this preserves the TypeScript workflow's existing setup and tightens the release workflow, which currently uses plain `bun install`.

### Docs group

- R6. `cargo dev docs refresh` regenerates both `docs/reference/cli.mdx` and `docs/reference/user-configuration.mdx` in one pass. Replaces the two `generate-*-reference` commands' default mode.
- R7. `cargo dev docs check` verifies both MDX artifacts are up to date without rewriting them. Replaces the two commands' `--check` mode. Non-zero exit on drift in either file.

### Release

- R8. `cargo dev release` cuts a stable release; `cargo dev release --nightly` cuts a nightly prerelease. The `[nightly]` positional is removed. Other release-mode flags (`--dry-run`, `--skip-tests`, `--release-date`) are unchanged.

### Migration

- R9. Hard cut: the old names (`refresh-spa`, `check-spa-budgets`, `generate-cli-reference`, `generate-options-reference`, `release nightly` positional) are removed in the same PR. No deprecated aliases.
- R10. All in-repo callers update in the same PR. Scope explicitly includes:
  - CI workflows under `.github/workflows/` (including `nightly.yml`'s `cargo dev release nightly` invocation and `release.yml`'s `verify-spa` job).
  - `AGENTS.md` (including its release-mode usage line that shows `[nightly]`) and `CLAUDE.md`.
  - Any live `docs/` references that show the user-facing command (historical artifacts under `docs/plans/` and `docs/brainstorms/` are left alone).
  - The `lib/crates/fabro-dev/` source crate itself: the subprocess invocation in `src/commands/release.rs` (currently `cargo dev refresh-spa`), `bail!` and help strings inside `src/commands/*.rs` that name the old commands, and integration tests under `tests/it/` that invoke subcommands by name. These must rename in lockstep with the clap subcommand declarations or `cargo nextest run -p fabro-dev` will fail.

## Success Criteria

- `cargo dev --help` lists `docker-build`, `spa`, `docs`, `release` (four entries) with consistent noun-verb naming.
- A contributor can answer "how do I verify the SPA before pushing?" with one command (`cargo dev spa check`) instead of two plus a `git diff`.
- CI's TypeScript Build job, the release pipeline's `verify-spa` job, and the docs-reference checks each call exactly one `cargo dev <group> check` command; the workflow YAML no longer contains a `git diff --exit-code` line for SPA assets, and both SPA-checking workflows use `bun install --frozen-lockfile`.
- `rg "refresh-spa|check-spa-budgets|generate-cli-reference|generate-options-reference|release nightly|release \\[nightly\\]" .github CLAUDE.md AGENTS.md docs/ lib/crates/fabro-dev/ -g '!docs/plans/**' -g '!docs/brainstorms/**'` returns no live references after the PR merges. Historical matches inside `docs/plans/` and `docs/brainstorms/` are acceptable.
- `cargo nextest run -p fabro-dev` passes after the rename, and no `bail!` or help text inside `lib/crates/fabro-dev/src/` references the removed command names.

## Scope Boundaries

- Out of scope: `cargo dev verify` aggregate command, `cargo dev doctor` environment check, `--json` output for agents, `--watch` mode for `spa refresh`, pre-commit hook installation. These were considered and deferred — worth a separate brainstorm if footgun-reduction proves still painful after this cleanup.
- Out of scope: any change to what `docker-build` does or its flag surface.
- Out of scope: extending the release prerelease vocabulary beyond `nightly` (`--prerelease <kind>` style). The single `--nightly` flag is enough for the current need; switch to `--prerelease` if/when a second prerelease kind is introduced.

## Key Decisions

- **Noun-verb word order** (uv style) wins over verb-noun (cargo style). Rationale: `cargo dev` was already modeled after `uv` per docs/plans/2026-04-24-001-refactor-adopt-uv-patterns-plan.md, and the user picked `spa refresh` directly.
- **One docs command pair, not per-artifact.** `docs refresh` regenerates both MDX files; `docs check` verifies both. Rationale: both regenerations are fast and idempotent, granular targeting has no real use case, and the simpler pair mirrors `spa refresh` / `spa check`.
- **`spa check` owns both the diff and the budget gate.** Rationale: contributors think of "is the SPA ready to ship?" as one question; splitting it across two commands and a CI YAML step is what produced today's friction.
- **Group-only commands are help-only.** Rationale: `cargo dev spa` and `cargo dev docs` are easy commands for humans and agents to discover; making them mutate the working tree by default is too easy to trigger accidentally. Explicit verbs keep the command surface predictable.
- **`spa refresh` also enforces the budget gate before replacing committed assets.** Rationale: a successful refresh that quietly produces an over-budget bundle is a footgun, but a failed refresh should not leave tracked assets dirty. Validate the built output first, then mirror it into `lib/crates/fabro-spa/assets/` only on success.
- **`spa check`'s bit-exact comparison relies on `bun install --frozen-lockfile`.** Rationale: CI is the authoritative environment because both SPA-checking workflows run `bun install --frozen-lockfile` before `spa check`; locally, contributors run `spa refresh` to make their working tree match what their bun produces, then trust CI's `spa check` to arbitrate cross-machine drift.
- **Hard cut, no deprecated aliases.** Rationale: the tool is internal-only with all callers in this repo; the user's standing preference is "simplest change, don't care about migration."

## Outstanding Questions

### Resolve Before Planning

(none)

### Deferred to Planning

- [Affects R4][Technical] Implementation strategy for `spa check`'s diff: build into a temporary output directory and compare it against `lib/crates/fabro-spa/assets/`, or rebuild `apps/fabro-web/dist/` and clean it up after comparison. Whichever path is chosen must leave tracked files untouched and must not require a clean working tree outside `lib/crates/fabro-spa/assets/`.
- [Affects R10][Needs research] Confirm the explicit caller list is exhaustive via `rg "refresh-spa|check-spa-budgets|generate-cli-reference|generate-options-reference|release nightly|release \\[nightly\\]" . -g '!target/**' -g '!docs/plans/**' -g '!docs/brainstorms/**'` immediately before merging.

## Next Steps

→ `/ce:plan` for structured implementation planning
