# Naming, Imports, and Prelude Policy

## Rule

Use idiomatic Rust names, explicit module-level imports grouped by rustfmt, selective Rust-style accessors, and no broad prelude by default.

## Why

Consistent names and imports make code easier for agents to scan and modify. Rust-style accessors and focused imports keep APIs explicit without falling back to Java-style getters or hidden prelude-heavy dependencies.

## Do

- Use Rust-style acronym casing: `HttpClient`, `UrlParser`, `JsonBody`, `ApiToken`.
- Use `SCREAMING_SNAKE_CASE` for constants and statics.
- Use explicit module-level imports.
- Let rustfmt group imports with `group_imports = "StdExternalCrate"` and `imports_granularity = "Module"`.
- Prefer `as _` imports for extension traits used only for methods.
- Name accessors and conversions per [conversions, getters, and method naming](conversions-getters-and-method-naming.md): `id()` not `get_id()`, predicates like `is_active()`.

## Avoid

- Do not write all-caps acronyms inside type names like `HTTPClient` or `URLParser`.
- Do not use broad glob imports in production modules.
- Do not rely on a broad crate prelude for ordinary application or library code.

## Example

```rust
use std::path::Path;

use anyhow::{Context as _, Result};

use crate::{Config, EmailAddress, RunId, RunStatus, Timestamp, UserId};

pub struct User {
    id:     UserId,
    email:  EmailAddress,
    active: bool,
}

impl User {
    pub fn id(&self) -> UserId {
        self.id
    }

    pub fn email(&self) -> &EmailAddress {
        &self.email
    }

    pub fn is_active(&self) -> bool {
        self.active
    }
}

#[derive(Clone, Debug)]
pub struct RunSummary {
    pub id:          RunId,
    pub status:      RunStatus,
    pub started_at:  Timestamp,
    pub finished_at: Option<Timestamp>,
}

pub fn load_config(path: &Path) -> Result<Config> {
    Config::load(path).context("loading config")
}
```

## Exceptions

- Use wildcard imports in tests, test support, or third-party prelude APIs when they improve test readability.
- Add a crate `prelude` only for broad ecosystem crates where users commonly need many traits and types together.
- Preserve conventional uppercase names required by external protocols, generated code, or wire formats.
