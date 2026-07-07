# Trait Design

## Rule

Write small, behavior-focused traits; make public traits open only when external implementations are intended, and use sealed traits when the crate must control implementors.

## Why

Traits are extension contracts. Small traits are easier to implement, test, object-check, and evolve. Public traits invite downstream implementations unless sealed, so their required methods and semantics become part of the crate's stable API.

## Do

- Start with concrete types or enums; introduce a trait when code genuinely needs caller-supplied behavior or an open extension point.
- Keep required methods small and cohesive.
- Name traits after behavior or capability, such as `Notifier`, `Store`, or `TokenSource`.
- Put convenience methods on the trait as provided methods when they can be implemented from the required core methods.
- Document public trait contracts: what implementors must guarantee, error behavior, blocking behavior, and whether methods may be called concurrently.
- Use associated types when each implementor chooses a related type.
- Use generic methods when each caller chooses the type for that call.
- Keep bounds close to the function that needs them, preferably in a `where` clause for complex bounds.
- Make traits object-safe when they are intended for `dyn Trait`.
- Add `where Self: Sized` to generic provided methods, such as ones taking `impl Into<String>`, on traits meant for trait objects; without that opt-out, a generic method makes the trait unusable as `dyn Trait`.
- Seal public traits when users should call trait methods but should not implement the trait outside the crate.

## Avoid

- Do not create a trait only to organize methods on one concrete type.
- Do not make broad traits with unrelated capabilities.
- Do not expose public traits by default for every behavior-bearing type.
- Do not add required methods to public traits casually; downstream implementors must update.
- Do not use blanket implementations unless the behavior is obvious and unlikely to block future impls.
- Do not make a trait object API from a trait with non-object-safe required methods.
- Do not encode inheritance hierarchies with supertraits unless each supertrait is a real contract.

## Public API Notes

An unsealed public trait is an open extension point. Treat it as a semver commitment to downstream implementors.

A sealed public trait is still public API for callers, but external crates cannot add implementations. Use it when the crate owns the valid implementor set but trait syntax is useful for bounds or shared behavior.

## Example

```rust
pub trait Notifier {
    fn notify(&self, message: &Message) -> Result<(), NotifyError>;

    fn notify_text(&self, body: impl Into<String>) -> Result<(), NotifyError>
    where
        Self: Sized,
    {
        self.notify(&Message::new(body))
    }
}

pub fn send_welcome<N>(notifier: &N, user: &User) -> Result<(), NotifyError>
where
    N: Notifier,
{
    notifier.notify_text(format!("welcome {}", user.name()))
}

pub trait DeliveryChannel: sealed::Sealed {
    fn name(&self) -> &'static str;
}

pub struct EmailChannel;

impl DeliveryChannel for EmailChannel {
    fn name(&self) -> &'static str {
        "email"
    }
}

mod sealed {
    pub trait Sealed {}
}

impl sealed::Sealed for EmailChannel {}

pub struct Message {
    body: String,
}

impl Message {
    pub fn new(body: impl Into<String>) -> Self {
        Self { body: body.into() }
    }

    pub fn body(&self) -> &str {
        &self.body
    }
}

pub struct User {
    name: String,
}

impl User {
    pub fn name(&self) -> &str {
        &self.name
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NotifyError;
```

## Exceptions

- Use a broader trait when matching a mature ecosystem abstraction that callers already know.
- Use a marker trait only when it carries a real compile-time contract that cannot be expressed more clearly another way.
- Leave a public trait unsealed when downstream crates are expected to provide their own implementations.
- Use concrete types instead of traits when variation is not required.
