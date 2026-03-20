The plan has been written to `/home/daytona/workspace/plan.md`.

It covers:
- **4 files to modify**: `fabro-agent/Cargo.toml` (add `rand` dep), `subagent.rs` (replace UUID with 8-char hex), `cli.rs` (remove 5 `short_id` truncations), `run_progress.rs` (remove 2 `short_id` truncations)
- **Step-by-step implementation** with exact line references and before/after code
- **Verification commands** to confirm correctness
- **Test case analysis** explaining why no test changes are needed