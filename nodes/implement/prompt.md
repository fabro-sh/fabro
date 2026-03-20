Goal: # Strip markdown headings and "Plan:" prefix from GOAL column in `fabro ps`

## Context
The `GOAL` column in `fabro ps` output sometimes shows raw markdown heading markers (`#`, `##`, etc.) and a `Plan:` prefix, which is noisy. We want to strip these for cleaner output.

## Change
**File:** `lib/crates/fabro-cli/src/commands/runs.rs` — `truncate_goal` function (line 229)

Before truncating, add two stripping steps:
1. Strip leading `#` characters and any whitespace after them (e.g. `## Fix bug` → `Fix bug`)
2. Strip a leading `Plan:` prefix (case-sensitive)
3. Trim whitespace after both strips

Current code:
```rust
fn truncate_goal(goal: &str, max_len: usize) -> String {
    let line = goal.lines().next().unwrap_or("");
    // ... truncation logic
}
```

Updated logic (inserted after extracting `line`, before truncation):
```rust
let line = line.trim_start_matches('#').trim();
let line = line.strip_prefix("Plan:").map(|s| s.trim()).unwrap_or(line);
```

## Verification
- `cargo test -p fabro-cli` — run existing tests
- `cargo clippy --workspace -- -D warnings` — lint check
- Manual: `fabro ps -a` to confirm goals render without `#` or `Plan:` prefixes


## Completed stages
- **toolchain**: success
  - Script: `command -v cargo >/dev/null || { curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y && sudo ln -sf $HOME/.cargo/bin/* /usr/local/bin/; }; cargo --version 2>&1`
  - Stdout:
    ```
    cargo 1.94.0 (85eff7c80 2026-01-15)
    ```
  - Stderr: (empty)
- **preflight_compile**: success
  - Script: `cargo check -q --workspace 2>&1`
  - Stdout: (empty)
  - Stderr: (empty)
- **preflight_lint**: success
  - Script: `cargo clippy -q --workspace -- -D warnings 2>&1`
  - Stdout: (empty)
  - Stderr: (empty)


Read the plan file referenced in the goal and implement every step. Make all the code changes described in the plan. Use red/green TDD.