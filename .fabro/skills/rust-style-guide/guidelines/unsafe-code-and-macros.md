# Unsafe Code and Macros

## Rule

Ban project-written unsafe code by default; allow `macro_rules!` and proc macros only when they materially improve code simplicity.

## Activation

Load this page when a task touches `unsafe`, FFI, raw pointers, custom macros, proc macros, generated implementations, or macro-heavy public APIs.

## Why

Unsafe code creates proof obligations the compiler cannot check, so the default should be no local unsafe. Macros can hide control flow and make errors harder to understand, but they are useful when they remove real repetition or express a small, consistent pattern better than ordinary Rust.

## Do

- Keep `unsafe_code = "deny"` in the default workspace lint policy.
- Prefer safe Rust and mature crates over project-written unsafe code.
- Treat project-written unsafe as an explicit crate-level exception, not a local convenience.
- If unsafe is truly required, isolate it behind the smallest safe API and document the crate's unsafe policy before implementation.
- Keep unsafe blocks as small as possible; put safe validation and branching outside them.
- Put a `SAFETY:` comment next to every unsafe block or impl in crates that are allowed to use unsafe.
- Document every public unsafe function or trait with `# Safety`.
- Run `cargo +nightly miri test` for crates with project-written unsafe when Miri supports the target (install once with `rustup +nightly component add miri`).
- Keep FFI crates thin: translate portable boundary types and call safe core logic.
- Use `macro_rules!` for repeated impls, repeated tests, small declarative patterns, and local boilerplate that ordinary functions or traits cannot simplify cleanly.
- Use proc macros only when a derive, attribute, or function-like macro materially reduces boilerplate across many call sites.
- Keep macro inputs narrow, generated APIs predictable, and compile errors understandable.
- Put proc macros in dedicated proc-macro crates and keep their public surface small.

## Avoid

- Do not add unsafe code to satisfy the borrow checker or optimize before measurement.
- Do not hide unsafe behavior behind broad helper names.
- Do not expose an unsafe public API unless callers truly must uphold invariants the crate cannot check.
- Do not lower `unsafe_code = "deny"` for a whole workspace because one crate needs an exception.
- Do not exchange Rust-owned allocations, `TypeId`-dependent values, or global-state assumptions across dynamic library boundaries.
- Do not use uninitialized memory patterns without a type-specific validity proof; prefer `MaybeUninit` when uninitialized memory is truly required.
- Do not write a macro for one or two call sites.
- Do not use macros to invent control flow that functions, traits, enums, or builders can express clearly.
- Do not write a proc macro when `macro_rules!`, a derive from a mature crate, or ordinary Rust would be enough.
- Do not make macro-generated names, modules, trait impls, or side effects surprising.

## Safety Notes

Project-written unsafe includes unsafe blocks, unsafe functions, unsafe traits and impls, raw-pointer dereferences, FFI boundaries, and other code that requires the `unsafe` keyword. Dependency code may contain unsafe, but that does not justify adding local unsafe to the project.

When a crate is granted an unsafe exception, review the safe abstraction boundary first: callers should be able to use the public API without knowing the internal unsafe invariant.

In Rust 2024, write FFI declarations and unsafe attributes in their explicit unsafe forms, such as `unsafe extern` and `#[unsafe(no_mangle)]`, when the language requires them.

## Public API Notes

Public macros are public API. Name them clearly, keep their accepted syntax small, document the generated behavior, and avoid exporting helper macros unless callers are meant to use them directly.

## Example

Keep the default lint strict:

```toml
[workspace.lints.rust]
unsafe_code = "deny"
```

Use a macro when it removes repeated, mechanical boilerplate that ordinary functions and traits cannot. This macro fits opaque, server-assigned IDs that are always valid by construction and share an identical, validation-free shape. IDs that need validation, a custom `Display`, or distinct behavior should be written by hand following the newtype guidance.

```rust
macro_rules! define_id_type {
    ($name:ident) => {
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

define_id_type!(UserId);
define_id_type!(WorkspaceId);
define_id_type!(RunId);
```

The macro earns its place only because every generated type is identical and correct on its own. If one ID needs validation or different behavior, or if the macro stops being simpler than the expanded code, delete it and write the types directly.

Bad: add ad hoc unsafe to bypass ordinary bounds or checks.

```rust
let item = unsafe { items.get_unchecked(index) };
```

Good: use safe Rust unless an unsafe exception has been approved and documented.

```rust
let item = items
    .get(index)
    .ok_or_else(|| IndexError { index, len: items.len() })?;
```

## Exceptions

- Allow unsafe in crates whose purpose requires it, such as FFI bindings, low-level platform integration, carefully measured performance primitives, or hardware-adjacent code.
- Keep an existing unsafe crate's local policy if removing unsafe is outside the current task; do not spread that exception to other crates.
- Use small test macros when they make repetitive case tables easier to scan.
- Use generated code or proc macros when they replace large, error-prone handwritten implementations with a smaller source of truth.
