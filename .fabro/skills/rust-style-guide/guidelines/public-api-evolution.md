# Public API Evolution

## Rule

Treat public API evolution as mostly relevant only for published crates or APIs consumed outside the repo; optimize internal application APIs for simplicity and accept coordinated breaking changes.

## Why

Most application code is changed with its callers. Semver ceremony, compatibility shims, sealed traits, and future-proof annotations add noise when the API is not externally consumed. Published library APIs are different: callers update independently, so compatibility becomes part of the contract.

## Do

- First classify the API as internal application code, shared in-repo workspace code, or externally consumed/published library code.
- Prefer simple current APIs for application and in-repo code.
- Accept breaking changes for internal APIs when the callers can be updated in the same change.
- Use `pub(crate)` for internal boundaries that should not become crate API.
- Keep published public APIs small and deliberate.
- For published crates, follow semver, use private fields, and consider `#[non_exhaustive]` where future fields or variants are likely.
- Add `#[must_use]` to types and methods where silently dropping the value is almost always a bug: builders, RAII guards, and task/owner types that must be shut down or joined.
- Seal public traits only when external implementations are not intended and the trait is part of a published API.

## Avoid

- Do not add semver compatibility shims for purely internal application code.
- Do not use `#[non_exhaustive]` in internal code just to future-proof ordinary enums or structs.
- Do not add `#[non_exhaustive]` to an already-published type as a later hardening step; adding it is itself a breaking change because downstream exhaustive matches, struct literals, and tuple-variant construction stop compiling. Apply it when the type is introduced.
- Do not create broad public facades for modules that are only used inside one application.
- Do not expose public fields on invariant-bearing types; [struct design](struct-design-and-encapsulation.md) owns the field-visibility policy.
- Do not leak dependency types through published public APIs unless that dependency is intentionally part of the contract.
- Do not remove or change published public APIs without treating it as a breaking change.
- Do not make public traits open for external implementations unless that extension point is intentional.
- Do not rely on the noisy `clippy::must_use_candidate` lint to find must-use types; apply `#[must_use]` deliberately where dropping the value is a real mistake.

## Library vs Application

Applications and internal workspace crates may optimize for directness. Refactor call sites together, delete stale APIs, and avoid compatibility layers that no outside caller needs.

Published crates and externally consumed APIs should optimize for compatibility. Keep the public surface narrow, document behavior, and use semver-aware tools such as `#[non_exhaustive]`, deprecation periods, and sealed traits when they solve a real evolution problem.

## Must-Use Types

Mark types and methods with `#[must_use]` when ignoring the returned value is almost always a mistake. This turns a silent bug into a compile-time warning at the call site.

- Use it on builders, RAII guards, and async task owners such as a `Poller` or `WorkerSet` that callers must shut down or join.
- Use it where discarding the value is almost certainly a bug: builders, guards, handles, and fallible or lazily-effective operations, not ordinary accessors.
- `Result` and `Option` are already `#[must_use]`, so the value comes from your own types.
- Apply it deliberately rather than enabling `clippy::must_use_candidate`, which is noisy.

```rust
/// Owns a background task. Dropping it without calling `shutdown` leaks the task.
#[must_use = "call `shutdown` to stop and join the task"]
pub struct Poller {
    shutdown: CancellationToken,
    task:     JoinHandle<Result<(), PollerError>>,
}
```

## Example

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RunSnapshot {
    pub id:     RunId,
    pub status: RunStatus,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RunStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
}

#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClientError {
    Timeout,
    Unauthorized,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RunId(u64);
```

## Exceptions

- Treat an internal API as external when another team, service, plugin, or generated client consumes it independently.
- Use conservative semver rules when publishing to crates.io or documenting a stable SDK surface.
- Keep temporary compatibility shims when a multi-step migration cannot update all callers in one change.
- Use `#[non_exhaustive]` internally only when it materially improves match-site clarity during active development.
