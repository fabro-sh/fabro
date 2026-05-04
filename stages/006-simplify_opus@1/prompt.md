Goal: # Plan: end-to-end steering for running agents

## Context

`README.md` advertises "Steer running agents mid-turn." Today the agent core fully supports it (`Session::steer`, `drain_steering`, `Turn::Steering` → user message, `agent.steering.injected` event, parity tests). Everything north of that is missing or stubbed:

- `POST /runs/{id}/steer` is registered as `not_implemented` (501).
- Endpoint is not in the OpenAPI spec.
- No CLI command, no web UI wiring (only a placeholder demo-mode-gated "Steer" button on the running-runs board with no handler).
- No bridge from the server's HTTP layer through the worker subprocess into the live `Session`.

Two flavors are required:

- **Append** — push to the steering queue; agent picks it up at the next turn boundary (existing `Session::steer`).
- **Interrupt** — cancel in-flight LLM stream and tool calls in the current round, then deliver as the next user turn. New code in `Session`.

Steers can arrive when no agent stage is active, or when only non-agent stages are active — these **buffer** for the next API-mode session. Steers that arrive when only CLI-mode agent stages are active are **rejected** (no steerable target). Mixed runs with at least one API-mode agent active are accepted and broadcast.

## Decisions

- Scope: full stack — wire protocol, agent, worker, server, OpenAPI, CLI, web UI.
- Parallel stages (`max_parallel = 4`): broadcast to every active API-mode `Session` in the run.
- Status policy: accept only when run status is `running`. Reject `blocked` with a hint to use the interview-answer endpoint. Reject terminal states with 409.
- **CLI-mode steerability predicate (target-oriented, best-effort).** Server's view derives from asynchronously consumed events, so the 409 below is best-effort. Stale state can lead to a forwarded steer that the worker hub then buffers (`agent.steer.buffered`) or drops at run end (`agent.steer.dropped { reason: "run_ended" }`). UI surfaces both via SSE.
   1. ≥1 API-mode agent stage active → forward (broadcast).
   2. No active agent stages at all (between stages, non-agent stage, idle) → forward (worker buffers for next session).
   3. Active agent stages exist but none are API-mode → **best-effort 409**.
- Web UI shows the Steer button whenever `status === "running"`; rejection reason flows through the 409 response and is surfaced inline.
- Every steer carries an `actor: Principal` end-to-end (HTTP → envelope → worker → agent). Per `docs/internal/events-strategy.md:83`, `actor` lives only at top-level `RunEvent.actor`; **not** in event-specific props.
- Both transport variants must work: `RunAnswerTransport::Subprocess` (worker control JSONL) and `RunAnswerTransport::InProcess` (direct call into the in-process hub).
- **Round-token cancellation is the sole marker for steering interrupts.** No new `InterruptReason::SteerInterrupt` variant. The loop distinguishes terminal cancel from steer-interrupt by which token fired (`cancel_token` vs `round_token`). Existing `interrupt_reason` (used for `WallClockTimeout` / `Cancelled`) is unchanged.
- **Bounded queues.** Per-session steering queue cap = 32 messages; per-run pending buffer cap = 32 messages. Overflow evicts oldest (FIFO) and emits `agent.steer.dropped { count, reason }`. Sizes are workspace constants in `fabro-workflow`.
- **Buffered-steer fanout semantics:** buffered steers go to the **first** session that registers after an empty-active period. Sister parallel sessions registering at almost the same time do not replay the buffer. Documented limitation; per-stage targeting (deferred) is the natural future fix.

## Message flow

```
HTTP POST /runs/{id}/steer  { text, interrupt }   (auth → actor: Principal)
  → fabro-server handler
     ├─ validates status + steerability predicate from active_api_stages /
     │   active_cli_stages tracked from worker-emitted events
     ├─ Subprocess: WorkerControlEnvelope::steer(text, kind, actor) → control_tx
     │   → pump_worker_control_jsonl → worker stdin → apply_worker_control_line
     │   → SteeringHub.deliver(text, kind, actor)
     └─ InProcess: directly call SteeringHub.deliver(text, kind, actor) on the
                   hub stored alongside the in-process interviewer
  → SteeringHub.deliver:
     ├─ active API handles → broadcast: handle.queue.push((text, kind, actor))
     │       + if Interrupt: handle.round_token.cancel()
     └─ none → push to pending Vec<PendingSteer>
  → Session round loop: top-of-loop drain_steering() emits
     AgentEvent::SteeringInjected { text, kind } with actor flowing through
     internal event metadata; agent_actor_for_event lifts it to RunEvent.actor.
```

## Implementation

### 1. Wire protocol — extend `WorkerControlEnvelope`

**Files:** `lib/crates/fabro-types/src/lib.rs` (or new `steering.rs`), `lib/crates/fabro-interview/src/control_protocol.rs`

Define `SteerKind` in `fabro-types` (not `fabro-interview` — `fabro-interview` already depends on `fabro-types` per `control_protocol.rs:1`, so the canonical enum must live in the lower crate to avoid a cycle):

```rust
// fabro-types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SteerKind { Append, Interrupt }
```

`fabro-interview` re-exports it and adds the envelope variant:

```rust
// fabro-interview/src/control_protocol.rs
pub use fabro_types::SteerKind;

#[serde(rename = "run.steer")]
Steer {
    text:  String,
    kind:  SteerKind,
    actor: Principal,    // matches interview.answer
},
```

Add `WorkerControlEnvelope::steer(text, kind, actor)`. Round-trip serde tests for both kinds + actor next to existing tests at line 104+.

### 2. Agent — `interrupt_with` + round-level cancel + control handle

**Files:** `lib/crates/fabro-agent/src/session.rs`, `lib/crates/fabro-agent/src/types.rs`, `lib/crates/fabro-agent/src/error.rs`, `lib/crates/fabro-agent/src/tool_execution.rs`, `lib/crates/fabro-agent/tests/it/parity_matrix.rs`

Changes to `Session`:

- New field `round_token: Arc<RwLock<CancellationToken>>` — replaceable per round.
- Change `steering_queue` element type from `String` to `(String, SteerKind, Option<Principal>)` so per-message kind+actor survive into the emitted event.
- New method `interrupt_with(&self, text: String, actor: Option<Principal>)`: push `(text, Interrupt, actor)` and cancel `round_token`. **Does not** touch `interrupt_reason` — round-token cancellation alone marks the steer-interrupt path.
- Existing `steer(text)` updated to push `(text, Append, None)`.
- No new `InterruptReason` variant. Existing `WallClockTimeout` / `Cancelled` semantics are unchanged. The loop disambiguates by inspecting tokens:
   - `cancel_token.is_cancelled()` → terminal close (existing behavior).
   - `round_token.is_cancelled() && !cancel_token.is_cancelled()` → steer interrupt → continue.
- New method `control_handle(&self) -> SessionControlHandle` returning `Arc` clones of `steering_queue` and `round_token`. The hub stores the *handle*, not the `Session`. This avoids the ownership mismatch with `AgentApiBackend` (Session is owned by value and mutated via `process_input(&mut self)` in `handler/llm/api.rs:444-494`).
- `SessionControlHandle::steer(text, actor)` and `interrupt_with(text, actor)` thin wrappers — the hub calls these.
- Existing `AgentEvent::SteeringInjected` props gain `kind: SteerKind` only. Actor flows through internal event metadata (set on the emitted event), then `agent_actor_for_event` lifts it to top-level `RunEvent.actor` per the events strategy.

**Loop changes in `process_input` (lines 603–941):**

- **Move `drain_steering()` to the top of the loop body**, before `compact_if_needed`/`build_request`. Today: line 694 (before loop, once) and line 924 (after tools). After a SteerInterrupt `continue`, neither runs before the next request. Top-of-loop drain fixes this. Remove the line-694 pre-loop call (top-of-loop covers it on iter 1) and the line-924 post-tool call (next iter's top-of-loop covers it).
- At top of each iteration: if `round_token` is cancelled, replace with a fresh `CancellationToken`. (No `interrupt_reason` to clear — round-token cancellation is the marker.)
- Build per-round composite token from `cancel_token` (terminal) and `round_token` (per-round).
- **Cancellation propagation — two distinct strategies:**
   - **LLM stream awaits** (preemptive — safe to drop in-flight): wrap with `tokio::select!` against `composite.cancelled()` at:
      - `open_stream_with_retry(...)` and any internal retry-backoff `tokio::time::sleep`
      - `event_stream.next()` per chunk (so an idle stream that never produces another chunk doesn't pin the loop)
   - **Tool execution** (cooperative — must NOT drop the future): pass the composite token as a parameter to `execute_tool_calls`. It runs to completion, returning "Cancelled" entries internally for any in-flight tool (existing path: `tool_execution.rs:80`). Do **not** wrap in `select!` — dropping the future would lose synthesized cancel results and break the `tool_use`↔`tool_result` invariant.
- After LLM stream and after tools, branch on whether the round was interrupted:
   - **Mid-LLM interrupt** (`record_assistant_turn` at line 859 has not run yet): drop the unrecorded turn. **Also clear visible UI output**: if any `TextDelta` or `ReasoningDelta` was emitted in the dropped round, emit `AgentEvent::AssistantOutputReplace { text: "", reasoning: None }` before `continue` (mirrors the existing retry-clears-output pattern at session.rs:828). No tool_results needed because no `tool_use` was committed to history.
   - **Mid-tool interrupt** (assistant turn with `tool_use` blocks already recorded at line 859 before `execute_tool_calls` ran): `execute_tool_calls` runs to completion and returns **one `ToolResult` per `tool_use` block**. Content varies by tool — bash returns `Ok("Command cancelled.\n…")` (tools.rs:265-266), other tools may return partial output, an error message, or a synthetic Cancelled marker. The Anthropic invariant only requires one-per-block, not a specific content shape. Always push `Turn::ToolResults` with whatever `execute_tool_calls` returned (existing line 909-921 path), then branch on which token fired. Refactor the current `cancel_token.is_cancelled()` branch (lines 907-915) to: append tool_results unconditionally → close+return-Err (if `cancel_token` fired) or `continue` (if only `round_token` fired).
- **Append-during-final-response fix (race-safe, dependency-safe).** Today line 881-883 unconditionally `break` when `tool_calls.is_empty()`. A naive `if steering_queue.is_empty() { break }` still loses steers that arrive between the empty check and the function return because the hub still considers the session active. The full close-the-door dance (unregister → check → re-register-or-break) crosses a crate boundary the wrong way (`fabro-agent` does not depend on `fabro-workflow`; reverse cycles per `Cargo.toml:22`). Solution: small trait owned by fabro-agent, implemented in fabro-workflow.

   ```rust
   // fabro-agent
   pub trait CompletionCoordinator: Send + Sync {
       /// Called at natural completion (tool_calls empty).
       /// Return true to continue the loop (queue is non-empty),
       /// false to break. Implementor coordinates with whatever
       /// owns the steering source.
       fn on_natural_completion(&self) -> bool;
   }
   ```

   `Session` gains `completion_coordinator: Option<Arc<dyn CompletionCoordinator>>`, defaulting to `None` (preserves existing behavior — direct `Session::new` callers and tests just break naturally).

   Loop:

   ```rust
   if tool_calls.is_empty() {
       let should_continue = self.completion_coordinator
           .as_ref()
           .is_some_and(|c| c.on_natural_completion());
       if should_continue { continue; }
       break;
   }
   ```

   In fabro-workflow, an adapter implementing the trait holds `(Arc<SteeringHub>, StageId, SessionControlHandle)` and does:

   ```rust
   fn on_natural_completion(&self) -> bool {
       self.hub.unregister(self.stage_id.clone());     // serializes vs hub.deliver
       if self.handle.queue_is_empty() { return false; }
       self.hub.register(self.stage_id.clone(), self.handle.clone());
       true   // session's next iteration drains
   }
   ```

   `AgentApiBackend::run` builds the adapter, sets it on the session before `process_input`, and removes it after.

   **Hub locking discipline (race safety):** `SteeringHub::deliver` holds the `active` *read* lock for the entire push (clone handle + push to queue happen under the read lock). `unregister` takes the *write* lock. RwLock semantics serialize them — once `unregister` returns, no in-flight push can be racing. The post-unregister queue check sees a stable result.
- `cancel_token.is_cancelled()` (terminal) still returns `Err(interrupted_error())` and closes — unchanged.

Tests in `parity_matrix.rs`:

- `steering_interrupt_mid_llm_idle_stream` — fire `interrupt_with` while the LLM stream is open but producing no chunks. Assert interrupt takes effect within ~1s (proves `tokio::select!` is wired around `next()`).
- `steering_interrupt_mid_llm_streaming` — fire mid-stream after at least one `TextDelta` has been emitted; assert (a) `AssistantOutputReplace { text: "", reasoning: None }` is emitted before the next round (clears stale partial output in the UI), (b) next turn includes the steer text, (c) event has `kind: "interrupt"`.
- `steering_interrupt_mid_tool` — fire while a Bash tool is running; assert (a) `Turn::ToolResults` immediately follows the assistant tool-use turn (one ToolResult per tool_use, content unspecified — could be partial output, "Command cancelled", or an error message), then (b) `Turn::Steering` with the new text. No dangling `tool_use`. Test asserts shape, not content.
- `steering_no_dangling_tool_use_invariant` — assert that no `Turn::Steering` immediately follows a `Turn::Assistant` containing `tool_use` blocks without an intervening `Turn::ToolResults`. Asserts shape (one ToolResult per tool_use), not content. Guards against `select!`-around-tools regressions.
- `steering_append_kind_field` — fire `steer()` between rounds; assert event carries `kind: "append"`.
- `append_during_final_response_triggers_extra_round` — fire `steer()` while LLM is producing a final no-tool response. Assert agent does NOT exit `process_input` after `tool_calls.is_empty()`; instead runs another model turn that incorporates the steer. (Test uses a stub `CompletionCoordinator` impl that returns `true` once when the queue is non-empty — keeps the agent test free of workflow/hub dependencies.)

Queue overflow tests live at the **`SteeringHub` layer in fabro-workflow**, not here. Direct `Session::steer` callers intentionally bypass the cap, so the agent has nothing to test for overflow.

### 3. Worker — `SteeringHub` + control plumbing

**Files:** `lib/crates/fabro-cli/src/commands/run/runner.rs`, `lib/crates/fabro-workflow/src/services.rs`, `lib/crates/fabro-workflow/src/operations/start.rs`, `lib/crates/fabro-workflow/src/handler/llm/api.rs`

New type (in `fabro-workflow`, alongside `RunServices`):

```rust
// All locks below are std::sync — methods are sync and never await while holding them.
pub struct SteeringHub {
    active:  std::sync::RwLock<HashMap<StageId, SessionControlHandle>>,
    pending: std::sync::Mutex<VecDeque<PendingSteer>>,    // bounded, FIFO
    emitter: Arc<Emitter>,
}

struct PendingSteer { text: String, kind: SteerKind, actor: Option<Principal> }

const PER_SESSION_QUEUE_CAP: usize = 32;
const PER_RUN_PENDING_CAP:   usize = 32;

impl SteeringHub {
    pub fn deliver(&self, text: String, kind: SteerKind, actor: Option<Principal>);
    pub fn register(&self, stage_id: StageId, handle: SessionControlHandle);
    pub fn unregister(&self, stage_id: StageId);
    pub fn drain_pending_at_run_end(&self);    // emits agent.steer.dropped { reason: "run_ended" } if any
}
```

- `register` decides drain-vs-replace based on **current active-map state**, not history:
   - If `stage_id` is **not already in active** → insert + drain pending into this handle as `Append` + emit `agent.steering.attached`. Covers first-register-after-empty AND close-the-door re-register (which closes the gap where steers can buffer between unregister and re-register).
   - If `stage_id` **is already in active** → replace the handle, do **not** drain pending, do **not** re-emit `attached`. Covers failover (handle replaced under the same id without an intervening unregister).
- `unregister` is **idempotent**: `agent.steering.detached` fires only when `active.remove(stage_id)` returns `Some`. The close-the-door call removes-and-emits once; the RAII guard at function exit becomes a no-op (entry already gone). Prevents double-emit on natural completion.
- `deliver` broadcasts to active handles **or** pushes to pending — branched **under the active read lock** so the empty/non-empty decision is atomic with the push. Documented lock order: **active first, then queue or pending; never reverse.** All locks are `std::sync::{RwLock, Mutex}`; **no `.await` while holding any of them.** Sync methods make `CompletionCoordinator::on_natural_completion` callable from the agent loop without converting it to async (tokio locks would force `.await`). This makes the close-the-door pattern race-safe end-to-end.
- Internal helper `enqueue_into_session_queue(handle, item)` is used by both the broadcast path and the pending-flush path (called from `register`), guaranteeing identical cap enforcement and drop-event emission across both code paths.
- Sister parallel sessions registering immediately after the first don't replay the buffer (it was drained on the first register) — documented limitation; broadcast-to-future-sessions is deferred with the per-stage targeting feature.
- **Queue bounds enforced at the hub layer.** Before pushing into a session's `steering_queue` via `SessionControlHandle`, the hub checks `len() >= PER_SESSION_QUEUE_CAP` and evicts the front. Before pushing into `pending`, checks against `PER_RUN_PENDING_CAP`. On eviction, emits `agent.steer.dropped { count: 1, reason: "queue_full" }`. **Direct callers of `Session::steer` (loop-detection auto-injection at session.rs:931, tests) bypass the cap** — that's intentional; internal one-shot warnings shouldn't trigger user-facing drop events.

**Plumbing (explicit, not "via the same path"):**

In `runner.rs::execute()` (around lines 88–101): construct `let steering_hub = Arc::new(SteeringHub::new(emitter.clone()));` next to `interviewer` and `cancel_token`. Pass it both into:

1. `spawn_worker_control_stream(interviewer, cancel_token, steering_hub.clone())` — extend the function signature to accept the hub.
2. `StartServices.steering_hub: Arc<SteeringHub>` — new required field. Threaded through `operations::start` → `RunServices` → `EngineServices` → handler dispatch.

In `runner.rs::apply_worker_control_line` (lines 226–250): add a match arm:

```rust
WorkerControlMessage::Steer { text, kind, actor } => {
    steering_hub.deliver(text, kind, Some(actor));
}
```

In `AgentApiBackend::run()` (`handler/llm/api.rs:444-494`):

- Compute `let stage_id = stage_scope.stage_id();` from the existing `stage_scope` at line 476 (`StageScope::stage_id()` returns `StageId::new(node_id, visit)` per `stage_scope.rs:64-65`). Use this `StageId` everywhere — **not** the bare `node.id` string.
- After the `Session` is built/cached but before `process_input`, call `services.steering_hub.register(stage_id.clone(), session.control_handle())`.
- Use a `scopeguard`-style RAII guard so `unregister(stage_id.clone())` runs on success, error, and panic.
- **Failover (lines 527-572):** inside the failover loop, immediately after `session = new_session;` (line 545) and before `session.initialize().await` (line 556), call `services.steering_hub.register(stage_id.clone(), session.control_handle())` again. The hub overwrites the abandoned handle with the new one. The RAII unregister still works because the same `stage_id` is keyed.
- The hub never holds the `Session` — only the `Arc`-clones in `SessionControlHandle`. Sidesteps the ownership mismatch.

`AgentCliBackend::run()` is **not** modified — it never registers, so the hub's `active` set never includes CLI stages. The server's steerability predicate uses the `agent.steering.attached/detached` and `agent.cli.started/completed` events to know what's active.

**Run-end drain placement (async cleanup pattern).** Inside `operations::start` (`lib/crates/fabro-workflow/src/operations/start.rs`), wrap the pipeline execution into a result-returning block, then drain pending and flush events explicitly **before** propagating:

```rust
let result = run_pipeline(...).await;            // success or error
steering_hub.drain_pending_at_run_end();         // sync emit of agent.steer.dropped
store_progress_logger.flush().await;             // awaited flush moves them through the sink
result?
```

A `scopeguard` calling `drain_pending_at_run_end()` is **only** a last-ditch panic fallback — it cannot await the flush, so it's not the primary delivery path. The explicit pattern handles both success and error cleanly. Calling drain from the worker's outer wrap-up (after `operations::start` returns) would lose events because `store_progress_logger.flush().await` at line 818 already ran.

### 4. Server — HTTP handler + OpenAPI + per-stage tracking + InProcess support

**Files:** `docs/public/api-reference/fabro-api.yaml`, `lib/crates/fabro-server/src/server/handler/mod.rs`, `lib/crates/fabro-server/src/server.rs` (or new `handler/steer.rs`), `lib/crates/fabro-server/src/server/tests.rs`

OpenAPI: `POST /runs/{id}/steer` with body `SteerRequest { text: string (required, 1..8192), interrupt: boolean (default false) }`. Responses: `202 Accepted`, `400`, `404`, `409`, `503`. Tag: `Human-in-the-Loop`. Authenticated user becomes `Principal` for the envelope.

Handler (mirror cancel at `handler/lifecycle.rs:162`):

1. Look up `ManagedRun` via `AppState.runs`.
2. Validate, in order:
   - 404 if missing.
   - 409 if status is `blocked` with `code: "use_answer_endpoint"`, hint: `POST /runs/{id}/questions/{qid}/answer`.
   - 409 if terminal (`succeeded`/`failed`/`cancelled`/`archived`).
   - 409 if not `running`.
   - 409 if **target-oriented predicate** rejects: `active_api_stages.is_empty() && !active_cli_stages.is_empty()` with `code: "cli_agent_not_steerable"`, message: "All currently running agent stages are CLI-mode and cannot be steered."
   - Otherwise: forward.
3. **Transport branch on `ManagedRun.answer_transport`:**
   - `Subprocess { control_tx }`: send `WorkerControlEnvelope::steer(text, kind, actor)` with the existing 1s timeout pattern. Map `Timeout`/`Closed` to 503.
   - `InProcess { interviewer, steering_hub }`: directly call `steering_hub.deliver(text, kind, Some(actor))`. No envelope, no JSONL hop, no timeout — same hub the in-process worker would use. Requires storing an `Arc<SteeringHub>` alongside `interviewer` in `RunAnswerTransport::InProcess` (`server.rs:245`). The in-process spawn site `execute_run_in_process` (line 2541) creates and stores both.
4. Return 202.

**Tracking active-stage modes (server side):** `ManagedRun` gains:

```rust
active_api_stages: HashSet<StageId>,   // primary: agent.steering.attached/detached
active_cli_stages: HashSet<StageId>,   // primary: agent.cli.started; backstops below
```

Plain `HashSet` (no inner lock) — `ManagedRun` is already accessed under `state.runs.lock()` (`server.rs:441` AppState definition; mutation pattern at `server.rs:1724, 1735` for the existing `accepted_questions: HashSet<String>` field at `server.rs:196`). Adding inner `Mutex<HashSet>` would be redundant nested locking.

Updated by the server's existing event-consumption path. **No reuse of `agent.session.started/ended`** — those events do not reliably fire per stage invocation: `Session::initialize()` (and thus `SessionStarted`) is skipped for reused sessions in `api.rs:490`, and `SessionEnded` only fires on explicit `close()`. The hub-emitted `attached/detached` events fire deterministically per `register/unregister` call inside `AgentApiBackend::run`, which is exactly the steerable window.

**Backstops to prevent leaks** (CLI tracking is fragile because `AgentCliStarted` at cli.rs:511 and `AgentCliCompleted` at cli.rs:648 are 137 lines apart with fallible operations between, and the existing CLI cancel bug means many error paths skip the completion emit):

- On `stage.completed` **and** `stage.failed` (any kind): remove the stage_id (read from top-level `RunEvent.stage_id`) from **both** `active_api_stages` and `active_cli_stages`. Both events fire from the workflow lifecycle (`lifecycle/event.rs:153, 220, 271`); covering only `stage.completed` would leak on the failure path — exactly where the existing CLI cancel bug already strands stages.
- On terminal run events (`run.completed` / `run.failed`): clear both sets entirely. (Cancellation is folded into `run.failed` via its `reason` field — there is no separate `run.cancelled` event in `lib/crates/fabro-types/src/run_event/mod.rs:87-90`.)

Implementation note for a follow-up PR (out of scope here, in the same area as the existing CLI-cancel debt): wrap the CLI backend's `AgentCliCompleted` emission in a scopeguard so it always fires regardless of error path.

**CLI-only rejection is best-effort.** Server consumes events asynchronously through the run-store subscription path (`server.rs:2023`), so its view of `active_api_stages` / `active_cli_stages` lags actual worker state by a small window. A steer that the server forwards based on a stale view will be handled correctly by the worker hub: if no API session is registered by arrival, the steer buffers and emits `agent.steer.buffered`, which the UI surfaces. The 409-on-all-CLI gate is an optimization for the synchronous user-feedback case; the worker-side hub is the authoritative safety net. Authoritative server-side rejection (round-tripping a confirmation back through the worker control plane) is out of scope.

**After OpenAPI changes, regenerate clients (per `CLAUDE.md` API workflow):**

```bash
cargo build -p fabro-api                                # regenerates Rust client via build.rs + progenitor
cd lib/packages/fabro-api-client && bun run generate    # regenerates TypeScript Axios client
```

Both must run before `bun run typecheck` in `apps/fabro-web` will pass.

### 5. CLI — `fabro steer`

**Files:** `lib/crates/fabro-cli/src/args.rs`, new `lib/crates/fabro-cli/src/commands/steer.rs`, `lib/crates/fabro-cli/src/commands/mod.rs`, `lib/crates/fabro-cli/src/server_client.rs`, `lib/crates/fabro-cli/src/main.rs`

Add a new top-level `Commands::Steer(SteerArgs)` (sibling to `Commands::RunCmd`, `Commands::Exec`, etc. in `args.rs:1016`). New top-level command from scratch — no existing `fabro cancel` to mirror (cancel today is Ctrl+C in attached or HTTP-direct).

```
fabro steer <run-id> <text> [--interrupt]
fabro steer <run-id> --text-stdin [--interrupt]   # editors / pipes
```

Implementation calls a new `server_client.steer_run(run_id, text, kind)` via the regenerated typed API client. Error mapping mirrors the cancel HTTP path.

### 6. Web UI

**Files:** `apps/fabro-web/app/components/steer-composer.tsx` (new), `apps/fabro-web/app/components/steer-composer.test.tsx` (new), `apps/fabro-web/app/lib/mutations.ts`, `apps/fabro-web/app/lib/run-events.ts`, `apps/fabro-web/app/hooks/use-run-toasts.ts` (new), `apps/fabro-web/app/routes/runs.tsx`, `apps/fabro-web/app/routes/run-detail.tsx`

**Composer:** new `SteerComposer` component — modal/popover with textarea, primary "Send" button, secondary "Interrupt" button, same Enter / Shift+Enter affordance as `interview-dock.tsx`. Reuse `ErrorMessage` from `ui.tsx`.

**Mutation:** `useSteerRun(runId)` in `lib/mutations.ts` mirroring `useSubmitInterviewAnswer` (lines 97–117). On 409 with `code: "cli_agent_not_steerable"`, surface inline ("All running agent stages are CLI-mode and can't be steered."). Invalidates run detail on success.

**Surfacing:** wire the existing "Steer" button in `routes/runs.tsx:42, 365–369`:
- Remove the demo-mode gate at lines 437–439.
- Open `SteerComposer` on click.

Add a "Steer" button to `run-detail.tsx` page header (only when `statusKind === "running"`), opening the same composer.

**Toast dispatch:** `lib/run-events.ts` only resolves SWR-invalidation keys (line 44) — does not dispatch toasts. Add `useRunToasts(runId)` hook in `app/hooks/use-run-toasts.ts` that subscribes to the same SSE stream (via existing `subscribeToSharedEventSource`) and calls `useToast().push(...)` for steer-related events, deduping by event id. Mount from `run-detail.tsx` next to `useRunEvents`. SSE invalidation in `run-events.ts` still gets the new event names so SWR refetches; toast is the new hook's job.

Toast copy:
- `agent.steering.injected` with `kind: "append"` → "Steer delivered."
- `agent.steering.injected` with `kind: "interrupt"` → "Agent interrupted — your message is the next turn."
- `agent.steer.buffered` → "Steer queued — will apply when an agent stage runs."
- `agent.steer.dropped` with `reason: "queue_full"` → "Steer rate limit reached; oldest queued steer dropped."
- `agent.steer.dropped` with `reason: "run_ended"` → "Run ended before queued steer(s) could apply."

Note: keep `InterviewDock` semantics untouched — `blocked` runs only. Steer composer handles `running` only. Mutually exclusive surfaces.

## Events

- `agent.steering.injected` — **modified**. `AgentSteeringInjectedProps` (in `lib/crates/fabro-types/src/run_event/agent.rs`) gains `kind: "append" | "interrupt"`. Actor lives at top-level `RunEvent.actor` only — set via `agent_actor_for_event` from the `actor` carried internally on the `AgentEvent::SteeringInjected` variant. Per `docs/internal/events-strategy.md:83`.
- `agent.steering.attached` / `agent.steering.detached` — **new**. Emitted by `SteeringHub::register` (only when newly inserted, not on replace) / `unregister` (only when active.remove returned Some). Workflow `Event` variants carry `StageId` internally; `stored_event_fields_for_variant` lifts to top-level `RunEvent.stage_id` (mod.rs:36) so generic event consumers see it where they expect. **Props are empty** — no duplicate `stage_id` in props. Distinct names from existing `agent.session.started/ended` to avoid confusion.
- `agent.steer.buffered` — **new**. Emitted by the worker hub when a steer arrives with no active session and is parked. Carries `{ kind }` in props (no actor in props — top-level only).
- `agent.steer.dropped` — **new**. Two shapes:
   - `reason: "queue_full"`, `count: 1` — single-item drop with a known dropped steer. Carries the dropped item's `actor` internally; `stored_event_fields_for_variant` lifts it to top-level `RunEvent.actor`.
   - `reason: "run_ended"`, `count: N` — aggregate; possibly multiple actors. **No user actor** at top level (system actor); aggregation loss documented.

For each new event: typed props in `lib/crates/fabro-types/src/run_event/agent.rs`, variant on `EventBody` in `mod.rs`, workflow conversion in `fabro-workflow/src/event/convert.rs`, name in `fabro-workflow/src/event/names.rs`.

**Actor lifting (different paths for different emitters):**
- `agent.steering.injected` is agent-emitted (`AgentEvent::SteeringInjected`). Carry `actor` on the internal variant; lift via `agent_actor_for_event` (`stored_fields.rs:198`).
- `agent.steer.buffered` is workflow-hub-emitted (`SteeringHub::deliver`, no agent involvement). Add `actor: Option<Principal>` to its workflow `Event` variant; lift via `stored_event_fields_for_variant` (`stored_fields.rs:57`).
- `agent.steer.dropped` with `reason: "queue_full"`: carry dropped item's `actor` on the workflow `Event` variant; lift via `stored_event_fields_for_variant`.
- `agent.steer.dropped` with `reason: "run_ended"`: system actor, no user actor lifted.
- `agent.steering.attached/detached`: lifecycle, no user actor.

## Test strategy

- **Unit** — control-protocol round-trip including actor (`fabro-interview`); `SteerKind` round-trip in `fabro-types`; `SteeringHub` buffer + broadcast + register-drains-or-replaces by active-map state + idempotent unregister + `drain_pending_at_run_end` + **per-session queue overflow drops oldest** + **per-run pending overflow drops oldest** (both at the hub layer, asserting `agent.steer.dropped { reason: "queue_full" }` carries the correct actor at top level); server's steerability predicate over mixed `(active_api_stages, active_cli_stages)` sets.
- **Agent integration** — `parity_matrix.rs` scenarios listed above (idle-stream interrupt, mid-streaming interrupt with stale-output clear, mid-tool interrupt with shape-only assertion, no-dangling-tool-use invariant, append kind field, append-during-final-response triggers extra round). Idle-stream guards against `tokio::select!` regression on stream awaits; no-dangling guards against `select!`-around-tools regression. **Queue overflow is exclusively a hub-layer test** — direct `Session::steer` callers intentionally bypass the cap.
- **Workflow event conversion** — new test that builds an `AgentEvent::SteeringInjected { actor, kind, text }`, runs it through the conversion machinery, and asserts the resulting `RunEvent` has `kind` in props (no `actor` in props) and the user actor at top-level `RunEvent.actor`. Without this test, a future refactor of `agent_actor_for_event` (`stored_fields.rs:198`) could silently regress steering to `None` — it currently falls through for all variants except `AssistantMessage`/`ToolCall*`.
- **Server** — handler unit tests for the full status + steerability matrix (running OK, blocked → 409, terminal → 409, missing → 404, all-CLI → 409, mixed API+CLI → accept, no-active-agent → accept, in-process transport delivers via direct hub call, subprocess delivers via control_tx). Existing `in_process_answer_transport_cancel_run_cancels_pending_interviews` (tests.rs:1710) is the template for an in-process steer test.
- **CLI** — happy-path `fabro steer` against a fake server, plus `--text-stdin`.
- **Web** — `SteerComposer` (textarea + two buttons + disabled-when-empty); `useSteerRun` posts the right body and surfaces 409 inline; `useRunToasts` dispatches expected toasts with dedup.

## Verification

```bash
# Backend
cargo build --workspace
cargo nextest run --workspace
cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings

# API client regeneration (after OpenAPI changes)
cargo build -p fabro-api
cd lib/packages/fabro-api-client && bun run generate
cd ../../..

# Web (depends on regenerated TS client above)
cd apps/fabro-web && bun run typecheck && bun test

# Manual end-to-end (subprocess transport)
fabro server start                                    # terminal 1
fabro run repl                                        # terminal 2 — start a run
fabro steer <id> "Try a different approach"          # terminal 3 — append
fabro steer <id> "Stop, do X instead" --interrupt    # terminal 3 — interrupt
# In browser: open the run, click Steer, type and Send / Interrupt; verify SSE event arrives and toast renders.

# Manual end-to-end (in-process transport)
# Use a registry override / test config that selects InProcess transport,
# repeat steer + interrupt; same observable behavior, no JSONL hop.
```

## Out of scope

- Per-stage steer targeting in UI/CLI (broadcast only for v1).
- Persisting unconsumed steers across run resume.
- Steering of non-agent stages (commands, conditionals, parallel).
- Steering CLI-mode agent stages (claude/codex/gemini): structurally impossible without changes to those external CLIs. Server returns 409 only when *all* active agent stages are CLI-mode.
- Fixing the pre-existing CLI-mode cancel bug (RunCancel currently ignored; subprocesses orphan). Tracked separately.
- Slack-driven steering.
- Auth/permission model beyond what `cancel` already does — same caller can do both.
- Worker-side `stdin` protocol versioning beyond the existing `v: 1` envelope.


## Completed stages
- **toolchain**: succeeded
  - Script: `command -v cargo >/dev/null || { curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y && sudo ln -sf $HOME/.cargo/bin/* /usr/local/bin/; }; cargo --version 2>&1`
  - Stdout:
    ```
    cargo 1.95.0 (f2d3ce0bd 2026-03-21)
    ```
  - Stderr: (empty)
- **preflight_compile**: succeeded
  - Script: `cargo check -q --workspace 2>&1`
  - Stdout: (empty)
  - Stderr: (empty)
- **preflight_lint**: succeeded
  - Script: `cargo +nightly-2026-04-14 clippy -q --workspace -- -D warnings 2>&1`
  - Stdout: (empty)
  - Stderr: (empty)
- **implement**: succeeded
  - Model: claude-opus-4-6, 261.9k tokens in / 102.9k out
  - Files: /home/daytona/workspace/apps/fabro-web/app/components/steer-composer.tsx, /home/daytona/workspace/apps/fabro-web/app/lib/mutations.ts, /home/daytona/workspace/apps/fabro-web/app/lib/query-keys.ts, /home/daytona/workspace/apps/fabro-web/app/lib/run-events.ts, /home/daytona/workspace/apps/fabro-web/app/routes/runs.tsx, /home/daytona/workspace/docs/public/api-reference/fabro-api.yaml, /home/daytona/workspace/lib/crates/fabro-agent/src/lib.rs, /home/daytona/workspace/lib/crates/fabro-agent/src/session.rs, /home/daytona/workspace/lib/crates/fabro-agent/src/types.rs, /home/daytona/workspace/lib/crates/fabro-agent/tests/it/parity_matrix.rs, /home/daytona/workspace/lib/crates/fabro-cli/src/args.rs, /home/daytona/workspace/lib/crates/fabro-cli/src/commands/mod.rs, /home/daytona/workspace/lib/crates/fabro-cli/src/commands/run/runner.rs, /home/daytona/workspace/lib/crates/fabro-cli/src/commands/steer.rs, /home/daytona/workspace/lib/crates/fabro-cli/src/main.rs, /home/daytona/workspace/lib/crates/fabro-client/src/client.rs, /home/daytona/workspace/lib/crates/fabro-interview/src/control_protocol.rs, /home/daytona/workspace/lib/crates/fabro-interview/src/lib.rs, /home/daytona/workspace/lib/crates/fabro-server/src/server.rs, /home/daytona/workspace/lib/crates/fabro-types/src/lib.rs, /home/daytona/workspace/lib/crates/fabro-types/src/run_event/agent.rs, /home/daytona/workspace/lib/crates/fabro-types/src/run_event/mod.rs, /home/daytona/workspace/lib/crates/fabro-types/src/steering.rs, /home/daytona/workspace/lib/crates/fabro-workflow/src/event/convert.rs, /home/daytona/workspace/lib/crates/fabro-workflow/src/event/events.rs, /home/daytona/workspace/lib/crates/fabro-workflow/src/event/names.rs, /home/daytona/workspace/lib/crates/fabro-workflow/src/event/stored_fields.rs, /home/daytona/workspace/lib/crates/fabro-workflow/src/handler/llm/api.rs, /home/daytona/workspace/lib/crates/fabro-workflow/src/lib.rs, /home/daytona/workspace/lib/crates/fabro-workflow/src/lifecycle/git.rs, /home/daytona/workspace/lib/crates/fabro-workflow/src/operations/start.rs, /home/daytona/workspace/lib/crates/fabro-workflow/src/pipeline/execute/tests.rs, /home/daytona/workspace/lib/crates/fabro-workflow/src/pipeline/finalize.rs, /home/daytona/workspace/lib/crates/fabro-workflow/src/pipeline/initialize.rs, /home/daytona/workspace/lib/crates/fabro-workflow/src/pipeline/retro.rs, /home/daytona/workspace/lib/crates/fabro-workflow/src/pipeline/types.rs, /home/daytona/workspace/lib/crates/fabro-workflow/src/services.rs, /home/daytona/workspace/lib/crates/fabro-workflow/src/steering_hub.rs, /home/daytona/workspace/lib/crates/fabro-workflow/src/test_support.rs


# Simplify: Code Review and Cleanup

Review changes vs. origin for reuse, quality, and efficiency. Fix any issues found.

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