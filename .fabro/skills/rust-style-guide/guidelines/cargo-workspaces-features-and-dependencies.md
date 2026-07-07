# Cargo, Workspaces, Features, and Dependencies

## Rule

Keep Cargo configuration explicit: use workspaces for shared policy, add dependencies deliberately, keep library features additive and minimal, and verify dependency changes against the declared MSRV.

## Why

Cargo choices shape compile time, public API, downstream compatibility, binary size, and release stability. Agents should avoid convenience changes that quietly become long-term constraints.

## Do

- Use a workspace when multiple crates share version, edition, dependencies, lints, or profiles.
- Put shared dependency versions in `[workspace.dependencies]`.
- Put shared lint policy in `[workspace.lints]`.
- Use conservative dependency policy for libraries.
- Use pragmatic dependency policy for applications when a dependency materially improves clarity or reliability.
- Prefer mature, maintained crates for domain behavior over small convenience crates.
- For application CLIs with subcommands, environment-backed options, generated help, or user-facing argument errors, prefer `clap` derive. Hand parsing is only for tiny private binaries with trivial arguments.
- Keep reusable library features additive and opt-in.
- Make `serde` optional for reusable libraries unless serialization is core to the crate.
- Verify reusable library changes with `--all-features` so feature-gated code stays compiled, linted, and tested.
- Check MSRV after adding dependencies or using newly stabilized APIs; [Rust edition and MSRV](rust-edition-and-msrv.md) owns the MSRV policy and verification command.

## Avoid

- Do not add a dependency for a trivial wrapper around `std`.
- Do not expose dependency types in public APIs unless that dependency is part of the intended contract.
- Do not use mutually exclusive Cargo features.
- Do not make default library features pull in heavy optional integrations.
- Do not add feature flags before there is a real optional integration.
- Do not derive serialization for a public type without deciding its wire-format compatibility policy.

## Library vs Application

Libraries should minimize default dependencies and keep feature flags additive. Applications can depend directly on the concrete crates they use and usually do not need feature flags around internal implementation details.

For libraries, treat public dependency exposure and MSRV bumps as compatibility decisions. For applications, still keep `rust-version` honest, but prefer simple direct configuration over library-style feature plumbing.

Treat serialized formats as API contracts. Choose field names, enum representation, defaults, and unknown-field behavior deliberately before publishing data that other processes or versions must read.

## Example

Use the new project workflow for initial workspace scaffolding. This page covers how to keep Cargo configuration simple after the project exists.

Library with additive optional integration:

```toml
[package]
name = "example-id"
edition.workspace = true
rust-version.workspace = true

[dependencies]
serde = { workspace = true, optional = true }
thiserror.workspace = true

[features]
serde = ["dep:serde"]
```

```rust
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunId(String);
```

Async application with direct concrete dependencies:

```toml
[package]
name = "example-service"
edition.workspace = true
rust-version.workspace = true

[dependencies]
anyhow.workspace = true
tokio = { version = "1", features = ["full"] }
tracing.workspace = true
```

## Exceptions

- Use a heavier dependency when it is the mature ecosystem standard for the domain.
- Use default features in a library when the crate is intentionally batteries-included and downstream compile impact is acceptable.
- Use exact or pinned dependency versions only when reproducibility, upstream breakage, or security response requires it.
- Split a crate from the workspace only when it has a truly different release, MSRV, or dependency policy.
- Use a documented feature matrix instead of `--all-features` only when a crate intentionally supports mutually incompatible feature sets.
