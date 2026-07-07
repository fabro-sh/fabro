# New Rust Project

Use this workflow when creating or configuring a new Rust crate, workspace, CLI, library, service, or application.

## Required Guidelines

Load [guidelines.md](../guidelines.md), then load these guideline pages as needed:

- [House style and Rust philosophy](../guidelines/house-style-and-rust-philosophy.md)
- [Library vs application conventions](../guidelines/library-vs-application-conventions.md)
- [Rust edition and MSRV](../guidelines/rust-edition-and-msrv.md)
- [rustfmt and formatting](../guidelines/rustfmt-and-formatting.md)
- [rustc and Clippy lints](../guidelines/rustc-and-clippy-lints.md)
- [Cargo, workspaces, features, and dependencies](../guidelines/cargo-workspaces-features-and-dependencies.md)
- [Testing and doctests](../guidelines/testing-and-doctests.md)
- [Property tests, snapshots, benchmarks, and CI](../guidelines/property-tests-snapshots-benchmarks-and-ci.md)
- [Unsafe code and macros](../guidelines/unsafe-code-and-macros.md)

Load the async guideline when the project is async. Load logging, public API, and error guidelines when those surfaces apply.

## Workflow

1. Identify the project shape: library, application, CLI, service, test support crate, or mixed workspace.
2. Make the sync-vs-async posture explicit before adding async dependencies; async projects use Tokio.
3. Prefer a workspace when multiple crates share version, edition, dependencies, lints, or profiles.
4. Set Rust 2024 and `rust-version = "1.85"` unless the project already has different constraints.
5. Add pinned rustfmt configuration and use `nightly-2026-04-14` for formatting.
6. Add curated workspace lints and tailor project-specific `clippy.toml` guardrails before copying async/blocking disallow rules.
7. Audit every Rust source file under `src/`, including nested modules: classify it as trivial or nontrivial, and add bottom-of-file `#[cfg(test)] mod tests` for each nontrivial file's focused behavior and private helpers. Record a specific exception when a nontrivial file does not get module-local tests.
8. Use `cargo nextest run --workspace --all-targets --all-features` as the normal workspace test runner.
9. Skip doctests by default; run `cargo test --doc --workspace --all-features` only when the project explicitly opts into maintaining rustdoc examples.
10. Add dependencies only when they remove real complexity or provide mature domain behavior.
11. Verify the project with the configured commands before handing it off.

## Cargo Baseline

Use a workspace shape when the project is likely to grow beyond one crate:

```toml
[workspace]
members = ["crates/*"]
resolver = "3"

[workspace.package]
edition = "2024"
rust-version = "1.85"

[workspace.dependencies]
anyhow = "1"
serde = { version = "1", features = ["derive"] }
thiserror = "2"
tracing = "0.1"

[workspace.lints.rust]
unsafe_code = "deny"
unreachable_pub = "warn"

[workspace.lints.clippy]
pedantic = { level = "warn", priority = -2 }
allow_attributes_without_reason = "warn"

implicit_hasher = "allow"
missing_errors_doc = "allow"
missing_panics_doc = "allow"
module_name_repetitions = "allow"
must_use_candidate = "allow"
similar_names = "allow"
struct_excessive_bools = "allow"
too_many_arguments = "allow"
too_many_lines = "allow"
cast_precision_loss = "allow"
doc_markdown = "allow"

print_stdout = "warn"
print_stderr = "warn"
dbg_macro = "warn"
empty_drop = "warn"
empty_structs_with_brackets = "warn"
disallowed_methods = "deny"
exit = "warn"
get_unwrap = "warn"
unwrap_used = "deny"
rc_buffer = "warn"
rc_mutex = "warn"
rest_pat_in_fully_bound_structs = "warn"
use_self = "warn"
wildcard_imports = "warn"
absolute_paths = "warn"
```

Workspace lint inheritance is opt-in per member crate: every member crate must set `[lints] workspace = true` in its own `Cargo.toml`, or the workspace lint tables do nothing.

```toml
[package]
name = "example-crate"
edition.workspace = true
rust-version.workspace = true

[lints]
workspace = true
```

For a single crate, put the same package fields and lint tables in the crate's `Cargo.toml` instead of a workspace root, renaming the tables to `[lints.rust]` and `[lints.clippy]`; copied `[workspace.lints.*]` tables do nothing in a standalone manifest.

For async projects, add Tokio deliberately to the package or workspace dependencies:

```toml
tokio = { version = "1", features = ["full"] }
```

## rustfmt Baseline

Use this `rustfmt.toml` at the project root:

```toml
edition = "2024"
style_edition = "2024"

max_width = 100
comment_width = 80

group_imports = "StdExternalCrate"
imports_granularity = "Module"

use_field_init_shorthand = true
merge_derives = true
overflow_delimited_expr = true
format_code_in_doc_comments = true
format_macro_matchers = true
normalize_doc_attributes = true
wrap_comments = true

struct_field_align_threshold = 20
enum_discrim_align_threshold = 20
```

Install the pinned formatter, the MSRV toolchain, and the test runner used by the verification commands:

```sh
rustup toolchain install nightly-2026-04-14 --profile minimal --component rustfmt
rustup toolchain install 1.85.0 --profile minimal
cargo install cargo-nextest --locked
```

## Optional Clippy Guardrails

Use `clippy.toml` for project-specific architectural guardrails. For async projects, review rules like these before copying them:

```toml
allow-unwrap-in-tests = true
allow-unwrap-types = ["std::sync::LockResult"]

disallowed-methods = [
  { path = "std::thread::sleep", reason = "Prefer tokio::time::sleep on Tokio paths; document intentional blocking sleeps with #[expect(clippy::disallowed_methods, reason = \"...\")]", replacement = "tokio::time::sleep" },
  { path = "std::thread::spawn", reason = "Prefer Tokio task APIs on async paths; document intentional dedicated OS threads with #[expect(clippy::disallowed_methods, reason = \"...\")]" },
  { path = "std::process::Command::new", reason = "Prefer tokio::process::Command on Tokio paths; document intentional synchronous subprocesses with #[expect(clippy::disallowed_methods, reason = \"...\")]" },
]

disallowed-types = [
  { path = "std::io::Read", reason = "Blocking trait; prefer tokio::io::AsyncReadExt on Tokio paths. Document intentional sync I/O with #[expect(clippy::disallowed_types, reason = \"...\")]" },
  { path = "std::net::TcpStream", reason = "Blocking socket; prefer tokio::net::TcpStream on Tokio paths. Document intentional sync networking with #[expect(clippy::disallowed_types, reason = \"...\")]" },
]
```

## Verification Commands

Use these commands as the default new-project validation set:

```sh
cargo +nightly-2026-04-14 fmt --check --all
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo nextest run --workspace --all-targets --all-features
cargo +1.85.0 check --workspace --all-targets --all-features
```

If the project intentionally maintains doctests, add:

```sh
cargo test --doc --workspace --all-features
```

## Avoid

- Do not add async casually; document the project posture first.
- Do not add every standard dependency to every project by default.
- Do not copy Tokio-specific Clippy guardrails into sync projects.
- Do not create broad preludes, public facades, or feature flags before the project needs them.
- Do not lower `unsafe_code = "deny"` unless the new crate's purpose requires unsafe code.
- Do not let integration tests under `tests/` silently replace module-local tests for nontrivial source files.
