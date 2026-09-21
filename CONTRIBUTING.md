# Contributing to Fabro

Thanks for your interest in contributing to Fabro!

## How to contribute

Outside contributions are welcome! Whether it's a bug fix, a new feature, documentation, or a typo -- we'd love your help making Fabro better.

- **Bug fixes and small improvements** -- Send a pull request directly. No need to open an issue first.
- **Larger features or changes** -- Please open a [GitHub Issue](https://github.com/fabro-sh/fabro/issues) or start a [Discussion](https://github.com/fabro-sh/fabro/discussions) first so we can align on the approach before you invest significant time.
- **Prefer not to write the code yourself?** -- As an alternative to opening a PR, you can file a [GitHub Issue](https://github.com/fabro-sh/fabro/issues) describing the bug or feature. A Fabro maintainer will implement it (supervising AI coding agents and workflows) and include you as a co-author on the commit that lands the change.
- **Questions** -- Open a Discussion or email [bryan@qlty.sh](mailto:bryan@qlty.sh).

## Development setup

The instructions below will help you build and test Fabro locally.

### Prerequisites

- [Rust](https://rustup.rs/) (latest stable)
- [Bun](https://bun.sh/) (for the web frontend)
- Git

### Build and test

```bash
# Build Fabro with its embedded web UI and matching sandbox plugins
bun install --frozen-lockfile
cargo dev build

# Build all Rust crates
cargo build --workspace

# Run all tests
cargo test --workspace

# Check formatting and lint
cargo fmt --check --all
cargo clippy --workspace -- -D warnings
```

`cargo dev build` builds the sandbox-driver executables from the revision in
`Cargo.lock`, embeds their SHA-256 hashes in Fabro through Petri, and copies
the plugins beside the resulting executable. Use `cargo dev build -- --release`
for a release bundle. Keep all four executables together when copying a build
to another directory or machine.

For release-mode tests, build the plugins before compiling the test binaries:

```bash
cargo dev plugins --target aarch64-apple-darwin
export PETRI_SANDBOX_PLUGIN_DIR="$PWD/target/aarch64-apple-darwin/plugins/bin"
export PETRI_SANDBOX_HOST_PLUGIN="$PETRI_SANDBOX_PLUGIN_DIR/sandbox-driver-host"
export PETRI_SANDBOX_DOCKER_PLUGIN="$PETRI_SANDBOX_PLUGIN_DIR/sandbox-driver-docker"
export PETRI_SANDBOX_DAYTONA_PLUGIN="$PETRI_SANDBOX_PLUGIN_DIR/sandbox-driver-daytona"
export PETRI_SANDBOX_PLUGIN_DEV=0
unset PETRI_SANDBOX_HOST_SHA256 PETRI_SANDBOX_DOCKER_SHA256 PETRI_SANDBOX_DAYTONA_SHA256
mkdir -p target/release
cp "$PETRI_SANDBOX_PLUGIN_DIR/"sandbox-driver-* target/release/
cargo nextest run --workspace --release --profile ci
```

Replace the target triple with your platform. `cargo dev release` prepares
these build inputs and runtime paths automatically for its release smoke test.
Avoid rebuilding the plugins between compiling Fabro and assembling its bundle:
the embedded hashes must describe the bytes you distribute.

### Web frontend (fabro-web)

```bash
cd apps/fabro-web
bun install
bun run dev        # start dev server
bun test           # run tests
bun run typecheck  # type check
```

## Development workflow

1. Create a branch from `main`
2. Make your changes
3. Ensure `cargo test --workspace`, `cargo fmt --check --all`, and `cargo clippy --workspace -- -D warnings` pass

## License

By contributing, you agree that your contributions will be licensed under the [MIT License](LICENSE.md).
