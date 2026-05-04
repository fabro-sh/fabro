Workspace builds successfully. Let me give a summary of all the changes:

## Summary

I implemented the patches across all 7 sites described in the plan:

### Site 1: Worktree skipped on non-git cwd (`pipeline/initialize.rs`)
Added `tracing::warn!` with `worktree_mode` field plus `emitter.notice(RunNoticeLevel::Warn, "worktree_skipped_no_git", …)` in the `worktree_plan.is_some() && !worktree_created` branch.

### Site 2: Sandbox `setup_git` returned `Ok(None)` when git was expected (`pipeline/initialize.rs`)
Wrapped the `Ok(None) => {}` arm in `if sandbox.origin_url().is_some()` and emit `sandbox_git_unavailable` notice. Gating ensures the no-clone case stays silent.

### Site 3: Checkpoint push failure (`lifecycle/git.rs`)
Kept the existing `tracing::warn!(refspec, error, …)`, added `self.emitter.notice_with_tail(RunNoticeLevel::Warn, "git_push_failed", …, exec_output_tail)`.

### Site 4: Parallel base checkpoint failure (`handler/parallel.rs`)
Added `RunNoticeLevel` import, kept the existing `tracing::warn!`, added `services.run.emitter.notice(RunNoticeLevel::Warn, "parallel_base_checkpoint_failed", …)`.

### Site 5: GitHub token mint failure (`pipeline/initialize.rs`)
Added a structured `tracing::warn!(error = %e, …)` immediately before the existing `emitter.notice("github_token_failed", …)`.

### Site 6: LLM provider failover via `one_shot` path (multiple files)
- `handler/agent.rs`: extended `CodergenBackend::one_shot` trait signature with `&Arc<Emitter>` and `&StageScope`.
- `handler/llm/api.rs`: updated `AgentApiBackend::one_shot` to accept those params, replaced the existing `tracing::warn!` with `emitter.emit_scoped(&Event::Failover { … }, stage_scope)` so the `chat()`-path failover path is mirrored, eliminating the duplicate WARN.
- `handler/llm/cli.rs`: forwarded the new params from `BackendRouter::one_shot`.
- `handler/prompt.rs`: caller passes `&services.run.emitter` and `&stage_scope`; updated 3 test stubs.
- `tests/it/integration.rs`: updated the integration test stub.

### Site 7: Sandbox stdout/stderr drain failure (`fabro-sandbox/src/local.rs`)
Replaced silent `let _ = …` with `if let Err(err) = … { tracing::warn!(error = %err, stream = "stdout"/"stderr", …) }`. Tracing-only as planned (event-stream surface is out of scope).

### Test added
Added `initialize_emits_worktree_skipped_no_git_in_non_git_cwd` test in `pipeline/initialize.rs` which:
- Creates a non-git tmpdir as the sandbox cwd
- Configures `worktree_mode: Some(WorktreeMode::Always)`
- Drives `initialize` end-to-end
- Asserts a `RunNotice` event with `code == "worktree_skipped_no_git"` at `Warn` level is emitted

### Verification
- `cargo build --workspace` ✅
- `cargo nextest run -p fabro-workflow -p fabro-sandbox` ✅ (1173 tests, all passing)
- `cargo +nightly-2026-04-14 fmt --check --all` ✅
- `cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings` ✅

### Stable codes added
- `worktree_skipped_no_git` (Warn) — site 1
- `sandbox_git_unavailable` (Warn, gated on `origin_url.is_some()`) — site 2
- `git_push_failed` (Warn) — site 3
- `parallel_base_checkpoint_failed` (Warn) — site 4

(Site 5 reuses the existing `github_token_failed` notice; site 6 reuses the existing `agent.failover` event; no new RunNotice code introduced.)