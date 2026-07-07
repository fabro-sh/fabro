# Performance Investigation

Use this workflow when investigating slow Rust code, performance regressions, excess resource use, or proposed optimization work.

## Required Guidelines

Load [guidelines.md](../guidelines.md), then load these guideline pages as needed:

- [Property tests, snapshots, benchmarks, and CI](../guidelines/property-tests-snapshots-benchmarks-and-ci.md)
- [Collections and data structures](../guidelines/collections-and-data-structures.md)
- [Ownership, borrowing, and clone policy](../guidelines/ownership-borrowing-and-clone-policy.md)
- [Concurrency primitives](../guidelines/concurrency-primitives.md)
- [Cancellation, shutdown, and blocking work](../guidelines/cancellation-shutdown-and-blocking-work.md)
- [Logging and observability](../guidelines/logging-and-observability.md)

Load async, Cargo/dependency, or public API guidelines when the suspected bottleneck touches those surfaces.

## Workflow

1. Define the symptom, workload, success metric, and acceptable tradeoffs before changing code.
2. Reproduce the issue with representative inputs in a release-like build; do not trust debug timings.
3. Record a baseline measurement and the exact command, input, machine, and feature set used.
4. Profile before optimizing. Use the project-standard profiler, `flamegraph`, `samply`, Instruments, `perf`, Tokio Console, or service telemetry as appropriate.
5. Identify the hot path from evidence, then classify the bottleneck: algorithm, allocation/copying, locking, blocking I/O, async scheduling, serialization, or logging overhead.
6. Change one thing at a time. Prefer simpler data flow, better algorithms, fewer clones, or narrower locks before allocator, profile, or compiler tuning.
7. Rerun the same measurement and keep the change only when it materially improves the target metric without violating style or correctness.
8. Add a benchmark, load test, regression test, or release note when the performance behavior is important enough to preserve.

## Measurement Commands

Use the tool that matches the code shape. Examples:

```sh
cargo bench
cargo test --release targeted_case -- --nocapture
hyperfine 'target/release/app input.txt'
cargo flamegraph --bench parser
```

Profilers need debug symbols to produce readable stacks; before capturing flamegraphs, enable debuginfo in the profiled release or bench profile (or a dedicated profiling profile):

```toml
[profile.release]
debug = true
```

For async services, prefer production-like tracing, metrics, load tests, and Tokio task/lock visibility over isolated microbenchmarks when the problem is scheduling or contention.

## Avoid

- Do not optimize before reproducing and measuring the issue.
- Do not compare debug builds to release builds.
- Do not tune allocators, profiles, `target-cpu`, or `#[inline]` before identifying a hot path.
- Do not keep changes that make code harder to understand without a measured win.
- Do not change several variables at once and then guess which one mattered.
- Do not use benchmarks with toy inputs when real workloads have different sizes, distributions, or contention.
