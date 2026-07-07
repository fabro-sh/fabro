# Modules, Visibility, and Re-exports

## Rule

Keep modules and fields private by default, expose focused public facades, give each local item one intended public path, and avoid broad preludes unless the crate is a broad ecosystem crate.

## Why

Rust visibility is an API design tool. Smaller public surfaces make invariants easier to protect and let crates reorganize internals without breaking callers.

## Do

- Make modules private unless callers need the module path as part of the API.
- Keep struct fields private by default; [struct design](struct-design-and-encapsulation.md) owns the public-fields-for-plain-data exception.
- Use `pub(crate)` for real internal boundaries across modules.
- Use `pub(super)` only for tight parent-child module collaboration.
- Re-export the public types callers should name from the crate root or a focused facade module.
- Choose one canonical public path for each local item: either a facade re-export or a public module path.
- Use `#[doc(inline)]` when re-exporting from a public module or another crate so rustdoc presents the item at the facade path; re-exports from private modules are inlined automatically.
- Keep internal helper modules behind `mod`, not `pub mod`.

## Avoid

- Do not expose deep module paths by accident.
- Do not use `pub` when `pub(crate)` is enough.
- Do not create a prelude for a small crate.
- Do not re-export every internal type from the crate root.
- Do not expose the same local type through both a deep public module and a facade path by accident.
- Do not make module layout mirror implementation churn in the public API.

## Public API Notes

For libraries, every `pub` item is part of the compatibility contract unless hidden behind documented instability. Prefer a small public facade that names the crate's main concepts and hides helper modules.

When a facade is the intended public API, keep implementation modules private and re-export the public item from the facade. If a deep module is itself a stable namespace, expose the module and avoid also re-exporting the same local item from the root unless the duplicate path is an intentional compatibility or ergonomics choice.

For applications, `pub(crate)` is often enough for cross-module use. Avoid public exports from binary crates unless integration tests or generated code require them.

## Example

```rust
// lib.rs
mod client;
mod error;
mod request;
mod response;

pub use client::Client;
pub use error::ClientError;
pub use request::Request;
pub use response::Response;
```

```rust
// client.rs
mod retry;
mod transport;

use url::Url;

use crate::{ClientError, Request, Response};

pub struct Client {
    transport: transport::Transport,
}

impl Client {
    pub fn new(base_url: Url) -> Self {
        Self {
            transport: transport::Transport::new(base_url),
        }
    }

    pub async fn send(&self, request: Request) -> Result<Response, ClientError> {
        retry::with_retry(|| self.transport.send(&request)).await
    }
}
```

```rust
// request.rs
pub struct Request {
    path: String,
}

impl Request {
    pub fn new(path: impl Into<String>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &str {
        &self.path
    }
}
```

## Exceptions

- Use `pub mod` when the module itself is a stable namespace callers should browse or import from.
- Add a `prelude` only when the crate has many commonly paired traits and types and users benefit from one import.
