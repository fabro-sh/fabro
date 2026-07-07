# Enums vs Traits vs Generics vs Trait Objects

## Rule

Use enums for closed sets, traits for open extension points, generics for static dispatch, and `dyn Trait` for runtime heterogeneity.

## Why

These choices encode different extension models. Enums make known variants explicit and exhaustively checked. Traits allow new implementors. Generics keep dispatch static when one implementor type flows through a call. Trait objects trade static dispatch for runtime selection and mixed collections.

## Do

- Use an enum when all variants are known to this crate or module.
- Put behavior directly on a closed enum when callers should not add new variants.
- Use a trait when downstream code or another layer should be able to provide new behavior.
- Use `impl Trait` or `T: Trait` when a function accepts one concrete implementor type at a time.
- Use `&dyn Trait`, `Box<dyn Trait>`, or `Arc<dyn Trait>` for plugin lists, runtime selection, or heterogeneous collections.
- Keep object-safety in mind when a trait is meant to be used as `dyn Trait`.
- Prefer returning concrete types or `impl Trait` unless callers need runtime polymorphism.

## Avoid

- Do not create a trait just because several closed enum variants share method names.
- Do not use a growing enum when external users are expected to add variants.
- Do not spread generic type parameters through many layers when a trait object would localize the choice.
- Do not use `dyn Trait` just to avoid writing a generic parameter.
- Do not make a public trait object API from a trait that is not object-safe.

## Public API Notes

For public libraries, choosing an enum means the crate controls the set of variants. Adding a variant can require downstream match updates unless the enum is marked `#[non_exhaustive]`.

Choosing a public trait means outside crates may implement it. Adding required methods later is usually a breaking change, so keep public traits small and intentional.

## Example

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeliveryTarget {
    Email(EmailAddress),
    Webhook(WebhookUrl),
}

impl DeliveryTarget {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Email(_) => "email",
            Self::Webhook(_) => "webhook",
        }
    }
}

pub trait Notifier {
    fn notify(&self, message: &Message) -> Result<(), NotifyError>;
}

pub fn notify_once<N>(notifier: &N, message: &Message) -> Result<(), NotifyError>
where
    N: Notifier,
{
    notifier.notify(message)
}

pub struct Broadcast {
    notifiers: Vec<Box<dyn Notifier>>,
}

impl Broadcast {
    pub fn new(notifiers: Vec<Box<dyn Notifier>>) -> Self {
        Self { notifiers }
    }

    pub fn notify_all(&self, message: &Message) -> Result<(), NotifyError> {
        for notifier in &self.notifiers {
            notifier.notify(message)?;
        }

        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmailAddress(String);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WebhookUrl(String);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Message(String);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NotifyError;
```

## Exceptions

- Use a trait for a small closed set when the behavior must be supplied by generic infrastructure that already expects a trait.
- Use an enum wrapper around trait objects when the public API needs a closed high-level category but each category uses runtime dispatch internally.
- Use `dyn Trait` in application code when runtime configuration matters more than static dispatch.
- Use generics in public APIs only when the caller benefits from type flexibility and the extra type parameter does not leak complexity.
