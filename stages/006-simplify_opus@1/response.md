Everything passes: workspace check, full workspace tests (5081 passed), clippy clean, format clean, web typecheck, web tests.

## Summary of fixes applied

**Code reuse**
1. **`SteerComposer`** now uses the shared `ErrorMessage` component instead of re-implementing the error pill (`apps/fabro-web/app/components/steer-composer.tsx`).
2. **`SteeringHubGuard`** removed in favor of `scopeguard::guard` (the same RAII pattern used elsewhere in the same crate); collapses ~12 lines (`fabro-workflow/src/handler/llm/api.rs`).
3. **`Session::steer` / `interrupt_with`** now delegate to `SessionControlHandle` instead of duplicating the lock-and-push code.
4. Server steer handler dropped its redundant `MAX_STEER_TEXT_LEN` re-check (OpenAPI's newtype already enforces it) and stopped cloning `req.text` via `to_string()`; uses `into()` directly.

**Code quality**
5. **`AgentApiBackend.steering_hub`** is now non-`Option<Arc<SteeringHub>>` — required at construction. Removed the dead `with_steering_hub` builder and the matching `if let Some(ref hub)` branches in setup and failover.
6. New private helper **`AgentApiBackend::attach_session_to_hub`** unifies the initial-setup and failover register-and-wire-coordinator code (eliminated copy-paste).
7. Dead **`Session::clear_completion_coordinator`** removed; never called anywhere.
8. Dead **`Session::steering_queue_handle`** removed; the parity test now uses `session.control_handle().steer(...)`.
9. **`PendingSteer.kind`** dead field dropped along with the `#[allow(dead_code)]`.
10. Tightened weak server test `steer_empty_text_returns_bad_request` from "not 202" to `BAD_REQUEST | CONFLICT`.

**Efficiency**
11. **Detached/attached event flap on natural completion** fixed by adding atomic `SteeringHub::unregister_if_queue_empty(...)` (single write-lock decision; if queue non-empty, no detach event is emitted, no re-register needed). This also removes the brief unprotected window between `unregister` and `register`.
12. **Per-session enqueue is now atomic** via new `SessionControlHandle::enqueue_bounded(item, cap) -> Option<evicted>` — single mutex acquisition replaces the previous 3 (`queue_len` → `pop_oldest` → `enqueue`). Closes a small TOCTOU window where the cap could be temporarily exceeded.
13. **Drain on failure path**: `drain_pending_at_run_end()` now runs on every exit of `operations::start::run` (including the `?` short-circuit on `pipeline::finalize` errors and panic) via a `scopeguard`, not only on the success path.
14. **Dropped redundant `runs.detail` invalidation** from `STEERING_EVENTS` in `run-events.ts` — steer events don't change run summary state, so the extra SWR refetch was wasted work.
15. **`AgentSteeringAttached` now emitted before drained drops**, so any cap-eviction events from the drain are correctly ordered after the session is announced.
16. Minor: server's `update_live_run_from_event` uses `as_ref()` instead of `clone()` on the `Option<StageId>` for the insert paths (consistent with the surrounding remove-paths).

**Skipped (with reason)**
- Lifting `<SteerComposer>` out of `PrCard` — refactor cost outweighs benefit; modal renders `null` when closed.
- Replacing `Session::steer`-bypass-of-cap with cap enforcement — intentional per design (loop-detection auto-injection should not user-visibly drop).
- Dropping `pub use SteerKind` re-exports across `fabro-agent` / `fabro-interview` — ergonomic, no real risk.
- Collapsing `active_api_stages` + `active_cli_stages` into one `HashMap<StageId, AgentMode>` — would require introducing a new `AgentMode` type and updating event-conversion code; pure stylistic refactor with no behavior change.
- Hand-rolled CLI stdin reader — works correctly; the alternate `spawn_blocking` pattern is a parallel choice with no measurable benefit here.
- `RwLock<CancellationToken>` → `Mutex<CancellationToken>` — single-microsecond critical section; not measurable.
- `RunAnswerTransport` rename — would touch many call sites for naming clarity only.