# Rust Edition and MSRV

## Rule

Use Rust 2024 for new code and declare `rust-version` in every package; default Rust 2024 crates to `rust-version = "1.85"` unless project constraints require otherwise.

## Why

The edition controls language compatibility, and `rust-version` tells Cargo and users the minimum compiler the crate supports. Declaring both prevents agents from accidentally depending on newer compiler features without making that policy visible.

## Do

- Set `edition = "2024"` for Rust 2024 crates.
- Set `rust-version = "1.85"` for Rust 2024 crates unless the project has a higher documented MSRV; supporting a lower MSRV requires an older edition.
- Keep workspace member editions and MSRVs consistent unless a crate has a specific reason to differ.
- Treat MSRV bumps in reusable libraries as public compatibility changes.
- Check library changes against the declared MSRV, not only the local stable compiler, and include all feature-gated code.
- Use stable Rust by default.

## Avoid

- Do not omit `rust-version` from `Cargo.toml`.
- Do not use Rust 2021 for new crates by habit.
- Do not set an MSRV lower than the selected edition supports.
- Do not use APIs stabilized after the declared MSRV without bumping `rust-version`.
- Do not use nightly-only language features as house style.
- Do not let a dependency upgrade silently raise a library's practical MSRV.

## Public API Notes

For libraries, an MSRV bump can affect downstream users even when the Rust API is otherwise semver-compatible. Make the bump deliberate and document it in release notes or the changelog when the crate is published.

Applications and internal services may track stable Rust more aggressively, but they should still declare `rust-version` so builds are reproducible and CI failures are easier to understand.

## Example

Package-level policy:

```toml
[package]
name = "example-crate"
version = "0.1.0"
edition = "2024"
rust-version = "1.85"
```

When changing a reusable library, verify the declared MSRV explicitly:

```sh
rustup toolchain install 1.85.0
cargo +1.85.0 check --workspace --all-targets --all-features
```

Use the new project workflow for initial workspace setup.

## Exceptions

- Use Rust 2021 when required by embedded targets, downstream users, tooling, or dependency constraints.
- Use a higher MSRV when the project already requires newer stable compiler features.
- Migrate existing crates to a newer edition as a focused mechanical change when possible.
