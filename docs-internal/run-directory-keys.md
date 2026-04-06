# Run Scratch Files

This document maps the files written under a run scratch directory to their event sources.

Scope:
- Scratch root: `~/.fabro/scratch/YYYYMMDD-{run_id}/`
- This covers local run files only
- Persistent store keys live in `lib/crates/fabro-store/src/keys.rs`
- Artifact object-store keys live in `lib/crates/fabro-store/src/artifact_store.rs`

There is no `_init.json` anymore. Run existence in the database is determined by stored run events, and local scratch directories are managed separately under `scratch/`.

## Root-Level Files

| File | Purpose | Event source |
|---|---|---|
| `run.json` | Run metadata snapshot: run id, resolved settings, graph, workflow slug, working directory, repo info, labels | Primarily `run.created`, plus request-time inputs persisted during create |
| `start.json` | Start timestamp and git context | `run.started` |
| `workflow.fabro` | Original dot source when available | `run.created.properties.workflow_source` |
| `workflow.toml` | Original workflow TOML when available | `run.created.properties.workflow_config` |
| `progress.jsonl` | Append-only event log | Every emitted run event |
| `live.json` | Latest live state snapshot | Derived continuously from emitted events |
| `checkpoint.json` | Crash-recovery snapshot | Derived from accumulated stage/checkpoint events plus engine state |
| `conclusion.json` | Final outcome summary | `run.completed` and `run.failed` |
| `retro.json` | Post-run retro output | `retro.completed` |
| `final.patch` | Final git diff for checkpointed runs | Local git state at finalize time, not a direct event payload |
| `cli.log` | Per-run tracing log | Local tracing output, not event-derived |
| `run.pid` | Legacy detached-run pid file from older runs | Legacy only; current flows do not rely on it |

## Local-Only Directories

These paths are local runtime state, not canonical event projections.

| Path | Purpose |
|---|---|
| `worktree/` | Git worktree used by checkpointed runs |
| `runtime/interview_request.json` | Detached-run interview request IPC file |
| `runtime/interview_response.json` | Detached-run interview response IPC file |
| `runtime/interview_request.claim` | Detached-run interview claim lock |
| `cache/artifacts/values/` | Large context values spilled to the filesystem |
| `cache/artifacts/files/` | Captured artifact files organized by node and retry |

## Node Directories

Per-node outputs are written under:
- `nodes/{node_id}/`
- `nodes/{node_id}-visit_{N}/` for retries, where `N` starts at `2`

### Agent and prompt nodes

| File | Purpose | Event source |
|---|---|---|
| `prompt.md` | Rendered prompt text | `stage.prompt.properties.text` is the closest event source |
| `response.md` | Final model response text | Reconstructable from message events, but written as a local convenience file |
| `status.json` | Final node status, notes, failure reason, timestamp | `stage.completed` |

### Command nodes

| File | Purpose | Event source |
|---|---|---|
| `script_invocation.json` | Command metadata: command, language, timeout | Partly from node config; not fully represented by a single event |
| `stdout.log` | Captured stdout | Local process output |
| `stderr.log` | Captured stderr | Local process output |
| `script_timing.json` | Duration, exit code, timeout result | Partly `stage.completed.properties.duration_ms`, otherwise local process state |
| `status.json` | Final node status | `stage.completed` |

### Nodes with git checkpointing

| File | Purpose |
|---|---|
| `diff.patch` | Per-node git diff captured at checkpoint time |

### Manager / child workflow nodes

Manager nodes may create a nested `child/` directory containing a full run scratch structure for the child workflow.

## Notes

- `progress.jsonl` is the event log; many other files are denormalized convenience snapshots derived from that stream plus local runtime state.
- `checkpoint.json` and `live.json` are projections, not single-event payloads.
- Artifact binaries are no longer stored in the SlateDB keyspace. They live in `ArtifactStore`; the run scratch tree only contains local cached copies when a workflow stage writes them to disk.
