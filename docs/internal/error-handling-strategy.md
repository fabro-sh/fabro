# Error Handling Strategy

Preserve error structure until the boundary where the error is rendered.

A `String` is a rendered message, not an internal error transport. Inside Rust crates, prefer typed errors, `anyhow::Error`, or small domain error enums. Convert to `String` only at projection boundaries: CLI output, API JSON detail, serialized config, logs/audit text, or an external contract that intentionally requires text.

## Preserve source chains

Prefer:

```rust
.map_err(anyhow::Error::new)?;
.with_context(|| "failed to fetch GitHub metadata")?;
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

HTTP API responses must stay curated. Log the full internal chain, but return only a safe public message. Do not expose `format!("{err:#}")` in API JSON unless the endpoint explicitly chooses that surface.

## Clone-bound storage

Do not fall back to `String` only because `anyhow::Error` is not `Clone`.

Prefer, in order:

1. Remove the clone requirement.
2. Use a small cloneable domain error.
3. Use one shared cloneable error wrapper for arbitrary error chains.
4. Use `String` only when the field is already a rendered projection.

## Tests

When changing error propagation, add a regression test that walks `err.chain()` and proves the underlying cause is still present.
