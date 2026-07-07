# House Style and Rust Philosophy

## Rule

Write idiomatic Rust with an OO-leaning default: model domain concepts as structs with methods and encapsulated invariants, compose behavior explicitly, and choose loops or iterator chains by clarity.

## Why

Rust supports data with behavior without inheritance. Clear types, ownership, and explicit composition give agents useful structure without forcing object-oriented patterns that do not fit Rust.

## Do

- Start with domain types instead of primitive-heavy APIs when the value has meaning.
- Put behavior on the type that owns the data or invariant.
- Keep fields private unless the type is plain data with no invariants.
- Prefer direct composition with explicit fields and methods.
- Use small, behavior-focused traits for open extension points.
- Use iterator chains for simple transformations and loops for branching, mutation, early exits, or multi-step logic; see [iterators, closures, and loops](iterators-closures-and-loops.md).
- Keep parsing, normalization, validation, and command behavior on the domain type that owns the data when there is a natural receiver.

## Avoid

- Do not emulate inheritance hierarchies with traits, enums, or nested structs.
- Do not split all behavior into stateless helper functions when methods would make ownership and invariants clearer.
- Do not expose free functions as public API merely to make tests reach private behavior.
- Do not create pass-through wrapper types whose main job is forwarding.
- Do not add delegation crates or macros to hide a confused boundary.
- Do not choose pattern names over Rust's simpler type, module, and ownership tools.

## Example

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Money {
    cents: u64,
}

impl Money {
    pub const ZERO: Self = Self { cents: 0 };

    pub fn checked_add(self, other: Self) -> Option<Self> {
        self.cents
            .checked_add(other.cents)
            .map(|cents| Self { cents })
    }
}

pub struct CartItem {
    price: Money,
    requires_shipping: bool,
}

impl CartItem {
    pub fn new(price: Money, requires_shipping: bool) -> Self {
        Self {
            price,
            requires_shipping,
        }
    }

    pub fn price(&self) -> Money {
        self.price
    }

    pub fn requires_shipping(&self) -> bool {
        self.requires_shipping
    }
}

pub struct Cart {
    items: Vec<CartItem>,
}

impl Cart {
    pub fn add_item(&mut self, item: CartItem) {
        self.items.push(item);
    }

    pub fn total(&self) -> Option<Money> {
        let mut total = Money::ZERO;

        for item in &self.items {
            total = total.checked_add(item.price())?;
        }

        Some(total)
    }

    pub fn shippable_items(&self) -> impl Iterator<Item = &CartItem> {
        self.items.iter().filter(|item| item.requires_shipping())
    }
}
```

## Exceptions

- Use free functions for pure algorithms or cross-type operations with no natural receiver; if a helper must be public, first ask whether it should be a method or a value type.
- Use plain data structs with public fields when the fields are the API and there are no invariants to protect.
- Prefer a functional pipeline over methods when a transformation chain is genuinely clearer than stateful updates.
- Introduce a trait before a second implementation exists only when callers need substitution or a testing seam now.
