# Documentation and Rustdoc Examples

## Rule

Document non-obvious public API behavior; when the project intentionally maintains rustdoc examples, write them as fallible snippets that use `?` instead of `unwrap`.

## Why

Rustdoc should explain intent, contracts, and caveats that names and types cannot express. Over-documenting obvious items adds noise, while maintained examples that panic teach careless error handling.

## Do

- Add rustdoc when a public item has non-obvious behavior, invariants, caveats, side effects, or examples.
- Use module docs (`//!`) for modules that define an important concept or public surface.
- Use item docs (`///`) for public types, traits, functions, and methods whose contract is not obvious.
- Include `# Errors` when a public `Result` function has caller-relevant failure modes.
- Include `# Panics` when a public function can panic.
- Include `# Safety` for every `unsafe` function or unsafe trait.
- Add rustdoc examples only when they materially clarify public API use and the project has opted into maintaining them.
- When rustdoc examples are used, prefer snippets that compile and use `?`.
- Hide boilerplate with `#` lines when it distracts from the example.

## Avoid

- Do not require `#![deny(missing_docs)]` as house style.
- Do not restate the name in prose.
- Do not document private helpers unless the explanation prevents mistakes.
- Do not use doctests as default test coverage.
- Do not use bare `unwrap` in public rustdoc examples.
- Do not include long examples that become harder to maintain than the API.
- Do not mark examples `ignore` just to avoid maintaining them; move behavior coverage to normal tests instead.

## Public API Notes

For reusable libraries, prioritize docs on public concepts, constructors, fallible operations, trait contracts, and behavior that affects callers. Internal application crates may keep docs sparse unless the module is a shared boundary or the behavior is easy to misuse.

## Example

```rust
use std::path::Path;

/// Loads application configuration from a TOML file.
///
/// Environment-specific overrides are applied after the file is parsed.
///
/// # Errors
///
/// Returns an error if the file cannot be read, the TOML is invalid, or a
/// required setting is missing.
///
/// # Examples
///
/// ```rust,no_run
/// # use example_config::Config;
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let config = Config::load("app.toml")?;
/// assert_eq!(config.profile(), "default");
/// # Ok(())
/// # }
/// ```
pub fn load(path: impl AsRef<Path>) -> Result<Config, ConfigError> {
    todo!()
}
```

Use `expect` only for setup invariants that are part of the example:

```rust
/// ```
/// # use example_config::Config;
/// let config = Config::from_static(include_str!("../../fixtures/app.toml"))
///     .expect("fixture app.toml should be valid");
/// assert_eq!(config.profile(), "default");
/// ```
```

## Exceptions

- Use `no_run` for examples that should compile but would start servers, make network calls, or read or mutate real state.
- Use `ignore` only when an example cannot be made portable.
- Use `expect` in examples for fixed fixtures or impossible setup failures when a fallible `main` would obscure the API being shown.
