# Library vs Application Conventions

## Rule

Identify the code context first: reusable library, shared in-repo crate, application or service, CLI, or test code. Libraries optimize for stable, caller-controlled APIs; applications, CLIs, and tests optimize for delivery and local clarity.

## Why

Library choices become another crate's constraints, while application choices optimize for delivery, observability, and deployment. Most policies in this guide split on this classification, so classifying wrong applies the wrong half of every other page.

## Do

- Classify code before choosing policies: published or reusable library, shared in-repo workspace crate, application or service, CLI, or test support.
- Treat public library APIs as long-lived contracts; treat application internals as freely refactorable with their callers.
- Follow the owner page for each policy that splits by context:
  - Errors: typed `thiserror` errors at library boundaries, `anyhow` inside applications; see [library errors vs application errors](library-errors-vs-application-errors.md).
  - Instrumentation: libraries emit `tracing` events, applications own subscriber setup; see [logging and observability](logging-and-observability.md).
  - Async: applications own the runtime, spawned tasks, and shutdown; see [async runtime](async-runtime-and-when-to-use-async.md) and [task lifecycle](async-api-design-and-task-lifecycle.md).
  - Dependencies and features: conservative for libraries, pragmatic for applications; see [Cargo, workspaces, features, and dependencies](cargo-workspaces-features-and-dependencies.md).
  - API evolution: semver care only for externally consumed code; see [public API evolution](public-api-evolution.md).

## Avoid

- Do not force library-level abstraction into application code when one concrete type is enough.
- Do not over-model one-off CLI failure paths with large public error enums.
- Do not apply application shortcuts, such as global process setup or `anyhow` in signatures, to reusable library boundaries.
- Do not treat shared in-repo crates as published libraries; they follow application rules until something outside the repo consumes them independently.

## Library vs Application

Library code protects caller choice where it affects API stability: typed errors, careful dependency exposure, documented runtime assumptions, and no global process setup.

Application and CLI code chooses concrete dependencies directly and owns process-wide setup: runtime, subscribers, configuration, and shutdown.

## Example

The same operation, classified two ways:

```rust
// Reusable library boundary: typed error, no process-wide assumptions.
pub fn parse_manifest(source: &str) -> Result<Manifest, ManifestError> {
    todo!()
}

// Application command handler: concrete choices, anyhow at the boundary.
pub async fn run_deploy(args: DeployArgs) -> anyhow::Result<()> {
    todo!()
}
```

## Exceptions

- Keep application internals typed when the caller must recover differently from different failures.
- Use a library-specific dependency when it is part of the crate's purpose and documented API.
- Use lighter examples or test helpers in tests when production error and logging structure would obscure the behavior under test.
