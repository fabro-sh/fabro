# Cancellation, Shutdown, and Blocking Work

## Rule

Use cooperative shutdown by default: pass explicit cancellation signals into long-lived async work, race loops with `select!`, join owned tasks, put timeouts at boundaries, and isolate blocking or CPU-bound work from Tokio worker threads.

## Why

Async cancellation can happen at any `.await`. Code that ignores cancellation, scatters timeouts, or blocks Tokio workers is harder to shut down cleanly and can make unrelated async work stall.

## Activation

Load this page when adding long-lived async loops, graceful shutdown, timeouts, external calls, blocking I/O, CPU-heavy work, or task teardown behavior.

## Do

- Pass an explicit shutdown signal, usually a cancellation token, into long-lived tasks.
- Use `select!` in service loops to race normal work with shutdown.
- Join owned tasks during shutdown and surface task errors; task owners and handles are defined on [async API design and task lifecycle](async-api-design-and-task-lifecycle.md).
- Put timeouts at operation boundaries: external calls, subprocesses, requests, jobs, and shutdown phases.
- Keep inner helper functions timeout-free unless they own a real operation boundary.
- Make cancellable sections idempotent or restartable when an `.await` can interrupt progress.
- Treat losing `select!` branches as dropped futures; keep partial reads, buffers, and side effects recoverable.
- Commit external side effects in small, explicit steps with clear retry or rollback behavior.
- Use `tokio::task::spawn_blocking` for blocking filesystem, compression, parsing through blocking APIs, or short CPU-heavy work.
- Use a dedicated pool, work queue, or `rayon` for sustained CPU-bound workloads.
- Drop locks before `.await`, blocking work, callbacks, or expensive computation.
- Log shutdown start, timeout, task failure, and final shutdown outcome with structured fields.

## Avoid

- Do not rely on dropping a future as the only shutdown mechanism for important work.
- Do not call blocking I/O, `std::thread::sleep`, or long CPU work directly on Tokio worker threads.
- Do not add `timeout` around every small helper call.
- Do not use `abort` as the normal shutdown path for tasks that need cleanup.
- Do not hold a lock guard across `.await` unless the design explicitly requires an async lock.
- Do not put non-cancel-safe work directly in a `select!` branch without owning the state needed to resume or retry it.
- Do not assume `spawn_blocking` makes unlimited CPU work cheap; it still needs backpressure.
- Do not expect `spawn_blocking` closures to be cancelled once started; cancellation tokens and `abort` do not interrupt them, and runtime shutdown waits for them, so keep blocking sections short or chunked with cancellation checks between chunks.

## Example

Race work with shutdown, place the timeout around the external operation, and isolate blocking work:

```rust
use std::path::PathBuf;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio::{select, task};
use tokio_util::sync::CancellationToken;

pub async fn run_worker(
    mut jobs: mpsc::Receiver<Job>,
    shutdown: CancellationToken,
    client: Client,
) -> Result<(), WorkerError> {
    loop {
        let job = select! {
            () = shutdown.cancelled() => return Ok(()),
            maybe_job = jobs.recv() => match maybe_job {
                Some(job) => job,
                None => return Ok(()),
            },
        };

        process_job(&client, job).await?;
    }
}

async fn process_job(client: &Client, job: Job) -> Result<(), WorkerError> {
    let record = timeout(
        Duration::from_secs(10),
        client.fetch(job.record_id()),
    )
    .await
    .map_err(|_| WorkerError::FetchTimedOut {
        record_id: job.record_id(),
    })??;

    let digest = hash_file(job.path()).await?;
    client.store_digest(record.id(), digest).await?;

    Ok(())
}

async fn hash_file(path: PathBuf) -> Result<Digest, WorkerError> {
    task::spawn_blocking(move || Digest::from_file(path))
        .await
        .map_err(WorkerError::HashJoin)?
        .map_err(WorkerError::Hash)
}
```

Shutdown interrupts only the idle wait: a job that has been received is driven to completion, bounded by the timeout inside `process_job`. Race in-progress work against shutdown only when something owns the state needed to resume or retry it.

## Exceptions

- Use `abort` for teardown of best-effort tasks that do not own external state and do not need cleanup.
- Let short-lived request tasks complete naturally when the caller already owns cancellation through request drop or timeout.
- Use shorter inner timeouts only when a lower-level operation has an independent service-level objective or resource limit.
- Keep CPU-heavy work on Tokio only when it is known to be tiny and bounded.
