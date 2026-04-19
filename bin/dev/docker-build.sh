#!/usr/bin/env bash
# Build a local fabro Docker image from the current working tree.
#
# The Docker image uses musl binaries for a small Alpine-based runtime. We
# compile fabro-cli for the target musl triple inside rust:1-bookworm using
# cargo-zigbuild (zig as the C compiler + linker). Cargo registry and target
# dir are cached in named volumes so subsequent runs are incremental.
#
# Usage:
#   bin/dev/docker-build.sh                    # host arch, build image at end
#   bin/dev/docker-build.sh --arch amd64       # force amd64 (uses QEMU on non-amd64 hosts — slow)
#   bin/dev/docker-build.sh --tag fabro:smoke  # tag image as fabro:smoke (default: fabro)
#   bin/dev/docker-build.sh --arch arm64 --compile-only
#                                              # stage binary only; skip `docker build`
#                                              # (useful for multi-arch buildx verification)

set -euo pipefail

cd "$(dirname "$0")/../.."

arch=""
compile_only=0
tag="fabro"
while [ $# -gt 0 ]; do
  case "$1" in
    --arch)
      arch="$2"
      shift 2
      ;;
    --tag)
      tag="$2"
      shift 2
      ;;
    --compile-only)
      compile_only=1
      shift
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 1
      ;;
  esac
done

if [ -z "$arch" ]; then
  case "$(uname -m)" in
    x86_64|amd64)   arch=amd64 ;;
    aarch64|arm64)  arch=arm64 ;;
    *) echo "unsupported host arch: $(uname -m)" >&2; exit 1 ;;
  esac
fi

case "$arch" in
  amd64) target=x86_64-unknown-linux-musl  zig_arch=x86_64 ;;
  arm64) target=aarch64-unknown-linux-musl zig_arch=aarch64 ;;
  *) echo "unsupported arch: $arch (expected amd64 or arm64)" >&2; exit 1 ;;
esac

ZIG_VERSION=0.13.0

echo "Building fabro-cli for $target inside rust:1-bookworm via cargo-zigbuild..."
docker run --rm --platform "linux/$arch" \
  -v "$PWD:/src" \
  -v fabro-docker-cargo-registry:/usr/local/cargo/registry \
  -v "fabro-docker-cargo-target-$arch:/target" \
  -v "fabro-docker-zig-$arch:/opt/zig" \
  -v "fabro-docker-cargo-tools-$arch:/opt/cargo-tools" \
  -w /src \
  -e CARGO_TARGET_DIR=/target \
  -e LIBZ_SYS_STATIC=1 \
  rust:1-bookworm \
  bash -c "
    set -e
    apt-get update -qq && apt-get install -y -qq pkg-config perl make cmake xz-utils curl >/dev/null
    if [ ! -x /opt/zig/zig-linux-$zig_arch-$ZIG_VERSION/zig ]; then
      curl -fsSL https://ziglang.org/download/$ZIG_VERSION/zig-linux-$zig_arch-$ZIG_VERSION.tar.xz | tar -xJ -C /opt/zig
    fi
    export PATH=/opt/cargo-tools/bin:/opt/zig/zig-linux-$zig_arch-$ZIG_VERSION:\$PATH
    if ! command -v cargo-zigbuild >/dev/null; then
      cargo install --locked --root /opt/cargo-tools cargo-zigbuild
    fi
    rustup target add $target
    cargo zigbuild --release -p fabro-cli --target $target
  "

echo "Extracting binary from builder cache..."
mkdir -p "docker-context/$arch"
docker run --rm --platform "linux/$arch" \
  -v "fabro-docker-cargo-target-$arch:/target" \
  -v "$PWD/docker-context/$arch:/out" \
  rust:1-bookworm \
  cp "/target/$target/release/fabro" /out/fabro

if [ "$compile_only" -eq 1 ]; then
  echo "Staged docker-context/$arch/fabro (skipping docker build per --compile-only)."
  exit 0
fi

echo "Building Docker image as $tag..."
docker build --platform "linux/$arch" -t "$tag" .
