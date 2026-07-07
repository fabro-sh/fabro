# Error Propagation, Context, and Messages

## Rule

Propagate errors with `?`, add context at operation and layer boundaries, keep inner propagation sparse when typed errors already explain the local failure, and never stringify a source error just to add context.

## Why

Good error chains explain both the local cause and the larger operation. Too little context hides what the program was trying to do; context on every fallible line creates noisy, repetitive chains.

## Do

- Use `?` for normal propagation.
- Use `From` or `#[from]` when converting a source error without adding extra fields.
- Use `.context(...)` for static application context.
- Use `.with_context(...)` when the context formats values or clones data.
- Add context at command, request, job, service, task, crate, or layer boundaries.
- Include safe identifiers such as paths, IDs, operation names, and remote resource names when they help diagnose the failure.
- Preserve source chains with `#[source]`, `#[from]`, `anyhow::Context`, or explicit source fields.
- Write context messages as concise operation descriptions, such as `failed to load configuration`.
- Keep typed error `Display` messages specific to the variant's local failure.
- Walk the source chain explicitly when rendering typed errors at a boundary that should show causes.

## Avoid

- Do not add context to every `?` by habit.
- Do not add context that only restates the lower-level error.
- Do not write `.map_err(|err| err.to_string())`.
- Do not write `.map_err(|err| anyhow::anyhow!("{err}"))`.
- Do not interpolate the source error into a new context string.
- Do not turn internal propagation messages into final user-facing copy.
- Do not put secrets, credentials, raw tokens, or unredacted request bodies in error messages.

## Library vs Application

Libraries should prefer typed errors whose variants describe local failures and preserve sources. Applications should add `anyhow` context at meaningful operation boundaries and let the final CLI, API, worker, or log boundary decide how much of the chain to render.

## Example

Library code describes local failures:

```rust
use std::{
    io,
    path::{Path, PathBuf},
};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("reading configuration file {path}")]
    Read {
        path: PathBuf,

        #[source]
        source: io::Error,
    },

    #[error("parsing configuration file {path}")]
    Parse {
        path: PathBuf,

        #[source]
        source: toml::de::Error,
    },
}

pub fn load_config(path: &Path) -> Result<Config, ConfigError> {
    let contents = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.to_path_buf(),
        source,
    })?;

    toml::from_str(&contents).map_err(|source| ConfigError::Parse {
        path: path.to_path_buf(),
        source,
    })
}
```

Good: application code adds boundary context and preserves the source:

```rust
use std::path::PathBuf;

use anyhow::{Context, Result};

fn run() -> Result<()> {
    let path = PathBuf::from("config.toml");
    let config = config_lib::load_config(&path)
        .with_context(|| format!("failed to load configuration from {}", path.display()))?;

    start_server(config).context("failed to start server")?;
    Ok(())
}
```

At the outermost boundary, render an `anyhow` chain with the alternate format (`{err:#}`) or by returning `Result` from `main`; `Display` on `anyhow::Error` prints only the outermost context.

```rust
#[expect(clippy::print_stderr, reason = "top-level CLI error report")]
fn report_error(error: &anyhow::Error) {
    eprintln!("error: {error:#}");
}
```

Bad: flatten the source into text and lose the chain:

```rust
let config = config_lib::load_config(&path)
    .map_err(|err| anyhow::anyhow!("failed to load config: {err}"))?;
```

## Exceptions

- Add context close to a fallible call when there is no meaningful higher boundary that can explain the operation.
- Add more context in quick scripts when it improves debugging and does not create repetitive chains.
- Keep propagation minimal in very small typed libraries where variants and sources already make the operation obvious.
