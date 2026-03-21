Here's a summary of what was fixed:

### Fixes Applied

1. **Eliminated ~120 lines of duplicated code in `resume.rs`** (604 → 420 lines, -184 lines): The two code paths (checkpoint-file and git-branch) had nearly identical blocks for: interviewer creation, dry-run detection, model/provider resolution, engine construction, retro generation, result printing, and exit code logic. Restructured into three functions:
   - `prepare_from_checkpoint()` — loads checkpoint/graph from files, creates simple local sandbox
   - `prepare_from_branch()` — resolves run ID from git, reads metadata, sets up worktree sandbox
   - `run_resumed()` — shared tail handling engine setup, execution, retro, and result display
   - A `ResumeContext` struct carries the divergent state between the two paths

2. **Extracted `default_run_dir()` helper in `run.rs`** — The run-dir defaulting logic (`~/.fabro/runs/YYYYMMDD-RUNID` or `/tmp/fabro-dry-run/RUNID`) was duplicated 4 times across `run.rs`, `create.rs`, and resume.rs. Now it's a single `pub(crate)` function reused by all three.

3. **Used `resolve_model_provider()` in resume.rs** — The original code had an inline simplified version that skipped model alias expansion (e.g., `--model opus` wouldn't resolve to `claude-opus-4-6`) and ignored graph `default_model`/`default_provider` attributes. Now it calls the full precedence chain through the existing tested helper.

4. **Simplified `RunSpec` Default impl** — Replaced the manual 16-field `impl Default for RunSpec` with `#[derive(Default)]` since all fields (`String`, `PathBuf`, `HashMap`, `bool`, `Option`) have built-in Default impls.

5. **Fixed missing trailing newlines** in `docs/core-concepts/how-fabro-works.mdx` and `docs/reference/cli.mdx`.

### Verification
- `cargo build --workspace` ✅
- `cargo test --workspace` ✅ (all tests pass)
- `cargo clippy --workspace -- -D warnings` ✅ (no warnings)
- `cargo fmt --check --all` ✅