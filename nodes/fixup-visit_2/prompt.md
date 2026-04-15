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
- **simplify_gpt**: success
  - Model: gpt-5.4, 6.7m tokens in / 33.5k out
- **verify**: fail
  - Script: `cargo clippy -q --workspace -- -D warnings 2>&1 && cargo nextest run --cargo-quiet --workspace --status-level fail 2>&1`
  - Stdout:
    ```
    (1 lines omitted)
     Nextest run ID 0dc64c20-0dba-4084-a161-247212491a6a with nextest profile: default
        Starting 3992 tests across 66 binaries (182 tests skipped)
    [>  6.000s] (─────────) fabro-cli::it cmd::ps::ps_accepts_local_tcp_server_target
    [>  6.000s] (─────────) fabro-cli::it cmd::runner::worker_exits_after_sigterm_cancel_even_when_stdin_stays_open
    [>  6.000s] (─────────) fabro-cli::it cmd::server_start::start_already_running_exits_with_error
    [>  6.000s] (─────────) fabro-cli::it cmd::server_start::start_with_tcp_host_only_bind_resolves_to_host_and_port
    [>  6.000s] (─────────) fabro-cli::it cmd::server_start::start_with_tcp_host_only_bind_warns_and_falls_back_when_default_port_is_unavailable
    [>  6.000s] (─────────) fabro-cli::it cmd::server_start::start_without_bind_uses_configured_tcp_listen_address
    [>  6.000s] (─────────) fabro-cli::it cmd::server_start::start_without_bind_uses_home_socket_instead_of_storage_socket
    [> 12.000s] (─────────) fabro-cli::it cmd::runner::worker_exits_after_sigterm_cancel_even_when_stdin_stays_open
    [>  6.000s] (─────────) fabro-cli::it scenario::server_lifecycle::start_status_stop_lifecycle
    [> 18.000s] (─────────) fabro-cli::it cmd::runner::worker_exits_after_sigterm_cancel_even_when_stdin_stays_open
     TERMINATING [> 24.000s] (─────────) fabro-cli::it cmd::runner::worker_exits_after_sigterm_cancel_even_when_stdin_stays_open
         TIMEOUT [  24.006s] (3992/3992) fabro-cli::it cmd::runner::worker_exits_after_sigterm_cancel_even_when_stdin_stays_open
      stdout ───
    
        running 1 test
    
        (test timed out)
    
      Cancelling due to test failure: 
    ────────────
         Summary [  27.906s] 3992 tests run: 3991 passed (7 slow), 1 timed out, 182 skipped
         TIMEOUT [  24.006s] (3992/3992) fabro-cli::it cmd::runner::worker_exits_after_sigterm_cancel_even_when_stdin_stays_open
    error: test run failed
    ```
  - Stderr: (empty)
- **fixup**: success
  - Model: claude-opus-4-6, 8.6k tokens in / 1.3k out
  - Files: /home/daytona/workspace/lib/crates/fabro-cli/src/commands/run/wait.rs
- **verify**: fail
  - Script: `cargo clippy -q --workspace -- -D warnings 2>&1 && cargo nextest run --cargo-quiet --workspace --status-level fail 2>&1`
  - Stdout:
    ```
    (1 lines omitted)
     Nextest run ID 0dc64c20-0dba-4084-a161-247212491a6a with nextest profile: default
        Starting 3992 tests across 66 binaries (182 tests skipped)
    [>  6.000s] (─────────) fabro-cli::it cmd::ps::ps_accepts_local_tcp_server_target
    [>  6.000s] (─────────) fabro-cli::it cmd::runner::worker_exits_after_sigterm_cancel_even_when_stdin_stays_open
    [>  6.000s] (─────────) fabro-cli::it cmd::server_start::start_already_running_exits_with_error
    [>  6.000s] (─────────) fabro-cli::it cmd::server_start::start_with_tcp_host_only_bind_resolves_to_host_and_port
    [>  6.000s] (─────────) fabro-cli::it cmd::server_start::start_with_tcp_host_only_bind_warns_and_falls_back_when_default_port_is_unavailable
    [>  6.000s] (─────────) fabro-cli::it cmd::server_start::start_without_bind_uses_configured_tcp_listen_address
    [>  6.000s] (─────────) fabro-cli::it cmd::server_start::start_without_bind_uses_home_socket_instead_of_storage_socket
    [> 12.000s] (─────────) fabro-cli::it cmd::runner::worker_exits_after_sigterm_cancel_even_when_stdin_stays_open
    [>  6.000s] (─────────) fabro-cli::it scenario::server_lifecycle::start_status_stop_lifecycle
    [> 18.000s] (─────────) fabro-cli::it cmd::runner::worker_exits_after_sigterm_cancel_even_when_stdin_stays_open
     TERMINATING [> 24.000s] (─────────) fabro-cli::it cmd::runner::worker_exits_after_sigterm_cancel_even_when_stdin_stays_open
         TIMEOUT [  24.006s] (3992/3992) fabro-cli::it cmd::runner::worker_exits_after_sigterm_cancel_even_when_stdin_stays_open
      stdout ───
    
        running 1 test
    
        (test timed out)
    
      Cancelling due to test failure: 
    ────────────
         Summary [  27.906s] 3992 tests run: 3991 passed (7 slow), 1 timed out, 182 skipped
         TIMEOUT [  24.006s] (3992/3992) fabro-cli::it cmd::runner::worker_exits_after_sigterm_cancel_even_when_stdin_stays_open
    error: test run failed
    ```
  - Stderr: (empty)

## Context
- failure_class: canceled
- failure_signature: verify|canceled|script failed with exit code: <n> ## stdout ──────────── nextest run id <hex>-0dba-<n>-a161-<hex> with nextest profile: default starting <n> tests across <n> binaries (<n> tests skipped) [> <n>.000s] (─────


The verify step failed. Read the build output from context and fix all clippy lint warnings and test failures.