# Reusable Library Release Verification

Use this workflow before releasing or handing off a reusable library crate, especially when it has optional features, public APIs, or an explicit MSRV.

## Required Guidelines

Load [guidelines.md](../guidelines.md), then load these guideline pages as needed:

- [Library vs application conventions](../guidelines/library-vs-application-conventions.md)
- [Rust edition and MSRV](../guidelines/rust-edition-and-msrv.md)
- [Cargo, workspaces, features, and dependencies](../guidelines/cargo-workspaces-features-and-dependencies.md)
- [rustc and Clippy lints](../guidelines/rustc-and-clippy-lints.md)
- [Testing and doctests](../guidelines/testing-and-doctests.md)
- [Property tests, snapshots, benchmarks, and CI](../guidelines/property-tests-snapshots-benchmarks-and-ci.md)
- [Public API evolution](../guidelines/public-api-evolution.md)

Also load error, documentation, unsafe, async, or observability guidelines when those surfaces are part of the library API.

## Workflow

1. Confirm the crate is a reusable library and identify its public API, feature flags, and declared MSRV.
2. Verify all features are additive. If features are intentionally incompatible, document the supported feature matrix before release.
3. Check that public dependency types are exposed only when they are part of the intended contract.
4. Run the default all-features verification commands.
5. Run dependency and supply-chain checks when the project has the tools installed.
6. Verify out-of-box behavior for the default feature set.
7. For published crates, run `cargo semver-checks` to detect accidental public API breaks and `cargo publish --dry-run` to validate the release artifact.
8. Record any MSRV bump, public API break, new optional dependency, or feature behavior change in release notes or the changelog.

## Default Verification

Use these commands before releasing a reusable library:

Use `--workspace` when verifying every library crate in the workspace. When releasing one crate from a mixed workspace, replace `--workspace` with `-p crate-name`.

Run the MSRV check with the crate's declared `rust-version` from step 1; `+1.85.0` below is illustrative, so a crate that declares `rust-version = "1.78"` is verified with `cargo +1.78.0 check`.

```sh
cargo +nightly-2026-04-14 fmt --check --all
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo nextest run --workspace --all-targets --all-features
cargo +1.85.0 check --workspace --all-targets --all-features
cargo check --workspace --all-targets --no-default-features
```

If the project intentionally maintains doctests, add:

```sh
cargo test --doc --workspace --all-features
```

## Feature Matrix

Use `--all-features` by default. Replace it with an explicit matrix only when a crate intentionally has incompatible feature sets.

For an explicit matrix, verify each supported combination that users can depend on:

```sh
cargo check --workspace --all-targets --no-default-features
cargo check --workspace --all-targets --features serde
cargo check --workspace --all-targets --features tokio
cargo check --workspace --all-targets --features "serde tokio"
```

Keep the matrix small and documented. If the matrix grows large, reconsider whether the features are too granular or too tightly coupled.

## Dependency Checks

When the project has the tools installed, run:

```sh
cargo audit
cargo deny check
cargo machete
```

Treat these as release gates for published crates when the project has adopted them. For internal libraries, use them when dependency churn, public dependency exposure, or supply-chain risk is material.

## Semver and Artifact Checks

For published crates, detect accidental public API breaks and validate the release artifact:

```sh
cargo semver-checks
cargo publish --dry-run
```

Install the checker once with `cargo install cargo-semver-checks --locked`. Use `cargo package` instead of the dry-run publish when the crate is not published to a registry. Treat any semver-major finding as either a bug to fix or an intentional break to record in step 8.

## Out-of-Box Build

Reusable libraries should build with the default feature set without hidden setup:

```sh
cargo check --workspace --all-targets
```

For crates with minimal default features, also verify the no-default-features build. Do not require users to enable unrelated integrations to compile the core crate.

## Avoid

- Do not release a library after checking only the default feature set when optional feature-gated code changed.
- Do not use `--all-features` as a substitute for documenting intentionally incompatible feature combinations.
- Do not let a dependency update raise MSRV without making that decision explicit.
- Do not add release-only verification commands that are never run locally or in CI.
- Do not require security or dependency tools for every tiny internal crate unless the project has adopted those gates.
