# rustc and Clippy Lints

## Rule

Use curated workspace lints: start from the workspace lint tables in the [new project workflow](../workflows/new-rust-project.md), tailor project-specific denies, and require justified local exceptions with `#[expect(..., reason = "...")]`.

## Why

A curated lint set catches real mistakes while the allow-list exempts the noisy pedantic lints the project has rejected; everything else is enforced in CI. Central policy keeps the baseline consistent, and local `expect` attributes make intentional exceptions auditable.

## Do

- Put shared lint policy in the workspace `Cargo.toml`.
- Run Clippy in CI with `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`.
- Enable `clippy::pedantic` at `warn`, then allow noisy lints the project has rejected.
- Deny lints that catch correctness or project-boundary violations.
- Use `#[expect(lint_name, reason = "...")]` for narrow local exceptions.
- Review the baseline `disallowed_methods` and `disallowed_types` in the [new project workflow](../workflows/new-rust-project.md) before copying them; these should reflect the target project's architecture.
- Put architecture-specific Clippy settings in `clippy.toml`.

## Avoid

- Do not enable all restriction lints.
- Do not deny all pedantic lints by default.
- Do not add unexplained `#[allow(...)]` attributes.
- Do not hide one-off exceptions in workspace-wide lint config.
- Do not copy project-specific disallowed methods, types, or environment rules without checking that they match the new codebase.
- Do not use local lint bypasses for combinator-vs-control-flow idioms; refactor to Clippy's preferred shape or change the workspace lint policy deliberately.

## Lint Levels and CI

CI runs Clippy with `-D warnings`, so the level controls where a violation is caught, not whether it is allowed:

- `deny`: denied rustc lints fail `cargo build` everywhere, including local builds; denied Clippy lints fail only `cargo clippy`, so the Clippy run, locally and in CI, is what enforces them.
- `warn`: a local warning, but promoted to an error in CI by `-D warnings`.
- `allow`: the only true exemption; every lint not allowed is enforced in CI.

Justify an intentional violation at the narrowest scope with `#[expect(lint, reason = "...")]`; a bare `#[allow]` is rejected by `allow_attributes_without_reason`. A CLI, for example, keeps `print_stdout = "warn"` and annotates each of its few real stdout functions:

```rust
#[expect(clippy::print_stdout, reason = "curated help is written directly to stdout")]
fn print_help() {
    println!("usage: app <command> [options]");
}
```

## Example

Use the new project workflow for initial workspace lint tables. In existing projects, justify narrow local exceptions near the code:

```rust
#[expect(
    clippy::too_many_arguments,
    reason = "Constructor mirrors the wire contract fields one-to-one"
)]
pub fn new(
    id: RunId,
    parent_id: Option<RunId>,
    status: RunStatus,
    attempt: AttemptNumber,
    started_at: Timestamp,
    finished_at: Option<Timestamp>,
    labels: Labels,
    metadata: Metadata,
) -> Self {
    Self {
        id,
        parent_id,
        status,
        attempt,
        started_at,
        finished_at,
        labels,
        metadata,
    }
}
```

## Exceptions

- Use `#[allow]` only when `#[expect]` is unavailable or the lint is intentionally disabled for generated code.
- Move a lint to workspace config when the project has rejected it as policy, not because one function is inconvenient.
- Lower or remove `unsafe_code = "deny"` only for crates whose purpose requires unsafe code, then document the local unsafe policy.
