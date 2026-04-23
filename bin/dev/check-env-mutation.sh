#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/../.."

if command -v rg >/dev/null 2>&1; then
  matches=$(rg -n 'std::env::(set_var|remove_var)' --glob '*.rs' || true)
else
  matches=$(grep -R -n -E 'std::env::(set_var|remove_var)' . --include='*.rs' --exclude-dir=target --exclude-dir=.git || true)
fi

fail=0
while IFS= read -r match; do
  [[ -z "$match" ]] && continue

  path=${match%%:*}
  rest=${match#*:}
  line=${rest#*:}
  line=${line#"${line%%[![:space:]]*}"}

  case "$path:$line" in
    "lib/crates/fabro-telemetry/src/spawn.rs:std::env::set_var(key, value);" | \
    "lib/crates/fabro-telemetry/src/spawn.rs:std::env::remove_var(key);")
      continue
      ;;
  esac

  echo "process env mutation check failed: $match" >&2
  fail=1
done <<< "$matches"

if [[ $fail -ne 0 ]]; then
  cat >&2 <<'EOF'

Do not mutate process-wide env with std::env::set_var/remove_var.
Inject env at construction time or on child-process Command values instead.
See docs-internal/server-secrets-strategy.md.
EOF
  exit 1
fi

echo "Process env mutation checks passed."
