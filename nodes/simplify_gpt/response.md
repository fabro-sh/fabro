Reviewed the committed `--force` change and cleaned up a couple of issues.

What I fixed
- Extracted the PR status gate into a small helper in `src/commands/pr/create.rs`
  - centralizes the allowed statuses
  - keeps the main command flow simpler
- Changed the new `--force` warning to structured `tracing::warn!` fields
  - aligns with the repo logging strategy
- Simplified the new failed-run test setup in `tests/it/cmd/support.rs`
  - reused the existing local-workflow execution path instead of a one-off branch
  - added a small `LocalWorkflowExit` enum so success-required vs allow-failure is explicit
- Switched the failed-run assertion from raw JSON string parsing to typed loading
  - now uses `Conclusion::load(...)` and `StageStatus::Fail`

Review outcome
- No important efficiency issue in the production code.
- I did address the meaningful quality/reuse issues the agents found:
  - duplicate-ish test helper flow
  - raw stringly-typed `"fail"` assertion
  - inline status policy in `pr create`
  - unstructured warning log message

Verification
- `cargo fmt --all` ✅
- `cargo clippy -p fabro-cli -- -D warnings` ✅
- `cargo build -p fabro-cli && ./target/debug/fabro pr create --help` ✅
  - confirms `-f, --force` appears

Blocked by environment
- `cargo nextest run -p fabro-cli` could not be completed here because the tool environment failed:
  - first with a transient transport error
  - then with `unauthorized: authentication failed: Bearer token is invalid`

If you want, I can retry the full `nextest` run once tool access is healthy again.