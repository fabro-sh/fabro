# Ownership, Borrowing, and Clone Policy

## Rule

Accept concrete borrowed parameters, store and return owned values at boundaries, and clone freely to keep APIs simple; use flexible generic bounds only when they clearly improve caller ergonomics.

## Why

Borrowed inputs such as `&str`, `&[T]`, and `&Path` keep call sites flexible and accept the common owned and borrowed caller types. Owned values keep lifetimes out of structs, snapshots, and return types. Plain accessors should not hide ownership or allocation costs, and generic bounds help callers only when they stay local instead of spreading type parameters through the API.

## Do

- Use `&self` for observation, `&mut self` for in-place mutation, and `self` for consuming transitions.
- Accept `&str` instead of `&String`, `&[T]` instead of `&Vec<T>`, and `&Path` instead of `&PathBuf` for read-only inputs.
- Store owned `String`, `Vec<T>`, and `PathBuf` inside structs.
- Take owned values or `impl Into<T>` in constructors and setters that store the value unchanged; borrow and clone at the boundary when storing a normalized or derived value.
- Return borrowed values from plain accessors when the lifetime is obvious.
- Return owned snapshots, IDs, handles, or collections when returning references would expose unnecessary lifetimes, and name owned snapshots explicitly.
- Use `.clone()` for ordinary values, `Rc`, and `Arc`; this deliberately deviates from the std docs' `Arc::clone(&value)` preference in favor of one consistent spelling.
- Use `IntoIterator` for APIs whose purpose is to consume or extend from a sequence of items.
- Use `AsRef<str>`, `AsRef<Path>`, or `impl Into<String>` bounds only when caller flexibility clearly helps and the bound stays local.
- Accept `impl Read` or `impl Write` when a reusable library should test I/O behavior without touching the filesystem.
- Use `Cow` only when the API genuinely often borrows but sometimes allocates, and the lifetime stays local.
- Revisit clone costs only when profiling or domain knowledge shows they matter.

## Avoid

- Do not accept owned `String`, `Vec<T>`, or `PathBuf` when the function only reads the input.
- Do not accept `&String`, `&Vec<T>`, or `&PathBuf` by habit.
- Do not store borrowed references in structs just to avoid allocation.
- Do not add lifetime parameters only to avoid cheap clones; see [lifetimes](lifetimes.md) for when explicit lifetimes are worth it.
- Do not hide clones in bare-noun accessors such as `labels() -> Vec<_>` or `settings() -> Arc<_>`.
- Do not return references from computed queries or snapshots when an owned value would make the API simpler.
- Do not add `AsRef`, `Into`, `Borrow`, or generic type parameters to every function by default; reserve `Borrow` for key-equivalence and lookup patterns.
- Do not use `Cow` as a general-purpose way to avoid deciding between borrowed and owned data.
- Do not mix `Arc::clone(&value)` and `value.clone()` styles in the same codebase.
- Do not hide expensive deep clones in hot paths once cost is known to matter.

## Public API Notes

For public APIs, concrete borrowed refs are usually clearer than generic bounds; add flexible bounds when they materially reduce caller friction without leaking type parameters through the API. For internal application code, favor the simplest signature and clone at boundaries. For published libraries, document ownership behavior when clones may be large or surprising.

## Example

Store owned data, borrow in plain accessors, and take `impl Into` when storing unchanged:

```rust
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Settings {
    service_name: String,
    root:         PathBuf,
}

impl Settings {
    pub fn new(service_name: impl Into<String>, root: PathBuf) -> Self {
        Self {
            service_name: service_name.into(),
            root,
        }
    }

    pub fn service_name(&self) -> &str {
        &self.service_name
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}
```

Bad: hide an owned clone behind a plain accessor.

```rust
pub fn labels(&self) -> Vec<String> {
    self.labels.clone()
}
```

Good: borrow by default and name owned snapshots explicitly.

```rust
pub fn labels(&self) -> &[String] {
    &self.labels
}

pub fn labels_snapshot(&self) -> Vec<String> {
    self.labels.clone()
}
```

Use flexible bounds where they genuinely help callers, and normalize at the boundary:

```rust
use std::borrow::Cow;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileMatcher {
    extensions: Vec<String>,
}

impl FileMatcher {
    pub fn from_extensions<I, S>(extensions: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let extensions = extensions
            .into_iter()
            .map(|extension| normalize_extension(extension.as_ref()).into_owned())
            .collect();

        Self { extensions }
    }

    pub fn matches_extension(&self, extension: &str) -> bool {
        let extension = normalize_extension(extension);

        self.extensions
            .iter()
            .any(|candidate| candidate.as_str() == extension.as_ref())
    }
}

pub fn normalize_extension(extension: &str) -> Cow<'_, str> {
    let trimmed = extension.trim();
    let normalized = trimmed.strip_prefix('.').unwrap_or(trimmed);

    if normalized.chars().any(|character| character.is_ascii_uppercase()) {
        Cow::Owned(normalized.to_ascii_lowercase())
    } else {
        Cow::Borrowed(normalized)
    }
}
```

## Exceptions

- Accept owned values when the function consumes ownership, stores without cloning, or mirrors a standard library convention.
- Use `impl AsRef<Path>` for top-level file-opening helpers when accepting many path-like caller types is the main ergonomic benefit.
- Use `impl Read` or `impl Write` for lower-level helpers whose purpose is data processing, not path handling.
- Use `Cow` in parsing, normalization, and formatting helpers that can usually return a borrowed value.
- Use slices of references, such as `&[&str]`, when the call sites naturally already have borrowed items.
- Return owned handles from methods whose names make shared ownership explicit.
- Avoid clones in measured hot paths, large data movement, or resource-heavy types.
- Use specialized clone spelling only when matching an existing local convention in code you are modifying.
