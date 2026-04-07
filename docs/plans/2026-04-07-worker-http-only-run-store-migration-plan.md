# Complete Worker HTTP-Only Run Store Migration

## Summary
- Finish the architecture change by removing all `RunDatabase` / SlateDB usage from the detached `fabro __run-worker` path.
- Keep the server as the only SlateDB owner and single writer.
- Do this with one run-scoped internal runtime abstraction used by workflow execution, plus two implementations:
  - local adapter for server-side execution and tests
  - HTTP-backed adapter for detached workers
- Let the HTTP-backed worker maintain a write-through in-memory mirror of acknowledged events and projection state so repeated worker-side reads do not turn into unnecessary HTTP round-trips.
- Reuse existing server endpoints; no OpenAPI or route changes are required for this migration.

## Internal Interface Changes
- Introduce a small run-scoped async runtime-facing store interface in `fabro-workflow` for the operations the executor actually needs:
  - `load_state() -> RunProjection`
  - `list_events() -> Vec<EventEnvelope>`
  - `append_run_event(&RunEvent) -> ()`
  - `write_blob(&[u8]) -> RunBlobId`
  - `read_blob(&RunBlobId) -> Option<Bytes>`
- Change workflow execution plumbing to depend on that interface instead of `RunDatabase`:
  - `StartServices`
  - `EngineServices`
  - the pipeline structs and options that currently carry `RunDatabase`
  - helpers that currently hard-code persisted-load, retro, and finalize store reads
- Replace the `RunEventSink::store(RunDatabase)` special case with a backend-based writer so event emission no longer assumes a local SlateDB handle.
- Keep the interface scoped to a single run so methods do not need a `RunId` parameter.

## Implementation Changes
### 1. Runtime backend abstraction
- Add a runtime store backend trait in `fabro-workflow` and migrate the worker-facing execution path to depend on it instead of `RunDatabase`.
- Keep the interface narrow and asynchronous, covering only the worker-side behaviors that still depend on store access:
  - load run projection / run record / graph source
  - list events
  - append run events
  - write blobs
  - read blobs
- Allow the HTTP-backed implementation to keep a write-through in-memory mirror of run state and events, but only update that mirror after the server has acknowledged the write. The server remains canonical; the worker cache is a derived mirror for read efficiency only.
- Define failure policy up front:
  - apply bounded retries to transient HTTP failures
  - if retries exhaust on a required read or write, fail the worker run with a clear fatal error
  - do not continue executing after the worker loses the ability to read or write canonical run state

### 2. Local adapter for server execution
- Add a local adapter in `fabro-workflow` that wraps `RunDatabase`.
- Keep server-side execution behavior unchanged:
  - the server still opens the durable `RunDatabase`
  - the server passes the local adapter into workflow execution
  - the server remains the only SlateDB owner and single writer in production execution
- Keep existing unit and integration tests that rely on in-process `RunDatabase` semantics working through this adapter.

### 3. HTTP-backed adapter for detached workers
- Add an HTTP-backed adapter in `fabro-cli` on top of `ServerStoreClient`.
- Extend `ServerStoreClient` with the missing worker-side helpers already supported by the server API:
  - write run blob
  - read run blob
  - get checkpoint only if a migrated path needs a direct checkpoint call rather than `load_state()`
- The detached worker should use this adapter for all run-state reads and event/blob writes.
- Seed the adapter from the server once at worker startup, then keep its in-memory mirror in sync from acknowledged appends and explicit refetches when needed.

### 4. Migrate confirmed worker-path reads off `RunDatabase`
- Move the confirmed detached-worker call sites to the new backend before deleting any local store construction:
  - startup validation and persisted-run loading
  - resume checkpoint loading
  - retro state and event reads
  - finalize conclusion building and metadata-finalize reads
  - artifact blob offload and any worker-path blob reads
  - git metadata checkpoint and finalize reads that currently load state from the local store
- Treat this as a worker-path refactor, not a whole-repo purge of `RunDatabase`.
- Explicitly out of scope for this migration:
  - server supervisor code
  - server routes and server-local run execution
  - `store dump` and other separate server-funneling work

### 5. Remove local worker store usage
- After the worker-path reads above are migrated, delete the local seeded in-memory store path from `lib/crates/fabro-cli/src/commands/run/runner.rs`:
  - remove local `Database::new(InMemory, ..., flush_interval)` construction
  - remove seeding via `list_run_events()` into a local `RunDatabase`
  - remove the worker-side `RunEventSink::fanout([store, callback])`
- After this change, detached workers send events directly to the server over HTTP and never construct or open any `fabro_store::Database`.

## Test Plan
- Add unit coverage for the new local adapter covering:
  - state loading
  - event append
  - blob write behavior
  - blob read behavior
- Add focused unit coverage for the HTTP-backed adapter covering:
  - write-through projection and event cache updates after acknowledged appends
  - bounded retry behavior
  - fatal failure when required HTTP reads or writes keep failing
- Add focused CLI and worker tests proving detached execution still works for:
  - start
  - resume
  - cancel / ctrl-c
  - human gate handling
  - large context value blob offload
- Add a regression test that would have failed under the old design:
  - detached worker event delivery is no longer paced by a local worker-side store append
  - `dry_run_simple` no longer shows the `~100ms` per-event cadence caused by the worker’s local store path
- Add a regression test that the detached worker path no longer constructs a local `fabro_store::Database`.
- Keep existing server execution tests green to prove the local adapter preserved current semantics.

## Assumptions and Defaults
- No public HTTP API changes are needed for this migration; existing run state, event, checkpoint, blob, and artifact routes are sufficient.
- The runtime backend is scoped to a single run, matching detached-worker execution semantics.
- The HTTP-backed worker cache is a derived mirror updated only after successful server acknowledgements; it does not make the worker a second source of truth.
- While a detached worker is executing, it is assumed to be the sole emitter of run events for that run; if server-originated events are later added to the live run stream, the cache model will need an explicit invalidation or subscription mechanism.
- The detached worker must have zero direct SlateDB access after this change.
- The server remains the only component allowed to own a `RunDatabase` in production execution.
- This should land as one coherent migration, not as a partial compatibility phase, because the current mixed model is both architecturally wrong and performance-visible.
