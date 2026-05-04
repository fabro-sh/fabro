Goal: # Patch silent-degrade sites in fabro-workflow / fabro-sandbox

> **Note on filename:** `make-a-plan-to-abstract-hamming.md` is the harness-prescribed
> path for this plan and does not reflect the content. Future readers should treat the
> file body as authoritative.

## Context

The user noticed that when `worktree_mode = always` is set and the cwd is not a Git repo,
`resolve_worktree_base_sha` (`lib/crates/fabro-workflow/src/pipeline/initialize.rs:74-76`)
returns `Ok(None)` on `"not a git repository"`, the caller's `else` branch (line 506-508)
wraps the bare sandbox, and `options.run_options.git` is reset to `None` — with no
`Emitter::notice` and no `tracing::warn!`. The user asked for X, got not-X, and was told nothing.

A short audit surfaced several more sites with the same anti-pattern. Goal: every
*genuine* silent-degrade site emits a stable signal that reaches `fabro logs` / SSE / retro.
Sandbox-internal sites without an Emitter are tracing-only, with the event-stream
follow-up tracked separately. Behavior is unchanged — the fallback still happens; it just
announces itself.

## Revised fix pattern

This pattern was rewritten in response to reviewer feedback (Event::trace already logs
warn-level notices; failover already has a typed event; not every `Ok(None)` is a degradation).

1. **Default:** at the fallback site, call
   `emitter.notice(RunNoticeLevel::Warn, "<stable_code>", "<message>")`.
   This routes through `Event::RunNotice` whose `Event::trace()` arm
   (`event/events.rs:696-710`) already emits a `warn!(code, message, "Run notice")` — so
   a notice alone covers both `server.log` and the run feed.
2. **Add a separate `tracing::warn!` only when** there are structured diagnostic fields
   absent from the notice trace (`error = %err`, `refspec`, `provider`, `model`,
   `worktree_mode`, etc.). Plain restatements of the notice message do not justify a
   second log line.
3. **Use a typed event when one already exists** (e.g. `Event::Failover` for LLM provider
   failover, `Event::RetroFailed` for retro problems). Don't introduce a parallel
   `RunNotice` for behavior already represented by a typed event.
4. **Gate the warning on user intent.** If the fallback path is the *expected* outcome
   for the user's configuration (e.g. local in-place, no-clone sandbox), do not warn.
   Only warn when the user implicitly or explicitly asked for the non-fallback path.
5. **Stable code naming:** `<feature>_<state>` lowercase snake. Codes are a contract;
   pick once.
6. **Severity:** `RunNoticeLevel::Warn` for "user asked for X, didn't get X."
   `RunNoticeLevel::Info` for benign post-conditions like sandbox preserved. LLM
   failover uses `Event::Failover` (already typed), not `RunNotice`.

Reference: `dirty_worktree` notice at `pipeline/initialize.rs:156-161`; `git_diff_failed`
at `lifecycle/git.rs:306-310`; existing `Event::Failover` emit at `handler/llm/api.rs:510-520`.

## Per-site patches

### 1. Worktree skipped on non-git cwd  *(the original)*

`lib/crates/fabro-workflow/src/pipeline/initialize.rs:506-520`

In the `else` branch at line 506 (where `resolve_worktree_base_sha` returned `Ok(None)`):

- `tracing::warn!(worktree_mode = ?options.worktree_mode, "worktree requested but cwd is not a git repository; running without a worktree")`
  — keeps the structured `worktree_mode` field that's not in the notice payload.
- `options.emitter.notice(RunNoticeLevel::Warn, "worktree_skipped_no_git", "Worktree mode requested but no Git repository was found; running without a worktree.")`

### 2. Sandbox `setup_git` returned `Ok(None)` **when git was expected**

`lib/crates/fabro-workflow/src/pipeline/initialize.rs:602-626`

The `Ok(None) => {}` arm covers two real cases:

a. The sandbox had an origin / git was expected → `Ok(None)` is a degradation.
b. The sandbox is clone-less / no-origin → `Ok(None)` is the normal outcome.

The surrounding code already discriminates: line 596 only calls `ensure_git_available`
when `sandbox.origin_url().is_some()`. Reuse that signal:

```rust
Ok(None) => {
    if sandbox.origin_url().is_some() {
        options.emitter.notice(
            RunNoticeLevel::Warn,
            "sandbox_git_unavailable",
            "Sandbox could not set up Git despite a configured origin; running without checkpointing or PR support.",
        );
    }
}
```

No additional `tracing::warn!` — the notice trace covers it; no extra structured fields
worth emitting.

### 3. Checkpoint push failure

`lib/crates/fabro-workflow/src/lifecycle/git.rs:277-289`

Existing `tracing::warn!(refspec, error, ...)` at line 280-284 carries structured
fields and stays. Add a notice in the same `Err(err)` arm, before `false`:

```rust
self.emitter.emit(&Event::RunNotice {
    level:   RunNoticeLevel::Warn,
    code:    "git_push_failed".to_string(),
    message: format!("Failed to push run branch {branch}: {err}"),
});
```

(Matches the local style at `lifecycle/git.rs:306-310` which already builds `Event::RunNotice`
directly because `self.emitter` is a `&Emitter`.)

### 4. Parallel base checkpoint failure

`lib/crates/fabro-workflow/src/handler/parallel.rs:200-209`

Existing `tracing::warn!(error = %e, ...)` at line 206 stays. Add a notice between the
`warn!` and `None`:

```rust
services.run.emitter.notice(
    RunNoticeLevel::Warn,
    "parallel_base_checkpoint_failed",
    format!("Could not checkpoint base state before parallel branches: {e}"),
);
```

Update the file-top imports to include `RunNoticeLevel` from `crate::event`
(see `pipeline/initialize.rs` for the same import shape).

### 5. GitHub token mint failure

`lib/crates/fabro-workflow/src/pipeline/initialize.rs:238-247`

Already emits `notice("github_token_failed", …)`. The notice message embeds `{e}` as a
plain string, but the structured `error` field is absent from the notice trace
(`events.rs:704-706` only carries `code` and `message`). Per the revised rule
(structured fields not in the notice trace justify a separate `tracing::warn!`), add
a structured warn line immediately before the existing `emitter.notice(...)`:

```rust
tracing::warn!(error = %e, "Failed to mint GitHub token");
```

### 6. LLM provider failover surfaced through `one_shot` path

`lib/crates/fabro-workflow/src/handler/llm/api.rs:283-402`

The existing `chat()` path at line 510-520 emits `Event::Failover` per attempt with
`stage`, `from_provider/model`, `to_provider/model`, `error`. The `one_shot` path
(line 283-402) does not, because:

- `AgentApiBackend` has no `emitter` field (struct definition at 117-125).
- The `CodergenBackend::one_shot` trait method has no emitter parameter (signature at 283-288).

Approach: plumb `&Arc<Emitter>` into the trait, then emit the existing `Event::Failover`
(no new code; reuses what `chat()` already does).

Steps:

1. Change `CodergenBackend::one_shot` signature in the trait at `handler/agent.rs:36-…`
   (default method at `handler/agent.rs:50`) to add `emitter: &Arc<Emitter>` and a
   `&StageScope` parameter (mirroring `chat()`'s emit at `api.rs:510-520`). Then update
   every implementor — find them with:
   ```
   rg -n "async fn one_shot\(" lib/crates/fabro-workflow
   ```
   At time of writing this finds:
   - Trait default — `handler/agent.rs:50`
   - `AgentApiBackend::one_shot` — `handler/llm/api.rs:283`
   - `BackendRouter::one_shot` — `handler/llm/cli.rs:808`
     (`AgentCliBackend` uses the trait default, not its own impl — leave as-is.)
   - Test stubs in `handler/prompt.rs:277, 337, 394`
   - Integration test stub in `tests/it/integration.rs:6219`
   Re-run the rg before editing in case more impls have been added.
2. Caller `handler/prompt.rs:107-109` passes `&services.run.emitter` and the prompt's
   `stage_scope`.
3. Inside the `one_shot` failover loop in `api.rs:349-399`, emit `Event::Failover` per
   attempt, exactly as the `chat()` loop at line 510-520 does. Each iteration of the
   `for target in fallback_chain` loop emits one event before attempting the call.
4. **Delete the existing `tracing::warn!` at `api.rs:361-369`.** `Event::Failover::trace()`
   at `events.rs:1083-1090` already emits `warn!(stage, from_provider, from_model,
   to_provider, to_model, error, ...)` — identical fields. Keeping both produces a
   duplicate WARN per attempt. (The `chat()` path correctly does not have a
   parallel `tracing::warn!`; this is making `one_shot` consistent with it.) No new
   `RunNotice` code; `agent.failover` is the canonical event name (`event/names.rs:113`).

Tests: extend whatever exercises the one-shot failover branch to assert an
`agent.failover` event is recorded.

### 7. Sandbox stdout/stderr drain failure (tracing-only, scope-bounded)

`lib/crates/fabro-sandbox/src/local.rs:282-295`

`fabro-sandbox` has no `Emitter` access at this depth, and the event-stream surface
is `SandboxEventCallback`. Plumbing a new `SandboxEvent::PipeReadFailed` through
`fabro-types` + `event_name` + `EventBody` is intentionally out of scope for this batch
(decided with the user). Do the tracing-only fix here:

```rust
let stdout_task = tokio::spawn(async move {
    let mut buf = String::new();
    if let Some(ref mut r) = stdout_pipe {
        if let Err(err) = r.read_to_string(&mut buf).await {
            tracing::warn!(error = %err, stream = "stdout", "Failed to drain child stdout");
        }
    }
    buf
});
// same shape for stderr_task
```

Goal-narrowing acknowledgment: this site is fixed in `server.log` only — event-stream
visibility is a follow-up.

## Out of scope (verified — adequately surfaced today)

- **MCP server failed (`fabro-agent/src/session.rs:253-265`)** — emits
  `AgentEvent::McpServerFailed` *and* `tracing::warn!`. Adequate.
- **Retro failures (`pipeline/retro.rs:19, 28, 40`)** — emits `Event::RetroFailed` *and*
  `tracing::warn!`. Adequate.
- **`pipeline/finalize.rs:72` (`state_result.ok()`)** / `pipeline/pull_request.rs:205`
  / `pipeline/initialize.rs:340-346` — internal projection / explicit user config; not
  silent-degrade.

## Follow-ups (deliberately deferred)

- Plumb `SandboxEvent::PipeReadFailed` through the existing `SandboxEventCallback`
  (variant + `event_name` + `EventBody` mapping per `docs/internal/events-strategy.md`)
  so site 7's truncation reaches the run feed.

## Files to modify

1. `lib/crates/fabro-workflow/src/pipeline/initialize.rs` (sites 1, 2, 5)
2. `lib/crates/fabro-workflow/src/lifecycle/git.rs` (site 3)
3. `lib/crates/fabro-workflow/src/handler/parallel.rs` (site 4 — also import `RunNoticeLevel`)
4. `lib/crates/fabro-workflow/src/handler/agent.rs` (site 6 — `CodergenBackend` trait + default `one_shot` signature)
5. `lib/crates/fabro-workflow/src/handler/llm/api.rs` (site 6 — `AgentApiBackend::one_shot` impl + `Event::Failover` emit + delete duplicate `tracing::warn!`)
6. `lib/crates/fabro-workflow/src/handler/llm/cli.rs` (site 6 — `BackendRouter::one_shot` forward params)
7. `lib/crates/fabro-workflow/src/handler/prompt.rs` (site 6 — caller plumbing + test stubs at 277/337/394)
8. `lib/crates/fabro-workflow/tests/it/integration.rs` (site 6 — test stub at 6219)
9. `lib/crates/fabro-sandbox/src/local.rs` (site 7 — tracing only)

Re-run `rg -n "async fn one_shot\(" lib/crates/fabro-workflow` before editing site 6 to
catch any new `one_shot` impls added since this plan.

## Stable codes added

- `worktree_skipped_no_git` — Warn (site 1)
- `sandbox_git_unavailable` — Warn (site 2, gated on `origin_url.is_some()`)
- `git_push_failed` — Warn (site 3)
- `parallel_base_checkpoint_failed` — Warn (site 4)

(Site 5 reuses the existing `github_token_failed` notice; only adds a structured
`tracing::warn!`. Site 6 reuses the existing `agent.failover` event; no new stable
notice code or `RunNotice` code is introduced.)

## Verification

1. Build: `cargo build --workspace`
2. Unit tests: `cargo nextest run -p fabro-workflow -p fabro-sandbox`
3. New unit tests, one per behavioral change:
   - **Worktree skip** — extend the existing
     `resolve_worktree_plan_uses_local_worktree_without_pre_run_git_context`
     (`pipeline/initialize.rs:957`) into an `init`-level test using a non-git scratch
     dir; assert a `RunNotice` with code `worktree_skipped_no_git` is emitted.
   - **Sandbox git unavailable (gated)** — two cases: (a) sandbox with `origin_url =
     Some(...)` returning `Ok(None)` from `setup_git` emits `sandbox_git_unavailable`;
     (b) sandbox with `origin_url = None` returning `Ok(None)` emits *no* notice.
     Confirms the gating works.
   - **Push failure** — extend lifecycle/git tests to fake a failing `git_push_ref`
     and assert `git_push_failed` notice + the push_results entry.
   - **Parallel base checkpoint failure** — `handler/parallel.rs` calls the free
     function `checked_git_checkpoint(...)` (line 188) on the sandbox; there is no
     creator interface to fake. Test by constructing the parallel handler with a
     scripted sandbox where the git probe succeeds (so `git_state` is `Some(_)`) and
     the actual `git commit` / checkpoint command fails, populate `services.git_state`,
     and assert the `parallel_base_checkpoint_failed` notice fires. Pattern off
     existing parallel-handler tests in the same file.
   - **One-shot LLM failover** — `AgentApiBackend::one_shot` constructs its
     `Client::from_source(self.source.as_ref())` internally (`api.rs:289`), so the
     existing seam is the `Arc<dyn CredentialSource>`. Test approach: provide a stub
     `CredentialSource` returning credentials that point at an `httpmock` server,
     program mock A to return a failover-eligible status (e.g. 529 / overloaded for
     Anthropic), program mock B to return success, and assert exactly one
     `Event::Failover` was emitted with the right `from_*` / `to_*` properties.
     Mirror an existing `httpmock`-based test from `fabro-llm` integration tests if
     one already covers `failover_eligible` mapping.
   - **GitHub token mint warn** — site 5 only adds a `tracing::warn!`; the user-facing
     notice is unchanged. If existing tests cover the `Err(e)` arm of `mint_token`
     (likely in `pipeline::initialize::tests` or `fabro-github` tests), assert the
     warn line via `tracing-test` / `tracing_subscriber::fmt::TestWriter`. Otherwise,
     this is covered by code inspection plus the manual smoke run; do not add a new
     test just for the warn line.
   - **Sandbox pipe drain** — a closed pipe reads as `Ok(0)` (EOF), not `Err`, so a
     direct unit test of the inline closure is awkward. Two acceptable approaches:
     (a) extract the read loop into a `drain_pipe<R: AsyncRead + Unpin>(reader: &mut R, stream: &str)`
     helper and unit-test it with a custom `AsyncRead` impl whose `poll_read` returns
     `Poll::Ready(Err(io::Error::other("simulated")))`; or (b) keep the inline
     `if let Err(...) = ...` and verify only by manual smoke (run a command that
     terminates abnormally and confirm the WARN line in `server.log`). Prefer (a) if
     the small refactor is cheap; otherwise (b) is fine — record the choice in the
     PR description.
4. **Manual smoke test (concrete)** for the worktree case end-to-end. Build the
   workflow inline so the test does not depend on repo workflows or LLM credentials —
   only the sandbox needs to start:
   ```bash
   tmp=$(mktemp -d)
   mkdir -p "$tmp/.fabro/workflows/baresmoke"
   cat > "$tmp/.fabro/workflows/baresmoke/workflow.toml" <<'EOF'
   _version = 1

   [workflow]
   graph = "workflow.fabro"
   EOF
   cat > "$tmp/.fabro/workflows/baresmoke/workflow.fabro" <<'EOF'
   digraph BareSmoke {
       graph [goal="non-git smoke test for worktree_skipped_no_git"]
       rankdir=LR

       start [shape=Mdiamond, label="Start"]
       exit  [shape=Msquare, label="Exit"]

       hello [label="Hello", shape=parallelogram, script="echo hello"]

       start -> hello -> exit
   }
   EOF
   cd "$tmp"
   fabro run baresmoke --no-retro --auto-approve
   ```
   - `fabro run <name>` resolves `<cwd>/.fabro/workflows/<name>/workflow.toml`, so the
     workflow must land at that exact path.
   - The `script` node uses the same shape as the existing `smoke` workflow
     (`.fabro/workflows/smoke/workflow.fabro`) — `parallelogram` + `script="..."` —
     which runs purely in the sandbox shell with no LLM calls. `goal_gate=true` is
     intentionally omitted: the real `smoke` workflow pairs it with `retry_target=exit`
     in graph attrs, and using `goal_gate` without `retry_target` trips the
     `goal_gate_has_retry` validation warning. This smoke only needs to reach
     initialization and exec one command, so the gate isn't needed.
   - `mktemp -d` is intentionally non-git, so this exercises the worktree-skipped path
     even with `worktree_mode = always` (the local-sandbox default).
   - Confirm `<storage>/logs/server.log` has `code="worktree_skipped_no_git" ... "Run notice"`.
   - Confirm `fabro logs <run_id>` (or the SSE/UI run feed) shows the notice.
5. Format and lint:
   - `cargo +nightly-2026-04-14 fmt --all`
   - `cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings`

## Reviewer feedback acknowledgments (round 1 P1/P2 incorporated)

- LLM failover patch redesigned around the existing `Event::Failover` and a
  trait-signature change to `CodergenBackend::one_shot`; `self.emitter` was a fiction.
- "Always emit both notice and `tracing::warn!`" rule replaced with a structured-fields
  predicate; relies on `Event::trace()` for the warn-level log of every notice.
- `setup_git` `Ok(None)` warning is now gated on `sandbox.origin_url().is_some()`.
- LLM failover severity contradiction removed; `RunNotice`-vs-`Event::Failover`
  distinction now explicit.
- Site 7 reframed as deliberate scope narrowing with a follow-up; goal text updated.
- Test plan now covers sites 4, 6, and 7.
- Manual smoke command made self-contained with a copied fixture workflow.

## Reviewer feedback acknowledgments (round 2)

- **Failover duplicate WARN**: `Event::Failover::trace()` (`events.rs:1083-1090`) already
  emits `warn!` with the same fields as the existing `tracing::warn!` at `api.rs:361-369`.
  Plan now explicitly deletes that line as part of site 6.
- **Trait file**: `handler/agent.rs` (where `CodergenBackend` and the default `one_shot`
  live) added to files-to-modify.
- **Implementor list**: replaced the hand-written list with an `rg` recipe; the only
  real impls today are the trait default, `AgentApiBackend`, and `BackendRouter`. Test
  stubs are now called out separately. `AgentCliBackend` does not have its own `one_shot`.
- **Parallel emitter handle**: now `services.run.emitter`, with the `RunNoticeLevel`
  import call-out.
- **Pipe-drain test**: closed pipes read as EOF; replaced the "pre-closed reader"
  shorthand with a real choice between (a) extract a `drain_pipe` helper testable with
  a custom `AsyncRead`, or (b) drop the automated test and rely on manual smoke.

## Reviewer feedback acknowledgments (round 3)

- **Smoke workflow doesn't exist**: this repo's workflows are `gh-triage`, `hello`,
  `implement-issue`, `implement-plan`, `smoke` — no `repl`, and `hello` requires LLM
  credentials. Smoke recipe rewritten to build a minimal command-only workflow inline
  using the same `parallelogram` + `script="..."` shape used by `.fabro/workflows/smoke/`,
  so it runs purely in the sandbox shell without LLM creds.
- **GitHub token rule contradiction**: an embedded `{e}` in a notice message is not a
  structured field. Site 5 reinstated with `tracing::warn!(error = %e, ...)` to honor
  the structured-fields rule. Removed the contradicting "out of scope" entry.
- **Failover test injection**: `AgentApiBackend::one_shot` constructs
  `Client::from_source(self.source.as_ref())` internally, so the seam is the existing
  `Arc<dyn CredentialSource>`. Test recipe spelled out with stub `CredentialSource` +
  `httpmock` returning failover-eligible from A and success from B.
- **Parallel test wording**: there is no checkpoint-creator interface — the handler
  calls free function `checked_git_checkpoint(...)`. Test recipe rewritten to drive
  a scripted sandbox where the git probe succeeds and the checkpoint command fails,
  with `services.git_state = Some(_)`.

## Reviewer feedback acknowledgments (round 4)

- **Files-to-modify**: site 5 also lives in `pipeline/initialize.rs`; entry corrected
  to `(sites 1, 2, 5)`.
- **Site 5 verification**: added an explicit verification entry stating the new
  `tracing::warn!` is covered by code inspection plus the manual smoke run, with an
  optional `tracing-test` assertion if existing token-mint test scaffolding exists.
- **Smoke `goal_gate` validation warning**: `goal_gate=true` removed from the inline
  `hello` node, with an explanation note. Real `smoke` works because it pairs
  `goal_gate` with `retry_target=exit` in graph attrs; we don't need the gate at all
  for this verification.
- **"No new code" wording**: clarified — site 6 introduces trait plumbing and an
  `Event::Failover` emit, but no new stable `RunNotice` code (`agent.failover` was
  already canonical).


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


Read the plan file referenced in the goal and implement every step. Make all the code changes described in the plan. Use red/green TDD.