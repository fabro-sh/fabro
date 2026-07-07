# Concurrency Primitives

## Rule

Choose the simplest primitive by ownership shape: owned values first, channels for ownership transfer, standard-library locks for short synchronous critical sections, Tokio locks only for async waiting, and dedicated CPU/blocking work tools when work is not async I/O.

## Why

Concurrency primitives encode ownership and scheduling choices. Picking the smallest primitive that matches the shape of the data keeps async code predictable and avoids blocking Tokio workers by accident.

## Activation

Load this page when adding channels, locks, atomics, worker pools, shared state, runtime boundaries, or CPU parallelism.

## Do

- Prefer one clear owner for mutable state.
- Use channels when a value or command should move to an owning task or worker.
- Use bounded channels when producers can outrun consumers.
- Use `Arc<T>` for shared ownership across threads or Tokio tasks.
- Use `std::sync::Mutex` or `std::sync::RwLock` for short, synchronous critical sections.
- Use `tokio::sync::Mutex`, `RwLock`, `Semaphore`, `Notify`, or channels when awaiting for coordination is part of the design.
- Keep lock scopes small and copy or clone owned data out before `.await`.
- Start with `Mutex`; use `RwLock` only when read-heavy access and contention make it worthwhile.
- Use atomics only for simple counters, flags, and low-level coordination with obvious ordering.
- Use `spawn_blocking` for bounded blocking work from async code.
- Use `rayon`, a dedicated pool, or a work queue for sustained CPU-bound work.
- Document lock ordering when more than one lock can be held at once.

## Avoid

- Do not choose `tokio::sync::Mutex` only because the surrounding function is async.
- Do not hold a standard-library lock guard across `.await`.
- Do not use `Arc<Mutex<T>>` to avoid deciding who owns the state.
- Do not use channels for simple shared counters or snapshots.
- Do not use unbounded channels unless memory growth is impossible or intentionally accepted.
- Do not use `RwLock` as a default replacement for `Mutex`.
- Do not put blocking I/O, subprocesses, sleep, or long CPU work directly on Tokio worker threads.
- Do not use `std::thread::spawn` from Tokio code unless a dedicated OS thread is intentional and documented.

## Async Notes

Async projects should enforce blocking-API bans with `clippy::disallowed_methods` and `clippy::disallowed_types`; the lint tables in [the new project workflow](../workflows/new-rust-project.md) are the baseline. Both lints match item paths, not modules: list functions such as `std::thread::sleep`, `std::thread::spawn`, and `std::process::Command::new` under `disallowed_methods`, and types or traits such as `std::net::TcpStream` and `std::io::Read` under `disallowed_types`.

Do not treat those lints as a blanket ban on `std::sync`. Standard-library locks are fine in async code when the critical section is short, does not block, and the guard is dropped before `.await`.

## Example

Use a standard lock for quick shared state, and do async work outside the lock:

```rust
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
pub struct SharedMetrics {
    inner: Arc<Mutex<Metrics>>,
}

impl SharedMetrics {
    pub fn record(&self, event: Event) {
        let mut metrics = self.inner.lock().expect("metrics mutex poisoned");
        metrics.record(event);
    }

    pub fn snapshot(&self) -> Metrics {
        self.inner
            .lock()
            .expect("metrics mutex poisoned")
            .clone()
    }
}

pub async fn handle_job(
    client: &Client,
    metrics: &SharedMetrics,
    job: Job,
) -> Result<(), Error> {
    let record = client.fetch(job.record_id()).await?;
    metrics.record(Event::Fetched);

    process(record).await?;
    metrics.record(Event::Processed);

    Ok(())
}
```

Use a channel when ownership should move to a worker:

```rust
use tokio::sync::mpsc;

pub struct JobQueue {
    sender: mpsc::Sender<Job>,
}

impl JobQueue {
    pub async fn enqueue(&self, job: Job) -> Result<(), QueueClosed> {
        self.sender.send(job).await.map_err(|_| QueueClosed)
    }
}

pub async fn run_worker(mut jobs: mpsc::Receiver<Job>) -> Result<(), Error> {
    while let Some(job) = jobs.recv().await {
        process_job(job).await?;
    }

    Ok(())
}
```

Bad: hold a lock while doing blocking or async work.

```rust
let mut cache = cache.lock().expect("cache mutex poisoned");
let path = cache.entry(key).or_insert_with(default_path).clone();
let bytes = std::fs::read(path)?;
client.upload(bytes).await?;
```

Good: copy the needed value out, drop the lock, and isolate blocking work.

```rust
let path = {
    let mut cache = cache.lock().expect("cache mutex poisoned");
    cache.entry(key).or_insert_with(default_path).clone()
};

let bytes = tokio::task::spawn_blocking(move || std::fs::read(path)).await??;
client.upload(bytes).await?;
```

## Exceptions

- Use Tokio locks when a task must wait asynchronously for shared state or a guard must intentionally live across `.await`.
- Use `std::sync::RwLock` or `tokio::sync::RwLock` when measured or obvious read contention justifies it.
- Use dedicated OS threads for blocking APIs that require thread affinity or long-lived blocking ownership, with a local `#[expect]` reason if lints disallow it.
- Use unbounded channels only for naturally bounded streams or explicit best-effort telemetry paths.
- Use channels even for same-thread code when ownership transfer makes control flow clearer.
