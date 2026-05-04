Exactly the four test files containing the inline snapshots — only expected changes, no spillover.

## Summary

The verify failure was 10 inline insta snapshot mismatches caused by the intentional new `worktree_skipped_no_git` `RunNotice` from the implementation. Every diff was a mechanical consequence of that one design choice:

- **9 stderr snapshots** (3 attach + 1 run + 5 dry_run_examples) gained a single `Warning: Worktree mode requested but no Git repository was found; running without a worktree. [worktree_skipped_no_git]` line — exactly the rendered `RunNotice` payload from the plan.
- **1 attach JSON snapshot** gained one `run.notice` event with `code: "worktree_skipped_no_git"`, `level: "warn"`.
- **1 dump file-listing snapshot** showed checkpoint indices shift by +1 (`0013/0017/0021 → 0014/0018/0022`), since the new event takes one sequence slot before the first checkpoint.

I installed `cargo-insta`, inspected each pending snapshot's diff to confirm it contained only the expected notice/renumber and no unrelated drift, then ran `cargo insta accept`. All four verify gates now pass cleanly:

- `cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings`: clean
- `cargo nextest run --workspace`: **5065 passed, 0 failed**, 182 skipped
- `cargo dev docs refresh` / `cargo dev docs check`: both reference docs up to date

Files changed (snapshot updates only):
- `lib/crates/fabro-cli/tests/it/cmd/attach.rs`
- `lib/crates/fabro-cli/tests/it/cmd/dump.rs`
- `lib/crates/fabro-cli/tests/it/cmd/run.rs`
- `lib/crates/fabro-cli/tests/it/workflow/dry_run_examples.rs`