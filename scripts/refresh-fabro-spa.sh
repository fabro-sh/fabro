#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
web_dir="$repo_root/apps/fabro-web"
dist_dir="$web_dir/dist"
asset_dir="$repo_root/lib/crates/fabro-spa/assets"

(
  cd "$web_dir"
  bun run build
)

rm -rf "$asset_dir"
mkdir -p "$asset_dir"
cp -R "$dist_dir"/. "$asset_dir"/
find "$asset_dir" -type f -name '*.map' -delete
