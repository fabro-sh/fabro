---
date: 2026-04-08
topic: eliminate-scratch-artifact-cache
---

# Eliminate Scratch Artifact Cache

## Problem Frame

The artifact collection pipeline uses `scratch/cache/artifacts/files/` as a staging area for CLI uploads: files are downloaded from the sandbox to local disk, hashed, then uploaded to the server's `ArtifactStore`. Now that `ArtifactStore` (backed by object store) is the durable source of truth, this persistent local cache is unnecessary overhead. It adds disk usage and complicates the scratch directory contract. It also obscures a separate bug: server-managed runs currently start `ArtifactLifecycle` with no artifact sink configured, so collected artifacts are discarded. Eliminating the cache alone does not fix that bug; server-managed runs also need a direct `ArtifactStore` write path.

## Requirements

**Pipeline: Replace persistent cache with transient local staging**

- R1. `collect_artifacts` must download each file from the sandbox into a transient per-attempt local directory tree that preserves each artifact's relative path, compute MD5/SHA256 from those transient local files, and keep that transient tree available until the configured artifact sink has finished consuming it, including upload retries. Delete the transient tree immediately after direct store writes or HTTP uploads finish. No persistent `cache/artifacts/` directory.
- R2. `CapturedArtifactInfo` (path, mime, hashes, bytes) must still be produced for each collected file and emitted via `ArtifactCaptured` events.

**Store: Write artifacts directly to ArtifactStore**

- R3. When running server-side, `ArtifactLifecycle` writes collected artifacts directly to the server's `ArtifactStore`, fixing the current gap where server-managed runs configure no artifact sink and silently discard artifacts.
- R4. When running via CLI, the existing HTTP upload path (`HttpArtifactUploader`) continues to work. Internal uploader abstractions may change as needed, but the CLI must continue uploading artifacts to the server rather than writing directly to `ArtifactStore`.
- R5. `ArtifactLifecycle` must be configured with exactly one artifact sink for artifact-enabled runs: either a direct `ArtifactStore`-backed sink for server-managed runs or an HTTP uploader-backed sink for CLI runs. It must also hold a `RunId` for direct `ArtifactStore::put` calls. The internal representation of that sink (enum, unified trait, or refined existing trait) is deferred to planning.

**Scratch cleanup**

- R6. Remove `artifact_cache_dir()`, `artifact_files_dir()`, and `artifact_stage_dir()` from `RunScratch` in `fabro-config/src/storage.rs`.
- R7. Remove the `create_dir_all(self.artifact_files_dir())` from `RunScratch::create()`.
- R8. Update `docs/reference/run-directory.mdx` and `docs-internal/run-directory-keys.md` to remove all `cache/artifacts/` entries (including `cache/artifacts/files/`).
- R10. Update integration tests in `fabro-workflow/tests/it/integration.rs` and `daytona_integration.rs` that assert on `artifact_stage_dir` paths to verify artifacts via `ArtifactStore` or uploader behavior instead of local filesystem paths.

**CLI output**

- R9. The post-run "=== Artifacts ===" output must stop printing local scratch paths. It should list durable artifact identifiers derived from stored metadata, at minimum `node_slug`, `retry`, and `relative_path`, and reference `fabro artifact cp` for retrieval.
- R11. Removing `artifact_stage_dir()` must not change durable per-stage/per-attempt grouping. Artifacts must still be addressable by `StageId`, and CLI/server artifact surfaces must continue exposing `node_slug`, `retry`, and `relative_path` from store-backed metadata rather than reconstructing local scratch paths.

## Success Criteria

- Server-managed workflow runs produce artifacts in `ArtifactStore` (previously they did not).
- No `cache/artifacts/` directory is created or written to during any workflow run.
- `fabro artifact list` and `fabro artifact cp` continue to work unchanged.
- The post-run artifact summary prints logical artifact identifiers (`node_slug`, `retry`, `relative_path`) instead of local scratch paths.
- `ArtifactCaptured` events still contain correct hashes and byte counts.

## Scope Boundaries

- No changes to `fabro artifact list` or `fabro artifact cp` commands.
- No changes to the `Sandbox` trait (no streaming download API).
- No changes to the HTTP artifact upload protocol between CLI and server.
- `runtime/blobs/` (context value materialization) is a separate concern, unchanged.

## Key Decisions

- **Transient local staging, not persistent scratch cache**: The `Sandbox` trait only has `download_file_to_local`. Rather than adding a streaming API to all sandbox impls, use transient local files outside `RunScratch`, but keep them alive until the configured sink has finished consuming them.
- **Server writes to ArtifactStore directly**: The server already owns the `ArtifactStore` instance. Server-managed runs should use that directly instead of relying on an uploader being present.
- **CLI keeps HTTP upload path**: The CLI continues uploading artifacts to the server over HTTP. The internal uploader interface may change, but the wire protocol stays the same. Smaller blast radius than giving the CLI its own local `ArtifactStore`.

## Outstanding Questions

### Deferred to Planning

- [Affects R5][Technical] What internal representation should `ArtifactLifecycle` use for the exactly-one artifact sink? Options: an enum with server/CLI variants, a new unified trait, or a refactor of the existing uploader trait.
- [Affects R1][Technical] Should `collect_artifacts` return transient local handles/paths alongside `CapturedArtifactInfo`, or should store/upload happen inside the collection step while the transient files are definitely still present?
- [Affects R1][Technical] Should transient staging use `tempfile::TempDir` (auto-cleanup on drop) or manual `std::fs::remove_dir_all`? The former is more robust against panics.
- [Affects R9][Needs research] What does the current CLI output look like for artifacts, and what is the best replacement format? Check `lib/crates/fabro-cli/src/commands/run/output.rs`.

## Next Steps

-> `/ce:plan` for structured implementation planning
