# Struct Design and Encapsulation

## Rule

Model meaningful concepts as structs with private fields and behavior-bearing methods; use public fields only for plain data with no invariants.

## Why

Rust structs can protect invariants without inheritance. Private fields let a type control construction and mutation, while methods make ownership and behavior explicit.

## Do

- Give a struct private fields when it has invariants, validation, or behavior.
- Put behavior on the type that owns the data it needs.
- Use `&self` for observation, `&mut self` for in-place mutation, and `self` for consuming transitions.
- Expose only the read accessors callers need.
- Use `pub(crate)` fields or methods only for real internal module boundaries.
- Use public fields for DTOs, config structs, snapshots, and other plain data.
- Keep structs focused enough that their invariants fit in one mental model.

## Avoid

- Do not make fields public just to avoid writing constructors or accessors.
- Do not create method-heavy wrappers around data they do not own.
- Do not split normal type behavior into unrelated helper modules when methods would be clearer.
- Do not generate getters and setters for every field by habit.
- Do not expose test-only mutation paths from production APIs.

## Public API Notes

For public libraries, public fields are hard to evolve because callers can construct and destructure them directly. Prefer private fields unless the type is intentionally plain data.

For application internals, private fields are still the default, but `pub(crate)` can be pragmatic when a module boundary is real and narrower APIs would add noise.

## Example

`EmailAddress` is a validated newtype; its constructor and validation live on the [newtype pattern](newtype-pattern-and-semantic-wrappers.md) page.

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmailAddress(String);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UserId(u64);

pub struct UserAccount {
    id:     UserId,
    email:  EmailAddress,
    active: bool,
}

impl UserAccount {
    pub fn id(&self) -> UserId {
        self.id
    }

    pub fn email(&self) -> &EmailAddress {
        &self.email
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn deactivate(&mut self) {
        self.active = false;
    }
}

#[derive(Clone, Debug)]
pub struct UserSummary {
    pub id:     UserId,
    pub email:  EmailAddress,
    pub active: bool,
}
```

## Exceptions

- Use public fields for plain data structures whose fields are the intended API.
- Use tuple structs for small newtypes when the inner value has no invariant or when a public wrapper is intentional.
- Use free functions for algorithms that do not belong to one owner type.
