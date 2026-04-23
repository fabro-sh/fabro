---
title: "refactor: CommandContext alignment at CLI command boundaries"
type: refactor
status: completed
date: 2026-04-23
deepened: 2026-04-23
---

# CommandContext Alignment At CLI Command Boundaries

## Overview

Finish the partially landed `CommandContext` refactor in `fabro-cli` by
making `CommandContext` the shared command-boundary abstraction for
invocation plumbing in the CLI command families that already depend on
it or immediately reconstruct it. The goal is to stop threading
`&CliNamespace`, `&CliLayer`, `Printer`, and `process_local_json`
through command entrypoints when that data is already part of the same
invocation context.

This is a follow-on plan to the earlier server-access refactor. The
current codebase already centralizes `cwd`, merged settings, and server
access in `CommandContext`, but command entrypoints still receive raw
plumbing and then rebuild a context locally. This plan aligns the
surface area around the existing struct instead of introducing a second
context or service wrapper.

## Problem Frame

`CommandContext` exists today in
`lib/crates/fabro-cli/src/command_context.rs`, and many commands
already depend on it for `cwd`, `machine_settings`, `user_settings`, and
`server()` access. The refactor is only half-finished:

- the `printer` field and `printer()` accessor are both marked
  `#[allow(dead_code, reason = "...still being wired through")]`
- `main.rs` still passes raw plumbing into most command families
- many command entrypoints still accept some combination of
  `&CliNamespace`, `&CliLayer`, `Printer`, and `process_local_json`
- those same commands often construct `CommandContext` immediately
  inside the handler

That leaves the CLI with two overlapping models:

- `CommandContext` as the intended abstraction for shared invocation
  state
- raw parameter threading as the de facto command API

The result is more churn on command signatures, more repeated output
format and JSON-gating branches, and a dead-code-marked `printer` field
that signals the abstraction boundary is unfinished.

The refactor needs to finish in a way that preserves current CLI
behavior and avoids turning `CommandContext` into a new god object.

## Requirements Trace

- **R1.** In-scope CLI command entrypoints use `CommandContext` as the
  shared invocation-plumbing abstraction instead of separately receiving
  `&CliNamespace`, `&CliLayer`, `Printer`, and `process_local_json`.
- **R2.** `CommandContext` remains narrow: it carries invocation-scoped
  plumbing and shared derived state, not command-specific args, styles,
  or workflow-specific data.
- **R3.** JSON output, verbosity behavior, printer routing, and global
  `--json` restrictions remain behaviorally unchanged for existing
  commands, including `auth status` remaining explicit-global-`--json`
  only rather than switching to persisted `cli.output.format = "json"`.
- **R4.** Target-based and storage-dir-based server resolution semantics
  remain unchanged, including `CommandContext::server()` behavior and
  `ServerSummaryLookup::from_client(...)` usage.
- **R5.** `main.rs` keeps its current pre-tracing bootstrap ordering for
  logging and upgrade checks; `CommandContext` begins after that phase.
- **R6.** The current dead-code allowance on the `printer` field/accessor
  is removed by making printer access a real part of the abstraction, or
  by deleting redundant surface area if a narrower accessor is better.
- **R7.** Representative unit and integration coverage exists for the
  context API, JSON/text output behavior, global `--json` gating, and
  target/connection-based command families touched by the refactor.

## Scope Boundaries

- **In scope:** command families in `fabro-cli` that already use
  `CommandContext` directly or immediately construct one from raw
  plumbing:
  `run`, `runs`, `artifact`, `store dump`, `preflight`, `validate`,
  `graph`, `model`, `secret`, `pr`, `repo`, `auth`, `provider`,
  `config`, `version`, `doctor`, `system`, and the public
  `sandbox preview` / `sandbox ssh` command boundary that currently
  forwards into `commands/run/*` leaf modules.
- **In scope:** top-level dispatch cleanup in
  `lib/crates/fabro-cli/src/main.rs` and family dispatch modules where
  the only reason raw CLI plumbing is still passed down is command
  signature inertia.
- **Out of scope:** `exec`, `server`, `install`, `upgrade`, `workflow`,
  `parse`, `render_graph`, hidden analytics/panic upload commands,
  hidden `run worker`, and `sandbox cp`, except for thin compile-only
  adapters if needed.
- **Out of scope:** changing CLI text, response payloads, exit-code
  semantics, or server connection behavior beyond what is required to
  move the plumbing boundary.
- **Out of scope:** moving `Styles` ownership into `CommandContext` or
  making low-level pure rendering helpers context-aware when explicit
  `Printer` or `Styles` parameters remain clearer.
- **Out of scope:** redesigning `server_client.rs` connection semantics.
  This plan consumes the current `CommandContext::server()` model rather
  than reopening the earlier server-access design.

## Context & Research

### Relevant Code and Patterns

- `lib/crates/fabro-cli/src/command_context.rs` — current
  `CommandContext`, constructors, `user_settings()` / `machine_settings()`
  accessors, and dead-code-marked printer storage.
- `lib/crates/fabro-cli/src/main.rs` — top-level command dispatch still
  passing raw plumbing into most families after bootstrap.
- `lib/crates/fabro-cli/src/server_client.rs` — current target and
  storage-dir connection behavior that must stay unchanged.
- `lib/crates/fabro-cli/src/commands/run/create.rs` — existing example
  of a helper that already takes `&CommandContext`.
- `lib/crates/fabro-cli/src/commands/run/mod.rs` — current mixed model:
  some subcommands build a `CommandContext`, some still read raw
  `cli.output.format`, and `process_local_json` is already vestigial at
  this boundary.
- `lib/crates/fabro-cli/src/commands/secret/mod.rs` — good family-level
  pattern where a single derived server/client can be shared across
  subcommands.
- `lib/crates/fabro-cli/src/commands/pr/mod.rs` — representative mixed
  local/server family using both `CommandContext::base(...)` and
  `CommandContext::for_target(...)`.
- `lib/crates/fabro-cli/src/commands/auth/mod.rs` and
  `lib/crates/fabro-cli/src/commands/provider/mod.rs` — the clearest
  examples of raw `process_local_json` still being threaded despite the
  rest of the state already belonging to the invocation.
- `files-internal/testing-strategy.md` — CLI integration tests should
  stay command-driven and black-box, with implementation-facing behavior
  covered by unit tests near the code.
- `lib/crates/fabro-cli/src/commands/sandbox/mod.rs` — the real public
  boundary for preview/ssh command dispatch; leaf implementation lives in
  `commands/run/preview.rs` and `commands/run/ssh.rs`, but the command
  family boundary is `sandbox`.
- `lib/crates/fabro-cli/tests/it/cmd/sandbox_preview.rs` and
  `lib/crates/fabro-cli/tests/it/cmd/sandbox_ssh.rs` — existing
  command-owned coverage for those public entrypoints.

### Current-State Inventory

- `CommandContext::{base, for_target, for_connection}` currently has
  **42** call sites across **37** command files under
  `lib/crates/fabro-cli/src/commands`.
- The command tree contains **one** existing helper that already accepts
  `&CommandContext` directly:
  `lib/crates/fabro-cli/src/commands/run/create.rs`.
- The `printer()` accessor currently has no call sites, which is why the
  dead-code allowance still exists.
- For in-scope command files, the dominant remaining use of raw
  `&CliNamespace` is reading `cli.output.format` or
  `cli.output.verbosity`; that is a signal that the data belongs on the
  shared invocation context rather than each command signature.
- `process_local_json` is now concentrated in `auth`, `provider`,
  `graph`, `sandbox preview`, and `sandbox ssh`. That makes it a good
  candidate for a dedicated invocation-context field/helper instead of
  continued parameter threading.

### Related Context

- `docs/plans/2026-04-08-cli-services-command-context-refactor-plan.md`
  — earlier plan that introduced the current `CommandContext` and server
  access model. This plan finishes the command-boundary alignment that
  document did not fully land.
- `docs/plans/2026-04-22-001-refactor-settings-api-entrypoints-plan.md`
  — recent owner-first context plan that reinforces the repo preference
  for dense, owner-scoped context objects instead of repeated free-form
  plumbing.
- `git log -- lib/crates/fabro-cli/src/command_context.rs` shows recent
  follow-on commits including `simplify: drop duplicate settings plumbing
  from cli/server refactor`, which is consistent with the current goal of
  collapsing overlapping command-boundary APIs.

### Institutional Learnings

- No relevant `docs/solutions/` entries currently cover this seam.

### External References

- No external research used. The repo already has sufficient local
  context, existing partial implementation, and tests for this refactor.

## Key Technical Decisions

- **Use `CommandContext` as the command-boundary API for in-scope
  commands.**
  The abstraction already owns the hard parts: working directory,
  merged settings, and server access. The remaining refactor should move
  entrypoint APIs onto that abstraction instead of continuing to thread
  raw plumbing beside it.

- **Keep `CommandContext` narrow and invocation-scoped, not god-shaped.**
  It should own:
  `printer`, merged CLI-derived settings (`machine_settings`,
  `user_settings`), `cwd`, config-path context, server-derivation state,
  and the invocation-only global `--json` flag.
  It should not own command args, `Styles`, render-only helpers, or
  workflow/build-specific state.

- **Do not store or expose the full `CliNamespace` publicly.**
  Commands in scope only need a small subset of CLI plumbing:
  output format, output verbosity, and global `--json` restrictions.
  Output format and verbosity should come from
  `ctx.user_settings().cli.output`, which already reflects CLI override
  precedence through merged settings. The only truly extra invocation
  field is the global `--json` switch, which is not part of persisted
  settings and therefore belongs on the context explicitly.

- **Preserve the current error-timing split.**
  `CommandContext` construction should keep doing what it does today:
  capture cwd, load local settings, and build merged invocation state.
  Server-target resolution and server-connection failures should remain
  deferred to `ctx.server().await?` or existing explicit
  `resolve_server_target(...)` calls. The refactor should not make
  malformed target/server resolution errors eager at context-construction
  time.

- **Keep `auth status` as an explicit-global-JSON command.**
  Most in-scope commands should read output mode from
  `ctx.user_settings().cli.output`, but `auth status` is a special case:
  it currently emits JSON only for the explicit invocation-wide global
  `--json` path, not merely because resolved CLI settings say
  `output.format = json`. That distinction should remain intact, so the
  context needs both resolved output settings and a separate helper for
  the explicit global JSON flag.

- **Create a base invocation context once, then derive target/connection
  variants from it.**
  `main.rs` should keep using raw settings during pre-tracing bootstrap.
  After that, it should build a base `CommandContext` once for each
  in-scope dispatch path. Command families then derive
  target-based or connection-based contexts from that base without
  re-supplying `Printer` and `CliLayer`.

- **Allow private derivation state inside `CommandContext` if that is the
  simplest way to avoid raw parameter threading.**
  Storing a private `CliLayer` or equivalent internal builder state is
  acceptable if it enables methods like `with_target(...)` or
  `with_connection(...)` and keeps the raw plumbing hidden behind the
  abstraction boundary.

- **Keep render helpers explicit.**
  Command entrypoints and family dispatchers should align on
  `CommandContext`, but low-level helpers such as table rendering,
  summary formatting, and browser-opening routines may keep explicit
  `Printer` / `Styles` / boolean parameters where that stays simpler than
  threading the full context downward.

- **Keep workflow/manifest layer assembly command-local.**
  Commands such as `run`, `preflight`, `validate`, and `graph` should
  continue to build their workflow/project/manifests with the existing
  command-owned helpers. `CommandContext` can provide `cwd`, merged user
  settings for output behavior, and server access, but it should not
  absorb manifest-building policy or workflow-layer composition.

- **Migrate family-by-family with temporary compatibility wrappers if
  needed.**
  This is a cross-cutting refactor with wide signature churn. It is
  better to allow short-lived constructor/adapter overlap during the
  migration than to force a single giant all-or-nothing patch that is
  harder to validate.

## Open Questions

### Resolved During Planning

- **Should this refactor introduce a second wrapper type such as
  `Services` or `InvocationContext`?**
  No. The repo already has a partially landed `CommandContext`, and the
  simplest aligned design is to finish that abstraction rather than
  splitting responsibilities across two overlapping context types.

- **Should `CommandContext` absorb the entire `CliNamespace`?**
  No. Public command consumers should read output mode and verbosity from
  `ctx.user_settings().cli.output`, while command-specific configuration
  stays local to the commands that own it.

- **Should `process_local_json` become part of `CommandContext`?**
  Yes. It is invocation-scoped, currently leaks across multiple command
  signatures, and is the one remaining piece of global command plumbing
  that is not already represented by merged settings.

- **Should preview/ssh be treated as `run` work or `sandbox` work in
  this plan?**
  Treat them as `sandbox` work at the public command boundary. The leaf
  implementation modules remain under `commands/run/*`, but the raw
  plumbing boundary in the current CLI is `commands/sandbox/mod.rs` and
  the plan should align to that boundary and its tests.

- **Should `auth status` remain “explicit global JSON only”?**
  Yes. R3 for this plan is behavioral preservation, and the current
  contract is that `auth status` emits JSON only when the explicit
  invocation-global `--json` switch is active.

- **Should `Styles` move into `CommandContext` in the same pass?**
  No. That would enlarge the abstraction without addressing the actual
  duplicated plumbing problem.

- **Should out-of-scope local commands be forced onto `CommandContext`
  just for uniformity?**
  No. This pass should target the families where `CommandContext` already
  provides real value or is already partially adopted.

### Deferred to Implementation

- **Exact API names for derived contexts.**
  The implementation may settle on `with_target(...)`,
  `for_target_from(...)`, or similar naming. The important contract is
  that callers no longer pass raw `Printer` and `CliLayer` repeatedly.

- **Whether static constructors remain temporarily during migration.**
  If temporary wrappers reduce compile churn while family-by-family
  patches land, they are acceptable. Final cleanup should remove the
  now-redundant raw-plumbing entrypoints from in-scope call sites.

- **Whether `CommandContext` should be cheaply cloneable or should build
  derived variants from private state on demand.**
  Either is acceptable if it preserves the abstraction boundary and does
  not change runtime behavior.

## High-Level Technical Design

> *This illustrates the intended approach and is directional guidance for review, not implementation specification. The implementing agent should treat it as context, not code to reproduce.*

| Boundary | Current shape | Target shape |
|---|---|---|
| `main.rs` -> command family | `dispatch(args, &cli_settings, &cli_layer, process_local_json, printer)` | `dispatch(args, &base_ctx)` |
| family dispatch -> leaf command | raw CLI plumbing plus `CommandContext::for_target(...)` inside the leaf | derive `target_ctx` / `connection_ctx` once from `base_ctx`, then pass `&CommandContext` or already-resolved client/output helpers |
| output mode lookup | `cli.output.format` / `cli.output.verbosity` | `ctx.user_settings().cli.output.*` |
| global `--json` guard | separate `process_local_json` parameter | `ctx` accessor/helper |
| printing | raw `printer` parameter | `ctx.printer()` or a narrower printer extracted from `ctx` |

Directional flow:

```text
bootstrap raw globals/settings for tracing + upgrade check
  -> build base CommandContext once for the in-scope command dispatch
  -> family dispatch receives &base_ctx
  -> family derives:
       target_ctx from target args
       connection_ctx from storage-dir-aware args
       base/local ctx for settings-only commands
  -> leaf command reads:
       ctx.cwd()
       ctx.machine_settings()
       ctx.user_settings().cli.output.*
       ctx.require_no_json_override() or equivalent
       ctx.printer()
       ctx.server().await?
  -> render-only helpers stay explicit over Printer / Styles where simpler
```

## Implementation Units

- [x] **Unit 1: Reframe `CommandContext` as the invocation-boundary object**

**Goal:** Make `CommandContext` capable of carrying the invocation
plumbing that is still leaking through command signatures, while keeping
the type narrowly scoped.

**Requirements:** R1, R2, R3, R4, R6

**Dependencies:** None

**Files:**
- Modify: `lib/crates/fabro-cli/src/command_context.rs`
- Modify: `lib/crates/fabro-cli/src/main.rs`
- Test: `lib/crates/fabro-cli/src/command_context.rs`

**Approach:**
- Add the remaining invocation-only plumbing that does not already exist
  on the context, specifically the global `--json` switch used today via
  `process_local_json`.
- Make printer access part of the live API so the dead-code allowance on
  the `printer` field/accessor can be removed.
- Add context-derivation methods that let callers obtain target-based or
  connection-based variants from a base invocation context without
  re-supplying raw `Printer` and `CliLayer`.
- Keep output format and verbosity sourced from
  `ctx.user_settings().cli.output` rather than storing a second public
  output-format copy on the side, while still exposing the explicit
  invocation-global JSON flag separately for commands like `auth status`
  whose contract is not identical to resolved output format.

**Patterns to follow:**
- `lib/crates/fabro-cli/src/command_context.rs`
- `docs/plans/2026-04-22-001-refactor-settings-api-entrypoints-plan.md`
  for owner-first context boundaries

**Test scenarios:**
- Happy path: a base context exposes the same output format and verbosity
  that commands currently read from merged CLI settings.
- Happy path: a base context preserves both resolved output settings and
  the explicit invocation-global JSON flag so commands can distinguish
  between “resolved output format is JSON” and “user passed global
  `--json`”.
- Happy path: deriving a target-based context preserves printer/global
  JSON state and resolves server access through the existing
  `server_client::connect_server_with_settings(...)` path.
- Edge case: deriving a connection-based context with a storage-dir
  override changes only the storage-backed settings path and preserves
  other invocation-scoped data.
- Error path: settings-load failures tied to context construction still
  surface during context construction, while malformed target resolution
  remains deferred until `ctx.server().await?` or explicit
  `resolve_server_target(...)` calls.

**Verification:**
- The `printer` field/accessor is no longer dead code.
- There is one clear way to obtain a base context and derive
  target/connection variants without raw plumbing at the call site.
- The context API makes the distinction between resolved output format
  and explicit global JSON invocation state unambiguous.

- [x] **Unit 2: Move `main.rs` and the run/preflight/graph/sandbox boundary to context-first dispatch**

**Goal:** Eliminate raw plumbing from the top-level dispatch path and the
run-oriented command family plus the public `sandbox preview` /
`sandbox ssh` boundary that already rely heavily on `CommandContext`.

**Requirements:** R1, R3, R4, R5, R6, R7

**Dependencies:** Unit 1

**Files:**
- Modify: `lib/crates/fabro-cli/src/main.rs`
- Modify: `lib/crates/fabro-cli/src/commands/run/mod.rs`
- Modify: `lib/crates/fabro-cli/src/commands/run/command.rs`
- Modify: `lib/crates/fabro-cli/src/commands/run/create.rs`
- Modify: `lib/crates/fabro-cli/src/commands/run/preview.rs`
- Modify: `lib/crates/fabro-cli/src/commands/run/ssh.rs`
- Modify: `lib/crates/fabro-cli/src/commands/sandbox/mod.rs`
- Modify: `lib/crates/fabro-cli/src/commands/run/resume.rs`
- Modify: `lib/crates/fabro-cli/src/commands/run/rewind.rs`
- Modify: `lib/crates/fabro-cli/src/commands/run/fork.rs`
- Modify: `lib/crates/fabro-cli/src/commands/run/wait.rs`
- Modify: `lib/crates/fabro-cli/src/commands/run/diff.rs`
- Modify: `lib/crates/fabro-cli/src/commands/run/logs.rs`
- Modify: `lib/crates/fabro-cli/src/commands/run/cp.rs`
- Modify: `lib/crates/fabro-cli/src/commands/preflight.rs`
- Modify: `lib/crates/fabro-cli/src/commands/validate.rs`
- Modify: `lib/crates/fabro-cli/src/commands/graph.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/run.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/create.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/start.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/attach.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/diff.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/logs.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/preflight.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/validate.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/graph.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/sandbox_preview.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/sandbox_ssh.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/json_global.rs`
- Test: `lib/crates/fabro-cli/tests/it/scenario/lifecycle.rs`
- Test: `lib/crates/fabro-cli/tests/it/scenario/recovery.rs`

**Approach:**
- Build a base `CommandContext` in `main.rs` only after the existing
  bootstrap phase completes.
- Change the run-family and run-adjacent entrypoints to accept context
  instead of raw `cli` / `cli_layer` / `printer` bundles.
- Replace direct reads of `cli.output.format` and
  `cli.output.verbosity` with `ctx.user_settings().cli.output.*`.
- Replace direct `process_local_json` threading with a context helper
  for the small number of commands that still need it (`graph`,
  `commands/sandbox/mod.rs` for the public preview/ssh boundary).
- Move the public `sandbox preview` / `sandbox ssh` dispatch boundary to
  the same context-first shape as the run-family boundary, while leaving
  the leaf implementation modules in `commands/run/*` if that remains
  the cleanest internal organization.
- Keep `Styles` local to the leaf commands and keep `attach` / `start`
  client helpers narrow if broadening them adds no value.

**Execution note:** Start by preserving or expanding characterization
coverage for JSON/text output and global `--json` gating before removing
the old raw-plumbing signatures from these entrypoints.

**Patterns to follow:**
- `lib/crates/fabro-cli/src/commands/run/create.rs`
- `lib/crates/fabro-cli/src/commands/preflight.rs`
- `lib/crates/fabro-cli/src/commands/graph.rs`
- `lib/crates/fabro-cli/src/commands/sandbox/mod.rs`

**Test scenarios:**
- Happy path: `fabro run --detach` still prints a bare run ID in text
  mode and the same JSON payload in JSON mode after output format moves
  behind `CommandContext`.
- Happy path: `fabro run` / `resume` / `attach` still inherit verbose
  rendering from CLI output verbosity and preserve idle-sleep behavior.
- Edge case: `fabro graph --json` without an explicit output file still
  rejects the global JSON override exactly as it does today.
- Edge case: `fabro sandbox preview --open` still opens the browser only
  when global JSON mode is not active.
- Edge case: `fabro sandbox ssh` still allows `--print` under global
  JSON mode but continues to reject the unsupported interactive
  combination.
- Error path: `preflight` and `validate` continue to fail on validation
  errors with the same text-vs-JSON contract and exit behavior.
- Integration: create -> start -> attach and resume/recovery flows still
  resolve targets, stream output, and summarize results through the same
  black-box CLI contracts.

**Verification:**
- `main.rs` no longer passes the raw plumbing bundle into the run /
  preflight / validate / graph / sandbox preview-ssh boundary.
- Those command boundaries obtain shared invocation data exclusively via
  `CommandContext`.

- [x] **Unit 3: Migrate runs/artifact/store families to context-first family dispatch**

**Goal:** Remove repeated target-context reconstruction from the command
families that already build a target-based `CommandContext` immediately
and mostly use raw CLI state only for output mode.

**Requirements:** R1, R3, R4, R6, R7

**Dependencies:** Units 1-2

**Files:**
- Modify: `lib/crates/fabro-cli/src/commands/runs/mod.rs`
- Modify: `lib/crates/fabro-cli/src/commands/runs/list.rs`
- Modify: `lib/crates/fabro-cli/src/commands/runs/archive.rs`
- Modify: `lib/crates/fabro-cli/src/commands/runs/rm.rs`
- Modify: `lib/crates/fabro-cli/src/commands/runs/inspect.rs`
- Modify: `lib/crates/fabro-cli/src/commands/artifact/mod.rs`
- Modify: `lib/crates/fabro-cli/src/commands/artifact/list.rs`
- Modify: `lib/crates/fabro-cli/src/commands/artifact/cp.rs`
- Modify: `lib/crates/fabro-cli/src/commands/store/mod.rs`
- Modify: `lib/crates/fabro-cli/src/commands/store/dump.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/archive.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/inspect.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/rm.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/artifact_list.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/artifact_cp.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/store_dump.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/store.rs`

**Approach:**
- Have each family receive a context-first boundary and derive its
  target-based command context once, close to the namespace/selector
  boundary.
- Switch output-mode branches to
  `ctx.user_settings().cli.output.format`.
- Preserve existing client-sharing patterns such as
  `ServerSummaryLookup::from_client(ctx.server().await?)`.
- Keep leaf helpers narrow where they already only need a resolved
  client, resolved run ID, or printer.

**Patterns to follow:**
- `lib/crates/fabro-cli/src/commands/secret/mod.rs`
- `lib/crates/fabro-cli/src/commands/artifact/mod.rs`

**Test scenarios:**
- Happy path: `runs list`, `archive`, `unarchive`, and `rm` still render
  the same JSON and text shapes after output-mode decisions move behind
  the context.
- Happy path: `runs inspect` remains a JSON-only projection of server
  state and still resolves the selected run before fetching the state.
- Edge case: “no runs found” and “no artifacts found” text-mode messages
  remain unchanged.
- Error path: ambiguous or missing run selectors still fail through the
  existing server/client resolution path.
- Integration: artifact listing/copy and store dump continue to hit the
  same server-backed data path and respect output-format selection.

**Verification:**
- These family dispatchers no longer need separate `cli_layer` and
  `printer` arguments merely to reconstruct a target context.

- [x] **Unit 4: Migrate PR, secret, and system command families**

**Goal:** Align the families that mix local settings, target-based
server access, and storage-dir-aware server access onto the same
context-first boundary.

**Requirements:** R1, R3, R4, R6, R7

**Dependencies:** Units 1-3

**Files:**
- Modify: `lib/crates/fabro-cli/src/commands/pr/mod.rs`
- Modify: `lib/crates/fabro-cli/src/commands/pr/list.rs`
- Modify: `lib/crates/fabro-cli/src/commands/pr/create.rs`
- Modify: `lib/crates/fabro-cli/src/commands/pr/view.rs`
- Modify: `lib/crates/fabro-cli/src/commands/pr/merge.rs`
- Modify: `lib/crates/fabro-cli/src/commands/pr/close.rs`
- Modify: `lib/crates/fabro-cli/src/commands/secret/mod.rs`
- Modify: `lib/crates/fabro-cli/src/commands/secret/list.rs`
- Modify: `lib/crates/fabro-cli/src/commands/secret/set.rs`
- Modify: `lib/crates/fabro-cli/src/commands/secret/rm.rs`
- Modify: `lib/crates/fabro-cli/src/commands/system/mod.rs`
- Modify: `lib/crates/fabro-cli/src/commands/system/info.rs`
- Modify: `lib/crates/fabro-cli/src/commands/system/df.rs`
- Modify: `lib/crates/fabro-cli/src/commands/system/events.rs`
- Modify: `lib/crates/fabro-cli/src/commands/system/prune.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/pr.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/pr_list.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/pr_create.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/pr_view.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/pr_merge.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/pr_close.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/secret.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/secret_list.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/secret_set.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/secret_rm.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/system.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/system_info.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/system_df.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/system_events.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/system_prune.rs`

**Approach:**
- Keep `pr`’s split between base-settings work and target-based run
  lookup, but move both sides onto a common context-first boundary so
  raw `cli_layer` / `printer` threading disappears from the family API.
- Preserve `secret`’s existing pattern of resolving the server once at
  the family boundary and passing the client into subcommands.
- For `system`, derive connection-aware contexts from the base
  invocation context so storage-dir override behavior remains explicit
  and unchanged.

**Patterns to follow:**
- `lib/crates/fabro-cli/src/commands/pr/mod.rs`
- `lib/crates/fabro-cli/src/commands/secret/mod.rs`
- `lib/crates/fabro-cli/src/commands/system/mod.rs`

**Test scenarios:**
- Happy path: PR list/view/create/merge/close continue to render the same
  JSON/text contracts and still resolve GitHub credentials from merged
  local settings.
- Happy path: secret list/set/rm continue to resolve one server client at
  the namespace boundary and honor JSON mode.
- Happy path: system info/df/events/prune continue to use
  storage-dir-aware connection mode where appropriate.
- Edge case: missing secret / no matching runs-to-prune / empty PR list
  still produce the same user-visible text-mode outcomes.
- Error path: invalid storage-dir or connection resolution still fails
  before the command attempts remote work.
- Integration: the `system` family continues to respect explicit
  `--storage-dir` overrides and local daemon resolution semantics.

**Verification:**
- These family boundaries no longer accept raw plumbing bundles when the
  only reason was to build a `CommandContext` or inspect output mode.

- [x] **Unit 5: Finish remaining base/target context users**

**Goal:** Complete the context-first migration for the remaining in-scope
command surfaces that already use `CommandContext` but still expose raw
plumbing at their entrypoints.

**Requirements:** R1, R2, R3, R5, R6, R7

**Dependencies:** Units 1-4

**Files:**
- Modify: `lib/crates/fabro-cli/src/commands/model.rs`
- Modify: `lib/crates/fabro-cli/src/commands/repo/mod.rs`
- Modify: `lib/crates/fabro-cli/src/commands/repo/init.rs`
- Modify: `lib/crates/fabro-cli/src/commands/repo/deinit.rs`
- Modify: `lib/crates/fabro-cli/src/commands/auth/mod.rs`
- Modify: `lib/crates/fabro-cli/src/commands/auth/login.rs`
- Modify: `lib/crates/fabro-cli/src/commands/auth/logout.rs`
- Modify: `lib/crates/fabro-cli/src/commands/auth/status.rs`
- Modify: `lib/crates/fabro-cli/src/commands/provider/mod.rs`
- Modify: `lib/crates/fabro-cli/src/commands/provider/login.rs`
- Modify: `lib/crates/fabro-cli/src/commands/config/mod.rs`
- Modify: `lib/crates/fabro-cli/src/commands/version.rs`
- Modify: `lib/crates/fabro-cli/src/commands/doctor.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/model.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/model_list.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/model_test.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/repo.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/repo_init.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/repo_deinit.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/auth.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/provider.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/provider_login.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/config.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/version.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/doctor.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/json_global.rs`

**Approach:**
- Move base-context-only command families (`auth`, `provider`, parts of
  `repo`) onto `CommandContext` so JSON gating and printer usage come
  from the shared invocation object.
- Use `ctx.machine_settings()` / `ctx.user_settings()` consistently for
  local settings lookups rather than parallel raw-CLI arguments.
- Preserve the special-case JSON contract for `auth status`: explicit
  invocation-global `--json` remains the only JSON trigger, even if
  resolved CLI settings say `output.format = json`.
- Keep `repo deinit` and similar pure-local leaf helpers narrow if a
  derived `json_output` boolean or `Printer` extracted from the context
  keeps the internal helper clearer than passing the full context.

**Patterns to follow:**
- `lib/crates/fabro-cli/src/commands/auth/login.rs`
- `lib/crates/fabro-cli/src/commands/auth/status.rs`
- `lib/crates/fabro-cli/src/commands/config/mod.rs`
- `lib/crates/fabro-cli/src/commands/version.rs`

**Test scenarios:**
- Happy path: `auth status` and `provider login` preserve global `--json`
  restrictions and still resolve the intended server target from merged
  settings.
- Happy path: `config`, `version`, `model`, and `doctor` continue to
  honor text-vs-JSON output without a direct `CliNamespace` parameter.
- Edge case: persisted `cli.output.format = "json"` without explicit
  global `--json` still leaves `auth status` on its current text-mode
  path.
- Edge case: explicit global `--json` still forces `auth status` onto
  its JSON output path even though the implementation no longer receives
  a raw `process_local_json` parameter.
- Edge case: `repo init` non-JSON progress output and JSON result
  payloads stay unchanged.
- Edge case: `repo deinit` still stays local and does not accidentally
  require server access just because the family boundary now uses
  `CommandContext`.
- Error path: missing auth sessions, invalid server targets, and missing
  GitHub access continue to fail with the same user-facing contract.

**Verification:**
- Remaining in-scope command entrypoints no longer expose the raw
  plumbing bundle as part of their public internal API.

- [x] **Unit 6: Remove migration scaffolding and prove the boundary is clean**

**Goal:** Delete transitional surface area and confirm the in-scope
command tree is consistently aligned on `CommandContext`.

**Requirements:** R1, R2, R6, R7

**Dependencies:** Units 1-5

**Files:**
- Modify: `lib/crates/fabro-cli/src/command_context.rs`
- Modify: `lib/crates/fabro-cli/src/main.rs`
- Modify: in-scope command modules touched by transitional wrappers
- Test: `lib/crates/fabro-cli/tests/it/cmd/top_level.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/json_global.rs`

**Approach:**
- Remove temporary wrappers or compatibility constructors that were only
  present to get through the migration.
- Delete stale comments and “still being wired through” dead-code
  annotations once the boundary is real.
- Do a final sweep to ensure in-scope command boundaries are not still
  taking `&CliNamespace`, `&CliLayer`, `Printer`, and
  `process_local_json` together out of habit.

**Patterns to follow:**
- Keep cleanup limited to true scaffolding removal; do not reopen command
  semantics or formatting behavior in the cleanup pass.

**Test scenarios:**
- Test expectation: none -- cleanup-only unit. Behavioral coverage should
  already exist from Units 1-5.

**Verification:**
- The command-boundary API is visibly simpler and consistent across the
  in-scope families.
- No in-scope command still looks half-migrated.

## System-Wide Impact

- **Interaction graph:** `main.rs` bootstrap -> base `CommandContext` ->
  family dispatchers -> derived target/connection contexts ->
  `server_client.rs` / `user_config.rs`. This touches nearly every
  user-facing CLI family that already talks to settings or server
  resolution.
- **Error propagation:** context-construction failures remain early and
  synchronous at the command boundary; server access errors still flow
  through `ctx.server().await?` and should not move deeper into render
  helpers.
- **State lifecycle risks:** the biggest correctness risk is accidental
  reuse of the wrong derived context, especially storage-dir-aware
  contexts in the `system` family and target-based contexts in run/PR
  flows.
- **API surface parity:** although this is an internal refactor, it
  touches external CLI contracts indirectly through JSON/text output,
  verbosity, global `--json` restrictions, and server-target resolution.
- **Integration coverage:** black-box command tests and scenario tests
  are the main safety net. Unit tests should only cover context
  construction/derivation and not replace CLI integration coverage.
- **Unchanged invariants:** tracing and upgrade bootstrap ordering stays
  in `main.rs`; `exec` keeps its distinct direct-provider path; server
  connection semantics stay in `server_client.rs`; `Styles` remain
  command-local.

## Risks & Dependencies

| Risk | Mitigation |
|------|------------|
| Output-mode drift when replacing `cli.output.format` reads with `ctx.user_settings().cli.output.format` | Keep characterization coverage in existing `cmd/*` tests for both text and JSON paths before deleting the raw parameters |
| Global `--json` behavior changes while moving `process_local_json` into the context | Add targeted coverage in `json_global.rs`, `auth.rs`, `provider_login.rs`, `graph.rs`, and sandbox preview/ssh command tests |
| Storage-dir-aware system commands accidentally derive a target-mode context instead of a connection-mode context | Keep connection-specific derivation explicit and cover `system_info`, `system_df`, `system_events`, and `system_prune` with CLI integration tests |
| `CommandContext` grows into a second god object | Keep explicit scope rules: invocation plumbing only, no command args, no `Styles`, no feature-specific render state |
| The refactor becomes a giant compile-fix patch with poor reviewability | Land the work family-by-family with temporary compatibility wrappers where needed, and verify each family with its existing tests before cleanup |

## Documentation / Operational Notes

- No user-facing documentation changes are expected.
- Internal comments in `command_context.rs` and nearby command modules
  should be updated to describe the final boundary, not the transitional
  “still being wired through” state.
- The earlier April 8 plan should remain as historical context; this plan
  supersedes it for the command-boundary alignment work.

## Sources & References

- Prior plan: `docs/plans/2026-04-08-cli-services-command-context-refactor-plan.md`
- Related plan: `docs/plans/2026-04-22-001-refactor-settings-api-entrypoints-plan.md`
- Related code:
  `lib/crates/fabro-cli/src/command_context.rs`
  `lib/crates/fabro-cli/src/main.rs`
  `lib/crates/fabro-cli/src/server_client.rs`
  `lib/crates/fabro-cli/src/commands/run/mod.rs`
  `lib/crates/fabro-cli/src/commands/pr/mod.rs`
  `lib/crates/fabro-cli/src/commands/secret/mod.rs`
  `lib/crates/fabro-cli/src/commands/auth/mod.rs`
  `lib/crates/fabro-cli/src/commands/provider/mod.rs`
  `lib/crates/fabro-cli/src/commands/system/mod.rs`
- Testing guidance: `files-internal/testing-strategy.md`
- Related history:
  `93b6577cd simplify: drop duplicate settings plumbing from cli/server refactor`
  `367fd9302 refactor(cli): centralize command settings and server access`
  `4b30a5f16 refactor(cli): route command output through Printer`
