#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
asset_dir="$repo_root/lib/crates/fabro-spa/assets"
asset_budget_bytes=$((15 * 1024 * 1024))
payload_budget_bytes=$((5 * 1024 * 1024))

if [[ ! -d "$asset_dir" ]]; then
  echo "fabro-spa assets directory is missing: $asset_dir" >&2
  exit 1
fi

asset_bytes=0
compressed_payload_bytes=0

while IFS= read -r -d '' file; do
  file_bytes="$(wc -c < "$file" | tr -d '[:space:]')"
  compressed_bytes="$(gzip -9 -n -c "$file" | wc -c | tr -d '[:space:]')"
  asset_bytes=$((asset_bytes + file_bytes))
  compressed_payload_bytes=$((compressed_payload_bytes + compressed_bytes))
done < <(find "$asset_dir" -type f -print0)

echo "fabro-spa asset bytes: $asset_bytes"
echo "fabro-spa estimated compressed payload bytes: $compressed_payload_bytes"

if (( asset_bytes > asset_budget_bytes )); then
  echo "fabro-spa committed assets exceed budget: $asset_bytes > $asset_budget_bytes" >&2
  exit 1
fi

if (( compressed_payload_bytes > payload_budget_bytes )); then
  echo "fabro-spa compressed payload exceeds budget: $compressed_payload_bytes > $payload_budget_bytes" >&2
  exit 1
fi
