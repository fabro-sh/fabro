# Re-architect ACP Steering

Date: 2026-05-20

## Context

The 2026-05-11 ACP backend plan intentionally classified ACP-backed agent stages
as non-steerable. That was correct for the first ACP cutover because the ACP
runner was a one-shot helper:

1. initialize
2. session/new
3. session/prompt
4. read stop reason
5. terminate

ACP itself supports live session control, so Fabro can steer ACP-backed stages
without raw stdin injection, process restarts, or API-backend-specific logic.

## Goal

Make active ACP-backed Fabro agent stages steerable through the existing run
control API:

- `POST /runs/{id}/steer`
- `POST /runs/{id}/interrupt`
- `POST /runs/{id}/steer` with `interrupt: true`

Keep CLI-backed stages non-steerable until a real CLI control channel exists.
Preserve existing API-backed steering behavior.

## Design

Introduce a backend-neutral live control abstraction in `fabro-workflow`:

- `enqueue_bounded(text, actor, cap)`
- `interrupt(actor)`
- `interrupt_then_enqueue_bounded(text, actor, cap)`
- `has_pending_control_work()`

`SteeringHub` should store this abstraction instead of
`fabro_agent::SessionControlHandle` directly.

Wrap API sessions by adapting the existing `SessionControlHandle` to the new
trait. Add an ACP control handle backed by an internal queue/notify pair that
the live ACP session loop can poll while reading protocol updates.

## ACP Protocol Control

Use official ACP protocol messages only:

- send normal and steering turns through `session/prompt`
- send interrupts through `session/cancel`
- keep the same ACP process and session alive across follow-up steering

Do not write user steering text directly to process stdin except as valid ACP
JSON-RPC protocol messages. Do not restart the ACP process to apply steering.

## Workflow Changes

Wire `AgentAcpBackend` with the workflow `SteeringHub`.

When an ACP stage starts:

- emit `agent.acp.started`
- create an ACP control handle
- emit `agent.session.activated` with `provider = "acp"` and `steer`
  capability
- attach the ACP handle to `SteeringHub`

When an ACP turn naturally stops with `end_turn` or `refusal`, release the
activation lease only if no control work is pending. If a steer or interrupt
arrives during the final-stop window, keep the live session and continue the
ACP loop.

When a steering prompt is injected into ACP, emit the existing
`agent.steering.injected` event so run events show accepted control work.

## Server Changes

Rename API-only active state to steerable-session state:

- `active_api_stages` becomes `active_steerable_stages`
- `agent.session.activated` with `SessionCapability::Steer` marks a stage
  steerable, regardless of backend
- `agent.session.deactivated` clears only the matching session id
- ACP terminal events and stage terminal events are cleanup backstops

`POST /runs/{id}/steer` and interrupt gates should accept active ACP sessions
because they are now represented by the same steer-capability lease as API
sessions.

Plain steers with no active steerable session continue to buffer for the next
steerable API or ACP session, unless a currently active non-steerable agent
stage is running.

## Projection Changes

ACP stage projection metadata should continue to come from `agent.acp.started`
because it carries ACP-specific process/config metadata. The generic
`agent.session.activated` event enables steering but must not overwrite
`provider_used.mode = "acp"` with API-style agent metadata.

## UI And API Copy

Replace API/CLI-specific steering error copy with backend-neutral wording:

- active non-steerable agent means the currently running backend has no live
  control channel
- ACP should no longer be described as non-steerable
- generated API client comments should match the OpenAPI descriptions

## Tests

Add or update coverage for:

- backend-neutral `SteeringHub` delivery to API and fake ACP handles
- ACP `session/prompt` follow-up steering
- ACP interrupt followed by follow-up `session/prompt`
- ACP workflow backend accepting steer and incorporating the follow-up result
- server steer route accepting active ACP sessions
- server cleanup clearing active ACP steerable markers on ACP/stage terminal
  paths
- CLI-backed active stages still returning non-steerable conflicts
- API-backed steering preserving existing behavior
- ACP cancellation/timeout still terminating the ACP process correctly
- ACP projection metadata remaining ACP-specific after session activation

## Validation

Run:

```bash
LC_ALL=C cargo nextest run --workspace --no-fail-fast
cd apps/fabro-web && LC_ALL=C bun test
cargo +nightly-2026-04-14 fmt --check --all
cargo +nightly-2026-04-14 clippy -p fabro-acp -p fabro-workflow -p fabro-server -p fabro-store --all-targets -- -D warnings
git diff --check
```

Use `LC_ALL=C` on macOS shells that otherwise emit unsupported locale warnings
to stderr; several existing stderr-sensitive tests compare command output.
