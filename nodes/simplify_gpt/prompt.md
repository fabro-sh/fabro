Goal: # Canonical `Blocked` Run Status Plan

## Summary

- Make `Blocked` a first-class shared run status across the durable projection, server, OpenAPI, generated TypeScript client, web UI, and CLI.
- Keep `Paused` separate. `Paused` remains operator intent; `Blocked` means the run cannot proceed until an external condition is resolved.
- This is a full status-unification pass: align the shared contract on `submitted`, `queued`, `starting`, `running`, `blocked`, `paused`, `removing`, `completed`, `failed`, and `cancelled`; remove `dead` from the canonical serialized lifecycle.
- No alerting/email in this pass. `BlockedReason` is introduced now so notification work can key off a stable domain contract later.

## Key Changes

- Canonical status contract: update [docs/api-reference/fabro-api.yaml](/Users/bhelmkamp/p/fabro-sh/fabro/docs/api-reference/fabro-api.yaml), [lib/crates/fabro-types/src/status.rs](/Users/bhelmkamp/p/fabro-sh/fabro/lib/crates/fabro-types/src/status.rs), and the generated models under `lib/packages/fabro-api-client/src/models/`.
- Public/internal type changes:
  - Add `Queued`, `Blocked`, `Completed`, and `Cancelled` to the shared Rust `RunStatus`.
  - Rename shared/internal `Succeeded` usages to `Completed`.
  - Add nullable `blocked_reason` with a new `BlockedReason` enum; initial value set is `human_input_required`.
  - Remove `Dead` from OpenAPI and generated API/client status enums. Callers that currently fall back to `Dead` must instead treat status as missing/unknown locally.
  - Add `blocked` to the `RunStatus` and `InternalRunStatus` enums in `fabro-api.yaml`.
- Projection and summary behavior: update [lib/crates/fabro-store/src/run_state.rs](/Users/bhelmkamp/p/fabro-sh/fabro/lib/crates/fabro-store/src/run_state.rs), `lib/crates/fabro-store/src/types.rs`, and `lib/crates/fabro-store/src/slate/mod.rs`.
  - Persist `Queued` as a real durable state by appending/projecting a `run.queued` transition when a run is start-requested and enqueued.
  - Project `run.failed` with `reason=cancelled` to canonical `Cancelled`.
  - Set canonical `Blocked` on `interview.started` with `blocked_reason=human_input_required`.
  - Clear `blocked_reason` and return to `Running` on `interview.completed`, `interview.timeout`, or `interview.interrupted` when no pending interviews remain.
  - Keep `Paused` driven only by pause/unpause control events; interview events must never produce `Paused`.
  - Update transition helpers so `Blocked` is non-terminal and `Completed`/`Failed`/`Cancelled` are terminal.
- Server/live read model: update [lib/crates/fabro-server/src/server.rs](/Users/bhelmkamp/p/fabro-sh/fabro/lib/crates/fabro-server/src/server.rs) and `lib/crates/fabro-server/src/demo/mod.rs`.
  - Remove the ad-hoc API remap layer; server responses should expose the canonical shared status directly.
  - Extend run status payloads and durable summaries to include `blocked_reason` alongside `status_reason` and `pending_control`.
  - Extend `update_live_run_from_event()` so `InterviewStarted` drives `Blocked`, and interview resolution (`InterviewCompleted`/`InterviewTimeout`/`InterviewInterrupted`) returns live runs to `Running` when no pending interviews remain.
  - Keep `/runs/{id}/questions` and answer submission unchanged; those endpoints remain the detailed question surface behind a blocked run.
- Board/UI model:
  - Change board columns to `working`, `blocked`, `review`, `merge`.
  - Map `Running` and `Paused` to `working`; map `Blocked` to `blocked`; map `Completed` to `merge`; keep `Submitted`, `Queued`, `Starting`, `Failed`, and `Cancelled` off-board.
  - Keep paused runs in the working lane with no extra indicator in this pass.
  - Update web mappings in `apps/fabro-web/app/{data/runs.ts,routes/run-detail.tsx,routes/runs.tsx}` so `blocked` is a real lifecycle/board value and `waiting` is removed.
  - Because this pass does not add a new `run.blocked` event family, update `STATUS_EVENTS` in `apps/fabro-web/app/routes/runs.tsx` to include `interview.started`, `interview.completed`, `interview.timeout`, and `interview.interrupted` as status-affecting events.
- CLI consumers: update `lib/crates/fabro-cli/src/{commands/run/wait.rs,commands/runs/list.rs,server_runs.rs}`.
  - Replace `Succeeded`/`Dead` handling with `Completed` plus explicit missing-status handling.
  - Add display/color handling for `Blocked`, `Queued`, and `Cancelled`.

## Test Plan

- `lib/crates/fabro-store/src/run_state.rs`:
  - `interview.started` sets `status=Blocked` and `blocked_reason=HumanInputRequired`.
  - interview completion/timeout/interruption returns the run to `Running` when no pending interviews remain.
  - pause/unpause still yields `Paused`/`Running` and never routes through `Blocked`.
  - cancelled failures project to `Cancelled`.
  - queued state round-trips through projection serialization.
- `lib/crates/fabro-store/src/slate/mod.rs` and `lib/crates/fabro-server/src/server.rs`:
  - durable summaries and `/runs/{id}` responses expose unified statuses plus `blocked_reason`.
  - no serialized API/store status is `dead`.
  - live managed runs enter `Blocked` while a pending interview exists.
  - board response emits a `blocked` column, places blocked runs there with question text, and keeps paused runs in `working`.
- `apps/fabro-web/app/data/runs.test.ts` and a new `apps/fabro-web/app/routes/runs.test.tsx`:
  - summary mapping accepts `blocked`, `paused`, `completed`, and `cancelled`.
  - blocked runs render in the blocked lane with the existing answer-question affordance.
  - paused runs stay in the working lane.
  - no UI code depends on `waiting`.
- CLI tests in `lib/crates/fabro-cli/src/commands/run/wait.rs` and `lib/crates/fabro-cli/src/commands/runs/list.rs`:
  - `Completed` is the success exit state.
  - `Blocked`, `Queued`, and `Cancelled` render correctly.
  - missing status no longer masquerades as `Dead`.
  - `Succeeded` is no longer accepted or displayed; all success paths use `Completed`.

## Assumptions

- `BlockedReason` starts with one value only: `human_input_required`.
- Notification behavior is intentionally deferred; this plan only makes blocked state canonical and queryable.
- `RunListItem.question` stays optional and unchanged in shape; `Blocked` plus `question` is sufficient for current UI behavior.
- `Paused` remains visible in the working board column for now; the paused-specific visual indicator is a separate follow-up.


## Completed stages
- **toolchain**: success
  - Script: `command -v cargo >/dev/null || { curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y && sudo ln -sf $HOME/.cargo/bin/* /usr/local/bin/; }; cargo --version 2>&1`
  - Stdout:
    ```
    cargo 1.94.0 (85eff7c80 2026-01-15)
    ```
  - Stderr: (empty)
- **preflight_compile**: success
  - Script: `cargo check -q --workspace 2>&1`
  - Stdout: (empty)
  - Stderr: (empty)
- **preflight_lint**: success
  - Script: `cargo clippy -q --workspace -- -D warnings 2>&1`
  - Stdout: (empty)
  - Stderr: (empty)
- **implement**: success
  - Model: claude-opus-4-6, 196.7k tokens in / 62.8k out
  - Files: /home/daytona/workspace/apps/fabro-web/app/data/runs.test.ts, /home/daytona/workspace/apps/fabro-web/app/data/runs.ts, /home/daytona/workspace/apps/fabro-web/app/routes/runs.tsx, /home/daytona/workspace/docs/api-reference/fabro-api.yaml, /home/daytona/workspace/lib/crates/fabro-cli/src/commands/run/attach.rs, /home/daytona/workspace/lib/crates/fabro-cli/src/commands/run/wait.rs, /home/daytona/workspace/lib/crates/fabro-cli/src/commands/runs/list.rs, /home/daytona/workspace/lib/crates/fabro-cli/src/commands/store/dump.rs, /home/daytona/workspace/lib/crates/fabro-cli/src/server_runs.rs, /home/daytona/workspace/lib/crates/fabro-cli/tests/it/cmd/resume.rs, /home/daytona/workspace/lib/crates/fabro-cli/tests/it/cmd/start.rs, /home/daytona/workspace/lib/crates/fabro-server/src/demo/mod.rs, /home/daytona/workspace/lib/crates/fabro-server/src/server.rs, /home/daytona/workspace/lib/crates/fabro-store/src/run_state.rs, /home/daytona/workspace/lib/crates/fabro-store/src/slate/mod.rs, /home/daytona/workspace/lib/crates/fabro-store/src/types.rs, /home/daytona/workspace/lib/crates/fabro-types/src/lib.rs, /home/daytona/workspace/lib/crates/fabro-types/src/status.rs, /home/daytona/workspace/lib/crates/fabro-workflow/src/operations/resume.rs, /home/daytona/workspace/lib/crates/fabro-workflow/src/pipeline/execute/tests.rs, /home/daytona/workspace/lib/crates/fabro-workflow/src/pipeline/finalize.rs, /home/daytona/workspace/lib/crates/fabro-workflow/src/run_lookup.rs, /home/daytona/workspace/lib/packages/fabro-api-client/src/models/blocked-reason.ts, /home/daytona/workspace/lib/packages/fabro-api-client/src/models/board-column.ts, /home/daytona/workspace/lib/packages/fabro-api-client/src/models/index.ts, /home/daytona/workspace/lib/packages/fabro-api-client/src/models/internal-run-status.ts, /home/daytona/workspace/lib/packages/fabro-api-client/src/models/run-status-record.ts, /home/daytona/workspace/lib/packages/fabro-api-client/src/models/run-status-response.ts, /home/daytona/workspace/lib/packages/fabro-api-client/src/models/run-status.ts
- **simplify_opus**: success
  - Model: claude-opus-4-6, 63.7k tokens in / 12.1k out
  - Files: /home/daytona/workspace/lib/crates/fabro-server/src/server.rs, /home/daytona/workspace/lib/crates/fabro-server/tests/it/scenario/lifecycle.rs


---
name: code-review-simplify
description: |
  Guidelines for writing and reviewing elegant, maintainable code in statically typed languages (TypeScript, Rust). Use when: (1) reviewing code for simplicity and clarity, (2) designing types to make invalid states unrepresentable, (3) evaluating whether abstractions or design patterns are justified, (4) naming variables/functions/classes, (5) deciding on encapsulation boundaries, (6) evaluating code comments, or (7) asked to simplify or improve code design.
---

# Code Design and Review for Simplicity

The job is not "How can I make this work?" but "How *should* this work?" The challenge is writing code that makes the task look easy.

> "Perfection is achieved, not when there is nothing more to add, but when there is nothing left to take away." —Antoine de Saint-Exupéry

Complexity is the enemy—not because complex problems don't exist, but because unnecessary complexity obscures solutions. KISS, DRY, YAGNI are necessary but not sufficient. True simplicity requires:

1. **Start simple and add complexity only when proven necessary.** The burden of proof is on complexity.
2. **Understand the problem fully before solving it.** You cannot simplify what you do not understand.
3. **Make the code explain itself.** If you need extensive comments, the code should be clearer.
4. **Prefer boring code.** Clever code is often complex code.

## Naming

Naming is the most fundamental tool for communicating intent. Developers spend 75% of their time understanding code—clear names dramatically reduce cognitive load.

**Principles:**
- **Purpose-driven.** What it represents should be obvious from its name alone.
- **Domain-specific.** Names reflect the business domain, not implementation details.
- **Functions are verbs, variables are nouns.**
- **Length follows scope.** Short-lived variables can have shorter names; widely-used entities need descriptive names.
- **Consistent.** Pick one word for one concept. Don't mix 'fetch', 'retrieve', and 'get'.
- **No abbreviations.** Prefer 'category' over 'cat'.

**When naming is hard:** Difficulty naming usually signals a design problem—the function is doing too much or the concept is unclear. Write a plain-language comment explaining what the code does, then condense it into a name. If you cannot condense it, restructure the code.

## Comments

Code should speak for itself. Comments are a last resort. Remove low-value comments—they add noise and rot over time.

**When comments are appropriate:**
- **WHY, not WHAT.** Explain reasoning behind non-obvious decisions.
- **Surprising behavior.** When something seems wrong but isn't.
- **External references.** A URL to docs or bug report explaining a workaround—sparingly.

**Remove these:**
- Comments explaining what code does (fix the names instead)
- Commented-out code (version control exists)
- Stale TODOs that will never be addressed

If you need a comment to make code understandable, first try renaming or simplifying. The best comment is the one you didn't need to write.

## Type System

The type system is the most powerful tool for ensuring correctness. The central principle: **make invalid states unrepresentable.**

Every type defines *representable* states. Business logic defines *valid* states. The gap between them is where bugs live. Close the gap by designing types where only valid data can be constructed—invalid combinations fail at compile time.

**Example:** A user profile that can be guest or authenticated. Naive: boolean flags and optional fields allowing "authenticated but no user ID." Better: discriminated unions (TypeScript) or enums (Rust) where Guest and Authenticated are distinct types. The compiler enforces validity—there is no gap.

**Practical type design:**
- **Avoid primitive obsession.** A `UserId` should not be interchangeable with a `ProductId`, even if both are strings.
- **Use union types to model states.** Each state is a distinct type with appropriate data.
- **Validate at boundaries, trust internally.** Parse data when it enters your system, then work with known-valid types.
- **Use private constructors with factory functions.** No way to create an invalid instance.
- **Types are documentation.** A well-designed type signature explains business rules better than comments.

## Design Patterns and Indirection

Design patterns are tools, not rules. They solve specific problems—apply them when those problems exist, not prophylactically.

Every layer of indirection has a cost: code becomes harder to trace, debug, and understand. An interface with only one implementation is usually wrong abstraction.

**Signs of over-abstraction:**
- Wrapper classes that add no logic, only forward calls
- Interfaces created "for future flexibility" with single implementations
- Factory patterns for objects that could be constructed directly
- Multiple inheritance layers that obscure what code runs
- Dependency injection for values that never change

**Before applying a pattern, ask:**
1. Will it enhance clarity or introduce unnecessary complexity?
2. Does the benefit justify the overhead, or does it just add bloat?
3. Can teammates understand it, or is it too obscure?

A pattern is justified when you have multiple implementations *today*, the abstraction makes code easier to understand, and the team recognizes it. Simplicity often beats sophistication—refactoring later is acceptable.

## Encapsulation

Object-oriented design groups data with operations that act on it. This enforces invariants and makes invalid states unrepresentable.

- **Hide internals aggressively.** Private fields maintain invariants, not secrecy.
- **Operations maintain validity.** If an object can only be created valid and all operations maintain validity, invalid states are impossible.
- **Avoid anemic domain models.** Data structures with getters/setters and separate service classes defeat encapsulation's purpose.
- **Favor composition over inheritance.** Deep hierarchies create tight coupling.
- **High cohesion.** Elements of a class should be closely related and focused on a single purpose.

## Code Review Checklist

1. **Can I understand this without comments?** If not, names are unclear.
2. **Can I construct invalid data?** Runtime checks should be compile-time guarantees.
3. **Is every abstraction earning its keep?** Single-implementation interfaces are suspect.
4. **Is this the simplest solution?** Could it be achieved with less?
5. **Is data encapsulated with behavior?** Service classes manipulating passive data structures are a smell.
6. **Would a new team member understand this?** Fresh eyes are the ultimate test.

Simple code is not easy code—it is code that has had complexity removed through careful thought. The goal is not to demonstrate cleverness but to make the solution so clear that the problem looks easy.
