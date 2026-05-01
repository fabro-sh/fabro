# Run Scratch Files

This document maps the files that still live under a run scratch directory. Durable run state lives in the run store and metadata branch; scratch is mostly local runtime state and caches.

Scope:
- Scratch root: `~/.fabro/scratch/YYYYMMDD-{run_id}/`
- This covers local run files only
- Persistent store keys live in `lib/crates/fabro-store/src/keys.rs`
- Artifact object-store keys live in `lib/crates/fabro-store/src/artifact_store.rs`

There is no `_init.json` anymore. Run existence in the database is determined by stored run events, and local scratch directories are managed separately under `scratch/`.

## Root-Level Files

| File | Purpose | Source |
|---|---|---|
| `workflow_bundle.json` | Bundled workflow input used by `start` to restore `workflow_path` and bundled child workflows/files | Written during create from the resolved workflow bundle |
| `run.pid` | Legacy detached-run pid file from older runs | Legacy only; current flows do not rely on it |

## Local-Only Directories

These paths are local runtime state, not canonical event projections.

| Path | Purpose |
|---|---|
| `worktree/` | Git worktree used by checkpointed runs |
| `runtime/server.log` | Worker tracing log for the run |
| `runtime/blobs/` | Materialized local blob payloads for file-backed `fabro+blob://` references |
| `nodes/{manager_node}_{visit}/child/` | Nested scratch root for manager-loop child workflows |

## Reconstructed / Exported Files

These names are still real, but they are no longer live scratch files by default:

- Metadata branch files such as `run.json`, `start.json`, `checkpoint.json`, and `retro.json`
- `fabro dump` exports such as `run.json`, `start.json`, `status.json`, `checkpoint.json`, `conclusion.json`, `retro.json`, `events.jsonl`, and per-node prompt/response/status/stdout/stderr files
- Retro-agent temp uploads named `events.jsonl`, `run.json`, `graph.fabro`, `checkpoints/{seq:04}.json`, `run.log` when available, and per-stage files under `stages/{node_id}@{visit}/...` inside the retro sandbox

## Notes

- Artifact binaries are no longer stored in the SlateDB keyspace. They live in `ArtifactStore`; the run scratch tree only contains local cached copies when a workflow stage writes them to disk.
- Final diffs for checkpointed runs are projected from the run store; they are no longer written as scratch files.
