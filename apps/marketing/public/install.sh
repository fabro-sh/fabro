#!/bin/sh
set -eu

REPO="fabro-sh/fabro"

# Colors (only when stderr is a terminal)
if [ -t 2 ]; then
  RED='\033[0;31m'
  GREEN='\033[0;32m'
  DIM='\033[2m'
  BOLD='\033[1m'
  BOLD_CYAN='\033[1;36m'
  RESET='\033[0m'
else
  RED=''
  GREEN=''
  DIM=''
  BOLD=''
  BOLD_CYAN=''
  RESET=''
fi

info()    { printf "  %b\n" "$1" >&2; }
step()    { printf "  ${BOLD}%b${RESET}\n" "$1" >&2; }
dim()     { printf "  ${DIM}%b${RESET}\n" "$1" >&2; }
success() { printf "  ${GREEN}✔${RESET} %b\n" "$1" >&2; }
error()   { printf "  ${RED}✗ %b${RESET}\n" "$1" >&2; exit 1; }

# --- Header ---
printf "\n  ⚒️  ${BOLD}Fabro Install${RESET}\n\n" >&2

# --- Require gh CLI ---
if ! command -v gh >/dev/null 2>&1; then
  error "gh CLI is required but not installed. Install it from ${BOLD_CYAN}https://cli.github.com${RESET}"
fi

# --- Detect platform ---
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
  Darwin)
    # Detect Rosetta translation
    if [ "$ARCH" = "x86_64" ]; then
      if sysctl -n sysctl.proc_translated 2>/dev/null | grep -q 1; then
        ARCH="arm64"
      fi
    fi
    case "$ARCH" in
      arm64) TARGET="aarch64-apple-darwin" ;;
      *)     error "Unsupported macOS architecture: $ARCH. Supported: Apple Silicon (arm64)" ;;
    esac
    ;;
  Linux)
    case "$ARCH" in
      x86_64)  TARGET="x86_64-unknown-linux-gnu" ;;
      aarch64) TARGET="aarch64-unknown-linux-gnu" ;;
      *)       error "Unsupported Linux architecture: $ARCH. Supported: x86_64, aarch64" ;;
    esac
    if ldd --version 2>&1 | grep -qi musl; then
      TARGET="${TARGET%-gnu}-musl"
    fi
    ;;
  *)
    error "Unsupported OS: $OS. Supported platforms: macOS (Apple Silicon), Linux (x86_64, aarch64)"
    ;;
esac

ASSET="fabro-${TARGET}.tar.gz"
download_dir="$(mktemp -d)"
bundle_dir=""
activation_dir=""
trap 'rm -rf "$download_dir"; [ -z "$activation_dir" ] || rm -rf "$activation_dir"' EXIT

TAG="$(gh api "repos/${REPO}/releases/latest" --jq '.tag_name')"
if [ -z "$TAG" ]; then
  error "Could not resolve the latest stable release tag"
fi

dim "Downloading fabro for ${TARGET}..."
gh release download "$TAG" --repo "$REPO" --pattern "$ASSET" --dir "$download_dir" --clobber

dim "Extracting..."
tar xzf "${download_dir}/${ASSET}" -C "$download_dir"

# --- Stage a complete, immutable installation ---
INSTALL_DIR="${FABRO_INSTALL_DIR:-$HOME/.fabro/bin}"
mkdir -p "$INSTALL_DIR"
INSTALL_DIR="$(cd "$INSTALL_DIR" && pwd -P)"
source_dir="${download_dir}/fabro-${TARGET}"
[ -f "$source_dir/fabro" ] && [ ! -L "$source_dir/fabro" ] || error "Release is missing the fabro executable"
# Older stable archives contain only fabro. Accept those, but never a
# partially delivered plugin bundle.
plugin_count=0
for kind in host docker daytona; do
  if [ -e "$source_dir/sandbox-driver-$kind" ] || [ -L "$source_dir/sandbox-driver-$kind" ]; then
    plugin_count=$((plugin_count + 1))
  fi
done
[ "$plugin_count" -eq 0 ] || [ "$plugin_count" -eq 3 ] || error "Release has an incomplete sandbox plugin bundle"
if [ "$plugin_count" -eq 3 ]; then
  for kind in host docker daytona; do
    plugin="$source_dir/sandbox-driver-$kind"
    [ -f "$plugin" ] && [ ! -L "$plugin" ] && [ -x "$plugin" ] || error "Invalid sandbox plugin: $kind"
  done
fi
mkdir -p "$INSTALL_DIR/.fabro-versions"
bundle_dir="$(mktemp -d "$INSTALL_DIR/.fabro-versions/bundle-XXXXXXXX")"
cp "$source_dir/fabro" "$bundle_dir/fabro"
chmod +x "$bundle_dir/fabro"
if [ "$plugin_count" -eq 3 ]; then
  for kind in host docker daytona; do
    cp "$source_dir/sandbox-driver-$kind" "$bundle_dir/"
  done
fi
# mktemp creates a private directory; shared install locations must remain
# traversable by the users who could execute the previous installation.
chmod 755 "$bundle_dir"
VERSION="$("$bundle_dir/fabro" --version 2>/dev/null)" || error "Installation failed: could not run fabro --version"
[ -n "$VERSION" ] || error "Installation failed: could not run fabro --version"
# Keep the old bundle for servers still using it. A flat old executable is
# retained too; do not modify existing sibling plugins.
if [ -f "$INSTALL_DIR/fabro" ] && [ ! -L "$INSTALL_DIR/fabro" ]; then
  previous_dir="$(mktemp -d "$INSTALL_DIR/.fabro-versions/previous-XXXXXXXX")"
  ln "$INSTALL_DIR/fabro" "$previous_dir/fabro"
fi
activation_dir="$(mktemp -d "$INSTALL_DIR/.fabro-activate-XXXXXXXX")"
ln -s ".fabro-versions/$(basename "$bundle_dir")/fabro" "$activation_dir/fabro"
# -f replaces the launcher itself; an existing bundle remains immutable.
mv -f "$activation_dir/fabro" "$INSTALL_DIR/fabro"

tildify() {
  if [ "${1#"$HOME"/}" != "$1" ]; then
    echo "~/${1#"$HOME"/}"
  else
    echo "$1"
  fi
}

success "Installed ${VERSION} to ${BOLD_CYAN}$(tildify "${INSTALL_DIR}/fabro")${RESET}"

# --- Ensure install dir is on PATH ---
if command -v fabro >/dev/null 2>&1; then
  dim "fabro is already on \$PATH, skipping shell config"
else
  tilde_bin_dir=$(tildify "$INSTALL_DIR")
  echo "" >&2

  if [ -t 2 ] && [ -e /dev/tty ]; then
    case $(basename "${SHELL:-sh}") in
    zsh)
      : "${ZDOTDIR:="$HOME"}"
      shell_config="${ZDOTDIR%/}/.zshrc"
      {
        printf '\n# fabro\n'
        echo "export PATH=\"$INSTALL_DIR:\$PATH\""
      } >>"$shell_config"
      info "Added ${BOLD_CYAN}${tilde_bin_dir}${RESET} to \$PATH in ${BOLD_CYAN}$(tildify "$shell_config")${RESET}"
      ;;
    bash)
      shell_config="$HOME/.bashrc"
      if [ -f "$HOME/.bash_profile" ]; then
        shell_config="$HOME/.bash_profile"
      fi
      {
        printf '\n# fabro\n'
        echo "export PATH=\"$INSTALL_DIR:\$PATH\""
      } >>"$shell_config"
      info "Added ${BOLD_CYAN}${tilde_bin_dir}${RESET} to \$PATH in ${BOLD_CYAN}$(tildify "$shell_config")${RESET}"
      ;;
    fish)
      fish_config="$HOME/.config/fish/config.fish"
      mkdir -p "$(dirname "$fish_config")"
      {
        printf '\n# fabro\n'
        echo "fish_add_path $INSTALL_DIR"
      } >>"$fish_config"
      info "Added ${BOLD_CYAN}${tilde_bin_dir}${RESET} to \$PATH in ${BOLD_CYAN}$(tildify "$fish_config")${RESET}"
      ;;
    *)
      info "Add ${BOLD_CYAN}${tilde_bin_dir}${RESET} to your PATH:"
      echo "" >&2
      info "  ${BOLD}export PATH=\"${INSTALL_DIR}:\$PATH\"${RESET}"
      ;;
    esac
  else
    info "Add ${BOLD_CYAN}${tilde_bin_dir}${RESET} to your PATH:"
    echo "" >&2
    info "  ${BOLD}export PATH=\"${INSTALL_DIR}:\$PATH\"${RESET}"
  fi

  export PATH="${INSTALL_DIR}:$PATH"
fi
echo "" >&2

# --- Prompt to start the server and open the install wizard ---
if [ -t 2 ] && [ -e /dev/tty ]; then
  printf "  ${BOLD}Run ${BOLD_CYAN}fabro server start${RESET}${BOLD} now to finish setup in your browser? [Y/n]${RESET} " >&2
  read -r answer </dev/tty
  case "$answer" in
    [nN]*) dim "Skipping. Run ${BOLD_CYAN}fabro server start${RESET}${DIM} whenever you're ready." ;;
    *)     echo "" >&2; exec "${INSTALL_DIR}/fabro" server start ;;
  esac
else
  info "Run ${BOLD_CYAN}fabro server start${RESET} to finish setup in your browser."
fi
