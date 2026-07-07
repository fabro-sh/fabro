# Collections and Data Structures

## Rule

Use standard-library collections by default; add specialized collection crates only when required semantics, deterministic ordering, or known performance needs justify them.

## Why

Standard collections are familiar, well-tested, dependency-free, and usually fast enough. Specialized collections are useful when they express real behavior, but they should not become incidental dependencies.

## Do

- Use `Vec<T>` for ordered, indexable, append-heavy lists.
- Use `VecDeque<T>` for queue-like data that pushes and pops at both ends.
- Use `HashMap<K, V>` and `HashSet<T>` for unordered lookup.
- Use `BTreeMap<K, V>` and `BTreeSet<T>` when sorted iteration or deterministic order matters.
- Sort a `Vec<T>` before output when deterministic order is only needed at the boundary.
- Use capacity hints such as `Vec::with_capacity` when the size is already known.
- Use `retain`, `drain`, and `std::mem::take` for clear in-place collection updates.
- Use `entry(key).or_insert_with(...)` or `or_default()` for map insert-or-update instead of a `contains_key` check followed by `insert`, the double lookup clippy's `map_entry` flags.
- Use newtypes around collections when the collection has domain invariants or behavior.
- Add crates such as `indexmap`, `smallvec`, or domain-specific data structures only when their semantics or measured performance matter.

## Avoid

- Do not add collection crates just because they are convenient in one small spot.
- Do not use `HashMap` when iteration order affects tests, logs, serialization, or public output.
- Do not use `BTreeMap` only because it feels more stable if lookup performance or ordering does not matter.
- Do not use `Vec` for repeated front removal; use `VecDeque`.
- Do not expose raw collection fields when the collection has invariants.
- Do not preallocate capacity when the estimate is guesswork.
- Do not optimize collection choice before the data size and access pattern are known.

## Public API Notes

Public APIs should prefer standard-library collection types unless another collection type is part of the API's real semantics. Exposing a specialized collection type makes that crate part of the public contract.

Return iterators or owned standard collections when that keeps the API independent of internal storage.

## Example

```rust
use std::collections::{BTreeMap, HashMap, VecDeque};

#[derive(Clone, Debug, Default)]
pub struct JobQueue {
    pending: VecDeque<Job>,
}

impl JobQueue {
    pub fn push(&mut self, job: Job) {
        self.pending.push_back(job);
    }

    pub fn pop(&mut self) -> Option<Job> {
        self.pending.pop_front()
    }
}

#[derive(Clone, Debug, Default)]
pub struct UserIndex {
    by_id: HashMap<UserId, User>,
}

impl UserIndex {
    pub fn insert(&mut self, user: User) {
        self.by_id.insert(user.id, user);
    }

    pub fn get(&self, id: UserId) -> Option<&User> {
        self.by_id.get(&id)
    }

    pub fn display_names_by_id(&self) -> BTreeMap<UserId, String> {
        self.by_id
            .iter()
            .map(|(id, user)| (*id, user.name.clone()))
            .collect()
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct UserId(u64);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct User {
    id:   UserId,
    name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Job {
    id: UserId,
}
```

## Exceptions

- Use `IndexMap` when insertion order is part of the data model or stable output is required while preserving insertion order.
- Use `SmallVec`, arena allocators, or specialized collections when profiling or domain knowledge shows allocation or layout matters.
- Use domain-specific crates for well-known data structures that are hard to implement correctly.
- Use deterministic collections in tests when order stability keeps assertions clear.
