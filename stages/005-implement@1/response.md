Here's a summary of everything implemented:

## Implementation Summary

### Step 1: Wire Protocol (`SteerKind` + `WorkerControlMessage::Steer`)
- **`lib/crates/fabro-types/src/steering.rs`** (new) — `SteerKind` enum (`Append`/`Interrupt`)
- **`lib/crates/fabro-types/src/lib.rs`** — Module and re-export
- **`lib/crates/fabro-interview/src/control_protocol.rs`** — Added `WorkerControlMessage::Steer { text, kind, actor }` variant with constructor and round-trip tests
- **`lib/crates/fabro-interview/src/lib.rs`** — Re-export `SteerKind`

### Step 2: Agent Layer (`Session` steering infrastructure)
- **`lib/crates/fabro-agent/src/types.rs`** — `AgentEvent::SteeringInjected` now carries `kind: SteerKind` and `actor: Option<Principal>`; added `CompletionCoordinator` trait
- **`lib/crates/fabro-agent/src/session.rs`** — Added `round_token`, `completion_coordinator` fields; `SessionControlHandle` struct; `interrupt_with()`, `control_handle()`, `set_completion_coordinator()` methods; refactored loop: drain_steering at top, composite cancel token with `tokio::select!` around LLM stream, mid-LLM interrupt with output clearing, mid-tool interrupt with tool_results preserved, `CompletionCoordinator` at natural completion
- **`lib/crates/fabro-agent/src/lib.rs`** — Exported `SessionControlHandle`, `CompletionCoordinator`

### Step 3: Workflow Layer (`SteeringHub` + plumbing)
- **`lib/crates/fabro-workflow/src/steering_hub.rs`** (new) — `SteeringHub` with register/unregister/deliver, pending buffer with overflow, run-end drain, unit tests
- **`lib/crates/fabro-workflow/src/handler/llm/api.rs`** — Register/unregister with RAII guard, `SteeringCompletionCoordinator` for close-the-door pattern, failover re-registration
- **`lib/crates/fabro-workflow/src/services.rs`** — `steering_hub` field on `RunServices`
- **`lib/crates/fabro-workflow/src/operations/start.rs`** — `steering_hub` on `StartServices` and `RunSession`
- **`lib/crates/fabro-workflow/src/pipeline/types.rs`** — `steering_hub` on `InitOptions`
- **`lib/crates/fabro-workflow/src/pipeline/initialize.rs`** — Pass hub through to backend
- **`lib/crates/fabro-cli/src/commands/run/runner.rs`** — Create hub, thread through control stream, handle `Steer` messages

### Step 4: Events
- **`lib/crates/fabro-types/src/run_event/agent.rs`** — Added `kind` field to `AgentSteeringInjectedProps`; new props: `AgentSteeringAttachedProps`, `AgentSteeringDetachedProps`, `AgentSteerBufferedProps`, `AgentSteerDroppedProps`
- **`lib/crates/fabro-types/src/run_event/mod.rs`** — New `EventBody` variants, `event_name()`, `is_known_event_name()`
- **`lib/crates/fabro-workflow/src/event/events.rs`** — New `Event` variants with trace
- **`lib/crates/fabro-workflow/src/event/names.rs`** — Event name strings
- **`lib/crates/fabro-workflow/src/event/convert.rs`** — Workflow→RunEvent body conversion
- **`lib/crates/fabro-workflow/src/event/stored_fields.rs`** — Actor lifting

### Step 5: Server
- **`docs/public/api-reference/fabro-api.yaml`** — `POST /runs/{id}/steer` endpoint with `SteerRunRequest` schema
- **`lib/crates/fabro-server/src/server.rs`** — Active stage tracking (`active_api_stages`, `active_cli_stages`), steer handler with status/steerability validation, route wiring, event-driven stage tracking

### Step 6: CLI
- **`lib/crates/fabro-cli/src/args.rs`** — `SteerArgs` struct, `Commands::Steer` variant
- **`lib/crates/fabro-cli/src/commands/steer.rs`** (new) — `fabro steer <run-id> <text> [--interrupt] [--text-stdin]`
- **`lib/crates/fabro-client/src/client.rs`** — `steer_run()` API client method

### Step 7: Web UI
- **`apps/fabro-web/app/components/steer-composer.tsx`** (new) — Floating composer with textarea, Send, Interrupt & Send buttons
- **`apps/fabro-web/app/lib/mutations.ts`** — `useSteerRun()` mutation hook
- **`apps/fabro-web/app/lib/run-events.ts`** — Steering events for SWR invalidation
- **`apps/fabro-web/app/routes/runs.tsx`** — Removed demo-mode gate, wired Steer button to composer