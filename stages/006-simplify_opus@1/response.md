All clean. Here's a summary of what was fixed:

---

## Summary of fixes

### Critical bugs fixed

1. **TypeScript event names mismatch** (`apps/fabro-web/app/lib/run-events.ts`): The `STEERING_EVENTS` set used `"steering.received"` and `"steering.applied"` — names that don't exist in the Rust event system. Fixed to use the actual Rust event names: `"agent.steering.injected"`, `"agent.steering.attached"`, `"agent.steering.detached"`, `"agent.steer.buffered"`, `"agent.steer.dropped"`. Without this fix, no SWR cache invalidation would fire for any steering event, making the UI stale.

2. **`SteerComposer` never rendered** (`apps/fabro-web/app/routes/runs.tsx`): `steerRunId` state was declared and `setSteerRunId` was passed as `onSteer` to child components, but `BoardColumn` was called without the `onSteer` prop (line 797), and the `SteerComposer` component was never conditionally rendered. Fixed both: pass `onSteer={setSteerRunId}` to `BoardColumn` in the columns view, and render `<SteerComposer>` when `steerRunId` is set.

### Code quality improvements

3. **Redundant read-then-write lock** (`lib/crates/fabro-agent/src/session.rs`): Round token reset acquired a read lock to check `is_cancelled()`, then a separate write lock to replace. Simplified to a single write lock since `is_cancelled()` is cheap and the write lock subsumes read access.

4. **Duplicated `SteerKind` → `String` conversion** (`lib/crates/fabro-workflow/src/event/convert.rs`): Two identical `match kind { Append => "append", Interrupt => "interrupt" }` blocks. Added `strum::Display` with `#[strum(serialize_all = "lowercase")]` to `SteerKind` (`lib/crates/fabro-types/src/steering.rs`) and replaced both match blocks with `kind.to_string()`.

5. **Duplicated steering registration code** (`lib/crates/fabro-workflow/src/handler/llm/api.rs`): Identical 6-line `register + SteeringCompletionCoordinator + set_completion_coordinator` blocks in both the initial setup and the failover loop. Extracted into `register_session_steering(hub, stage_id, session)` helper.

6. **Session method duplication** (`lib/crates/fabro-agent/src/session.rs`): `Session::steer()` and `Session::interrupt_with()` duplicated the push-to-queue logic from `SessionControlHandle`. Refactored to delegate to `self.control_handle()`.

7. **Unnecessary clone** (`lib/crates/fabro-agent/src/session.rs`): `drain_steering()` cloned `msg` into `text` then moved `msg` into history. Renamed binding and cloned for the cheaper history push path instead.

8. **Missing trailing newlines**: 37 files had `\n` stripped from their final line. Restored all.