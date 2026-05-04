Net **-80 / +107 = +27** lines (offset by ~6 added doc comments and the new helpers). The duplicated logic is now centralized.

## Summary

Reviewed the implementation diff (~5,950 lines across 55 files) with three parallel review agents (reuse, quality, efficiency) and applied the following targeted cleanups:

### Bugs fixed (high priority)
1. **`prompt.rs:77` was swallowing cancellation.** `discover_memory(...).await.unwrap_or_default()` discarded `Err(Interrupted(Cancelled))` and continued to the LLM backend with empty docs, defeating the cancel-token plumbing into `discover_memory`. Now propagates as `Error::Cancelled`; other errors still degrade gracefully to `Vec::new()`.
2. **Daytona `session.close()` was unbounded.** `delete_session(...).await` had no timeout, so a stalled Daytona REST call could hang every cancellation/timeout path indefinitely. Wrapped in a `tokio::time::timeout(DAYTONA_SESSION_CLOSE_TIMEOUT, ...)` (10s) with a warn-on-elapse log.

### Duplication removed
3. **Three near-identical `Option<u64>` timeout-future blocks** in `local.rs`, `docker.rs`, and `daytona/mod.rs` collapsed into a single `crate::sandbox::optional_timeout(timeout_ms)` helper.
4. **Duplicated `tail` closure + detail-formatting** in `cli.rs` (TimedOut and post-exit non-success arms) extracted into module-level `tail_chars` / `cli_failure_detail` helpers.

### Efficiency
5. **`tokio::sync::Mutex<Vec<u8>>` replaced with `std::sync::Mutex<Vec<u8>>`** in the CLI streaming output callback. The critical sections were synchronous `extend_from_slice` calls; using an async mutex paid per-chunk scheduling overhead for nothing. Final reads also became synchronous.
6. **Redundant `u64::try_from(launch_start.elapsed().as_millis()).unwrap_or(u64::MAX)`** replaced with the existing `fabro_util::time::elapsed_ms` helper that was already imported but unused.

### Things flagged but intentionally not done
- `Session::link_to(token)` API redesign to remove `SessionCancelBridgeGuard` — appealing but a public-API change to fabro-agent beyond cleanup scope.
- Unifying `RunOptions.cancel_token` and `RunServices.cancel_token` into a single source of truth — would change cross-crate plumbing materially.
- Collapsing `Event::AgentCli{Completed,Cancelled,TimedOut}` into one variant with a tagged termination — already explicitly part of the implementation plan as three separate events.
- A generic `bail_if_cancelled(&token)?` helper for the ~10 repeated check-stanzas in `fabro-agent/src/{session,memory,skills}.rs` — the explicit form is clear and the helper would only save ~2 lines per site.

### Verification
- `cargo check --workspace --all-targets`: clean.
- `cargo +nightly-2026-04-14 fmt --check --all`: clean.
- `cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo nextest run -p fabro-workflow -p fabro-agent -p fabro-sandbox -p fabro-types`: 1716 passed, 0 failed.