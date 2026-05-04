Goal: # Fix Agent Stage Cancellation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task.

**Goal:** Make workflow cancellation stop in-flight agent stages, including CLI-mode agent subprocesses and API-mode agent sessions.

**Architecture:** Make `tokio_util::sync::CancellationToken` the workflow/executor cancellation primitive, then clone or derive child tokens through setup, node execution, manager-loop children, CLI subprocesses, and API sessions. Dropping services or tokens must not mean "user cancelled"; only an explicit `.cancel()` does. CLI-mode agents will run through `Sandbox::exec_command_streaming` after local, Docker, and Daytona streaming cancellation terminate descendants; API-mode agents will link each backend invocation to the existing `Session` interrupt token with a bridge guard that aborts stale bridge tasks before cached sessions are reused.

**Tech Stack:** Rust, Tokio cancellation tokens, Fabro workflow events, Fabro sandbox streaming command execution, fabro-types run event schemas.

---

## Summary

- Replace workflow-run cancellation's `Arc<AtomicBool>` core path with `CancellationToken`, including manager-loop child workflows and executor between-node checks.
- Replace CLI agent detached subprocess execution with sandbox-managed execution that observes the workflow run cancellation token and terminates descendants for local, Docker, and Daytona.
- Add explicit cancellation plumbing to all agent backends so API-mode and CLI-mode stages share the same non-optional cancellation contract.
- Preserve run semantics: cancellation returns `Error::Cancelled`, reaches `fabro-core::Error::Cancelled`, and terminates the run as cancelled instead of as a retryable stage failure.

## Key Changes

- Promote `CancellationToken` to the workflow-run cancellation type.
  - In `lib/crates/fabro-core/src/executor.rs`, change `ExecutorOptions.cancel_token` and `ExecutorBuilder::cancel_token(...)` from `Option<Arc<AtomicBool>>` to `Option<CancellationToken>`. The run loop must check `token.is_cancelled()` at the existing between-node cancellation point.
  - Keep `ExecutorOptions.stall_token: Option<CancellationToken>` separate from user cancellation. User cancellation must return `Error::Cancelled`; stall timeout must continue returning `Error::StallTimeout { node_id }`.
  - In `lib/crates/fabro-workflow/src/run_options.rs`, change `RunOptions.cancel_token` from `Option<Arc<AtomicBool>>` to non-optional `CancellationToken`. Tests and constructors that currently use `None` must pass `CancellationToken::new()`.
  - In `lib/crates/fabro-workflow/src/services.rs`, replace `cancel_requested: Option<Arc<AtomicBool>>` with `cancel_token: CancellationToken` and expose `RunServices::cancel_token(&self) -> CancellationToken`.
  - Do **not** implement cancellation in `Drop` for `RunServices` or any wrapper type. A successfully completed run may drop every token handle; that must not be observable as user cancellation by a child task that outlives the run.
  - Remove `sandbox_cancel_token(...)` and the 10ms atomic-polling bridge once call sites are migrated. New cancellation-aware code must receive `CancellationToken` directly.
  - Update `RunServices::new(...)` and add a doc comment: production construction is expected to happen from pipeline initialization with the run's root token; use `with_cancel_token(...)` only with the same root token or a `child_token()` derived from it.
  - Make `with_cancel_token(token: CancellationToken)` `pub(crate)`. It must document that the token semantically means "cancel this run or child run," not a generic shutdown signal.
  - Update `lib/crates/fabro-workflow/src/pipeline/execute.rs` to pass `run_options.cancel_token.clone()` into `ExecutorBuilder::cancel_token(...)`.
  - Update setup/devcontainer paths in `lib/crates/fabro-workflow/src/pipeline/initialize.rs` and `lib/crates/fabro-workflow/src/devcontainer_bridge.rs` to pass `Some(run_options.cancel_token.child_token())` into sandbox commands instead of creating a new bridge from an atomic.
  - Update `lib/crates/fabro-workflow/src/handler/command.rs` to pass `Some(services.run.cancel_token().child_token())` into `exec_command_streaming` instead of calling `services.run.sandbox_cancel_token()`.
  - Do not wire stall timeout into the run cancel token. If `lib/crates/fabro-core/src/stall.rs` is migrated away from `Arc<AtomicBool>`, give it a field named `stall_token: CancellationToken` and call `stall_token.cancel()` on timeout. The executor must continue racing node execution against `ExecutorOptions.stall_token` and returning `Error::StallTimeout { node_id }` from that select branch.
  - Update CLI and server run entry points (`lib/crates/fabro-cli/src/commands/run/runner.rs`, `lib/crates/fabro-server/src/server.rs`, `lib/crates/fabro-workflow/src/operations/start.rs`) to create/store/cancel `CancellationToken` directly. `StartServices.cancel_token` and `RunSession.cancel_token` must become non-optional `CancellationToken` fields; managed server run state and CLI worker-control/signal handlers must use `CancellationToken`; places that currently call `load(Ordering::SeqCst)` must use `token.is_cancelled()`.
  - Specific `cancel_requested.load(SeqCst)` / `is_some_and(|flag| flag.load(...))` sites that must migrate (this is not exhaustive — the migration is compiler-driven once the type changes — but these are the ones easy to miss):
    - `lib/crates/fabro-workflow/src/handler/human.rs:328-332` — `cancel_requested.as_ref().is_some_and(|flag| flag.load(Ordering::SeqCst))` becomes `services.run.cancel_token().is_cancelled()`.
    - `lib/crates/fabro-workflow/src/operations/start.rs:858-886` (`DetachedRunBootstrapGuard::drop`) and `start.rs:913-958` (`DetachedRunCompletionGuard::drop`) read the cancel state to choose `FailureReason::Cancelled` vs other reasons. The "do not implement cancellation in `Drop`" rule above prohibits *triggering* cancellation in `Drop`, not *reading* it; these reads are load-bearing and must migrate to `cancel_token.is_cancelled()`.
  - Do not keep a compatibility atomic in `RunOptions`, `RunServices`, or the core executor. If server/CLI code still needs a separate boolean for status bookkeeping during migration, keep that flag local to the server/CLI module and set it in the same code path that calls `CancellationToken::cancel()`.
  - Tests that need to trigger cancellation from outside the system under test must create a token, clone it into `RunOptions`, and retain the original clone. The example below pre-cancels (run never starts a stage); for in-flight cancellation, replace the synchronous `cancel_token.cancel()` with a `tokio::spawn(...)` that awaits a marker (e.g., the first stage event) before cancelling, or call `cancel_token.cancel()` from inside a handler hook.
    ```rust
    // Pre-cancellation example:
    let cancel_token = CancellationToken::new();
    let mut run_options = test_run_options(run_dir, run_id);
    run_options.cancel_token = cancel_token.clone();
    cancel_token.cancel(); // for in-flight cancellation, fire from a spawned task or hook instead
    ```

- Fix manager-loop child workflow cancellation in `lib/crates/fabro-workflow/src/handler/manager_loop.rs`.
  - Do not build child `RunServices` with `.with_cancel_requested(None)`; that method is removed by the token migration.
  - Create a child run token with `let child_run_token = services.run.cancel_token().child_token();` before spawning the child engine.
  - Put `child_run_token.clone()` into `child_run_options.cancel_token`.
  - Pass `child_run_token.clone()` into child `RunServices` with `.with_cancel_token(child_run_token.clone())`.
  - At the current stop-condition and max-cycle sites (`manager_loop.rs:322` and `manager_loop.rs:340`), call `child_run_token.cancel()`. Parent cancellation propagates parent-to-child through `child_token()`, and the child executor sees cancellation between every node because `RunOptions.cancel_token` is now a `CancellationToken`.
  - Cancellation is intentionally one-way for manager-loop child workflows: parent cancellation cancels the child, and manager-loop stop/max-cycle cancellation cancels the child, but child cancellation does not cancel the parent run.

- Update `CodergenBackend::run` in `lib/crates/fabro-workflow/src/handler/agent.rs` to accept `cancel_token: CancellationToken`.
  - `AgentHandler` passes `services.run.cancel_token()` into every backend invocation.
  - `BackendRouter` still implements `CodergenBackend`; it routes as today and forwards the same token to either `AgentApiBackend` or `AgentCliBackend`.
  - `AgentApiBackend`, `AgentCliBackend`, `BackendRouter`, and all test stubs must update to the non-optional signature.
  - In `AgentHandler::execute`, add an explicit `Err(Error::Cancelled) => return Err(Error::Cancelled)` match arm before the bare `Err(e) => Ok(e.to_fail_outcome())` arm at the current `handler/agent.rs:310-315` decision point.
  - Add the same explicit `Err(Error::Cancelled) => return Err(Error::Cancelled)` arm in `lib/crates/fabro-workflow/src/handler/prompt.rs:118-123`, because prompt backends use the same retryable/non-retryable-to-failure-outcome pattern.
  - Add explicit `Error::Cancelled` propagation in `lib/crates/fabro-workflow/src/handler/parallel.rs:466`, where a branch's `Err(e)` is currently converted via `e.to_fail_outcome()` into a `BranchResult`. A branch that returns `Err(Error::Cancelled)` from a parallel agent stage must propagate cancellation to the parent rather than aggregating into a failed-branch outcome.
  - Audit the remaining handlers with `rg -n "is_retryable\\(\\)|to_fail_outcome\\(" lib/crates/fabro-workflow/src/handler lib/crates/fabro-workflow/src/pipeline` and add explicit `Error::Cancelled` propagation anywhere cancellation could otherwise be converted to a stage failure outcome.

- Rework `AgentCliBackend::run` in `lib/crates/fabro-workflow/src/handler/llm/cli.rs`.
  - Remove the detached `setsid ... &`, PID logging-only path, exit-code temp file, and polling loop.
  - Run `. <env_path> && <cli command>` via `sandbox.exec_command_streaming(..., Some(cancel_token.child_token()), callback)`.
  - Treat the `cancel_token` argument as the invocation's parent token. Pass child tokens into CLI version checks, install commands, credential login commands, and the final CLI command. Do not create new `sandbox_cancel_token()` bridge tasks for each subprocess.
  - Preserve today's unbounded CLI-agent runtime when `node.timeout()` is absent. Do **not** introduce a 10-minute or 24-hour default cap.
  - Change `Sandbox::exec_command_streaming` in `lib/crates/fabro-sandbox/src/sandbox.rs` and every implementation/decorator (`local.rs`, `docker.rs`, `daytona/mod.rs`, `worktree.rs`, test fakes) from `timeout_ms: u64` to `timeout_ms: Option<u64>`. `None` means wait until natural exit or cancellation; `Some(ms)` means return `CommandTermination::TimedOut` after that duration.
  - Keep the default trait implementation in `sandbox.rs` for test mocks and simple fakes. Update it to bridge `Option<u64>` into the existing non-streaming `exec_command(..., timeout_ms: u64, ...)` fallback with:
    ```rust
    let fallback_timeout_ms = timeout_ms.unwrap_or(u64::MAX);
    let result = self
        .exec_command(command, fallback_timeout_ms, working_dir, env_vars, cancel_token)
        .await?;
    ```
    This `u64::MAX` conversion is allowed only in the default non-streaming fallback. Production streaming implementations and decorators must override `exec_command_streaming` and implement `None` with a pending timeout future, not a giant Tokio sleep.
  - Implement optional timeout arms with a pending future, not a giant duration:
    ```rust
    let timeout_future = async {
        match timeout_ms {
            Some(ms) => tokio::time::sleep(Duration::from_millis(ms)).await,
            None => std::future::pending::<()>().await,
        }
    };
    tokio::pin!(timeout_future);

    tokio::select! {
        result = wait_for_process => { /* natural exit */ }
        () = &mut timeout_future => { /* CommandTermination::TimedOut */ }
        () = cancel_token.cancelled() => { /* CommandTermination::Cancelled */ }
    }
    ```
  - Apply that pattern in `local.rs` and `daytona/mod.rs` where timeout is currently a pinned `time::sleep(...)` select branch. Do not use `Duration::from_millis(u64::MAX)`.
  - Docker streaming (`docker.rs:377`) currently uses `Duration::from_millis(timeout_ms)` with no grace window, so no streaming grace adjustment is required there. The only `timeout_ms + 2000` grace site in the sandbox crate is `daytona/mod.rs:1105` inside the non-streaming `exec_command` impl, which this PR does not change. If a future change makes `exec_command` also accept `Option<u64>`, that grace window should become `timeout_ms.map(|ms| ms.saturating_add(2000))`.
  - Command stages keep their existing behavior by passing `Some(node.timeout().map_or(600_000, crate::millis_u64))` — note the outer `Some(...)` is required because `exec_command_streaming` now takes `Option<u64>`.
  - CLI agent stages pass `node.timeout().map(crate::millis_u64)` so missing `timeout` remains unbounded and an explicit timeout still works.
  - On `CommandTermination::Cancelled`, emit `agent.cli.cancelled`, clean temp prompt/env files, and return `Error::Cancelled`. This intentionally diverges from command stages because workflow run cancellation must propagate to `fabro-core::Error::Cancelled`, not become a stage failure outcome.
  - On `CommandTermination::TimedOut`, emit `agent.cli.timed_out`, clean temp prompt/env files, and return `Error::handler("CLI command timed out after ...")` with stdout/stderr tails like command stages.
  - On `CommandTermination::Exited`, keep existing parsing, usage accounting, changed-file detection, and cleanup behavior. Emit `agent.cli.completed` only for natural process exit.

- Make sandbox streaming cancellation actually terminate CLI-shaped descendants.
  - Local and Docker provider behavior must be covered by process-probe tests before switching CLI agents to `exec_command_streaming`.
  - Daytona is in scope and merge-blocking. Today `lib/crates/fabro-sandbox/src/daytona/mod.rs:1628-1634` returns `CommandTermination::Cancelled` without killing the process. Update Daytona streaming cancellation and timeout paths to terminate the running command/session and verify that descendant processes are gone before returning.
  - If the Daytona SDK has no per-command kill operation, delete/close the Daytona session on cancellation/timeout and wait for the process probe to show the marker process has exited. The PR is not complete until Daytona's streaming cancellation contract is reliable enough for CLI agents.

- Harden `AgentApiBackend` cancellation in `lib/crates/fabro-workflow/src/handler/llm/api.rs`.
  - Do not drop a running `session.initialize()` or `session.process_input(prompt)` future. Check `cancel_token.is_cancelled()` at fallback boundaries, and once a `Session` exists let the session bridge handle in-flight cancellation.
  - Do not race/drop `create_session_for(...)` or `self.create_session(...)`. Both call `Client::from_source(source).await`, which may refresh or persist credentials; use a pre-check and post-check around the awaited call instead of dropping it mid-flight. Specific sites that need pre/post-cancellation checks: the main-path constructions at `api.rs:450` and `api.rs:457`, and the failover-path construction at `api.rs:527`. Pattern: `if cancel_token.is_cancelled() { return Err(Error::Cancelled); } let session = self.create_session(...).await?; if cancel_token.is_cancelled() { return Err(Error::Cancelled); }` — the post-check catches cancellation that arrived during credential refresh inside `Client::from_source`.
  - Immediately after the `Session` is acquired (whether freshly created or pulled from `self.sessions` cache) and before any further `session.initialize().await` or `session.process_input(prompt).await`, install a per-invocation bridge task: await `cancel_token.cancelled()`, set `InterruptReason::Cancelled` through `session.interrupt_reason_handle()`, and cancel `session.cancel_token()`. The bridge must be installed on both the fresh-session path and the reuse path so cached sessions are also cancellable mid-`process_input`.
  - Add a local bridge guard type in `api.rs` so fallback cannot overwrite and leak old handles:
    ```rust
    struct SessionCancelBridgeGuard {
        handle: Option<tokio::task::JoinHandle<()>>,
    }

    impl SessionCancelBridgeGuard {
        fn replace(&mut self, run_token: CancellationToken, session: &Session) {
            self.abort();
            let interrupt_reason = session.interrupt_reason_handle();
            let session_token = session.cancel_token();
            self.handle = Some(tokio::spawn(async move {
                run_token.cancelled().await;
                *interrupt_reason.lock().unwrap() = Some(InterruptReason::Cancelled);
                session_token.cancel();
            }));
        }

        fn abort(&mut self) {
            if let Some(handle) = self.handle.take() {
                handle.abort();
            }
        }
    }

    impl Drop for SessionCancelBridgeGuard {
        fn drop(&mut self) {
            self.abort();
        }
    }
    ```
  - Use one `SessionCancelBridgeGuard` for the backend invocation. Call `bridge.replace(cancel_token.clone(), &session)` after acquiring the initial session and again after each fallback session replacement; `replace` aborts the previous bridge before installing the new one. Call `bridge.abort()` before reinserting a full-fidelity session into `AgentApiBackend.sessions`, before replacing/dropping a `Session` outside `bridge.replace(...)`, and before every explicit `return`. The guard's `Drop` is the panic-safety fallback, not the primary cleanup path.
  - Add an `AgentApiErrorDisposition` helper instead of a lossy `fabro_agent::Error -> fabro_workflow::Error` conversion:
    ```rust
    enum AgentApiErrorDisposition {
        Cancelled,
        FailoverEligible(fabro_llm::Error),
        Terminal(Error),
    }

    fn classify_agent_error(
        err: fabro_agent::Error,
        allow_failover: bool,
    ) -> AgentApiErrorDisposition {
        match err {
            fabro_agent::Error::Interrupted(InterruptReason::Cancelled) => {
                AgentApiErrorDisposition::Cancelled
            }
            fabro_agent::Error::Interrupted(InterruptReason::WallClockTimeout) => {
                AgentApiErrorDisposition::Terminal(Error::Precondition(
                    "Agent session hit its wall-clock timeout".to_string(),
                ))
            }
            fabro_agent::Error::Llm(err) if allow_failover && err.failover_eligible() => {
                AgentApiErrorDisposition::FailoverEligible(err)
            }
            fabro_agent::Error::Llm(err) => AgentApiErrorDisposition::Terminal(Error::Llm(err)),
            other @ (
                fabro_agent::Error::SessionClosed
                | fabro_agent::Error::InvalidState(_)
                | fabro_agent::Error::ToolExecution(_)
            ) => {
                AgentApiErrorDisposition::Terminal(Error::Precondition(format!(
                    "Agent session failed: {other}"
                )))
            }
        }
    }
    ```
  - Use failover-aware classification for `initialize()` as well as `process_input()`. On the primary provider, call `classify_agent_error(err, !self.fallback_chain.is_empty())` for `initialize()` errors; if it returns `FailoverEligible(err)`, set `last_err = Error::Llm(err)` and enter the fallback loop without calling primary `process_input()`.
  - Inside the fallback loop, compute `let allow_more_failover = index + 1 < self.fallback_chain.len();` and pass that value to `classify_agent_error` for both `session.initialize().await` and `session.process_input(prompt).await`. `FailoverEligible(err)` records `last_err = Error::Llm(err)` and continues to the next provider; `Terminal(err)` returns immediately; `Cancelled` returns `Error::Cancelled`.
  - Update `fabro-agent::Session::initialize` signature to `pub async fn initialize(&mut self) -> Result<(), fabro_agent::Error>`.
  - Sweep test call sites with `rg -n "session\\.initialize\\(\\)\\.await" lib/crates/fabro-agent` and update each to `.await?` (where the surrounding fn returns a Result) or `.await.unwrap()` for tests; the signature change is a compile break and these sites are not enumerated below. Known non-test call sites:
    - `lib/crates/fabro-workflow/src/handler/llm/api.rs:491`: convert `Interrupted(Cancelled)` to `fabro_workflow::Error::Cancelled`; convert other `fabro_agent::Error` values with the same helper used for `process_input` errors.
    - `lib/crates/fabro-workflow/src/handler/llm/api.rs:556`: same conversion as the main provider path, inside the fallback loop, before `process_input(prompt)` is attempted.
    - `lib/crates/fabro-retro/src/retro_agent.rs:207`: propagate with context, e.g. `session.initialize().await.context("Retro agent session initialization failed")?;`.
    - `lib/crates/fabro-agent/src/cli.rs:727`: use `session.initialize().await?;` so the CLI exits non-zero and renders the existing `fabro_agent::Error`.
    - `lib/crates/fabro-agent/src/subagent.rs:114`: use `session.initialize().await?;` inside the spawned task so subagent initialization failure is returned to the parent as `fabro_agent::Error`.
    - `lib/crates/fabro-agent/src/v4a_patch.rs:1469`: use `.await.unwrap()` or `?` according to the surrounding test/helper return type.
  - Update public examples/docs that call `initialize()`:
    - `lib/crates/fabro-agent/README.md:143` (the top-level repo `README.md` does not contain a call site at line 143)
    - `docs/public/reference/sdk.mdx:45` (code example)
    - `docs/public/reference/sdk.mdx:82` is a method-description table row and can stay as-is (or update to `initialize().await?` if the example signature changes)
  - Make initialization cancellation-aware by threading a `CancellationToken` through helper methods that start or wait on sandbox work:
    - `lib/crates/fabro-agent/src/session.rs::resolve_sandbox_mcp_servers`
    - `lib/crates/fabro-agent/src/session.rs::start_sandbox_mcp_server`
    - `lib/crates/fabro-agent/src/session.rs::build_env_context`
    - `lib/crates/fabro-agent/src/memory.rs::discover_memory`
    - `lib/crates/fabro-agent/src/skills.rs::discover_skills`
  - Pass child tokens to all `exec_command` calls in initialization (`session.rs:300`, `312`, `324`, `363`, `373`, `385`). Check the token before and after `read_file` and `glob` calls in `discover_memory` and `discover_skills`; this PR does not change the `Sandbox::read_file` or `Sandbox::glob` signatures, so an individual provider file call is not interruptible mid-await.
  - For sandbox MCP startup, if cancellation happens after a detached MCP server PID is known, terminate the MCP process group before returning `fabro_agent::Error::Interrupted(InterruptReason::Cancelled)`.
  - Apply `AgentApiErrorDisposition` consistently at `api.rs:491`, `api.rs:556`, and every `process_input(prompt)` error match. `Interrupted(Cancelled)` must propagate as `Error::Cancelled`; `Interrupted(WallClockTimeout)`, `SessionClosed`, `InvalidState`, and `ToolExecution` must be terminal/non-retryable workflow errors; failover-eligible LLM errors must still advance to the next configured provider.
  - The full-fidelity session cache is `AgentApiBackend.sessions`, keyed by thread id. The backend already removes a cached session before use and reinserts it only on success. Keep that pattern: cancelled, failed, or timed-out sessions are dropped and never reinserted.
  - Apply the same cancellation bridge and conversion behavior to fallback-provider sessions.

- Add public event shapes for non-exited CLI termination.
  - Add `Event::AgentCliCancelled` and external name `agent.cli.cancelled`.
  - Add `Event::AgentCliTimedOut` and external name `agent.cli.timed_out`.
  - Add matching `EventBody` variants and props in `fabro-types`.
  - Props for both events: `stdout`, `stderr`, `duration_ms`.
  - Store `node_id` in the event envelope like `agent.cli.started` and `agent.cli.completed`.
  - `RunEvent` is reused into `fabro-api` via `lib/crates/fabro-api/build.rs`, while the OpenAPI schema currently models `event` as a free string and `properties` as `additionalProperties`. Adding typed `EventBody` variants therefore requires Rust type changes and event tests, not an OpenAPI schema discriminator change. Change `docs/public/api-reference/fabro-api.yaml` only if adding or updating event examples.
  - Audit run-event consumers with:
    ```bash
    rg "agent\\.cli\\.completed|AgentCliCompleted|agent\\.cli|EventBody::AgentCli|RunEvent" apps lib docs/public README.md
    rg "agent\\.cli\\.completed|agent\\.cli\\.started|AgentCli" apps/fabro-web/app
    ```
  - Update every exhaustive `EventBody` match that needs to compile after adding variants, including run projection, fork replay filters, CLI progress rendering, and server event handling if the compiler reports them.
  - Inspect by hand (these compile silently because they use `_ =>` or `matches!` and the compiler will NOT flag them):
    - `lib/crates/fabro-store/src/run_state.rs` apply_event match — populate `stdout`/`stderr`/`duration_ms`/termination for the new variants analogously to `CommandCompleted`/`AgentCliCompleted`, otherwise stage projection drops the cancellation/timeout metadata.
    - `lib/crates/fabro-workflow/src/operations/fork.rs` `is_replay_relevant` `matches!` — decide whether `AgentCliCancelled`/`AgentCliTimedOut` are replay-relevant and add to the list.
    - `lib/crates/fabro-cli/src/commands/run/run_progress/event.rs` — add explicit progress rendering for the new variants.
    - `lib/crates/fabro-server/src/server.rs` event-dispatch matches — confirm wildcard arms are intentional or add explicit handling.
    - `apps/fabro-web/app/**/*.ts*` — `rg -i "agent\\.cli|AgentCli|agent_cli" apps/fabro-web/` is currently empty; the web app does not render `agent.cli.*` events explicitly today. The new variants will fall through whatever generic event-rendering path `agent.cli.completed` uses today (likely none beyond the run timeline). No web changes are required for cancellation/timeout unless a renderer is added in this PR.
  - If `docs/public/api-reference/fabro-api.yaml` changes, run `cargo build -p fabro-api` and `cd lib/packages/fabro-api-client && bun run generate`. If it does not change, record why regeneration is unnecessary in the PR notes.

## Test Plan

- Add core/workflow cancellation-token tests.
  - `fabro-core` executor: a cancelled `CancellationToken` returns `Err(Error::Cancelled)` at the existing between-node check.
  - `fabro-core` executor: cancelling the token from a handler causes the next node boundary to return `Err(Error::Cancelled)`.
  - `fabro-workflow` run options: default/test constructors create a non-cancelled `CancellationToken`.
  - `RunServices`: `with_emitter`, `with_run_store`, `with_sandbox`, and `with_cancel_token` clone/rebuild paths must not cancel the original run token when intermediate `Arc<RunServices>` values are dropped.
  - Stall timeout remains distinct from user cancellation: existing `executor_stall_token_interrupts_handler`, `executor_stall_token_interrupts_backoff_sleep`, and `executor_stall_token_interrupts_before_attempt` tests must continue asserting `Err(Error::StallTimeout { .. })`, while user cancellation tests assert `Err(Error::Cancelled)`.
  - Manager loop: parent-token cancellation cancels the child token and the child executor stops before the next non-agent node.

- Add focused workflow tests for agent cancellation.
  - CLI backend: fake sandbox returns `ExecStreamingResult` with `CommandTermination::Cancelled`; assert backend returns `Error::Cancelled`, records a streaming cancel token, emits `agent.cli.cancelled`, does not emit `agent.cli.completed`, and runs temp cleanup.
  - CLI backend: fake sandbox returns `ExecStreamingResult` with `CommandTermination::TimedOut`; assert backend returns a handler timeout error, emits `agent.cli.timed_out`, does not emit `agent.cli.completed`, and runs temp cleanup.
  - CLI backend: no `node.timeout()` passes `None` to `exec_command_streaming`, preserving the current unbounded CLI-agent runtime.
  - Command handler: command stages still pass `Some(600_000)` when `node.timeout()` is absent.
  - Agent handler: mock backend captures its `CancellationToken`; assert `AgentHandler` passes the same run token semantics as `services.run.cancel_token()` and that it fires when the run token is cancelled.
  - Agent handler: mock backend returns `Error::Cancelled`; assert `AgentHandler::execute` returns `Err(Error::Cancelled)`.
  - Prompt handler: mock backend returns `Error::Cancelled`; assert `PromptHandler::execute` returns `Err(Error::Cancelled)` instead of a failed outcome.
  - End-to-end workflow executor: cancel during an agent stage and assert the run terminates through the cancelled path, not through a non-retryable failed stage outcome. This test must cover the bridge from `AgentHandler::execute` through node-handler outcome conversion, `Error::is_retryable`, retry handling, and final run status classification.
  - Manager loop: child workflow containing an agent stage receives a token that fires both on parent run cancellation and on direct manager-loop child cancellation from stop-condition and max-cycle paths.

- Add API backend cancellation coverage.
  - Unit test the run-token-to-session-token bridge: when the run token fires, the session cancel token fires and `InterruptReason::Cancelled` is set.
  - Unit test bridge cleanup: after a successful full-fidelity backend invocation reinserts a cached session, cancelling the old invocation token does not cancel or interrupt that cached session.
  - Unit test `SessionCancelBridgeGuard::replace`: replacing the bridge aborts the prior handle before storing the new handle.
  - Unit test `SessionCancelBridgeGuard::drop`: dropping the guard aborts an installed bridge.
  - Unit test fallback cleanup: when failover replaces one `Session` with another, the bridge for the previous session is aborted before the previous session is dropped.
  - Unit test `AgentApiErrorDisposition`: `Interrupted(Cancelled)` becomes `Cancelled`; failover-eligible `Llm` becomes `FailoverEligible` only when `allow_failover` is true; non-eligible `Llm` becomes `Terminal(Error::Llm(_))`; `Interrupted(WallClockTimeout)`, `SessionClosed`, `InvalidState`, and `ToolExecution` become terminal non-retryable workflow errors.
  - Unit test failover loop behavior: failover-eligible `process_input` LLM errors still advance to the next fallback provider instead of returning immediately through the conversion helper.
  - Unit test initialize failover behavior: a failover-eligible LLM error from primary `session.initialize().await` enters the fallback loop when providers remain, and a failover-eligible LLM error from a fallback session's initialize continues to the next fallback provider when one remains.
  - Add a test that a cancelled API backend path does not reinsert the session into the reuse cache.
  - Add `Session::initialize` tests that cancel before memory discovery, during sandbox MCP startup/readiness polling, and during environment-context `exec_command`; each returns `Interrupted(Cancelled)` and does not proceed to `process_input`.
  - Add call-site tests or compile-time updates proving `retro_agent`, `fabro-agent` CLI, subagent spawning, and `v4a_patch` handle `initialize().await?` or explicit error conversion.

- Add event conversion tests.
  - Verify `agent.cli.cancelled` event name.
  - Verify `agent.cli.timed_out` event name.
  - Verify `to_run_event` maps `node_id` into the envelope and serializes props under `properties` for both new events.
  - If OpenAPI docs/examples change, run the existing OpenAPI conformance test and regenerate the TypeScript client.

- Add sandbox-provider verification for CLI subprocess cleanup.
  - Local fake/unit tests cover token propagation.
  - Sandbox trait tests cover `exec_command_streaming(..., None, ...)`: it does not time out by default and still returns promptly on cancellation.
  - Default trait implementation test: a mock that implements only `exec_command` receives `u64::MAX` when `exec_command_streaming(..., None, ...)` uses the fallback implementation.
  - Docker: add or reuse a streaming timeout/cancel process-probe test that proves descendant CLI-shaped commands are gone before return.
  - Daytona: add an ignored live test that runs a long `node` or shell command through `exec_command_streaming`, cancels it, then probes the Daytona sandbox for the marker process. This is a merge gate for the Daytona streaming path: the process must be gone before CLI agents are routed through `exec_command_streaming` on Daytona.

- Run verification:
  - `cargo nextest run -p fabro-workflow`
  - `cargo nextest run -p fabro-agent`
  - `cargo nextest run -p fabro-types`
  - `cargo nextest run -p fabro-sandbox`
  - `cargo nextest run -p fabro-server openapi_conformance`
  - `cd apps/fabro-web && bun test`
  - `cd apps/fabro-web && bun run typecheck`
  - `cargo +nightly-2026-04-14 fmt --check --all`
  - `cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings`

## Assumptions

- Scope includes both CLI and API agent backend cancellation, per the chosen direction.
- The fix uses the existing `Sandbox::exec_command_streaming` cancellation behavior instead of introducing local-only `tokio::process::Command` management, but only after provider cancellation actually kills descendant processes.
- `agent.cli.cancelled` and `agent.cli.timed_out` are additive events; existing `agent.cli.completed` remains only for natural process completion.
- `CodergenBackend::run` signature churn is accepted because cancellation is a required execution input. Do not hide cancellation in `Context`.
- `Session::initialize` signature churn is accepted and must be propagated to all workspace callers and public examples.
- This PR does not make `Sandbox::read_file` or `Sandbox::glob` cancellable mid-await. Initialization checks cancellation before and after those calls; sandbox `exec_command` calls receive child tokens.
- CLI-agent runtime remains effectively unbounded when `node.timeout()` is absent. The rejected alternative was reusing the command-stage 600-second default; the plan instead makes streaming timeout optional.
- Daytona streaming cancellation is merge-blocking for routing Daytona CLI agents through the new managed streaming path.
- Live steering of CLI-mode agents remains out of scope.


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
  - Script: `cargo +nightly-2026-04-14 clippy -q --workspace --all-targets -- -D warnings 2>&1`
  - Stdout: (empty)
  - Stderr: (empty)
- **implement**: succeeded
  - Model: claude-opus-4-7, 357.9k tokens in / 125.5k out
  - Files: /home/daytona/workspace/docs/public/reference/sdk.mdx, /home/daytona/workspace/lib/crates/fabro-agent/README.md, /home/daytona/workspace/lib/crates/fabro-agent/src/cli.rs, /home/daytona/workspace/lib/crates/fabro-agent/src/memory.rs, /home/daytona/workspace/lib/crates/fabro-agent/src/session.rs, /home/daytona/workspace/lib/crates/fabro-agent/src/skills.rs, /home/daytona/workspace/lib/crates/fabro-agent/src/subagent.rs, /home/daytona/workspace/lib/crates/fabro-cli/src/commands/run/runner.rs, /home/daytona/workspace/lib/crates/fabro-core/src/executor.rs, /home/daytona/workspace/lib/crates/fabro-core/src/stall.rs, /home/daytona/workspace/lib/crates/fabro-retro/src/retro_agent.rs, /home/daytona/workspace/lib/crates/fabro-sandbox/src/daytona/mod.rs, /home/daytona/workspace/lib/crates/fabro-sandbox/src/docker.rs, /home/daytona/workspace/lib/crates/fabro-sandbox/src/local.rs, /home/daytona/workspace/lib/crates/fabro-sandbox/src/sandbox.rs, /home/daytona/workspace/lib/crates/fabro-sandbox/src/worktree.rs, /home/daytona/workspace/lib/crates/fabro-sandbox/tests/daytona_streaming_live.rs, /home/daytona/workspace/lib/crates/fabro-sandbox/tests/docker_streaming.rs, /home/daytona/workspace/lib/crates/fabro-server/Cargo.toml, /home/daytona/workspace/lib/crates/fabro-server/src/server.rs, /home/daytona/workspace/lib/crates/fabro-server/src/server/handler/lifecycle.rs, /home/daytona/workspace/lib/crates/fabro-workflow/src/devcontainer_bridge.rs, /home/daytona/workspace/lib/crates/fabro-workflow/src/handler/agent.rs, /home/daytona/workspace/lib/crates/fabro-workflow/src/handler/command.rs, /home/daytona/workspace/lib/crates/fabro-workflow/src/handler/fan_in.rs, /home/daytona/workspace/lib/crates/fabro-workflow/src/handler/human.rs, /home/daytona/workspace/lib/crates/fabro-workflow/src/handler/llm/api.rs, /home/daytona/workspace/lib/crates/fabro-workflow/src/handler/llm/cli.rs, /home/daytona/workspace/lib/crates/fabro-workflow/src/handler/manager_loop.rs, /home/daytona/workspace/lib/crates/fabro-workflow/src/handler/mod.rs, /home/daytona/workspace/lib/crates/fabro-workflow/src/handler/parallel.rs, /home/daytona/workspace/lib/crates/fabro-workflow/src/handler/prompt.rs, /home/daytona/workspace/lib/crates/fabro-workflow/src/lifecycle/git.rs, /home/daytona/workspace/lib/crates/fabro-workflow/src/operations/start.rs, /home/daytona/workspace/lib/crates/fabro-workflow/src/pipeline/execute.rs, /home/daytona/workspace/lib/crates/fabro-workflow/src/pipeline/execute/tests.rs, /home/daytona/workspace/lib/crates/fabro-workflow/src/pipeline/finalize.rs, /home/daytona/workspace/lib/crates/fabro-workflow/src/pipeline/initialize.rs, /home/daytona/workspace/lib/crates/fabro-workflow/src/pipeline/retro.rs, /home/daytona/workspace/lib/crates/fabro-workflow/src/run_options.rs, /home/daytona/workspace/lib/crates/fabro-workflow/src/services.rs, /home/daytona/workspace/lib/crates/fabro-workflow/tests/it/integration.rs, lib/crates/fabro-types/src/run_event/misc.rs, lib/crates/fabro-types/src/run_event/mod.rs, lib/crates/fabro-workflow/src/event/convert.rs, lib/crates/fabro-workflow/src/event/events.rs, lib/crates/fabro-workflow/src/event/names.rs, lib/crates/fabro-workflow/src/event/stored_fields.rs, lib/crates/fabro-workflow/src/operations/fork.rs


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