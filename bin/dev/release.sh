#!/usr/bin/env bash
set -euo pipefail

die() {
  printf '%s\n' "$1" >&2
  exit 1
}

release_date() {
  printf '%s\n' "${FABRO_RELEASE_DATE:-$(date "+%Y-%m-%d")}"
}

days_since_2026() {
  local target_date="$1"
  python3 -c "from datetime import date; print((date.fromisoformat('$target_date') - date(2026, 1, 1)).days)"
}

next_base_version() {
  local date minor patch
  date="$1"
  minor=$(( $(days_since_2026 "$date") + 100 ))
  patch=0

  while git rev-parse "v0.${minor}.${patch}" >/dev/null 2>&1; do
    patch=$((patch + 1))
  done

  printf '0.%s.%s\n' "$minor" "$patch"
}

validate_prerelease_label() {
  local label="$1"
  case "$label" in
    nightly) ;;
    *)
      die "invalid pre-release label: $label (expected: nightly)"
      ;;
  esac
}

compute_release_version() {
  local base_version="$1"
  local prerelease_label="$2"

  if [[ -z "$prerelease_label" ]]; then
    printf '%s\n' "$base_version"
    return 0
  fi

  local prerelease_number=0
  while git rev-parse "v${base_version}-${prerelease_label}.${prerelease_number}" >/dev/null 2>&1; do
    prerelease_number=$((prerelease_number + 1))
  done

  printf '%s-%s.%s\n' "$base_version" "$prerelease_label" "$prerelease_number"
}

parse_args() {
  DRY_RUN=0
  SKIP_TESTS=0
  PRERELEASE_LABEL=""

  while [[ $# -gt 0 ]]; do
    case "$1" in
      --dry-run)
        DRY_RUN=1
        ;;
      --skip-tests)
        SKIP_TESTS=1
        ;;
      --help|-h)
        cat <<'EOF'
Usage: bin/dev/release.sh [nightly] [--dry-run] [--skip-tests]

Create the next stable release from main, or compute a prerelease tag.

Before tagging, runs the same release-mode test smoke the CI release
workflow does for the native target, with SEGMENT_WRITE_KEY baked in so
telemetry-active code paths are exercised. This catches regressions
like "uninstall recreates ~/.fabro via telemetry" or "sender tests
assume no write key" before we burn a 60-minute CI cycle. Skip only if
you've already run `cargo nextest run --workspace --release` yourself.
EOF
        exit 0
        ;;
      --*)
        die "unknown option: $1"
        ;;
      *)
        if [[ -n "$PRERELEASE_LABEL" ]]; then
          die "expected at most one pre-release label"
        fi
        validate_prerelease_label "$1"
        PRERELEASE_LABEL="$1"
        ;;
    esac
    shift
  done
}

verify_spa_assets() {
  local repo_root
  repo_root="$(git rev-parse --show-toplevel)"

  "${repo_root}/scripts/refresh-fabro-spa.sh"

  if ! git -C "$repo_root" diff --exit-code -- lib/crates/fabro-spa/assets >/dev/null; then
    die "fabro-spa assets are stale. Commit the refreshed assets before releasing."
  fi
}

verify_release_tests() {
  if [[ "$SKIP_TESTS" == "1" ]]; then
    echo "--skip-tests set, skipping release-mode test smoke"
    return 0
  fi

  echo "Running release-mode test smoke (SEGMENT_WRITE_KEY baked in)..."
  SEGMENT_WRITE_KEY="fake-for-local-smoke" \
    cargo nextest run \
      --workspace \
      --release \
      --profile ci \
      --status-level slow
}

main() {
  parse_args "$@"

  verify_spa_assets
  verify_release_tests

  local repo_root cargo_toml current_version base_version new_version tag
  repo_root="$(git rev-parse --show-toplevel)"
  cargo_toml="${repo_root}/Cargo.toml"

  current_version=$(grep -m1 '^version = ' "$cargo_toml" | sed 's/version = "\(.*\)"/\1/')
  echo "Current version: $current_version"

  base_version=$(next_base_version "$(release_date)")
  new_version=$(compute_release_version "$base_version" "$PRERELEASE_LABEL")
  tag="v$new_version"

  echo "Releasing $new_version (tag $tag)"

  if [[ "$DRY_RUN" == "1" ]]; then
    return 0
  fi

  sed -i.bak "s/^version = \"$current_version\"/version = \"$new_version\"/" "$cargo_toml"
  rm "${cargo_toml}.bak"
  echo "Updated $cargo_toml"

  cargo update --workspace
  echo "Updated Cargo.lock"

  git add "$cargo_toml" Cargo.lock
  git commit -m "Bump version to $new_version"
  git tag -a "$tag" -m "$tag"
  git push origin main "$tag"

  echo ""
  echo "Released $tag"
  echo "Watch the build: https://github.com/fabro-sh/fabro/actions"
}

main "$@"
