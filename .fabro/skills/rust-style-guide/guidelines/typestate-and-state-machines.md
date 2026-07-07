# Typestate and State Machines

## Rule

Use typestate broadly for workflows with ordered states; use runtime enums when state is dynamic, persisted, or naturally handled by exhaustive matching.

## Why

Typestate makes invalid transitions fail to compile. It is a good fit for workflows where values move through known phases and later operations require earlier steps to have happened.

## Activation

Load this page when a value moves through ordered phases such as draft-to-published or connected-to-authenticated, or when choosing between compile-time states and runtime state enums. Skip it for ordinary optional configuration, which uses plain constructors and builders.

## Do

- Use typestate for ordered workflows such as draft-to-published, configured-to-started, connected-to-authenticated, or parsed-to-validated.
- Model each compile-time state with a small marker type.
- Store shared data in one generic struct like `Workflow<State>`.
- Put transition methods on the source state and return the destination state.
- Put state-independent accessors on `impl<State>`.
- Use `PhantomData<State>` when the state type is only a compile-time marker.
- Keep transition methods consuming when the old state should no longer be usable.
- Use runtime enums when state is read from a database, received over the network, chosen by users, or stored in a mixed collection.
- Keep ordinary optional-configuration builders simple unless the builder enforces important ordered steps.

## Avoid

- Do not use typestate for states that are only labels in a UI or report.
- Do not use typestate when every call site immediately erases the state into `dyn Trait` or an enum.
- Do not create many marker types for a workflow with unclear or frequently changing states.
- Do not encode runtime data as type parameters.
- Do not force typestate through async task boundaries, persistence layers, or message queues when runtime state is clearer.
- Do not use typestate to hide validation that still must happen at external boundaries.

## Public API Notes

Typestate-heavy public APIs expose type-level workflow structure to callers. Use clear state names and transition method names, and keep generic state parameters out of unrelated APIs.

When a public library must evolve states over time, consider a runtime enum or a sealed state marker pattern so the crate can add states without forcing callers to name every marker type.

## Example

```rust
use std::marker::PhantomData;

#[derive(Clone, Debug)]
pub struct Draft;

#[derive(Clone, Debug)]
pub struct Reviewed;

#[derive(Clone, Debug)]
pub struct Published;

#[derive(Clone, Debug)]
pub struct Article<State> {
    title:  String,
    body:   String,
    marker: PhantomData<State>,
}

impl Article<Draft> {
    pub fn new(title: &str, body: &str) -> Self {
        Self {
            title:  title.to_owned(),
            body:   body.to_owned(),
            marker: PhantomData,
        }
    }

    pub fn revise(&mut self, body: &str) {
        self.body = body.to_owned();
    }

    pub fn submit(self) -> Article<Reviewed> {
        Article {
            title:  self.title,
            body:   self.body,
            marker: PhantomData,
        }
    }
}

impl Article<Reviewed> {
    pub fn reject(self) -> Article<Draft> {
        Article {
            title:  self.title,
            body:   self.body,
            marker: PhantomData,
        }
    }

    pub fn publish(self) -> Article<Published> {
        Article {
            title:  self.title,
            body:   self.body,
            marker: PhantomData,
        }
    }
}

impl Article<Published> {
    pub fn public_body(&self) -> &str {
        &self.body
    }
}

impl<State> Article<State> {
    pub fn title(&self) -> &str {
        &self.title
    }
}
```

## Exceptions

- Use data-bearing enums when all states must be stored together, matched exhaustively, serialized, or loaded dynamically.
- Use runtime validation for inputs from outside the process even when the internal workflow uses typestate.
- Use a simpler builder when typestate would only enforce optional configuration order.
- Use a plain struct with validation when the workflow has only one meaningful transition.
