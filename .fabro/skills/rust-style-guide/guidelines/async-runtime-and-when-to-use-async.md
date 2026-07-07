# Async Runtime and When to Use Async

## Rule

Treat sync vs async as an explicit project-level architecture decision; document the project posture first, and use Tokio when the project chooses async.

## Why

Async changes function signatures, trait design, tests, runtime setup, cancellation, shutdown, and dependency choices. It spreads through a codebase, so agents should not introduce or remove async as a local convenience.

## Activation

Load this page when choosing or reviewing a project's sync-vs-async posture or when adding the first async dependency. The task-lifecycle, cancellation, and concurrency pages cover the details once the posture is set.

## Do

- Check the project's documented async posture before adding async APIs, blocking calls, runtime setup, or spawned tasks.
- Document the posture when it is missing: sync or async.
- Document where async is allowed, such as HTTP handlers, workers, clients, subprocess orchestration, streaming, or background tasks.
- Document runtime conventions: Tokio version/features, test macros, shutdown style, timeout policy, and blocking-work policy.
- Use Tokio for async runtime integration when the project is async.
- Use async for real async work: network I/O, timers, streaming, subprocess orchestration, concurrent service work, and APIs that are already Tokio-based.
- Keep CPU-bound computation, parsing, validation, formatting, and simple local transforms synchronous.
- Use sync helpers inside async code when they are short, CPU-local, and do not block on I/O or hold contended locks; see [concurrency primitives](concurrency-primitives.md) for the lock policy.
- For reusable libraries, make runtime assumptions visible in docs, feature names, or crate-level conventions.

## Avoid

- Do not convert a module to async only because the caller is async.
- Do not hide runtime creation inside a reusable library.
- Do not put blocking I/O or long CPU work directly on Tokio worker threads; [cancellation, shutdown, and blocking work](cancellation-shutdown-and-blocking-work.md) owns the isolation rules.
- Do not add runtime-agnostic abstraction after the project has explicitly chosen Tokio and no caller needs another runtime.
- Do not expose async APIs from a library without documenting runtime assumptions.
- Do not maintain parallel sync and async APIs unless both are real project requirements.
- Do not make tests async unless the behavior under test needs async.

## Library vs Application

Applications own the runtime, task lifecycle, shutdown, and subscriber setup. Async applications use Tokio when services, workers, clients, or orchestration need async.

Libraries should not install runtimes or hide task lifecycles. A library may expose Tokio-based APIs when async behavior is central to its purpose, but the runtime dependency should be documented instead of accidental.

## Example

Document the project posture near the project rules:

```markdown
## Async Policy

This project is async and uses Tokio for HTTP handlers, background workers,
external API clients, timers, and subprocess orchestration.

Keep parsing, validation, formatting, and pure domain logic synchronous. Do not
add parallel sync and async APIs without an explicit caller requirement.

Applications own `#[tokio::main]`, task spawning, cancellation, and shutdown.
Library crates may expose async functions but must not create a Tokio runtime.
Use `#[tokio::test]` only for tests that await async behavior.
```

Use async at the operation boundary and sync for local computation:

```rust
pub async fn handle_request(request: Request, client: &ApiClient) -> Result<Response, Error> {
    let command = parse_command(&request)?;
    let record = client.fetch_record(command.record_id()).await?;
    Ok(render_response(record))
}

fn parse_command(request: &Request) -> Result<Command, Error> {
    Command::try_new(request.path(), request.query())
}

fn render_response(record: Record) -> Response {
    Response::from_record(record)
}
```

## Exceptions

- Use a sync posture for CLIs, libraries, or tools whose work is mostly local, CPU-bound, or short-lived.
- Add runtime abstraction only when the project has real callers on multiple runtimes.
- Keep a small sync wrapper around async code only when it is an application convenience and runtime ownership is obvious. The obvious implementation (`Runtime::block_on` or `Handle::block_on`) panics when called from within a runtime, so the wrapper must be reachable only from genuinely synchronous call paths.
