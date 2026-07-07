# Guidelines

Load this file for Rust style policy, then load only the guideline pages needed for the task.

Guideline pages are policy. Do not load every guideline page by default.

## Foundations

- [House style and Rust philosophy](guidelines/house-style-and-rust-philosophy.md) - load for overall code shape, OO-leaning defaults, and Rust idiom tradeoffs.
- [Library vs application conventions](guidelines/library-vs-application-conventions.md) - load before choosing policies that differ for libraries, apps, CLIs, tests, or services.
- [Rust edition and MSRV](guidelines/rust-edition-and-msrv.md) - load when setting edition, `rust-version`, stable/nightly posture, or checking MSRV impact.

## Tooling and Project Shape

- [rustfmt and formatting](guidelines/rustfmt-and-formatting.md) - load when configuring rustfmt or handling formatting exceptions.
- [rustc and Clippy lints](guidelines/rustc-and-clippy-lints.md) - load when configuring lints, fixing Clippy, or justifying lint exceptions.
- [Cargo, workspaces, features, and dependencies](guidelines/cargo-workspaces-features-and-dependencies.md) - load for workspace layout, features, dependency choices, and MSRV-aware dependency changes.
- [Modules, visibility, and re-exports](guidelines/modules-visibility-and-re-exports.md) - load when changing `mod`, `pub`, facades, re-exports, or public paths.
- [Naming, imports, and prelude policy](guidelines/naming-imports-and-prelude-policy.md) - load for item names, acronym casing, imports, getters, and preludes.
- [Documentation and rustdoc examples](guidelines/documentation-and-rustdoc-examples.md) - load when writing rustdoc, public docs, examples, or `Errors`/`Panics`/`Safety` sections.

## Type and API Design

- [Struct design and encapsulation](guidelines/struct-design-and-encapsulation.md) - load when designing structs, fields, invariants, receivers, or encapsulation boundaries.
- [Constructors and builders](guidelines/constructors-and-builders.md) - load when choosing `new`, `try_new`, `Default`, builders, or typestate builders.
- [Newtype pattern and semantic wrappers](guidelines/newtype-pattern-and-semantic-wrappers.md) - load when adding IDs, units, validated strings, value objects, or orphan-rule wrappers.
- [Enums vs traits vs generics vs trait objects](guidelines/enums-vs-traits-vs-generics-vs-trait-objects.md) - load when choosing closed sets, extension points, static dispatch, or dynamic dispatch.
- [Trait design](guidelines/trait-design.md) - load when designing traits, bounds, associated types, blanket impls, sealed traits, or object-safe APIs.
- [Deriving and common trait implementations](guidelines/deriving-and-common-trait-implementations.md) - load when adding derives or manual impls for standard traits.
- [Conversions, getters, and method naming](guidelines/conversions-getters-and-method-naming.md) - load for `From`, `TryFrom`, `AsRef`, `Deref`, accessors, and `as_`/`to_`/`into_` names.
- [Typestate and state machines](guidelines/typestate-and-state-machines.md) - load for ordered workflow states, data-bearing enums, `PhantomData`, or compile-time transitions.
- [Public API evolution](guidelines/public-api-evolution.md) - load for externally consumed APIs, semver, `#[non_exhaustive]`, `#[must_use]`, public fields, or sealed traits.

## Ownership and Data Flow

- [Ownership, borrowing, and clone policy](guidelines/ownership-borrowing-and-clone-policy.md) - load when choosing borrowed inputs, owned outputs, `String`/`&str`, `Path` parameters, `IntoIterator`, `AsRef`, `Cow`, accessors, snapshots, or clone tradeoffs.
- [Lifetimes](guidelines/lifetimes.md) - load when explicit lifetimes, borrowed structs, or lifetime-heavy APIs appear.
- [Smart pointers and interior mutability](guidelines/smart-pointers-and-interior-mutability.md) - load when choosing `Box`, `Rc`, `Cell`, `RefCell`, `Weak`, or one-time initialization.
- [Collections and data structures](guidelines/collections-and-data-structures.md) - load when choosing `Vec`, maps, sets, deterministic ordering, capacity, or specialized collection crates.

## Errors, Safety, and Diagnostics

- [Error taxonomy and layer boundaries](guidelines/error-taxonomy-and-layer-boundaries.md) - load when defining domain, infrastructure, boundary, or branch-oriented error layers.
- [Library errors vs application errors](guidelines/library-errors-vs-application-errors.md) - load before choosing `thiserror`, `anyhow`, `miette`, or public error stability.
- [Error propagation, context, and messages](guidelines/error-propagation-context-and-messages.md) - load when adding `?`, context, source chains, or error message text.
- [Panics, unwrap, expect, and assertions](guidelines/panics-unwrap-expect-and-assertions.md) - load when using panic, `unwrap`, `expect`, assertions, `unreachable!`, `todo!`, or public panic docs.
- [Validation and invariants](guidelines/validation-and-invariants.md) - load when parsing inputs, enforcing constructors, encoding invariants, or re-checking stale state.
- [Logging and observability](guidelines/logging-and-observability.md) - load when adding `tracing`, spans, fields, levels, error logs, or redaction.

## Async and Concurrency

- [Async runtime and when to use async](guidelines/async-runtime-and-when-to-use-async.md) - load when deciding sync vs async posture, Tokio use, or runtime boundaries.
- [Async API design and task lifecycle](guidelines/async-api-design-and-task-lifecycle.md) - load when adding async APIs, async traits, spawning, task owners, `Send`, or shutdown handles.
- [Cancellation, shutdown, and blocking work](guidelines/cancellation-shutdown-and-blocking-work.md) - load for cancellation tokens, `select!`, timeouts, `spawn_blocking`, CPU work, or graceful shutdown.
- [Concurrency primitives](guidelines/concurrency-primitives.md) - load when adding channels, locks, atomics, `Arc` shared state, worker pools, or blocking APIs on async paths.

## Everyday Implementation

- [Control flow](guidelines/control-flow.md) - load when choosing `match`, `if let`, `let else`, guards, early returns, combinators, mutable locals, or in-place updates.
- [Option and Result idioms](guidelines/option-and-result-idioms.md) - load when transforming `Option`/`Result`, using `ok_or_else`, `transpose`, `map`, or explicit branching.
- [Iterators, closures, and loops](guidelines/iterators-closures-and-loops.md) - load when choosing iterator chains, loops, closure capture, `collect`, `fold`, or `try_fold`.

## Testing and Release

- [Testing and doctests](guidelines/testing-and-doctests.md) - load when writing unit tests, integration tests, doctests, fixtures, or test helpers.
- [Property tests, snapshots, benchmarks, and CI](guidelines/property-tests-snapshots-benchmarks-and-ci.md) - load when configuring test commands, snapshots, property tests, benchmarks, or CI gates.
- [Unsafe code and macros](guidelines/unsafe-code-and-macros.md) - load when touching `unsafe`, FFI, raw pointers, `macro_rules!`, proc macros, or generated APIs.

## Routing Notes

- For new Rust project setup, load [workflows/new-rust-project.md](workflows/new-rust-project.md) before individual setup guidelines.
- For reusable library release verification, load [workflows/reusable-library-release.md](workflows/reusable-library-release.md) before individual release guidelines.
- For performance investigation, load [workflows/performance-investigation.md](workflows/performance-investigation.md) before individual performance-related guidelines.
- For code review or refactor work, load [workflows/code-review-refactor.md](workflows/code-review-refactor.md) before individual review guidelines.
- For public API work, always include public API evolution.
- For async service work, include logging and observability.
- For error-handling work, distinguish library errors from application errors before choosing crates.
- For advanced topics like typestate, unsafe, macros, or specialized collections, load the page only when the task directly needs it.
