# Validation and Invariants

## Rule

Validate data at input boundaries, encode invariants in newtypes and constructors, and let internal code operate on trusted types instead of repeatedly checking raw values.

## Why

Boundary validation makes invalid data fail early and keeps checks close to parsing. Once a value has a validated type, internal code can rely on the invariant without repeating defensive checks everywhere.

## Do

- Validate external input at boundaries: CLI args, HTTP requests, config files, environment variables, database rows, messages, and deserialization.
- Convert raw values into domain types as soon as practical.
- Use `try_new`, `parse`, `TryFrom`, or `FromStr` for fallible construction.
- Keep invariant-bearing fields private.
- Use newtypes for validated strings, IDs, units, ranges, and values with public API meaning.
- Use `NonZero*` types when zero is invalid and the primitive representation still matters.
- Use fallible startup validation for configuration so services fail before doing work with invalid settings.
- Pass validated types through internal code instead of raw `String`, `u64`, or `bool` values.
- Deserialize into types that enforce invariants, or deserialize raw input and convert with `TryFrom`.
- Use assertions for internal invariants that should already have been guaranteed by earlier parsing or construction.

## Avoid

- Do not validate the same invariant at every use site by habit.
- Do not accept raw primitives deep inside the system when a validated domain type already exists.
- Do not expose public fields that allow callers to break a type's invariant.
- Do not make `new` panic for caller-provided input; use `try_new` for validation.
- Do not rely on comments like `// must be non-empty` when the type can enforce it.
- Do not push every invariant into typestate or generics when a fallible constructor is enough.
- Do not treat deserialization as validation unless the deserialized type enforces the invariant.

## Library vs Application

Libraries should encode public API invariants in types and constructors so callers cannot accidentally create invalid values. Applications should validate at process and request boundaries, then pass trusted domain types through services, jobs, and handlers.

## Example

```rust
use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceName(String);

impl WorkspaceName {
    pub fn try_new(value: &str) -> Result<Self, WorkspaceNameError> {
        let value = value.trim();

        if value.is_empty() {
            return Err(WorkspaceNameError::Empty);
        }

        let valid = value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-');
        if !valid {
            return Err(WorkspaceNameError::InvalidCharacter);
        }

        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum WorkspaceNameError {
    #[error("workspace name must not be empty")]
    Empty,
    #[error("workspace name must contain only ASCII letters, digits, or '-'")]
    InvalidCharacter,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Workspace {
    name: WorkspaceName,
}

impl Workspace {
    pub fn new(name: WorkspaceName) -> Self {
        Self { name }
    }

    pub fn name(&self) -> &WorkspaceName {
        &self.name
    }
}

pub fn create_workspace(raw_name: &str) -> Result<Workspace, WorkspaceNameError> {
    let name = WorkspaceName::try_new(raw_name)?;
    Ok(Workspace::new(name))
}

pub fn workspace_path(root: &Path, name: &WorkspaceName) -> PathBuf {
    root.join(name.as_str())
}
```

`workspace_path` does not re-check for an empty name or invalid character because the `WorkspaceName` constructor already owns that invariant.

## Exceptions

- Re-check constraints that depend on changing external state, such as authorization, database uniqueness, file existence, quotas, or time.
- Re-validate data loaded from untrusted storage, legacy tables, external caches, or older serialized formats.
- Use runtime checks inside hot paths only when profiling or safety requirements show they are needed.
- Use typestate when ordered workflow states are important enough that invalid transitions should not compile.
