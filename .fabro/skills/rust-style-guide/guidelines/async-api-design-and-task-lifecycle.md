# Async API Design and Task Lifecycle

## Rule

Design async APIs so task ownership is explicit: applications own spawned tasks and shutdown, while reusable libraries expose awaitable work or return an owner type instead of hiding background tasks.

## Why

Spawned tasks can outlive the call that created them. If no API owns cancellation, errors, and joining, work leaks, failures disappear, shutdown becomes unreliable, and tests become timing-dependent.

## Activation

Load this page when adding async APIs, spawning Tokio tasks, introducing async traits, adding `Send + 'static` bounds, or changing shutdown behavior. Load the async runtime page first if the project posture is not documented.

## Do

- Prefer `async fn` returning `Result<T, E>` for operations callers should await directly; keep pure helpers synchronous per [async runtime](async-runtime-and-when-to-use-async.md).
- Use async traits only when callers need an abstraction, not just because implementations are async.
- Add `Send + 'static` bounds only when values cross a spawned task, thread, or stored future boundary.
- Keep spawned futures and task-boundary errors `Send + 'static`; `tokio::spawn` requires only `Send + 'static`, and adding `Sync` to erased errors is an interop convention for `anyhow`-style errors, not a spawn requirement.
- Spawn tasks from an owner that stores handles, cancellation tokens, and task-specific state.
- Model long-lived application services, external connections, gateways, pollers, and subscribers as owner structs with `new` and `run`/`shutdown` methods, even when the first version only awaits one client future.
- Name task owner types by responsibility, such as `Poller`, `WorkerSet`, `TaskGroup`, or `Supervisor`.
- Store `JoinHandle<Result<(), Error>>` when task failures must be reported.
- Provide an explicit `shutdown`, `stop`, or `join` method that cancels and awaits owned tasks.
- Pass cancellation or shutdown signals into long-lived loops.
- Attach `tracing` spans or fields that identify the task, entity ID, and operation.
- In reusable libraries, expose `async fn`, futures, streams, or an owner type; let callers decide where task spawning belongs.

## Avoid

- Do not call `tokio::spawn` and drop the `JoinHandle` for important work.
- Do not assume dropping a `JoinHandle` cancels the task; it detaches, and the task keeps running, so dropping an owner type without calling `shutdown` leaks the loop unless `Drop` cancels the token.
- Do not hide background tasks inside constructors unless the returned value owns their lifecycle.
- Do not swallow task errors with `let _ = handle.await`.
- Do not spawn in a library merely to make the API look nonblocking.
- Do not add `Send`, `Sync`, or `'static` bounds by habit on ordinary async functions.
- Do not hold non-`Send` values across `.await` in tasks that must run on a multithreaded Tokio runtime.
- Do not let `Rc`, `RefCell`, or non-`Send` guards leak into public futures that should run on Tokio's multithreaded runtime.

## Library vs Application

Applications own runtime setup, task spawning, cancellation, shutdown, and joining. They can provide application-level owners for workers, pollers, subscribers, schedulers, and service task groups.

Use a plain `async fn` for one-shot operations. Use an owner type for long-lived services whose state, lifecycle, or shutdown may grow.

Libraries should normally return awaitable work and let callers spawn it. If a library truly owns background work, return an owner or guard type that makes shutdown observable and reports task failures.

## Example

Prefer an owner type for application background tasks:

```rust
use tokio::{select, task::JoinHandle};
use tokio_util::sync::CancellationToken;

pub struct Poller {
    shutdown: CancellationToken,
    task:     JoinHandle<Result<(), PollerError>>,
}

impl Poller {
    pub fn start(client: Client) -> Self {
        let shutdown = CancellationToken::new();
        let task_shutdown = shutdown.clone();

        let task = tokio::spawn(async move {
            run_poller(client, task_shutdown).await
        });

        Self { shutdown, task }
    }

    pub async fn shutdown(self) -> Result<(), PollerError> {
        self.shutdown.cancel();

        match self.task.await {
            Ok(result) => result,
            Err(error) => Err(PollerError::Join(error)),
        }
    }
}

pub async fn run_poller(
    client: Client,
    shutdown: CancellationToken,
) -> Result<(), PollerError> {
    loop {
        select! {
            () = shutdown.cancelled() => return Ok(()),
            result = poll_once(&client) => result?,
        }
    }
}
```

Dropping a `Poller` without calling `shutdown` detaches the task: the loop keeps running until the token is cancelled.

Reusable libraries should expose the `run_poller`-style future unless they need the owner type for real lifecycle behavior.

## Exceptions

- Fire-and-forget spawning is acceptable only for best-effort work where loss is acceptable and documented, such as opportunistic telemetry or cache warming.
- Tests may spawn short-lived tasks when the test owns aborting or joining them.
- Application convenience APIs may spawn internally when they return a value that controls cancellation and shutdown.
