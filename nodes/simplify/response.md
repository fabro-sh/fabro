## Summary of Changes

All three review agents completed. Here's what was found and fixed:

### Issues Fixed

1. **Copy-paste duplication between `AgentHandler::simulate()` and `PromptHandler::simulate()`** — The two implementations were byte-for-byte identical (building `simulated_text`, setting `LAST_STAGE`, `LAST_RESPONSE`, `response_key`). Extracted a shared `simulate_llm_handler(node)` helper in `agent.rs` that both handlers now delegate to.

2. **Inconsistent `Outcome::success()` vs `Outcome::simulated()` in `CommandHandler::simulate()`** — Every other handler used `Outcome::simulated(&node.id)` as the base, but `CommandHandler` used `Outcome::success()` and manually set notes. Changed to use `Outcome::simulated()` for consistency (the custom notes string is still applied afterward).

3. **Missing trailing newlines in all 9 changed files** — The original implement commit stripped trailing newlines from every file it touched. Restored them all.

4. **Clarifying comment in `FanInHandler::simulate()`** — Added a comment noting that the `Outcome::simulated()` notes are intentionally overridden with handler-specific detail.

### Findings Not Actioned (correctly skipped)

- **ParallelHandler::simulate() runs branches sequentially** — Not a problem. Simulate is pure in-memory work (sub-microsecond per branch), parallelism would add overhead.
- **`truncate()` call on always-short string** — Negligible cost (fast-path no-op comparison).
- **`dispatch_handler()` overhead** — One boolean check per handler invocation; negligible vs. the handler work itself.
- **`shell_quote()` duplication across crates** — Pre-existing, not introduced by this diff.
- **Pre-existing clippy warning in `cli/pr.rs`** — Not related to this change.