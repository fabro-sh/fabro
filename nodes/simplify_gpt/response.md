All clean. Here's a summary of what was fixed:

## Changes Made

### Fix 1: Eliminated `worktree_event_callback` duplication (Code Reuse)

The function was defined identically in both `parallel.rs` and `run.rs`. Moved it as `EventEmitter::worktree_callback(self: Arc<Self>) -> WorktreeEventCallback` in `fabro-workflows/src/event.rs` — the natural home since it bridges `WorktreeEvent` (sandbox layer) → `WorkflowRunEvent` (workflows layer) via the emitter.

Both call sites now use `Arc::clone(&emitter).worktree_callback()`. The `WorktreeEvent` and `WorktreeEventCallback` imports were removed from `parallel.rs` and `run.rs`.

### Fix 2: Removed no-op `git reset --hard` from `WorktreeSandbox::initialize()` (Efficiency)

After `git branch --force {branch} {sha}` (step 1) and `git worktree add {path} {branch}` (step 3), the worktree is already checked out at exactly `sha` — `git reset --hard {sha}` was resetting a fresh worktree to the commit it was already at. This saved one git invocation (a process spawn locally, or an SSH round-trip on remote) per worktree initialization.

Removed `WorktreeEvent::Reset` from the enum since it was only emitted from the now-deleted reset step. `WorkflowRunEvent::GitReset` (the serialized protocol event) was left untouched for historical log compatibility.

Updated 3 affected tests: renamed the events assertion test, removed `initialize_reset_uses_worktree_path_as_working_dir`, and updated command-count assertions from 4 → 3.