# Error Handling Strategy

Preserve error structure until the boundary where the error is rendered.

A `String` is a rendered message, not an internal error transport. Inside Rust crates, prefer typed errors, `anyhow::Error`, or small domain error enums. Convert to `String` only at projection boundaries: CLI output, API JSON detail, serialized config, logs/audit text, or an external contract that intentionally requires text.

## Preserve source chains

Prefer:

```rust
.map_err(anyhow::Error::new)?;
.context("failed to fetch GitHub metadata")?;
.with_context(|| format!("failed to fetch GitHub metadata for {repo}"))?;
?;
```

Avoid for real errors:

```rust
.map_err(|err| anyhow!("{err}"))?;
.map_err(|err| err.to_string())?;
```

When adding context, use `context` / `with_context`; do not interpolate the source error into a new string. Interpolation turns a structured error into text and drops its `source()` chain.

## Public vs internal surfaces

CLI/miette output, logs, and telemetry may render the full cause chain.

HTTP API responses must stay curated. Log the full internal chain, but return only a safe public message. Do not expose `format!("{err:#}")` in API JSON.

### Rendering the chain

Anyhow walks the chain in `format!("{err:#}")`. `thiserror`-derived `Display` impls do **not** — they only render the variant's `#[error("...")]` template, even with `:#`. Rendering a typed error directly silently drops every `#[source]`.

Use `fabro_util::error::collect_chain` at any boundary that should preserve the chain:

```rust
let rendered = fabro_util::error::collect_chain(&err).join(": ");
```

When migrating from `anyhow::Result<T>` to a typed `Result<T, E>`, audit every formatter on the call path. `format!("{err}")` and `format!("{err:#}")` look identical but change behavior at the boundary — anyhow walks the chain, a typed error does not.

## Clone-bound storage

Do not fall back to `String` only because `anyhow::Error` is not `Clone`.

Prefer, in order:

1. Remove the clone requirement.
2. Use a small cloneable domain error.
3. Use one shared cloneable error wrapper for arbitrary error chains.
4. Use `String` only when the field is already a rendered projection.

## Tests

When changing error propagation, add a regression test that walks the source chain and proves the underlying cause is still present. For `anyhow::Error`, iterate `err.chain()`. For typed errors, recurse `err.source()` or assert on `collect_chain(&err)`.
