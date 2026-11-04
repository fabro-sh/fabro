#!/usr/bin/env bash
set -euo pipefail

# Run from the tap checkout after committing the formula and versions.json.
# Retry the same commit: if GitHub accepted it but lost the response, the next
# push is a no-op. Never force or rebase over another publisher's update.
for delay in 0 10 30; do
  if (( delay > 0 )); then
    echo "Homebrew push failed; retrying in ${delay}s" >&2
    sleep "$delay"
  fi
  if git push origin HEAD:refs/heads/main; then
    exit 0
  fi
done

echo "::error::Homebrew publication failed after 3 push attempts" >&2
exit 1
