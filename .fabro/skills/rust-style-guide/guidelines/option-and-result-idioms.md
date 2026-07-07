# Option and Result Idioms

## Rule

Use simple combinators for short local transformations, and switch to explicit branching when `Option` or `Result` handling carries behavior, side effects, context, or recovery logic.

## Why

`Option` and `Result` make absence and failure visible in the type system. Small combinators keep simple cases compact, but complex chains hide decisions that agents need to see and modify safely.

## Do

- Use `Option` for expected absence and `Result` for failures that need a reason.
- Use `?` to propagate `Result` in fallible functions.
- Use `?` on `Option` only inside functions that return `Option`.
- Convert required `Option` values to `Result` with `ok_or_else` when constructing the error is nontrivial.
- Use `ok_or` for cheap, static, or already-built errors.
- Use `map`, `filter`, and `unwrap_or_else` for short, side-effect-free `Option` transforms.
- Use `map_err` only for local typed error conversion that preserves the source error.
- Use `transpose` for `Option<Result<T, E>>` to produce `Result<Option<T>, E>`.
- Use `let else`, `if let`, or `match` when the missing/error case has branching, logging, metrics, cleanup, retries, or recovery.
- Add error context at boundaries per [error propagation](error-propagation-context-and-messages.md), not on every small combinator.
- Treat Clippy as authoritative for combinator-vs-branching idioms; refactor instead of adding local bypasses ([rustc and Clippy lints](rustc-and-clippy-lints.md)).

## Avoid

- Do not chain combinators until the control flow is harder to read than a `match`.
- Do not hide side effects in `map`, `and_then`, `or_else`, or `inspect`.
- Do not use `.ok()` unless intentionally discarding the error cause at a boundary where absence is the right model.
- Do not use `unwrap_or` when the fallback is expensive or allocates; use `unwrap_or_else`.
- Do not use `unwrap_or_default` when absence is a domain error.
- Do not use `is_some` followed by `unwrap`; use `if let`, `let else`, or `match`.
- Do not replace domain-specific errors with generic missing-value messages.

## Example

Use combinators for local extraction and explicit branching for meaningful decisions:

```rust
pub fn build_request(input: &Input) -> Result<Request, Error> {
    let id = input
        .id()
        .ok_or(Error::MissingField { field: "id" })?;

    let label = input
        .label()
        .filter(|label| !label.trim().is_empty())
        .map(str::to_owned);

    let timeout = match input.timeout_ms() {
        Some(0) => return Err(Error::InvalidTimeout),
        Some(ms) => Timeout::from_millis(ms)?,
        None => Timeout::default(),
    };

    let mode = input
        .mode()
        .map(Mode::parse)
        .transpose()?
        .unwrap_or_else(Mode::default);

    Ok(Request::new(id, label, timeout, mode))
}
```

Prefer explicit handling when the error path has behavior:

```rust
pub fn load_profile(name: Option<&str>, store: &ProfileStore) -> Result<Profile, Error> {
    let Some(name) = name else {
        tracing::debug!("profile omitted; using default profile");
        return store.default_profile().map_err(Error::DefaultProfile);
    };

    store.load(name).map_err(|source| Error::LoadProfile {
        name: name.to_owned(),
        source,
    })
}
```

## Exceptions

- Use a longer combinator chain when every step is a pure transformation and the names remain clear.
- Use `match` for simple cases when exhaustiveness or domain documentation matters.
- Use `.ok()` at external boundaries where a detailed failure intentionally becomes optional data.
