# Code Review and Refactor

Use this workflow when reviewing, refactoring, or changing existing Rust code in a project that already has structure and conventions.

## Required Guidelines

Load [guidelines.md](../guidelines.md), then load these guideline pages as needed:

- [Library vs application conventions](../guidelines/library-vs-application-conventions.md)
- [Public API evolution](../guidelines/public-api-evolution.md)
- [rustc and Clippy lints](../guidelines/rustc-and-clippy-lints.md)
- [Property tests, snapshots, benchmarks, and CI](../guidelines/property-tests-snapshots-benchmarks-and-ci.md)
- [Panics, unwrap, expect, and assertions](../guidelines/panics-unwrap-expect-and-assertions.md)
- [Error propagation, context, and messages](../guidelines/error-propagation-context-and-messages.md)
- [Ownership, borrowing, and clone policy](../guidelines/ownership-borrowing-and-clone-policy.md)
- [Concurrency primitives](../guidelines/concurrency-primitives.md)
- [Logging and observability](../guidelines/logging-and-observability.md)
- [Unsafe code and macros](../guidelines/unsafe-code-and-macros.md)

Load narrower pages for the code you touch, such as newtypes, traits, async task lifecycle, validation, collections, or documentation.

## Workflow

1. Classify the code first: published library API, shared in-repo library, application/service, CLI, test support, or tests.
2. Identify the behavioral surface being changed and the callers affected. Treat externally consumed APIs as stricter than internal application code.
3. Load only the guideline pages relevant to that surface.
4. Scan high-risk patterns before editing: accidental public API changes, hidden panics, flattened errors, unnecessary clones or lifetimes, locks across `.await`, blocking work on async paths, unredacted logs, unsafe, and macro-generated behavior.
5. Make the smallest coherent change. Preserve existing local style unless it conflicts with this guide or the requested behavior.
6. Add or update tests at the level where the behavior is observable.
7. Run verification appropriate to the change: formatter, Clippy, tests, MSRV/all-features checks, or a narrower command when the project makes the full suite impractical.
8. Report what changed, what was verified, and any exceptions or skipped checks with the reason.

## Review Checklist

- Scope: Did the change affect library, application, CLI, or test-only behavior?
- API: Did `pub`, re-exports, features, MSRV, or public dependencies change?
- Errors: Are recoverable failures returned with source chains and boundary context?
- Panics: Are `unwrap`, `expect`, `panic!`, and assertions limited to invariants?
- Ownership: Are clones, borrows, and owned snapshots named honestly?
- Async/concurrency: Are task ownership, cancellation, blocking work, and lock scopes explicit?
- Observability: Are logs structured, low-noise, and free of secrets?
- Unsafe/macros: Is any unsafe or macro complexity justified, isolated, and documented?
- Tests: Does coverage protect behavior rather than private implementation churn?
- Verification: Were the commands run fresh, and are skipped checks explained?

## Avoid

- Do not load every guideline page by default.
- Do not refactor unrelated code while reviewing a focused change.
- Do not apply library-level ceremony to private application internals without a reason.
- Do not relax lint, test, or safety policy to make a local change easier.
- Do not report a change as verified without naming the commands that ran.
- Do not hide exceptions; document why the local case differs from the default rule.
