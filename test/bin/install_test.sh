#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
INSTALL_SCRIPT="$REPO_ROOT/apps/marketing/public/install.sh"

fail() {
  printf 'FAIL: %s\n' "$1" >&2
  exit 1
}

target_triple() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"

  case "$os" in
    Darwin)
      if [[ "$arch" == "x86_64" ]] && sysctl -n sysctl.proc_translated 2>/dev/null | grep -q 1; then
        arch="arm64"
      fi
      case "$arch" in
        arm64) printf 'aarch64-apple-darwin\n' ;;
        *) fail "unsupported macOS architecture in test: $arch" ;;
      esac
      ;;
    Linux)
      case "$arch" in
        x86_64) printf 'x86_64-unknown-linux-gnu\n' ;;
        aarch64) printf 'aarch64-unknown-linux-gnu\n' ;;
        *) fail "unsupported Linux architecture in test: $arch" ;;
      esac
      ;;
    *)
      fail "unsupported OS in test: $os"
      ;;
  esac
}

home_dir="$(mktemp -d)"
trap 'rm -rf "$home_dir"' EXIT
fake_bin="$home_dir/fake-bin"
gh_log="$home_dir/gh.log"
install_dir="$home_dir/install"
mkdir -p "$fake_bin"

cat >"$fake_bin/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

printf '%s\n' "$*" >>"$FAKE_GH_LOG"

case "$1" in
  --version)
    echo "gh version 2.89.0"
    ;;
  api)
    test "$2" = "repos/fabro-sh/fabro/releases/latest"
    test "$3" = "--jq"
    test "$4" = ".tag_name"
    echo "v9.9.9"
    ;;
  release)
    shift
    test "$1" = "download"
    shift

    tag=""
    if [[ "${1:-}" != "" && "${1#-}" == "$1" ]]; then
      tag="$1"
      shift
    fi

    asset=""
    dest_dir=""
    while [[ $# -gt 0 ]]; do
      case "$1" in
        --repo)
          shift 2
          ;;
        --pattern)
          asset="$2"
          shift 2
          ;;
        --dir)
          dest_dir="$2"
          shift 2
          ;;
        --clobber)
          shift
          ;;
        *)
          echo "unexpected gh release download args: $*" >&2
          exit 1
          ;;
      esac
    done

    test -n "$tag"
    test -n "$asset"
    test -n "$dest_dir"

    work_dir="$(mktemp -d)"
    bundle_dir="$work_dir/${asset%.tar.gz}"
    mkdir -p "$bundle_dir"
    cat >"$bundle_dir/fabro" <<'INNER'
#!/bin/sh
echo "fabro 9.9.9"
INNER
    chmod +x "$bundle_dir/fabro"
    case "${FAKE_BUNDLE_MODE:-legacy}" in
      complete|partial|symlink)
        for kind in host docker daytona; do
          printf '%s\n' "${FAKE_PLUGIN_CONTENT:-first}" > "$bundle_dir/sandbox-driver-$kind"
          chmod +x "$bundle_dir/sandbox-driver-$kind"
        done
        ;;
    esac
    case "${FAKE_BUNDLE_MODE:-legacy}" in
      partial) rm "$bundle_dir/sandbox-driver-daytona" ;;
      symlink)
        rm "$bundle_dir/sandbox-driver-daytona"
        ln -s fabro "$bundle_dir/sandbox-driver-daytona"
        ;;
    esac
    tar czf "$dest_dir/$asset" -C "$work_dir" "$(basename "$bundle_dir")"
    rm -rf "$work_dir"
    ;;
  *)
    echo "unexpected gh invocation: $*" >&2
    exit 1
    ;;
esac
EOF
chmod +x "$fake_bin/gh"

asset="fabro-$(target_triple).tar.gz"
output="$(
  HOME="$home_dir" \
  PATH="$fake_bin:/usr/bin:/bin:/usr/sbin:/sbin" \
  FABRO_INSTALL_DIR="$install_dir" \
  FAKE_GH_LOG="$gh_log" \
  "$INSTALL_SCRIPT" 2>&1
)" || {
  printf '%s\n' "$output" >&2
  fail "install script should succeed"
}

[[ -x "$install_dir/fabro" ]] || fail "install script should install fabro binary"
[[ "$output" == *"Installed fabro 9.9.9"* ]] || fail "install script should report installed version"
grep -Fqx "api repos/fabro-sh/fabro/releases/latest --jq .tag_name" "$gh_log" \
  || fail "install script should resolve the stable release tag explicitly"
grep -Fq "release download v9.9.9 --repo fabro-sh/fabro --pattern $asset" "$gh_log" \
  || fail "install script should download the resolved stable tag explicitly"

install_bundle() {
  HOME="$home_dir" \
  PATH="$fake_bin:/usr/bin:/bin:/usr/sbin:/sbin" \
  FABRO_INSTALL_DIR="$install_dir" \
  FAKE_GH_LOG="$gh_log" \
  FAKE_BUNDLE_MODE="$1" \
  FAKE_PLUGIN_CONTENT="$2" \
  "$INSTALL_SCRIPT" >"$home_dir/install.log" 2>&1
}

install_bundle complete first || fail "complete bundle should install"
first_launcher="$(readlink "$install_dir/fabro")"
first_bundle="$install_dir/$(dirname "$first_launcher")"
for kind in host docker daytona; do
  [[ "$(cat "$first_bundle/sandbox-driver-$kind")" == first ]] || fail "missing bundled $kind plugin"
done

install_bundle complete second || fail "second bundle should install"
second_launcher="$(readlink "$install_dir/fabro")"
[[ "$first_launcher" != "$second_launcher" ]] || fail "upgrade should switch bundles"
for kind in host docker daytona; do
  [[ "$(cat "$first_bundle/sandbox-driver-$kind")" == first ]] || fail "running server's plugin should remain unchanged"
  [[ "$(cat "$install_dir/$(dirname "$second_launcher")/sandbox-driver-$kind")" == second ]] || fail "upgrade should install every plugin"
done

for mode in partial symlink; do
  if install_bundle "$mode" invalid; then fail "$mode bundle should be rejected"; fi
  [[ "$(readlink "$install_dir/fabro")" == "$second_launcher" ]] || fail "failed install changed active bundle"
done

printf 'Installer accepts complete bundles and preserves the active bundle on invalid input.\n'
