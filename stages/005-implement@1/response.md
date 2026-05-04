All 12 tests pass, fmt check passes, full workspace clippy passes with `-D warnings`. 

## Summary

Implemented the bounded concurrency plan task by task:

**Task 1 — CLI option (`--jobs`/`-j`):**
- Added `jobs: usize` field to `ModelTestArgs` with default `4`, range `1..` (Clap rejects `0`). Used `RangedU64ValueParser::<usize>` since `value_parser!(usize)` doesn't expose `.range(...)`.
- Updated the `help` insta snapshot in `tests/it/cmd/model_test.rs` to match clap's interleaved output.

**Task 2 — Thread `jobs` through:**
- Added `jobs: usize` parameter to `test_models_via_server`.
- Destructured and forwarded `jobs` from `run_models`.

**Task 3 — Bounded concurrent bulk execution:**
- Added `use futures::{StreamExt, stream}`.
- Added private `CompletedModelTest` struct and `configured_model_test_status` helper.
- Replaced only the configured-loop in bulk mode with `stream::iter(...).buffer_unordered(jobs).collect()`, then `sort_by_key(index)` so final stdout/JSON order matches listing order. Single-model path and unconfigured loop unchanged. No `catch_unwind`.

**Task 4 — Tests:**
- Added an inline Axum-based concurrency harness (`ConcurrencyGate` using `Semaphore` + `released` flag with 15s timeout, dedicated thread + Tokio runtime, RAII `Drop`).
- Three new tests: default `--jobs 4`, explicit `--jobs 2`, and `--jobs 5 --json` ordering test with reverse-listing-order response delays.
- Existing `model_test_does_not_announce_unconfigured` regression preserved.

**Task 5 — Verification:**
- `cargo +nightly-2026-04-14 fmt --check --all`: ✅
- `cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings`: ✅ (added `#[expect(...)]` on the intentional sync `std::net::TcpListener::bind` and `std::thread::spawn` calls per the disallowed-types/methods workspace policy).
- `cargo nextest run -p fabro-cli --test it cmd::model_test`: 12/12 pass.

Pre-existing unrelated failure: `server_client::tests::http_target_transport_times_out_when_peer_accepts_without_http_response` requires `/root/.fabro/auth.lock` (verified to fail identically on `git stash`).