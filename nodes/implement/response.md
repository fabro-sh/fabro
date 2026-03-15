All changes are correct. Let me provide a summary of everything that was implemented:

## Summary of Changes

### 1. `outcome.rs` — Added `Outcome::simulated()`
- New factory method `Outcome::simulated(node_id)` returning `Success` with `notes: "[Simulated] {node_id}"`
- Added test `outcome_simulated_factory`

### 2. `handler/mod.rs` — Added `simulate()` to trait + `dispatch_handler()`
- Added `Handler::simulate()` trait method with default implementation returning `Outcome::simulated(&node.id)`
- Added `dispatch_handler()` free function that routes to `simulate()` when `services.dry_run` is true, otherwise `execute()`
- Added tests: `dispatch_handler_routes_to_simulate_when_dry_run`, `dispatch_handler_routes_to_execute_when_not_dry_run`

### 3. `engine.rs` — Dispatch change
- Replaced `handler.execute(...)` with `crate::handler::dispatch_handler(handler, ...)` in `execute_with_retry()`

### 4. `handler/command.rs` — Moved dry-run to `simulate()`
- Added `simulate()` override with empty `command.output`/`command.stderr` context updates + note with script text
- Removed `if services.dry_run { ... }` block from `execute()`
- Updated test `dry_run_skips_execution` → `simulate_skips_execution` (tests `simulate()` directly)
- Added test `dispatch_routes_to_simulate_in_dry_run`

### 5. `handler/agent.rs` — Added `simulate()` override
- Sets `last_stage`, `last_response`, `response.{id}` context updates with simulated text
- Updated test `codergen_handler_simulation_mode` → `codergen_handler_simulate`

### 6. `handler/prompt.rs` — Added `simulate()` override
- Same context updates as agent handler (simulated response)
- Updated test `prompt_handler_simulation_mode` → `prompt_handler_simulate`

### 7. `handler/fan_in.rs` — Added `simulate()` override
- Calls `heuristic_select()` directly (no LLM, no git merge)
- Added test `fan_in_simulate_uses_heuristic`

### 8. `handler/human.rs` — Added `simulate()` override
- Auto-selects first choice from outgoing edges
- Sets `human.gate.selected`, `human.gate.label`, and `suggested_next_ids`
- Added test `simulate_selects_first_choice`

### 9. `handler/parallel.rs` — Added `simulate()` override + dispatch change
- Finds join node → sets `jump_to_node`; sets `parallel.branch_count`; dispatches children via `dispatch_handler()` (which recurses into their `simulate()`); skips all git worktree ops
- Updated child dispatch in `execute()` to use `dispatch_handler()`
- Added test `parallel_handler_simulate`

### Handlers using default `simulate()` (no override needed)
- **Start, Exit, Conditional, Wait**: Use default generic `[Simulated]` success — WaitHandler no longer sleeps during dry-run
- **SubWorkflowHandler**: Uses default; already propagates `dry_run` to child engine via `RunConfig`