#!/usr/bin/env bash
set -euo pipefail

CARGO_TOML="$(git rev-parse --show-toplevel)/Cargo.toml"

current_version=$(grep -m1 '^version = ' "$CARGO_TOML" | sed 's/version = "\(.*\)"/\1/')
echo "Current version: $current_version"

IFS='.' read -r major minor patch <<< "$current_version"

echo ""
echo "Select bump type:"
echo "  1) patch ($current_version -> $major.$minor.$((patch + 1)))"
echo "  2) minor ($current_version -> $major.$((minor + 1)).0)"
echo "  3) major ($current_version -> $((major + 1)).0.0)"
echo ""
read -rp "Choice [1/2/3]: " choice

case "$choice" in
  1) new_version="$major.$minor.$((patch + 1))" ;;
  2) new_version="$major.$((minor + 1)).0" ;;
  3) new_version="$((major + 1)).0.0" ;;
  *) echo "Invalid choice"; exit 1 ;;
esac

tag="v$new_version"

if git rev-parse "$tag" >/dev/null 2>&1; then
  echo "Error: tag $tag already exists"
  exit 1
fi

echo ""
echo "Will bump $current_version -> $new_version and push tag $tag"
read -rp "Continue? [y/N]: " confirm
if [[ "$confirm" != [yY] ]]; then
  echo "Aborted"
  exit 1
fi

sed -i '' "s/^version = \"$current_version\"/version = \"$new_version\"/" "$CARGO_TOML"
echo "Updated $CARGO_TOML"

git add "$CARGO_TOML"
git commit -m "Bump version to $new_version"
git tag -a "$tag" -m "$tag"
git push origin main "$tag"

echo ""
echo "Released $tag"
echo "Watch the build: https://github.com/fabro-sh/fabro/actions"
