# Run-Owned Sandbox Lifecycle Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make Docker and Daytona sandboxes owned by a Fabro run: create/start for execution, stop on terminal by default, reactivate on access, and delete only when the run is deleted.

**Architecture:** Split sandbox lifecycle into explicit provider operations instead of using terminal cleanup as deletion. Persisted `SandboxRecord` remains the durable handle while the run exists for post-terminal access, resume, and deletion. `preserve = true` becomes an external handoff mode that skips provider deletion when the run is deleted and emits/returns the provider identifier before Fabro metadata is removed.

**Tech Stack:** Rust workspace, `fabro-sandbox`, `fabro-workflow`, `fabro-server`, `fabro-config`, OpenAPI-generated API types and TypeScript client.

---

## Summary

Implement this default lifecycle:

```text
run submitted
  no sandbox yet

run starts
  create sandbox
  start sandbox
  execute workflow

run resumes
  reconnect to persisted sandbox
  start sandbox
  continue from checkpoint

run terminal: succeeded / failed / cancelled
  stop sandbox by default
  keep sandbox record and provider resource

run accessed after terminal
  start or restore sandbox as needed

run deleted
  delete sandbox unless preserve = true
  delete run metadata, artifacts, and scratch state
```

Missing sandboxes are errors. Fabro does not recreate a missing sandbox from a checkpoint, including on resume.

Once run deletion has begun, missing provider resources are treated as already deleted so deletion retries are idempotent.

## Public Configuration

Add `stop_on_terminal` beside the existing `preserve` setting:

```toml
[run.sandbox]
preserve = false
stop_on_terminal = true
```

Semantics:

| `preserve` | `stop_on_terminal` | Terminal behavior | Run delete behavior |
| --- | --- | --- | --- |
| `false` | `true` | stop sandbox | delete sandbox |
| `false` | `false` | leave sandbox running | delete sandbox |
| `true` | `true` | stop sandbox | skip sandbox delete |
| `true` | `false` | leave sandbox running | skip sandbox delete |

Avoid naming this `auto_stop` because Daytona already uses `auto_stop_interval` for provider-side inactivity behavior.

## Key Implementation Changes

- Add `run.sandbox.stop_on_terminal: bool`, default `true`, to config layers, resolved settings, serialized run settings, OpenAPI schema, and generated TypeScript client surfaces where run settings are exposed.
- Extend the sandbox trait with explicit lifecycle methods:
  - `start()`
  - `stop()`
  - `delete()`
- Keep init-failure cleanup as provider deletion so partially-created sandboxes do not leak before a durable run/sandbox record exists.
- Audit every `sandbox.cleanup()` call and replace it with the correct lifecycle operation:
  - init failure before durable sandbox record persistence: `delete()`
  - terminal finalization and post-initialize early execution exits: `stop()` when `stop_on_terminal = true`
  - run deletion with `preserve = false`: `delete()`
- Change workflow finalization from "cleanup/delete sandbox unless preserved" to:
  - emit terminal event
  - run the terminal lifecycle hook/notice path
  - if `stop_on_terminal = true`, call `sandbox.stop()`
  - never delete the sandbox from finalization
- Change resume to:
  - load the persisted `SandboxRecord`
  - reconnect to the provider resource
  - call `sandbox.start()`
  - health-check the existing sandbox
  - rebuild runtime services around the attached sandbox
  - continue from checkpoint using the existing sandbox
  - do not call provider `initialize()`, create a new provider resource, clone the repo, or rerun provider workspace creation
  - return a conflict/precondition error if the provider resource is missing
- Change run deletion to:
  - mark the run as `Removing` or otherwise persist a delete-started marker before provider deletion
  - load the persisted `SandboxRecord`
  - reconnect to the provider resource
  - call `sandbox.delete()` unless `preserve = true`
  - delete run metadata, artifacts, and scratch state
  - before delete has started, return `409 Conflict` on missing provider resource unless `force=true`
  - after delete has started, treat a missing provider resource as already deleted and continue removing Fabro metadata/artifacts/scratch
  - with `force=true`, surface/log the missing-sandbox condition, then remove Fabro store state, artifacts, and scratch idempotently
- Change sandbox-backed access paths to activate as needed:
  - reconnect from `SandboxRecord`
  - call `start()` before sandbox file routes, run-files diff materialization, preview, SSH, and other sandbox operations
  - return an error if the provider resource is missing
- Add explicit sandbox stop/delete lifecycle events, or rename the existing cleanup event vocabulary so logs and hooks distinguish stop from delete. Do not reuse "cleanup" wording for stop without documenting it as an umbrella compatibility term.
- Define lifecycle trait behavior across all implementors:
  - `LocalSandbox.start/stop/delete` are no-ops
  - `WorktreeSandbox` and other wrappers/decorators forward lifecycle calls to their inner sandbox
  - delegation macros forward `start`, `stop`, and `delete`
  - test doubles record lifecycle calls so tests can assert stop-vs-delete behavior

## Resume Attach Mode

Resume must use an explicit attach-existing path. This path reuses the persisted sandbox and skips all provider creation behavior.

Attach-existing should:

- reconnect from the existing `SandboxRecord`
- call `start()`
- run provider health checks required for exec/file operations
- rebuild the in-memory workflow runtime services, event sink, artifact sink, LLM client source, handlers, hooks, and run control plumbing
- reuse existing Git/run branch state from the checkpoint projection

Attach-existing must not:

- call provider `initialize()`
- create a new Docker container or Daytona sandbox
- clone the repository
- create an empty provider workspace
- overwrite persisted sandbox identity

Setup-command behavior on resume should remain checkpoint-aware: do not rerun provider creation or clone. Only rerun explicit resume setup commands already defined by the sandbox/provider when needed to reattach to the existing run branch.

## Delete And Preserve API Semantics

Normal delete keeps the current `204 No Content` response when no handoff information is needed.

For `preserve = true`, run deletion is an operator handoff. Add a response body and OpenAPI shape for this case:

```json
{
  "deleted": true,
  "sandbox_preserved": true,
  "sandbox": {
    "provider": "docker",
    "identifier": "container-id-or-daytona-name"
  }
}
```

Clients that only care about success can continue treating any 2xx as success. The handler may return `200 OK` for preserve handoff responses and `204 No Content` for ordinary deletes.

Delete retries:

- before delete starts, missing sandbox is a conflict unless `force=true`
- after delete starts, missing sandbox is treated as already deleted
- metadata/artifact/scratch deletion must be idempotent so a retry after partial deletion can finish cleanly

## Provider Behavior

Docker:

- `initialize`: create and start the container as today.
- `stop`: run `docker stop`; tolerate already-stopped containers.
- `start`: run `docker start`; tolerate already-running containers; perform a lightweight health check before exec/file operations.
- `delete`: verify managed labels and the run ownership label before removing anything, stop if needed, then remove the container.
- Reconnect/delete/start/stop flows must receive or recover the Fabro `RunId` so `sh.fabro.run_id` can be checked against the run being operated on.

Daytona:

- `initialize`: create a non-ephemeral sandbox, apply Fabro/run labels, and explicitly disable provider auto-delete.
- `stop`: call Daytona stop.
- `start`: call Daytona start; if the sandbox is archived, rely on Daytona start/restore semantics.
- `delete`: call Daytona delete.
- Validate Daytona ownership using persisted sandbox metadata and Fabro/run labels before destructive operations when the provider exposes labels through the SDK/API.
- Keep the existing CLI-agent behavior that disables Daytona auto-stop during active long-running CLI work, but restore the configured Daytona auto-stop interval after CLI work finishes. If restoration fails, emit a warning notice.
- Defer Daytona auto-archive support to a separate change.

## Important Edge Cases

- `preserve = true` skips sandbox deletion on run delete and treats the provider resource as externally handed off. Before deleting Fabro metadata, return the provider identifier in the delete response so the operator is not left without a handle.
- Missing provider resources are errors for access, resume, and normal delete before delete starts. After delete starts, missing provider resources are treated as already deleted so retrying a partially-failed delete does not get stuck.
- Terminal failure, cancellation, and success all use the same stop-on-terminal policy.
- Post-initialize execution errors and scopeguard exits stop the sandbox when `stop_on_terminal = true`; they do not delete it.
- A sandbox initialization failure before durable sandbox record persistence must still delete any partially-created provider resource.
- Existing `SandboxRecord` shape should remain unchanged unless implementation needs additional ownership metadata to validate provider resources safely.

## Test Plan

Config and schema tests:

- Default config resolves `stop_on_terminal = true`.
- Explicit `stop_on_terminal = false` resolves and serializes.
- Existing `preserve` behavior remains compatible except deletion now happens on run delete rather than terminal finalization.

Workflow/finalize tests:

- Terminal success with default settings calls `stop`, not `delete`.
- Terminal failure calls `stop`, not `delete`.
- Cancellation calls `stop`, not `delete`.
- `stop_on_terminal = false` calls neither `stop` nor `delete` at terminal.
- Post-initialize execution error stops but does not delete.
- Sandbox initialization failure still deletes a partially-created sandbox.
- Resume starts an existing stopped sandbox and does not create a new one.
- Resume with a missing provider resource fails without recreation.

Server/delete tests:

- Deleting a run deletes the sandbox when `preserve = false`.
- Deleting a run skips sandbox deletion when `preserve = true`.
- Normal delete returns `409 Conflict` when the sandbox is missing.
- Delete retry after successful provider deletion but failed metadata/artifact/scratch deletion treats the missing sandbox as already deleted and completes metadata cleanup.
- Forced delete with a missing sandbox removes Fabro metadata only after explicit missing-sandbox handling.
- Run delete validates ownership before deleting provider resources.
- `preserve = true` delete returns a handoff response with provider and identifier before removing Fabro metadata.

Access tests:

- Sandbox file routes, run-files diff materialization, preview, and SSH routes reconnect and start a stopped sandbox before use.
- Missing sandbox on access returns a conflict/error and does not recreate.

Provider tests:

- Docker stop/start/delete tolerate already-stopped or already-running states where appropriate and still verify managed labels before destructive operations.
- Daytona lifecycle methods call the correct SDK operations and create non-ephemeral, non-auto-delete sandboxes with Fabro/run ownership labels.
- Daytona CLI-agent auto-stop disablement restores the configured interval after CLI work.
- Local lifecycle methods are no-ops.
- Worktree/decorator lifecycle methods forward to the inner sandbox.
- Test doubles record `start`, `stop`, and `delete` calls.
- Live Daytona coverage stays ignored or e2e-gated.

## Assumptions

- "Active on access" means Fabro starts/restores the sandbox for all sandbox-backed access paths, even if a provider could read files while stopped.
- "Missing sandbox = error" means no recreate-from-checkpoint behavior in this feature.
- Daytona auto-delete is disabled for run-owned sandboxes because deletion belongs to Fabro run deletion.
- Daytona auto-archive is intentionally deferred.
