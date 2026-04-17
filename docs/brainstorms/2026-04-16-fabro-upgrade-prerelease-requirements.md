# `fabro upgrade --prerelease` — requirements

## Problem

`fabro upgrade` hits GitHub's `/releases/latest`, which excludes prereleases. Users on a prerelease (e.g. `0.204.0-beta.0`) or wanting to opt into betas must supply an exact `--version v0.X.Y-alpha.N`. There is no channel opt-in.

## Goal

Add `--prerelease` to `fabro upgrade` so a single invocation picks the highest semver across stable **and** prereleases.

## Behavior

- New bool flag `--prerelease` on `fabro upgrade`. Conflicts with `--version` (explicit version already selects any tag).
- Scope: only the explicit `fabro upgrade` command. Background auto-check (`spawn_upgrade_check` / `check_and_print_notice`) stays stable-only.
- Selection when `--prerelease` is set:
  1. Fetch `GET /repos/fabro-sh/fabro/releases` (first page, 30 items — `gh` and HTTP backends).
  2. Drop entries with `draft: true` or an unparseable `tag_name`.
  3. Pick the max by semver over the remaining set (stable + prereleases together). This means a newer stable beats an older beta; `--prerelease` *widens* the candidate set, it does not prefer prereleases.
  4. If the filtered set is empty, fall back to the existing stable-latest path (`/releases/latest`).
- Downgrade protection unchanged:
  - Target < current, no explicit version → bail ("latest release … is older than installed version …, skipping").
  - Target == current, no `--force` → "Already on version …".
- `--dry-run`, `--force`, JSON output, SHA256 verify, atomic swap — all unchanged.

## Code touchpoints

- `lib/crates/fabro-cli/src/args.rs` — add `prerelease: bool` with `#[arg(long, conflicts_with = "version")]`.
- `lib/crates/fabro-cli/src/commands/upgrade.rs`:
  - New local `ReleaseSummary { tag_name: String, draft: bool, prerelease: bool }` (serde).
  - New `Backend::fetch_releases(&self) -> Result<Vec<ReleaseSummary>>` that hits `/releases` via HTTP or `gh api repos/{repo}/releases`.
  - New pure `fn pick_latest_tag(releases: &[ReleaseSummary]) -> Option<String>` — filters drafts + unparseable tags, returns max-semver `tag_name`.
  - `run_upgrade`: when `args.prerelease`, call `fetch_releases` → `pick_latest_tag`; on `None`, fall back to `fetch_latest_release_tag`.
  - `spawn_upgrade_check` / `check_and_print_notice` untouched.

## Tests (unit, no network)

- `pick_latest_tag`:
  - mixed stable + prerelease, newest is prerelease → picks prerelease.
  - mixed stable + prerelease, newest is stable → picks stable.
  - all drafts → `None` (triggers fallback).
  - malformed tag (`"weekly-build-3"`) alongside valid tags → skipped, valid winner returned.
  - empty input → `None`.
- Clap: `fabro upgrade --prerelease --version 0.1.0` exits with `conflicts_with` error. (Existing `UpgradeArgs` tests style.)

## Out of scope

- Persistent channel config, env var (`FABRO_UPGRADE_CHANNEL`), `--stable` opt-out.
- Changing background upgrade notice to include prereleases.
- Docs/changelog updates beyond the `--help` string (can follow in the same PR as trivial diffs).

## Open questions

None.
