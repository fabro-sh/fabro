# Property Tests, Snapshots, Benchmarks, and CI

## Rule

Use `cargo nextest run --workspace --all-targets --all-features` as the default workspace test runner; add `insta` when snapshots make complex output easier to review, and add property or benchmark tools only for real invariant or performance needs.

## Activation

Load this page when configuring test commands, CI, snapshot tests, property tests, benchmarks, or release verification.

## Why

Nextest gives a consistent test runner for local and CI workflows. Snapshot, property, and benchmark tools are valuable when they match the code shape, but they add dependencies, review process, and maintenance cost.

## Do

- Run `cargo nextest run --workspace --all-targets --all-features` as the normal local and CI test command.
- Keep `cargo test` available for cases Nextest does not cover; the doctest opt-in policy lives on [testing and doctests](testing-and-doctests.md).
- Run pinned rustfmt and Clippy checks in CI alongside tests.
- Add `insta` for stable textual or structured outputs such as CLI output, diagnostics, generated config, serialized data, and rendered reports.
- Commit snapshot files and review snapshot diffs before accepting them.
- Redact, sort, or normalize nondeterministic fields before snapshotting values.
- Use `proptest` for parsers, serializers, round trips, normalization, state machines, and invariants over broad input spaces.
- Prefer `proptest` for new property tests; keep `quickcheck` only when the project already uses it.
- Use `criterion` when performance is a stated requirement or a likely regression risk.
- Keep benchmark inputs realistic, named, and stable across runs.

## Avoid

- Do not add every testing tool to every crate by default.
- Do not use snapshot tests for simple scalar assertions.
- Do not snapshot timestamps, random IDs, absolute paths, map iteration order, or environment-specific output without normalizing them.
- Do not blindly accept snapshot changes.
- Do not write property tests whose generated cases are so broad that failures are impossible to diagnose.
- Do not treat benchmarks as correctness tests.
- Do not fail ordinary CI on benchmark thresholds unless the project has stable performance infrastructure.
- Do not maintain separate local and CI test commands that cover different test sets without documenting the difference.

## Example

Run the configured CI commands before handing off Rust changes:

```sh
cargo +nightly-2026-04-14 fmt --check --all
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo nextest run --workspace --all-targets --all-features
```

Use the new project workflow for initial CI setup.

Add snapshot tests when reviewing the full output is clearer than hand-picking many assertions:

```rust
#[test]
fn renders_validation_errors() {
    let report = render_validation_errors(&[
        ValidationError::missing_field("email"),
        ValidationError::invalid_field("limit"),
    ]);

    insta::assert_snapshot!(report);
}
```

Add property tests for broad invariants:

```rust
use proptest::prelude::*;

proptest! {
    #[test]
    fn trim_is_idempotent(input in "[a-zA-Z0-9 ]{0,64}") {
        let once = normalize_whitespace(&input);
        let twice = normalize_whitespace(&once);

        prop_assert_eq!(once, twice);
    }
}
```

## Exceptions

- Existing projects may keep `cargo test` as the primary runner until Nextest is deliberately added.
- Use `quickcheck` when it is already the established project convention.
- Use custom benchmark or load-test infrastructure for services where `criterion` does not model the real performance risk.
- Skip specialized tooling for small crates where ordinary tests make the behavior clear.
