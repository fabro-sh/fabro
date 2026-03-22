# False Positives Filtered from Code Review

## 1. Git checkpoint disabled in `prepare_from_checkpoint` path

**Summary:** `prepare_from_checkpoint` constructs RunConfig with `git_checkpoint_enabled: false`, `run_branch: None`, `meta_branch: None`, and `host_repo_path: None`, preventing git checkpoints after each stage.

**Why filtered:** This is intentional behavior, not a bug. The `--checkpoint <file>` path is for resuming from a raw checkpoint file without any git branch context. There's no worktree or branch to checkpoint into. The `prepare_from_branch` path correctly enables git checkpointing because it has a branch to work with. The engine still writes `checkpoint.json` to the `run_dir`, so the user can resume from the checkpoint file again if needed. This is a deliberate design choice for the simpler checkpoint-file resume path.

## 2. `status_guard` defused after `engine.run_with_lifecycle()` instead of before

**Summary:** The status_guard that writes `SandboxInitFailed` is defused after `engine.run_with_lifecycle()` returns (line 1022), whereas in `run.rs` it is defused before the engine call (line 1494). If the engine panics during workflow execution, the guard writes a misleading `SandboxInitFailed` status.

**Why filtered:** This is a nit. The incorrect status only manifests during panics in the engine (uncommon in production Rust code), and the impact is limited to a misleading status string in `fabro ps` output. A senior engineer would not prioritize this during code review of a large feature extraction. The bug itself was marked `nit` severity.
