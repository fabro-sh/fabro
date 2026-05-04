Goal: # Model Test Bounded Concurrency Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make bulk `fabro model test` run configured model checks concurrently with a bounded default of 4 requests.

**Architecture:** Keep the existing single-model path serial. In the bulk path, list models once, partition configured and unconfigured models as today, then run configured model POSTs through a `futures` stream with `buffer_unordered(jobs)`. Store each completed configured result with its original configured-list index and sort before rendering so final stdout and JSON remain deterministic within the existing configured/unconfigured grouping.

**Tech Stack:** Rust, Clap, `futures::stream`, existing `fabro_client::Client`, existing `httpmock` integration tests, an inline Axum concurrency harness for deterministic in-flight assertions, `cargo nextest`.

---

## File Structure

- Modify `lib/crates/fabro-cli/src/args.rs`: add the user-facing `--jobs/-j` option to `ModelTestArgs`.
- Modify `lib/crates/fabro-cli/src/commands/model.rs`: add bounded concurrent execution for configured bulk tests and keep output/result semantics unchanged.
- Modify `lib/crates/fabro-cli/tests/it/cmd/model_test.rs`: update help output and add integration coverage for default concurrency and stable final ordering.

## Behavior Contract

- `fabro model test` defaults to `--jobs 4`.
- `--jobs 1` is allowed and behaves like the current serial bulk behavior.
- `--jobs 0` is rejected by Clap.
- `fabro model test --model <MODEL>` ignores the concurrency setting and remains a single POST.
- Bulk mode never POSTs unconfigured models.
- Bulk mode completion progress prints one stderr line per completed configured model, `Testing <model>... done`, in completion order. Tests may assert presence of these lines but must not assert their relative ordering.
- The model listing order means the order returned by `GET /api/v1/models`.
- Final stdout table rows include only configured models, in listing order.
- Final JSON preserves the current grouping: unconfigured rows first in listing order, then configured rows in listing order.
- Existing failure semantics stay the same: configured model test failures increment `failures`; unconfigured listed models increment `skipped` but do not make bulk mode fail; a configured model returning `skip` after listing is a failure.
- `--deep` uses the same `--jobs` value as basic mode. Users who want serial deep tests can pass `--jobs 1`.
- The default can send four simultaneous requests to the same provider. Provider-specific throttling and retry budgets are out of scope for this change; `--jobs 1` is the manual mitigation for low rate limits.
- Do not wrap per-model futures in `catch_unwind`. Panics are programming bugs and should unwind the command as they would in the current serial path; request failures remain normal per-row errors.

### Task 1: Add the CLI Option

**Files:**
- Modify: `lib/crates/fabro-cli/src/args.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/model_test.rs`

- [ ] **Step 1: Add `jobs` to `ModelTestArgs`**

Add this field after `model` and before `deep`:

```rust
    /// Number of model tests to run concurrently in bulk mode
    #[arg(short = 'j', long, default_value_t = 4, value_parser = clap::value_parser!(usize).range(1..))]
    pub(crate) jobs: usize,
```

- [ ] **Step 2: Update the help snapshot**

Update the `help` snapshot in `lib/crates/fabro-cli/tests/it/cmd/model_test.rs` so the options include:

```text
      -j, --jobs <JOBS>          Number of model tests to run concurrently in bulk mode [default: 4]
```

- [ ] **Step 3: Run the help test and confirm the snapshot is the only expected change**

Run:

```bash
cargo nextest run -p fabro-cli --test it cmd::model_test::help
```

Expected: the test passes after the snapshot text is updated.

### Task 2: Thread `jobs` Through the Command

**Files:**
- Modify: `lib/crates/fabro-cli/src/commands/model.rs`

- [ ] **Step 1: Update `test_models_via_server` signature**

Change the function signature to accept `jobs` and keep the existing `request_mode` derivation as the first line of the body:

```rust
async fn test_models_via_server(
    client: &server_client::Client,
    provider: Option<&str>,
    model: Option<&str>,
    deep: bool,
    jobs: usize,
    styles: &Styles,
    json_output: bool,
) -> Result<()> {
    let request_mode = deep.then_some(ModelTestMode::Deep);
```

- [ ] **Step 2: Pass `jobs` from `run_models`**

Change the `ModelsCommand::Test` match arm to destructure and forward `jobs`:

```rust
        ModelsCommand::Test(ModelTestArgs {
            provider,
            model,
            deep,
            jobs,
            ..
        }) => {
            test_models_via_server(
                client,
                provider.as_deref(),
                model.as_deref(),
                deep,
                jobs,
                &styles,
                json_output,
            )
            .await?;
        }
```

- [ ] **Step 3: Verify compile catches no missed call sites**

Run:

```bash
cargo check -p fabro-cli
```

Expected: no errors related to `test_models_via_server` arguments.

### Task 3: Add Bounded Concurrent Bulk Execution

**Files:**
- Modify: `lib/crates/fabro-cli/src/commands/model.rs`

- [ ] **Step 1: Add futures imports**

Add this import near the existing imports:

```rust
use futures::{StreamExt, stream};
```

- [ ] **Step 2: Add a completed result record**

Add this private struct near `ModelTestOutput`:

```rust
struct CompletedModelTest {
    index:        usize,
    model:        Model,
    result_color: Color,
    status:       String,
}
```

- [ ] **Step 3: Extract configured response handling**

Add this helper near `model_test_row_from_status`:

```rust
fn configured_model_test_status(
    result: Result<api_types::ModelTestResult>,
) -> (Color, String, bool) {
    match result {
        Ok(resp) if resp.status == api_types::ModelTestResultStatus::Ok => {
            (Color::Green, "ok".to_string(), false)
        }
        Ok(resp) if resp.status == api_types::ModelTestResultStatus::Skip => (
            Color::Red,
            "error: provider became unconfigured after listing".to_string(),
            true,
        ),
        Ok(resp) => {
            let message = resp
                .error_message
                .unwrap_or_else(|| "unknown error".to_string());
            (Color::Red, format!("error: {message}"), true)
        }
        Err(err) => (Color::Red, format!("error: {err}"), true),
    }
}
```

- [ ] **Step 4: Replace only the serial configured loop**

Keep the existing `models_to_test` list, `partition(...)`, and unconfigured loop unchanged. Replace only the existing `for info in &configured { ... }` loop in bulk mode with the following code. The new code consumes `configured` with `into_iter()` because the vector is not used after this point.

```rust
        let mut completed = stream::iter(configured.into_iter().enumerate())
            .map(|(index, info)| {
                let client = client.clone();
                async move {
                    let result = client.test_model(&info.id, request_mode).await;
                    if !json_output {
                        eprintln!("Testing {}... done", info.id);
                    }
                    let (result_color, status, failed) = configured_model_test_status(result);
                    (
                        CompletedModelTest {
                            index,
                            model: info,
                            result_color,
                            status,
                        },
                        failed,
                    )
                }
            })
            .buffer_unordered(jobs)
            .collect::<Vec<_>>()
            .await;

        completed.sort_by_key(|(completed, _)| completed.index);

        for (completed, failed) in completed {
            if failed {
                failures += 1;
            }

            let mut row = model_row(&completed.model, use_color);
            row.push(
                completed
                    .status
                    .clone()
                    .cell()
                    .foreground_color(color_if(use_color, completed.result_color)),
            );
            rows.push(row);
            json_rows.push(model_test_row_from_status(
                &completed.model,
                &completed.status,
                completed.result_color,
            ));
        }
```

- [ ] **Step 5: Confirm clone and panic semantics**

Do not add `catch_unwind` around the mapped future. `client.clone()` is intentional: `fabro_client::Client` derives `Clone` and shares its underlying state through `Arc` fields, so normal requests can run concurrently. In production, OAuth-authenticated clients serialize refresh work through the existing refresh lock; this does not affect normal request concurrency and is irrelevant to the harness tests below, which use a credential-less HTTP target.

- [ ] **Step 6: Keep single-model progress unchanged**

Do not change the existing single-model block:

```rust
        if !json_output {
            eprint!("Testing {model_id}...");
        }
        let result = client.test_model(model_id, request_mode).await;
        if !json_output {
            eprintln!(" done");
        }
```

- [ ] **Step 7: Run focused compile check**

Run:

```bash
cargo check -p fabro-cli
```

Expected: command succeeds.

### Task 4: Test Bounded Concurrency and Stable Output

**Files:**
- Modify: `lib/crates/fabro-cli/tests/it/cmd/model_test.rs`

- [ ] **Step 1: Add an inline deterministic concurrency harness**

Keep this helper inline in `model_test.rs`; do not move it to shared support unless another test file needs it. The helper uses Axum because `httpmock` is not a good fit for deterministic barrier-style in-flight assertions.

Add imports for the helper:

```rust
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, oneshot};
```

Add these helper types and functions near the existing mock helpers:

```rust
#[derive(Clone)]
struct ConcurrentModelServerState {
    models: Vec<serde_json::Value>,
    gate: Arc<ConcurrencyGate>,
    response_delays: Arc<HashMap<String, Duration>>,
}

struct ConcurrentModelServer {
    base_url: String,
    gate: Arc<ConcurrencyGate>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    join_handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for ConcurrentModelServer {
    fn drop(&mut self) {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }
        if let Some(join_handle) = self.join_handle.take() {
            join_handle
                .join()
                .expect("concurrent model test server thread should not panic");
        }
    }
}

struct ConcurrencyGate {
    expected: usize,
    arrived: AtomicUsize,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
    released: AtomicBool,
    timed_out: AtomicBool,
    release: Semaphore,
}

impl ConcurrencyGate {
    fn new(expected: usize) -> Self {
        Self {
            expected,
            arrived: AtomicUsize::new(0),
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
            released: AtomicBool::new(expected == 0),
            timed_out: AtomicBool::new(false),
            release: Semaphore::new(0),
        }
    }

    async fn enter(&self) {
        let in_flight = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_in_flight.fetch_max(in_flight, Ordering::SeqCst);

        if self.released.load(Ordering::SeqCst) {
            return;
        }

        let arrived = self.arrived.fetch_add(1, Ordering::SeqCst) + 1;
        if arrived >= self.expected {
            if !self.released.swap(true, Ordering::SeqCst) {
                self.release.add_permits(self.expected);
            }
            return;
        }

        let permit = self.release.acquire();
        if self.released.load(Ordering::SeqCst) {
            return;
        }

        if tokio::time::timeout(Duration::from_secs(15), permit)
            .await
            .is_err()
        {
            self.timed_out.store(true, Ordering::SeqCst);
            if !self.released.swap(true, Ordering::SeqCst) {
                self.release.add_permits(self.expected);
            }
        }
    }

    fn exit(&self) {
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
    }

    fn max_in_flight(&self) -> usize {
        self.max_in_flight.load(Ordering::SeqCst)
    }

    fn timed_out(&self) -> bool {
        self.timed_out.load(Ordering::SeqCst)
    }
}

fn start_concurrent_model_server(
    models: Vec<serde_json::Value>,
    gate_expected: usize,
    response_delays: HashMap<String, Duration>,
) -> ConcurrentModelServer {
    let std_listener =
        std::net::TcpListener::bind("127.0.0.1:0").expect("test server should bind");
    std_listener
        .set_nonblocking(true)
        .expect("test server listener should be nonblocking");
    let addr: SocketAddr = std_listener.local_addr().expect("test server should have addr");
    let gate = Arc::new(ConcurrencyGate::new(gate_expected));
    let state = ConcurrentModelServerState {
        models,
        gate: Arc::clone(&gate),
        response_delays: Arc::new(response_delays),
    };
    let (shutdown_tx, shutdown_rx) = oneshot::channel();

    let join_handle = std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().expect("test runtime should start");
        runtime.block_on(async move {
            let listener =
                TcpListener::from_std(std_listener).expect("test listener should convert");
            let app = Router::new()
                .route("/api/v1/models", get(concurrent_list_models))
                .route("/api/v1/models/{id}/test", post(concurrent_test_model))
                .with_state(state);
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await;
        });
    });

    ConcurrentModelServer {
        base_url: format!("http://{addr}"),
        gate,
        shutdown_tx: Some(shutdown_tx),
        join_handle: Some(join_handle),
    }
}

async fn concurrent_list_models(
    State(state): State<ConcurrentModelServerState>,
) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "data": state.models,
        "meta": { "has_more": false }
    }))
}

async fn concurrent_test_model(
    State(state): State<ConcurrentModelServerState>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    state.gate.enter().await;
    if let Some(delay) = state.response_delays.get(&id) {
        tokio::time::sleep(*delay).await;
    }
    state.gate.exit();

    Json(serde_json::json!({
        "model_id": id,
        "status": "ok"
    }))
}
```

The gate intentionally uses a `Semaphore` plus a `released` re-check instead of `Notify`, so late arrivals cannot miss a wake after the trigger request releases the gate. The 15-second timeout has no happy-path cost; when it fires, tests must fail explicitly through `gate.timed_out()` before checking `max_in_flight`. The Tokio runtime is constructed inside the spawned server thread, and `Drop` joins that thread after sending shutdown so helper failures surface in the owning test.

- [ ] **Step 2: Add a default concurrency test**

Build the configured model JSON list with the existing `model_json` helper near the top of `model_test.rs`. Use IDs that exist in `Catalog::builtin()` so table rendering can look them up:

```rust
let models = vec![
    model_json("claude-opus-4-7", "anthropic", true),
    model_json("claude-opus-4-6", "anthropic", true),
    model_json("claude-sonnet-4-5", "anthropic", true),
    model_json("claude-sonnet-4-6", "anthropic", true),
    model_json("claude-haiku-4-5", "anthropic", true),
];
```

Start the helper with those five configured models, `gate_expected = 4`, and no response delays. Wire the spawned CLI to the harness: each test in Steps 2-4 must call `context.set_http_target(&server.base_url)` (and `remove_provider_env(&mut cmd)`) before invoking the command, mirroring the existing tests in this file. Without this the CLI hits the default target and the harness receives zero requests.

Run `fabro model test` without `--jobs`, then assert:

```rust
assert!(
    output.status.success(),
    "model test should succeed:\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
);
assert!(
    !server.gate.timed_out(),
    "concurrency gate timed out before four requests arrived"
);
assert_eq!(
    server.gate.max_in_flight(),
    4,
    "default jobs should run four model tests concurrently before the gate releases"
);
```

The gate timeout makes a serial fallback fail with `max_in_flight() == 1` instead of hanging the test.

- [ ] **Step 3: Add an explicit `--jobs 2` concurrency test**

Reuse the same local-server helper and run:

```rust
cmd.args(["model", "test", "--jobs", "2"]);
```

Assert:

```rust
assert!(
    !server.gate.timed_out(),
    "concurrency gate timed out before two requests arrived"
);
assert_eq!(
    server.gate.max_in_flight(),
    2,
    "--jobs 2 should run two model tests concurrently before the gate releases"
);
```

- [ ] **Step 4: Add a stable JSON ordering test**

The existing `model_test_json_partitions_skip_and_fail` test pins the bulk JSON shape as `results[].model`; keep this ordering test on that same shape. Use the same five configured models, start the helper with `gate_expected = 5`, and run with `--jobs 5 --json` so all five requests are in flight before any response is allowed to complete. Set response delays so completion order is the reverse of listing order:

```rust
let response_delays = HashMap::from([
    ("claude-opus-4-7".to_string(), Duration::from_millis(250)),
    ("claude-opus-4-6".to_string(), Duration::from_millis(200)),
    ("claude-sonnet-4-5".to_string(), Duration::from_millis(150)),
    ("claude-sonnet-4-6".to_string(), Duration::from_millis(100)),
    ("claude-haiku-4-5".to_string(), Duration::from_millis(50)),
]);
```

Run:

```rust
cmd.args(["model", "test", "--jobs", "5", "--json"]);
```

Parse stdout and assert the JSON result order still matches listing order:

```rust
assert!(
    !server.gate.timed_out(),
    "concurrency gate timed out before five requests arrived"
);
assert_eq!(
    server.gate.max_in_flight(),
    5,
    "ordering test should have all five model requests in flight"
);
let json: serde_json::Value =
    serde_json::from_slice(&output.stdout).expect("invalid JSON output");
let models = json["results"]
    .as_array()
    .expect("results should be an array")
    .iter()
    .map(|row| row["model"].as_str().expect("model should be a string"))
    .collect::<Vec<_>>();
assert_eq!(
    models,
    vec![
        "claude-opus-4-7",
        "claude-opus-4-6",
        "claude-sonnet-4-5",
        "claude-sonnet-4-6",
        "claude-haiku-4-5",
    ]
);
```

- [ ] **Step 5: Keep the existing unconfigured-model regression test**

Do not remove or weaken `model_test_does_not_announce_unconfigured`; it already verifies that bulk mode does not POST unconfigured models. If the new Axum helper makes this easier to express later, keep the same assertion that the unconfigured test route receives zero calls.

- [ ] **Step 6: Run the model test integration suite**

Run:

```bash
cargo nextest run -p fabro-cli --test it cmd::model_test
```

Expected: all `cmd::model_test` tests pass.

### Task 5: Final Verification

**Files:**
- Verify: `lib/crates/fabro-cli/src/args.rs`
- Verify: `lib/crates/fabro-cli/src/commands/model.rs`
- Verify: `lib/crates/fabro-cli/tests/it/cmd/model_test.rs`

- [ ] **Step 1: Format check**

Run:

```bash
cargo +nightly-2026-04-14 fmt --check --all
```

Expected: formatting check passes.

- [ ] **Step 2: Clippy**

Run:

```bash
cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings
```

Expected: clippy passes without warnings.

- [ ] **Step 3: Full CLI tests if time allows**

Run:

```bash
cargo nextest run -p fabro-cli
```

Expected: `fabro-cli` tests pass. If this is too slow, record that focused integration coverage and clippy passed.

## Assumptions

- The public option name is `--jobs` with short flag `-j`.
- The default concurrency is exactly 4.
- A global concurrency limit is sufficient for this change; provider-specific throttling is out of scope and low-rate-limit users can pass `--jobs 1`.
- `--deep` does not force serial execution; users can combine `--deep --jobs 1` when they want that behavior.
- Progress lines in bulk mode may complete out of order; final table and JSON order must stay stable within the configured/unconfigured grouping described above.
- The new Axum concurrency harness remains inline in `model_test.rs` because it is specific to this command's concurrency behavior.
- `client.clone()` is expected to share client state through the existing `Arc` fields; auth refresh serialization is acceptable and does not invalidate request concurrency.
- Panics inside per-model futures are not caught; this matches the current command's treatment of programmer bugs.
- No server-side bulk endpoint is added.


## Completed stages
- **toolchain**: succeeded
  - Script: `command -v cargo >/dev/null || { curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y && sudo ln -sf $HOME/.cargo/bin/* /usr/local/bin/; }; cargo --version 2>&1`
  - Stdout:
    ```
    cargo 1.95.0 (f2d3ce0bd 2026-03-21)
    ```
  - Stderr: (empty)
- **preflight_compile**: succeeded
  - Script: `cargo check -q --workspace 2>&1`
  - Stdout: (empty)
  - Stderr: (empty)
- **preflight_lint**: succeeded
  - Script: `cargo +nightly-2026-04-14 clippy -q --workspace --all-targets -- -D warnings 2>&1`
  - Stdout: (empty)
  - Stderr: (empty)
- **implement**: succeeded
  - Model: claude-opus-4-7, 87.5k tokens in / 18.7k out
  - Files: /home/daytona/workspace/lib/crates/fabro-cli/src/args.rs, /home/daytona/workspace/lib/crates/fabro-cli/src/commands/model.rs, /home/daytona/workspace/lib/crates/fabro-cli/tests/it/cmd/model_test.rs


# Simplify: Code Review and Cleanup

Review changes vs. origin for reuse, quality, and efficiency. Fix any issues found.

## Phase 1: Identify Changes

Run git diff (or git diff HEAD if there are staged changes) to see what changed. If there are no git changes, review the most recently modified files that the user mentioned or that you edited earlier in this conversation.

## Phase 2: Launch Three Review Agents in Parallel

Use the Agent tool to launch all three agents concurrently in a single message. Pass each agent the full diff so it has the complete context.

### Agent 1: Code Reuse Review

For each change:

1. Search for existing utilities and helpers that could replace newly written code. Use Grep to find similar patterns elsewhere in the codebase — common locations are utility directories, shared modules, and files adjacent to the changed ones.
2. Flag any new function that duplicates existing functionality. Suggest the existing function to use instead.
3. Flag any inline logic that could use an existing utility — hand-rolled string manipulation, manual path handling, custom environment checks, ad-hoc type guards, and similar patterns are common candidates.

Note: This is a greenfield app, so focus on maximizing simplicity and don't worry about changing things to achieve it.

### Agent 2: Code Quality Review

Review the same changes for hacky patterns:

1. Redundant state: state that duplicates existing state, cached values that could be derived, observers/effects that could be direct calls
2. Parameter sprawl: adding new parameters to a function instead of generalizing or restructuring existing ones
3. Copy-paste with slight variation: near-duplicate code blocks that should be unified with a shared abstraction
4. Leaky abstractions: exposing internal details that should be encapsulated, or breaking existing abstraction boundaries
5. Stringly-typed code: using raw strings where constants, enums (string unions), or branded types already exist in the codebase

Note: This is a greenfield app, so be aggressive in optimizing quality.

### Agent 3: Efficiency Review

Review the same changes for efficiency:

1. Unnecessary work: redundant computations, repeated file reads, duplicate network/API calls, N+1 patterns
2. Missed concurrency: independent operations run sequentially when they could run in parallel
3. Hot-path bloat: new blocking work added to startup or per-request/per-render hot paths
4. Unnecessary existence checks: pre-checking file/resource existence before operating (TOCTOU anti-pattern) — operate directly and handle the error
5. Memory: unbounded data structures, missing cleanup, event listener leaks
6. Overly broad operations: reading entire files when only a portion is needed, loading all items when filtering for one

## Phase 3: Fix Issues

Wait for all three agents to complete. Aggregate their findings and fix each issue directly. If a finding is a false positive or not worth addressing, note it and move on — do not argue with the finding, just skip it.

When done, briefly summarize what was fixed (or confirm the code was already clean).