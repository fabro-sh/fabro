Goal: # Plan: Extract `fabro resume` subcommand

## Context

Resume functionality is currently embedded in `fabro run` via `--resume` (checkpoint file) and `--run-branch` (git branch). This makes the `run` command's arg surface complex with `conflicts_with` annotations, and the UX is unintuitive — users must construct `fabro/run/RUN_ID` branch names manually. The new `fabro resume` subcommand provides a cleaner interface: `fabro resume RUN_ID_OR_PREFIX`.

## New `ResumeArgs` struct

```rust
pub struct ResumeArgs {
    /// Run ID, prefix, or branch (fabro/run/...)
    #[arg(required_unless_present = "checkpoint")]
    pub run: Option<String>,

    /// Resume from a checkpoint file (requires --workflow)
    #[arg(long)]
    pub checkpoint: Option<PathBuf>,

    /// Override workflow graph (required with --checkpoint)
    #[arg(long)]
    pub workflow: Option<PathBuf>,

    // Shared run options: run_dir, dry_run, auto_approve, goal, goal_file,
    // model, provider, verbose, sandbox, no_retro, ssh, preserve_sandbox
}
```

**Run ID resolution** (at top of `resume_command()`):
- If `run` starts with `fabro/run/` → strip prefix to get run_id
- Otherwise → call `find_run_id_by_prefix(&repo, &run)` (same as `rewind`/`fork`)
- Then construct branch name as `fabro/run/{run_id}`

## Files to modify

### 1. New: `lib/crates/fabro-cli/src/commands/resume.rs`
- Define `ResumeArgs` struct
- Move `run_from_branch()` body (~315 lines, `run.rs:1811-2125`) into `pub async fn resume_command()`
- Add run ID resolution logic at top (prefix → full ID via `find_run_id_by_prefix`)
- Add `--checkpoint` path: validate `--workflow` is present, load graph via `prepare_from_file()`, load checkpoint via `Checkpoint::load()`, then run engine

### 2. `lib/crates/fabro-cli/src/commands/run.rs`
- **Remove from `RunArgs`**: `resume` field (line 97-99), `run_branch` field (line 101-103)
- **Simplify `workflow`**: remove `required_unless_present = "run_branch"` — it's now always required
- **Update `conflicts_with_all`**: remove `"resume"`/`"run_branch"` from `preflight` (line 90) and `detach` (line 146)
- **Remove** `run_from_branch()` function (lines 1811-2125)
- **Remove** the `run_branch` early-return at top of `run_command()` (lines 602-604)
- **Simplify** engine call: remove `if let Some(ref checkpoint_path) = args.resume` branch (lines 1467-1476), always pass `None` for checkpoint
- **Widen visibility** of helpers used by `resume.rs`:
  - `local_sandbox_with_callback` (line 439) → `pub(crate)`
  - `resolve_ssh_config` (line 341) → `pub(crate)`
  - `resolve_ssh_clone_params` (line 355) → `pub(crate)`
  - `resolve_exe_config` (line 313) → `pub(crate)`
  - `resolve_exe_clone_params` (line 328) → `pub(crate)`
  - `resolve_preserve_sandbox` (line 261) → `pub(crate)`
  - `generate_retro` (line 2560) → `pub(crate)`
  - `write_finalize_commit` (line 2523) → `pub(crate)`
  - `print_final_output` (line 2128) → `pub(crate)`
  - `print_assets` (line 2149) → `pub(crate)`

### 3. `lib/crates/fabro-cli/src/commands/mod.rs`
- Add `pub mod resume;`

### 4. `lib/crates/fabro-cli/src/main.rs`
- Add `Resume(commands::resume::ResumeArgs)` to `Command` enum (near line 170, alongside `Rewind`/`Fork`)
- Add `Command::Resume(_) => "resume"` to command_name match
- Add dispatch handler (pattern follows `Rewind`/`Fork`/`Wait` — create styles, load cli_config, build github_app/git_author, call `resume_command()`)

### 5. `lib/crates/fabro-workflows/src/run_spec.rs`
- Remove `resume` and `run_branch` fields from `RunSpec`
- Add `#[serde(default)]` to `RunSpec` for backward compat with existing `spec.json` files
- Update `sample_spec()` in tests

### 6. `lib/crates/fabro-cli/src/commands/create.rs`
- Remove lines 86-87 that set `resume` and `run_branch` in the spec

### 7. `lib/crates/fabro-cli/src/main.rs` (`_run_engine` handler)
- Remove lines setting `resume` and `run_branch` when reconstructing `RunArgs` from `RunSpec`

### 8. `lib/crates/fabro-cli/src/commands/rewind.rs` (line 48-52)
- Change hint: `"To resume: fabro resume {run_id}"` (use short prefix)

### 9. `lib/crates/fabro-cli/src/commands/fork.rs` (line 56-60)
- Change hint: `"To resume: fabro resume {new_run_id}"` (use short prefix)

### 10. `lib/crates/fabro-cli/tests/cli.rs`
- Update/remove tests referencing `--resume` or `--run-branch` on `fabro run`
- Add basic parse test for `fabro resume`

### 11. Documentation (`docs/`)
- Update `docs/reference/cli.mdx`: add `fabro resume` section, remove `--resume`/`--run-branch` from `fabro run`
- Update `docs/execution/checkpoints.mdx`: change resume examples
- Update any other docs referencing `fabro run --run-branch` or `fabro run --resume`

## Verification

1. `cargo build --workspace` — compiles cleanly
2. `cargo test --workspace` — all tests pass
3. `cargo clippy --workspace -- -D warnings` — no warnings
4. Manual: `fabro resume --help` shows expected args
5. Manual: `fabro run --help` no longer shows `--resume` or `--run-branch`


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
- **implement**: success
  - Model: claude-opus-4-6, 91.6k tokens in / 34.3k out
  - Files: /home/daytona/workspace/docs/core-concepts/how-fabro-works.mdx, /home/daytona/workspace/docs/execution/checkpoints.mdx, /home/daytona/workspace/docs/reference/cli.mdx, /home/daytona/workspace/lib/crates/fabro-cli/src/commands/create.rs, /home/daytona/workspace/lib/crates/fabro-cli/src/commands/fork.rs, /home/daytona/workspace/lib/crates/fabro-cli/src/commands/mod.rs, /home/daytona/workspace/lib/crates/fabro-cli/src/commands/resume.rs, /home/daytona/workspace/lib/crates/fabro-cli/src/commands/rewind.rs, /home/daytona/workspace/lib/crates/fabro-cli/src/commands/run.rs, /home/daytona/workspace/lib/crates/fabro-cli/src/commands/start.rs, /home/daytona/workspace/lib/crates/fabro-cli/src/main.rs, /home/daytona/workspace/lib/crates/fabro-cli/tests/cli.rs, /home/daytona/workspace/lib/crates/fabro-workflows/src/run_spec.rs


# Simplify: Code Review and Cleanup

Review all changed files for reuse, quality, and efficiency. Fix any issues found.

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