## Summary

### What was fixed

**Extracted `exec_local_workflow` helper to eliminate copy-paste** (`lib/crates/fabro-cli/tests/it/cmd/support.rs`):
- The new `setup_failed_run` function duplicated 13 lines of command construction from the existing `run_local_workflow` function (same args, env, timeout, current_dir).
- Extracted a lower-level `exec_local_workflow` helper that builds and executes the command without asserting success, returning the raw `Output`.
- Rewired `run_local_workflow` to call `exec_local_workflow` and then assert success (preserving its existing behavior).
- Simplified `setup_failed_run` to call `exec_local_workflow` in a single line instead of manually rebuilding the command.

### What was already clean (no action needed)

- **`--force` flag** (`args.rs`): Uses `#[arg(short, long)]` giving `-f`/`--force`, consistent with other destructive-override flags like `runs rm --force`. No short-flag conflicts.
- **Match pattern** (`pr/create.rs`): The guard-based match arm is idiomatic Rust and the `tracing::warn!` follows the project's logging strategy.
- **Test structure** (`pr_create.rs`): Two focused tests with clear names and expected-output snapshots. Each needs its own `setup_failed_run` call since tests must be independent.
- **No efficiency concerns**: The production code change adds a trivial match arm with no performance impact. Test helpers appropriately do the minimum work needed.