# Error Taxonomy and Layer Boundaries

## Rule

Use layered, branch-oriented errors: model domain failures where callers branch, convert infrastructure errors at boundaries, preserve source chains and data, and render errors to strings only at external boundaries.

## Why

Error values are structured control-flow and diagnostics. Turning errors into strings inside Rust code drops type information, source chains, and useful fields before the right boundary can decide how to log, display, redact, or recover.

## Do

- Create typed domain variants for failures callers can act on, such as not found, duplicate, forbidden, invalid state, or validation failure.
- Keep infrastructure causes as error sources with `#[source]` or `#[from]` when using `thiserror`.
- Keep useful fields on error variants, such as IDs, paths, states, retry hints, and safe context values.
- Convert lower-layer errors into the current layer's error type at crate, domain, service, command, or API boundaries.
- Add context at layer crossings so operators can tell which operation failed.
- Preserve `source()` chains until a rendering boundary; [error propagation](error-propagation-context-and-messages.md) owns how to render the chain.
- Render to `String` only for CLI output, API response details, logs, telemetry, serialized files, or external contracts that require text.
- For public API responses, log the internal chain but return a curated safe message.

## Avoid

- Do not add enum variants for every low-level failure unless callers branch on them.
- Do not expose database, HTTP, SDK, or parser errors from a public domain API unless that dependency is intentionally part of the contract.
- Do not transport internal errors as `String`, `Message(String)`, or `Other(String)` just because the real error type is inconvenient.
- Do not stringify errors during propagation; the `.map_err(to_string)` and `anyhow!("{err}")` bans live on [error propagation](error-propagation-context-and-messages.md).
- Do not include secrets, tokens, raw URLs with credentials, or unredacted request bodies in error fields or display messages.

## Library vs Application

Reusable libraries should expose typed errors for their public boundary and keep implementation details behind variants or sources. Internal application code may use `anyhow`, but it should keep typed domain errors where code needs to branch and should not stringify errors before the final rendering boundary.

## Example

```rust
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum LoadProfileError {
    #[error("profile {id} was not found")]
    NotFound { id: ProfileId },

    #[error("reading profile file {path}")]
    Read {
        path: PathBuf,

        #[source]
        source: std::io::Error,
    },

    #[error("parsing profile file {path}")]
    Parse {
        path: PathBuf,

        #[source]
        source: toml::de::Error,
    },
}

pub fn load_profile(id: ProfileId) -> Result<Profile, LoadProfileError> {
    let path = profile_path(id);
    let contents = std::fs::read_to_string(&path)
        .map_err(|source| LoadProfileError::Read {
            path: path.clone(),
            source,
        })?;

    toml::from_str(&contents).map_err(|source| LoadProfileError::Parse { path, source })
}
```

At the boundary, render or serialize deliberately:

```rust
fn to_api_error(err: LoadProfileError) -> ApiError {
    match &err {
        LoadProfileError::NotFound { id } => {
            tracing::warn!(error = ?err, "profile not found");
            ApiError::not_found(format!("profile {id} not found"))
        }
        _ => {
            tracing::error!(error = ?err, "failed to load profile");
            ApiError::internal("failed to load profile")
        }
    }
}
```

## Exceptions

- Use a coarse error variant when callers cannot make a different decision and the source chain carries the detail.
- Use text-only errors at external boundaries that are already rendered projections.
- Use cloneable domain errors or a shared error wrapper before falling back to `String` for clone-bound storage.
