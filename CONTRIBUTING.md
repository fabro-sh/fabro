# Contributing to Fabro

Thanks for your interest in contributing to Fabro!

## How to contribute

Outside contributions are welcome! Whether it's a bug fix, a new feature, documentation, or a typo -- we'd love your help making Fabro better.

- **Bug fixes and small improvements** -- Send a pull request directly. No need to open an issue first.
- **Larger features or changes** -- Please open a [GitHub Issue](https://github.com/fabro-sh/fabro/issues) or start a [Discussion](https://github.com/fabro-sh/fabro/discussions) first so we can align on the approach before you invest significant time.
- **Questions** -- Open a Discussion or email [bryan@qlty.sh](mailto:bryan@qlty.sh).

## Development setup

The instructions below will help you build and test Fabro locally.

### Prerequisites

- [Rust](https://rustup.rs/) (latest stable)
- [Bun](https://bun.sh/) (for the web frontend)
- Git

### Build and test

```bash
# Build all Rust crates
cargo build --workspace

# Run all tests
cargo test --workspace

# Check formatting and lint
cargo fmt --check --all
cargo clippy --workspace -- -D warnings
```

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
