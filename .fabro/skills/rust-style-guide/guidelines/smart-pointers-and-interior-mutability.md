# Smart Pointers and Interior Mutability

## Rule

Prefer ordinary ownership first; use `Box` for single-owner heap allocation, `Rc` and `RefCell` only for single-threaded sharing and interior mutation, and `OnceLock` or `LazyLock` for one-time initialization.

## Why

Rust's ownership model is usually the simplest mutation model. Smart pointers and interior mutability are useful when ownership really is shared or mutation must happen through a shared handle, but they add coordination costs and failure modes.

Cross-thread and cross-task sharing (`Arc`, locks, channels) is chosen on [concurrency primitives](concurrency-primitives.md).

## Do

- Use owned values and borrowing before introducing smart pointers.
- Use `Box<T>` for recursive data, large enum variants, or single-owner heap allocation.
- Use `Box<dyn Trait>` for owned dynamic dispatch when one owner is enough.
- Use `Rc<T>` only for single-threaded shared ownership.
- Use `Weak` (`std::rc::Weak` or `std::sync::Weak`) to break parent-child or observer cycles.
- Use `OnceLock` or `LazyLock` for one-time initialization.

## Avoid

- Do not use `Rc` or `RefCell` in multi-threaded code.
- Do not create `Rc` or `Arc` cycles; two strong references pointing at each other are never freed and leak the whole graph.
- Do not use `RefCell` when a normal `&mut self` API would work.
- Do not create global mutable state unless initialization and access rules are clear.

## Pointer and Thread-Safety Table

| Need | Prefer | Thread-safe use |
| --- | --- | --- |
| Single owner, heap allocation | `Box<T>` | Movable across threads when `T: Send` |
| Single-thread shared ownership | `Rc<T>` | No; use only on one thread |
| Single-thread interior mutation | `Cell<T>` or `RefCell<T>` | No; use only on one thread |
| One-time initialization | `OnceLock<T>` or `LazyLock<T>` | Yes when the initialized value is thread-safe |
| Cross-thread shared ownership | `Arc<T>` | See [concurrency primitives](concurrency-primitives.md) |
| Shared mutable state | `Mutex<T>` or `RwLock<T>` | See [concurrency primitives](concurrency-primitives.md) |
| Ownership transfer | Channel | See [concurrency primitives](concurrency-primitives.md) |

## Example

`Box` for recursion, `Weak` to break the parent-child cycle, and `OnceLock` for one-time initialization:

```rust
use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::sync::OnceLock;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Expr {
    Literal(i64),
    Add(Box<Expr>, Box<Expr>),
}

impl Expr {
    pub fn evaluate(&self) -> i64 {
        match self {
            Self::Literal(value) => *value,
            Self::Add(left, right) => left.evaluate() + right.evaluate(),
        }
    }
}

pub struct Node {
    parent:   RefCell<Weak<Node>>,
    children: RefCell<Vec<Rc<Node>>>,
}

impl Node {
    pub fn new() -> Rc<Self> {
        Rc::new(Self {
            parent:   RefCell::new(Weak::new()),
            children: RefCell::new(Vec::new()),
        })
    }

    pub fn add_child(parent: &Rc<Self>, child: Rc<Self>) {
        *child.parent.borrow_mut() = Rc::downgrade(parent);
        parent.children.borrow_mut().push(child);
    }
}

static DEFAULT_LOCALE: OnceLock<String> = OnceLock::new();

pub fn default_locale() -> &'static str {
    DEFAULT_LOCALE.get_or_init(|| "en-US".to_owned())
}
```

## Exceptions

- Use `Cell` or `RefCell` for narrow single-threaded caches, adapters, tests, or APIs where runtime borrow checking is genuinely simpler.
- Use `Box` for indirection only when recursion, variant size, or owned dynamic dispatch requires it, not by habit.
