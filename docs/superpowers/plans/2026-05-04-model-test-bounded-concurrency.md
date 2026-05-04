# Model Test Bounded Concurrency Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make bulk `fabro model test` run configured model checks concurrently with a bounded default of 4 requests.

**Architecture:** Keep the existing single-model path serial. In the bulk path, list models once, partition configured and unconfigured models as today, then run configured model POSTs through a `futures` stream with `buffer_unordered(jobs)`. Store each completed result with its original index and sort before rendering so final stdout and JSON remain deterministic.

**Tech Stack:** Rust, Clap, `futures::stream`, existing `fabro_client::Client`, existing `httpmock` integration tests, `cargo nextest`.

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
- Bulk mode completion progress may print in completion order, but final table and JSON rows for configured models must follow the model listing order.
- Existing failure semantics stay the same: configured model test failures increment `failures`; unconfigured listed models increment `skipped` but do not make bulk mode fail; a configured model returning `skip` after listing is a failure.

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

Change the function signature to accept `jobs`:

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

- [ ] **Step 4: Replace the serial configured loop**

Replace the existing `for info in &configured { ... }` loop in bulk mode with:

```rust
        let mut completed = stream::iter(configured.into_iter().enumerate())
            .map(|(index, info)| {
                let client = client.clone();
                async move {
                    if !json_output {
                        eprintln!("Testing {}...", info.id);
                    }
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

- [ ] **Step 5: Keep single-model progress unchanged**

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

- [ ] **Step 6: Run focused compile check**

Run:

```bash
cargo check -p fabro-cli
```

Expected: command succeeds.

### Task 4: Test Bounded Concurrency and Stable Output

**Files:**
- Modify: `lib/crates/fabro-cli/tests/it/cmd/model_test.rs`

- [ ] **Step 1: Add a default concurrency test**

Add a test that starts a small Axum server instead of `httpmock`, returns five configured models from `/api/v1/models`, delays each `/api/v1/models/{id}/test` request, and tracks max in-flight POSTs with `Arc<AtomicUsize>`. The assertion must be:

```rust
assert_eq!(
    max_in_flight.load(std::sync::atomic::Ordering::SeqCst),
    4,
    "default jobs should run at most four model tests concurrently"
);
```

Use model IDs that exist in `Catalog::builtin()` so table rendering can look them up when needed:

```rust
[
    "claude-opus-4-7",
    "claude-opus-4-6",
    "claude-sonnet-4-5",
    "claude-sonnet-4-6",
    "claude-haiku-4-5",
]
```

- [ ] **Step 2: Add an explicit `--jobs 2` concurrency test**

Reuse the same local-server helper and run:

```rust
cmd.args(["model", "test", "--jobs", "2"]);
```

Assert:

```rust
assert_eq!(
    max_in_flight.load(std::sync::atomic::Ordering::SeqCst),
    2,
    "--jobs 2 should cap concurrent model tests at two"
);
```

- [ ] **Step 3: Add a stable JSON ordering test**

Use delayed responses that finish in reverse order, run:

```rust
cmd.args(["model", "test", "--json"]);
```

Parse stdout and assert the output order still matches the list order:

```rust
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

- [ ] **Step 4: Run the model test integration suite**

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
- A global concurrency limit is sufficient for this change; provider-specific throttling is out of scope.
- Progress lines in bulk mode may complete out of order; final table and JSON order must stay stable.
- No server-side bulk endpoint is added.
