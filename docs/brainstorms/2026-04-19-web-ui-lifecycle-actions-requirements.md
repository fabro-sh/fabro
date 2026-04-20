---
date: 2026-04-19
topic: web-ui-lifecycle-actions
---

# Expose CLI Lifecycle Actions in the Web UI

## Problem Frame

The Fabro web UI today is essentially read-only for run management. The only mutating action exposed is **Preview** (opens a sandbox port URL). Every other run lifecycle action — cancelling a stuck run, cleaning up the board, revisiting archived runs — requires dropping to the CLI.

Two concrete user problems drive this work:

1. **Daily friction for CLI users.** People live in the board view and detail pages but have to context-switch to a terminal to manage state.
2. **Excludes non-CLI teammates.** PMs, reviewers, and stakeholders can watch runs but cannot participate in managing them, cutting the UI off as a collaboration surface.

The web UI should own the **everyday lifecycle operations** these users hit. Rarer or more dangerous CLI verbs (force-delete of active runs, checkpoint ops, etc.) can remain CLI-only by design; the goal is user value per surface, not parity for parity's sake.

Most server infrastructure already exists — `POST /runs/{id}/{cancel,archive,unarchive}` are live, wired to the workflow engine's operations, and emit SSE events. The remaining work is UI surface + a handful of concrete SSE reconciliation gaps (see Dependencies).

## Requirements

**Action set**
- R1. Expose three lifecycle actions on the run detail page (`/runs/{id}`): **cancel**, **archive**, **unarchive**.
- R2. Do not expose `pause`, `unpause`, or `delete` in this first pass. Pause/unpause has no evidenced daily-user need; delete's tab-close-mid-undo failure mode is unacceptable for a non-CLI teammate because observability is destroyed (there's no run to revisit to self-verify).
- R3. Do not expose checkpoint operations (resume, rewind, fork) or HITL question answering in this first pass.
- R4. Do not expose these actions on the board kanban cards or as bulk selection in this first pass.

**State-aware visibility**
- R5. Only show an action when the run's current status makes it valid:
  - **cancel (primary)**: visible as a primary affordance when status is `submitted`, `queued`, `starting`, `running`, or `paused`. Not shown as primary when `blocked` — see R6. The server also accepts cancel on `blocked` runs; that path is only reachable via the secondary surface from R6.
  - **archive**: visible only when status is terminal (`succeeded`, `failed`, `dead`) AND not already archived.
  - **unarchive**: visible only when `archived`.
- R6. When a run is `blocked` (waiting on an HITL question), do not show cancel as a primary action. Instead, show an inline notice with the pending question text (from `GET /api/v1/runs/{id}/questions`, whose `ApiQuestion.text` field is already human-readable) and the instruction: "Answer this question via `fabro` CLI to continue." Cancel remains reachable via an overflow/secondary affordance (e.g., a "…" menu) for users who truly want to abandon the run. This protects the common case (non-CLI teammate accidentally cancelling work that was waiting for them) without fully hiding the escape hatch.
- R7. Visibility updates live when the run status changes. The detail page must subscribe to the run's SSE event stream (`GET /runs/{id}/attach`) so action affordances appear/disappear without a manual refresh.

**Interaction & feedback**

Two toast patterns, matched to the reversibility of each action:

- R8. **Cancel uses a client-side deferred toast with a fixed 5-second countdown.** Cancel has partially-irreversible side effects (the agent stops mid-stage) so the undo window guards against misclicks.
  - Clicking cancel shows a toast with the action description, a 5-second countdown, and an **Undo** button. The cancel affordance enters a disabled/pending state during the window (not hidden — hiding mid-window misrepresents actual run state).
  - If the user clicks **Undo** before the countdown expires, the pending client-side timer is cancelled and no API call is made.
  - If the countdown expires, the client fires the API call. **Before firing**, the UI performs a `GET /api/v1/runs/{id}` refetch: if the run's status is no longer one where cancel is valid (e.g., the run completed or was cancelled elsewhere), the pending action is aborted and a brief "Run transitioned — cancel aborted" notice is shown instead. This covers the SSE-channel-unreachable case where R9's event-driven abort cannot fire.
  - If the tab is closed or navigated away (including SPA navigation to another route) mid-window, the pending action is silently cancelled. Accepted tradeoff of the client-only approach.

- R9. **Archive and unarchive fire immediately with an inverse-action toast.** Both are fully reversible by the opposite API call, so the 5-second countdown is overhead with no safety benefit. Gmail-archive shape:
  - Clicking archive fires `POST /runs/{id}/archive` immediately (optimistic UI: the run disappears from terminal views / moves to archived views right away).
  - On success, show a toast: "Run archived. **Unarchive**" with a visible action button. The toast remains dismissable for ~8 seconds.
  - Clicking **Unarchive** in the toast fires `POST /runs/{id}/unarchive`. The toast updates to "Run restored" briefly, then dismisses.
  - Unarchive-triggered-from-the-primary-affordance works identically, with the inverse verb.

- R10. **Undo-window collisions (cancel only).** While the cancel timer is pending for a run, other primary affordances for that run are disabled (not hidden). If an SSE event arrives for that run that would change its status (another tab, a CLI user, the run finishing naturally), the pending client-side timer is cancelled, the toast is dismissed with a brief notice ("Run transitioned — action cancelled"), and the UI reconciles to the new status. If a newly-valid action (e.g., archive becomes valid because the run just completed) results from the transition, its affordance lights up immediately rather than waiting for the toast to fully dismiss.

- R11. On a successful async cancel (status unchanged in the response body), the UI relies on SSE for the final status flip. No additional success toast beyond the deferred-toast already shown.

- R12. On API failure (including 409 precondition failures — e.g., the run transitioned out of a valid state between toast-expiry and API call), show an error toast that includes the server's error message, and refetch the run so the UI reconciles to actual state. For the archive/unarchive fire-immediately path, additionally roll back the optimistic UI change on failure.

- R13. **Multi-tab behavior (single-client rules apply per tab).** R8/R10 are scoped per-client: each tab runs its own timer. An SSE-delivered status transition in tab B (caused by tab A firing cancel) will cancel tab B's own pending timer for the same run per R10 and surface the transition notice. Cross-tab action coordination beyond what SSE already provides is not a requirement for this pass.

**Accessibility**
- R14. Both toast patterns (deferred cancel toast; immediate archive/unarchive toast) must meet baseline a11y expectations:
  - Toast container uses `role="status"` with `aria-live="polite"` (assertive interrupts screen-reader output and is wrong here). Announcement names the action, e.g., "Cancel run requested, undoing in 5 seconds" or "Run archived. Press Unarchive to undo."
  - Toast does **not** steal focus, but is reachable via keyboard (tab order places the action button — Undo or Unarchive — immediately after the triggering affordance).
  - For the cancel deferred toast, while keyboard focus is on the **Undo** button, the 5-second countdown **pauses** and resumes counting down from the paused value when focus leaves. Focus leaving the Undo button after the countdown would have expired does not auto-fire the action — the user must still explicitly close the toast or navigate away for the countdown to resume its final tick.
  - Archive/unarchive toasts do not count down (they fire on click); they remain dismissable and focusable for ~8 seconds.
  - Touch targets (Undo / Unarchive buttons) meet 44×44 CSS px minimum. The detail page is expected to work on tablet and larger; phone-size support is not a requirement for this pass.
  - The action cluster is keyboard-operable end to end (no mouse-only affordances). Keyboard shortcuts for individual actions are out of scope for this pass.

## Success Criteria

- A user managing runs day-to-day can complete a full session (cancelling a stuck run, archiving finished ones, revisiting an archived one) without touching the CLI.
- A non-CLI teammate can cancel or archive a run in the web UI without onboarding documentation beyond "click the button."
- A non-CLI teammate who lands on a `blocked` run understands the run is waiting on a human answer, sees the question text, and does not accidentally cancel work in progress. **Note:** unblocking the teammate so they can actually *answer* the question requires HITL-answering in the web UI, which is deferred — this first pass prevents destruction, not participation. If blocked-teammate-can't-proceed proves to be a real painful pattern in usage, HITL answering should be the next scope to pick up.
- When a run transitions state while the detail page is open (e.g., finishes, gets archived from the CLI, gets cancelled by someone else), the available actions update live without a page refresh.
- The cancel deferred-toast component (toast container + Undo + pause-on-focus countdown + polite aria-live) is shaped so it can host future destructive actions (delete if we solve the observability problem; force-cancel if ever added) without redesign. The archive/unarchive immediate-inverse toast is a simpler shape that other reversible fire-and-forget actions can reuse.

## Scope Boundaries

**Out of scope for this first pass:**
- `pause` and `unpause` (no evidenced daily-user pain for the target personas; revisit if a real workflow surfaces).
- `delete` (tab-close-silently-cancels destroys observability — there's no run to revisit to self-verify — which is unacceptable for the non-CLI teammate persona; revisit with server-side soft-delete or a different UX).
- Force-delete of active runs (`rm --force`).
- Checkpoint operations: `resume`, `rewind`, `fork`. These need parameter input (which checkpoint? which branch?) that doesn't fit the uniform button+toast pattern.
- Answering HITL questions from the web UI. R6 surfaces the question read-only and points to the CLI.
- Board-card actions and bulk multi-select on the board.
- Dense table/list view of runs.
- Server-side deferred/pending states.
- Keyboard shortcuts for individual actions.
- Role-based authorization (assumed unchanged from today; all authenticated users can take all actions).
- Phone-size responsive layout.

## Key Decisions

- **Cancel + archive + unarchive only.** Cancel addresses the clearest daily pain (stuck runs). Archive/unarchive addresses board clutter and is fully reversible by the opposite action — lowest-risk place to validate the interaction pattern. Pause/unpause are deferred for lack of evidenced need; delete is deferred because its client-side-undo failure mode destroys observability for the non-CLI teammate persona.
- **Run detail page only, not the board.** Keep the first pass tight.
- **Two toast patterns matched to reversibility, not one uniform pattern.** Cancel is partially irreversible → deferred-toast with a bound 5-second countdown. Archive/unarchive are fully reversible via the opposite API call → fire-immediately with an inverse-action toast (Gmail-archive shape). This is more spec than "one pattern for everything" but maps to a real property of the actions and eliminates a pointless friction tax on the reversible ones. The shared toast infrastructure (container, action-button slot, aria-live, keyboard reachability) is reused across both; only the cancel path carries the countdown + focus-pause machinery.
- **Pre-fire status recheck on the deferred cancel path.** At countdown expiry, the client refetches `GET /runs/{id}` before firing the cancel API call. This covers the SSE-unreachable failure mode where R10's event-driven abort cannot fire: without the recheck, a dead SSE channel would silently degrade to "timer fires, 409 error, user sees error toast for a race they didn't create." One extra GET per cancel is a cheap premium for reliable UX.
- **Blocked runs suppress cancel as a primary affordance and show the pending question.** Protects non-CLI teammates from the worst failure mode (cancelling work that was waiting for them). Cancel remains reachable via a secondary/overflow affordance for users who genuinely want to abandon. This solves the destruction problem but leaves the participation problem for HITL-in-the-web to solve later.
- **Client-side deferral, not server-side.** Chosen on shipping speed; does not add new lifecycle states. Accepted tradeoff: the "undo" promise is soft — tab close or SPA nav silently cancels. This is tolerable precisely because the action with the worst silent-cancellation consequence (delete) was scoped out.
- **Accessibility is in the requirements, not deferred to implementation.** ARIA semantics, keyboard reachability, focus-pauses-countdown, touch-target sizing, and aria-live politeness are specified rather than left as "standard best practices."
- **Pattern reuse is claimed only within this action family.** We do not claim "adding resume/rewind/fork/HITL later is same shape, new button." Those need parameter input (checkpoint choice, answer text) that the button+toast pattern doesn't host. That's fine — these patterns are for fire-and-forget lifecycle mutations, and the rest of the verbs will need their own UX.

## Dependencies / Assumptions

- The existing API endpoints (`POST /runs/{id}/{cancel,archive,unarchive}`) are stable and will not require spec changes. Verified against `docs/api-reference/fabro-api.yaml`.
- The generated TypeScript client in `lib/packages/fabro-api-client` exposes (or will trivially expose after regeneration) methods for these endpoints.
- **SSE coverage is not uniform.** The server emits `run.*` events for status transitions including `run.archived` / `run.unarchived` (`fabro-workflow/src/event.rs`, `fabro-server/src/server.rs`). Known gaps the plan must address:
  - The board's event allowlist (`apps/fabro-web/app/routes/runs.tsx` `BOARD_STATUS_EVENTS`) does not currently include `run.archived` / `run.unarchived`. Adding them is in scope.
  - The per-run `/attach` SSE stream terminates on `RunCompleted` / `RunFailed`. Archive/unarchive events fire on already-terminal runs, so the detail page must either reconnect to a non-terminating channel, refetch on the successful archive/unarchive response, or listen at a layer above `/attach`. Planning should pick an approach.
- **Run detail page is not yet SSE-subscribed.** `apps/fabro-web/app/routes/run-detail.tsx` currently fetches the run once via the React Router loader and does not subscribe to `/api/v1/runs/{id}/attach`. Wiring this subscription at the detail-page level (the owner of `run.status` that drives R5 visibility) is net-new work for R7. Individual tab components (stage-sidebar, run-files) already have per-run SSE subscriptions that can be used as a pattern.
- **No undo-capable toast system exists yet.** The only Toast in `apps/fabro-web` is a read-only live-region banner (`apps/fabro-web/app/routes/run-files/states.tsx`) with local `useState`/`setTimeout`. R8 + R9 + R12 together require a shared toast component with: a countdown, action-button slot, programmatic dismiss, multi-toast coexistence, polite aria-live, and focus-pauses-countdown behavior. This is net-new UI infrastructure.
- **Cancel semantics.** For `submitted` and `queued` runs, cancel synchronously flips lifecycle `status` to `failed` with `status_reason: cancelled` and returns that on the response. For `starting`/`running`/`blocked`/`paused` runs, cancel returns 200 with unchanged status and the transition lands asynchronously via the workflow engine. The UI should treat cancel as "request accepted" and rely on SSE for the final status flip — R10 covers this implicitly, but the plan should make the optimistic-UI behavior explicit (e.g., the cancel affordance stays in its disabled/pending state until either the response body carries the synchronous `failed`/`cancelled` result or the SSE-driven reconciliation arrives).
- **Per-run `/attach` stream has silent termination paths beyond the terminal-event case.** `attach_event_is_terminal` only matches `RunCompleted | RunFailed`, but the task that drives the stream can also exit without a terminal marker if the store read errors, if the run projection becomes non-active mid-replay, or if cancel lands on a queued run that never transitioned to running (covered by the `cancel_before_run_transitions_to_running_returns_empty_attach_stream` test in `fabro-server/src/server.rs`). R7 and R10 must therefore not assume a terminal event will always land: the plan needs a fallback refetch path for "SSE stream ended without a terminal marker" and for "SSE channel unreachable during an undo window."
- Authorization is a non-issue today (single-user / trusted deployment assumption). If multi-tenant auth lands, the action affordances will need to respect it, but that's a separate workstream.

## Outstanding Questions

### Deferred to Planning
- [Affects R8][Design] Exact visual placement of the action cluster in the detail page header: single "Actions" dropdown vs. inline buttons vs. split primary+overflow. Behavior is specified here; visual placement resolves in design/implementation. Consider how cancel-on-blocked lives in the overflow while the primary slot is occupied by the R6 inline notice.
- [Affects R11][Technical] Confirm the 409 error-body shape from the server so error-toast copy can use the server-provided message verbatim.
- [Affects R7, R10][Technical] Decide the post-terminal reconciliation mechanism for archive/unarchive: reconnect SSE after terminal close, refetch on 2xx archive/unarchive response, or subscribe on a non-terminating channel. Either the per-run `/attach` contract extends or the UI uses response-driven reconciliation.

## Next Steps

→ `/ce:plan` for structured implementation planning
