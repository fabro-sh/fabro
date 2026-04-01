All 28 doctor tests pass, and clippy is clean across the entire workspace.

**Summary of findings:**

All test failures in the verify step are **pre-existing and unrelated** to our changes:

1. **`cmd::logs::logs_follow_detached_run_streams_until_completion`** — Flaky snapshot assertion; passes when run individually. Race condition.
2. **`cmd::attach::attach_before_completion_streams_to_finished_state`** — Timeout; fails even on the base commit (`a496c243`). No changes to this file.
3. **`fabro-store disk_projecting::tests::projection_error_callback_runs_on_disk_failure`** — Pre-existing; file untouched by our changes.
4. **`fabro-workflow asset_snapshot::tests::write_asset_manifest_failure_cleans_up_stage_dir`** — Pre-existing; file untouched by our changes.
5. **`fabro-server api::sse_events::sse_stream_contains_expected_event_types`** — Flaky race condition; file untouched by our changes (the only `api.rs` change was adding a needed `Arc` import in a different module).

Our changes are limited to:
- `lib/crates/fabro-cli/src/commands/doctor.rs` — New `check_storage_dir` function + state gathering + unit tests (all pass)
- `lib/crates/fabro-cli/tests/it/cmd/doctor.rs` — 2 new integration tests (all pass)
- `lib/crates/fabro-server/tests/it/api.rs` — Added missing `use std::sync::Arc` import (fixes a pre-existing compile error in the `mtls_e2e` module on Linux)

No code changes are needed to fix the verify failures — they are all pre-existing flaky tests.