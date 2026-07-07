# Conversions, Getters, and Method Naming

## Rule

Use `From` only for infallible conversions and `TryFrom` or `FromStr` for validated ones, and follow Rust naming so method names carry ownership expectations: `as_` borrows, `to_` allocates, `into_` consumes, and accessors use bare field names.

## Why

Rust method names carry ownership and allocation expectations, and conversion trait impls become part of the public API. Consistent names and honest conversions let callers reason about cost and failure without reading function bodies.

Parameter and return ownership defaults live on [ownership, borrowing, and clone policy](ownership-borrowing-and-clone-policy.md).

## Do

- Use `From` for infallible, obvious conversions.
- Use `TryFrom` or `FromStr` for validation and fallible parsing.
- Use `From` for lossless numeric widening and `TryFrom` or `TryInto` for narrowing or signedness changes.
- Choose explicit integer overflow behavior with `checked_*`, `saturating_*`, `wrapping_*`, or `overflowing_*` when overflow is possible and meaningful.
- Use `as_*` for cheap borrowed or scalar views.
- Use `to_*` for cloning, allocation, or conversion without consuming `self`.
- Use `into_*` for consuming conversions.
- Use Rust-style accessors such as `id()`, `name()`, and `status()` instead of `get_id()`; borrow unless returning a small `Copy` value.
- Use predicate names for booleans: `is_active()`, `has_children()`, `can_retry()`.

## Avoid

- Do not use `From` for conversions that can fail, validate, allocate surprisingly, or lose important meaning.
- Do not use `as` for narrowing numeric casts or float-to-integer conversion unless range, sign, and NaN behavior are checked locally.
- Do not use `as_*` for methods that allocate or clone.
- Do not use `==` for approximate float equality; use a named tolerance, and use `total_cmp` when sorting floats that may include NaN.
- Do not use `get_*` for simple field-like accessors.
- Do not generate accessors for every private field by habit.
- Do not implement `Deref` just to forward methods from an inner value.

## Public API Notes

Trait impls such as `From`, `TryFrom`, `AsRef`, and `Deref` become part of the public API. Add them only when the conversion semantics are stable.

## Example

```rust
use std::fmt;
use std::str::FromStr;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectName(String);

impl ProjectName {
    pub fn try_new(value: &str) -> Result<Self, ProjectNameError> {
        let value = value.trim();

        if value.is_empty() {
            return Err(ProjectNameError::Empty);
        }

        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn to_slug(&self) -> String {
        self.0.to_ascii_lowercase().replace(' ', "-")
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for ProjectName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ProjectName {
    type Err = ProjectNameError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_new(value)
    }
}

impl From<ProjectName> for String {
    fn from(name: ProjectName) -> Self {
        name.into_string()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectNameError {
    Empty,
}
```

## Exceptions

- Use `get_*` for keyed lookups, cache retrieval, or fallible or computed access where the method is not simple field-like observation.
- Return owned snapshots from methods whose names signal ownership, such as `snapshot`, `to_*`, or `*_snapshot`.
