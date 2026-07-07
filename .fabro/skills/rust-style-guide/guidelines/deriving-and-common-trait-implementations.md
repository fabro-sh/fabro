# Deriving and Common Trait Implementations

## Rule

Derive standard traits when their semantics are obvious, hand-write `Display`, and avoid deriving semantics-heavy traits by habit.

## Why

Derived impls are cheap and correct when the type's structure matches the trait semantics. They become misleading when equality, ordering, defaults, debug output, or cloning require domain judgment.

## Do

- Derive `Debug` for ordinary data types.
- Hand-write `Debug` for secret-bearing types or types whose internals should not leak.
- Derive `Clone` when the type has value semantics and clone cost is acceptable.
- Derive `Copy` only for small scalar-like types with no ownership, resource, or surprising duplication behavior.
- Derive `PartialEq` and `Eq` when field-by-field equality is the domain equality.
- Derive `Hash` only when equality and hashing should use the same stable fields.
- Derive `Ord` and `PartialOrd` only when there is one obvious total ordering.
- Keep hand-written `PartialEq`, `Eq`, `Hash`, and `Ord` coherent: `a == b` must imply equal hashes, every impl must use the same fields, and mixing a manual `PartialEq` with a derived `Hash` silently breaks `HashMap` and `HashSet` lookups.
- Derive or implement `Default` only when the default is valid, useful, and unsurprising.
- Hand-write `Display` for stable user-facing text.

## Avoid

- Do not derive traits just to satisfy a test, log statement, or temporary call site.
- Do not derive `Debug` for tokens, credentials, or secret-bearing structs.
- Do not derive `Copy` for types that may grow owned data or represent scarce resources.
- Do not derive `Ord` when ordering is arbitrary or caller-specific.
- Do not derive `Default` when the result would be invalid, empty-but-broken, or environment-dependent.
- Do not use `Display` for programmer diagnostics; use `Debug` for that.
- Do not derive external serialization traits unless the wire format is intentionally part of the type's role.

## Public API Notes

For public libraries, trait impls are part of the API surface. Removing a public impl is breaking, and adding broad impls can affect downstream method resolution or trait coherence. Derive only traits the type is meant to support over time.

## Example

```rust
use std::{fmt, num::NonZeroU64};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct UserId(NonZeroU64);

impl UserId {
    pub fn new(value: NonZeroU64) -> Self {
        Self(value)
    }

    pub fn as_u64(self) -> u64 {
        self.0.get()
    }
}

impl fmt::Display for UserId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RetryMode {
    Disabled,
    #[default]
    Standard,
    Aggressive,
}

#[derive(Clone, Eq, PartialEq)]
pub struct ApiToken(String);

impl ApiToken {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ApiToken(<redacted>)")
    }
}
```

## Exceptions

- Keep impl surface smaller for public types whose long-term semantics are not settled.
- Derive additional traits for test-only helper types when the trait does not leak into production API.
- Hand-write equality, hashing, or ordering when the domain semantics differ from field-by-field behavior.
- Derive `Default` for configuration structs when all field defaults are valid and match the documented behavior.
