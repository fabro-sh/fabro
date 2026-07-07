# Newtype Pattern and Semantic Wrappers

## Rule

Use newtypes for IDs, units, validated values, and public API meaning; avoid wrapping primitives when the wrapper adds no useful type safety or behavior.

## Why

Newtypes make invalid argument swaps harder, keep validation attached to the value, and give public APIs domain names without committing callers to raw primitive meaning.

## Do

- Use tuple structs for small semantic wrappers around primitives.
- Keep newtype fields private when the type has meaning, validation, or future API concerns.
- Use `new` for infallible wrappers and `try_new` for validated wrappers, following [constructors and builders](constructors-and-builders.md).
- Expose focused accessors such as `as_str`, `as_u64`, or `into_inner`.
- Derive standard traits when semantics are obvious: `Debug`, `Clone`, `Copy`, `Eq`, `PartialEq`, `Hash`, `PartialOrd`, `Ord` (`Ord` always requires `PartialOrd`).
- Implement `Display` when the wrapper has a stable user-facing representation.
- Use `From` only for conversions that cannot fail or violate invariants.
- Use `TryFrom` or `FromStr` for validated conversions.
- Use `#[repr(transparent)]` only when layout guarantees matter, such as FFI or carefully documented ABI boundaries.

## Avoid

- Do not wrap every primitive by default.
- Do not expose the inner value as a public field for invariant-bearing wrappers.
- Do not implement `Deref` to `str`, `String`, `Vec`, or other primitives just to inherit methods.
- Do not add `From` implementations that skip validation.
- Do not use vague wrapper names like `Value`, `Key`, or `Id` outside a narrow module where the domain is obvious.
- Do not create a newtype if a plain private field inside a behavior-bearing struct communicates the invariant better.

## Public API Notes

Public library APIs should use newtypes more readily than application internals when primitive arguments can be confused or have domain meaning. A `UserId` parameter is harder to misuse than a `u64`, and it gives the library room to change representation later.

For application internals, prefer newtypes at boundaries, identifiers, units, and validated inputs. Do not add wrappers that only create conversion noise inside one small module.

## Example

```rust
use std::fmt;
use std::str::FromStr;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct UserId(u64);

impl UserId {
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    pub fn as_u64(self) -> u64 {
        self.0
    }
}

impl fmt::Display for UserId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmailAddress(String);

impl EmailAddress {
    pub fn try_new(value: impl Into<String>) -> Result<Self, EmailAddressError> {
        let value = value.into();

        if !value.contains('@') {
            return Err(EmailAddressError::MissingAt);
        }

        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Display for EmailAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for EmailAddress {
    type Err = EmailAddressError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_new(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EmailAddressError {
    MissingAt,
}
```

## Exceptions

- Use a public tuple field for intentionally transparent wrappers with no invariant and no expected evolution pressure.
- Use `Deref` for pointer-like wrappers where dereference behavior is the core abstraction, not for ordinary semantic wrappers.
- Use a plain primitive when the value is local, obvious, and not crossing an API boundary.
- Use a domain struct instead of multiple newtypes when the invariant belongs to a combined value.
