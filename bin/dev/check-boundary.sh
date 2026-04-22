#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/../.."

symbol_allowlist=(
  "lib/crates/fabro-cli/src/local_server.rs"
  "lib/crates/fabro-cli/src/commands/install.rs"
  "lib/crates/fabro-cli/src/commands/run/runner.rs"
  "lib/crates/fabro-cli/src/commands/pr/mod.rs"
  "lib/crates/fabro-cli/src/commands/pr/create.rs"
)

deprecated_helper_allowlist=(
  "lib/crates/fabro-cli/src/user_config.rs"
  "lib/crates/fabro-cli/src/commands/pr/mod.rs"
  "lib/crates/fabro-cli/src/commands/pr/create.rs"
)

temporary_exemptions=(
  "lib/crates/fabro-cli/src/commands/pr/mod.rs"
  "lib/crates/fabro-cli/src/commands/pr/create.rs"
)

exemption_marker="boundary-exempt(pr-api): remove with follow-up #1"

in_array() {
  local needle=$1
  shift
  local item
  for item in "$@"; do
    if [[ "$item" == "$needle" ]]; then
      return 0
    fi
  done
  return 1
}

find_matches() {
  local pattern=$1
  if command -v rg >/dev/null 2>&1; then
    rg -l "$pattern" lib/crates/fabro-cli/src --glob '*.rs' || true
  else
    grep -R -l -E "$pattern" lib/crates/fabro-cli/src --include='*.rs' || true
  fi
}

fail=0

while IFS= read -r path; do
  [[ -z "$path" ]] && continue
  if ! in_array "$path" "${symbol_allowlist[@]}"; then
    echo "boundary check failed: gated server symbol used outside allowlist: $path" >&2
    fail=1
  fi
done < <(find_matches 'fabro_config::resolve_server_from_file|fabro_config::resolve_server\b|Storage::new')

while IFS= read -r path; do
  [[ -z "$path" ]] && continue
  if ! in_array "$path" "${deprecated_helper_allowlist[@]}"; then
    echo "boundary check failed: deprecated user_config::storage_dir used outside allowlist: $path" >&2
    fail=1
  fi
done < <(find_matches 'user_config::storage_dir')

for path in "${temporary_exemptions[@]}"; do
  if ! grep -q "$exemption_marker" "$path"; then
    echo "boundary check failed: missing temporary exemption marker in $path" >&2
    fail=1
  fi
done

while IFS= read -r path; do
  [[ -z "$path" ]] && continue
  if ! in_array "$path" "${temporary_exemptions[@]}"; then
    echo "boundary check failed: unexpected temporary exemption marker in $path" >&2
    fail=1
  fi
done < <(find_matches "$exemption_marker")

if [[ $fail -ne 0 ]]; then
  exit 1
fi

echo "CLI/server boundary checks passed."
