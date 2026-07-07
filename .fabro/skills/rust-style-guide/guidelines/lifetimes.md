# Lifetimes

## Rule

Prefer lifetime elision, and reserve explicit lifetimes for APIs where borrowing is the point: views, parsers, iterators, and zero-copy abstractions.

## Why

Explicit lifetimes are valuable for borrowed views into another value, but they add coupling that agents often spread too far through signatures and structs. Most APIs are easier to call and refactor when they own returned data; the borrow/own/clone defaults live on [ownership, borrowing, and clone policy](ownership-borrowing-and-clone-policy.md).

## Do

- Rely on lifetime elision for ordinary `&self`, `&str`, `&[T]`, and `&Path` APIs.
- Use lifetime-bearing structs only for real borrowed views into another value.
- Name lifetimes when an output borrow must clearly be tied to a particular input borrow.
- Use `'_` when the lifetime exists but does not need a name in the local API.
- Use iterator lifetimes such as `impl Iterator<Item = &str> + '_` when returning borrowed iteration is the natural API.
- Keep lifetime parameters local; do not push them through unrelated types.

## Avoid

- Do not use self-referential structs in ordinary code.
- Do not add named lifetimes where elision communicates the relationship.
- Do not make public APIs lifetime-heavy unless borrowing is the point of the abstraction.

## Public API Notes

Published library APIs may use explicit lifetimes when the crate is fundamentally a parser, view, iterator, or zero-copy abstraction. For ordinary libraries and application code, keep lifetime complexity low and prefer owned outputs.

## Example

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Document {
    body: String,
}

impl Document {
    pub fn new(body: &str) -> Self {
        Self {
            body: body.to_owned(),
        }
    }

    pub fn words(&self) -> impl Iterator<Item = &str> + '_ {
        self.body.split_whitespace()
    }

    pub fn first_word(&self) -> Option<Token<'_>> {
        first_token(&self.body)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Token<'a> {
    text: &'a str,
}

impl Token<'_> {
    pub fn as_str(&self) -> &str {
        self.text
    }
}

pub fn first_token(input: &str) -> Option<Token<'_>> {
    input
        .split_whitespace()
        .next()
        .map(|text| Token { text })
}

pub fn owned_tokens(input: &str) -> Vec<String> {
    input.split_whitespace().map(str::to_owned).collect()
}
```

## Exceptions

- Use explicit lifetimes for borrowed views, parsers, iterators, and APIs where zero-copy behavior is the main value.
- Use lifetime-bearing structs for short-lived adapters that cannot outlive their source.
- Accept more lifetime complexity in measured hot paths where allocation cost is known to matter.
