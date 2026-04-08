# Object-Backed Artifact Uploads

## Summary
- Make `ArtifactStore` the durable source of truth for all stage artifacts, backed by a configurable object store that can be local filesystem or S3.
- Keep the server as the only durable artifact writer. Worker subprocesses upload artifacts to the server over the existing stage-artifacts POST route; they do not write `ArtifactStore` directly and do not use the run scratch directory as IPC.
- Ship both upload modes on `POST /api/v1/runs/{id}/stages/{stageId}/artifacts`:
  - `application/octet-stream` for single-file upload
  - `multipart/form-data` for batch upload on the same path
- Default artifact-upload failure policy is non-fatal: after bounded retries, the worker emits a warning notice and the run continues.
- Multipart uploads are a strict wire format: the `manifest` part must arrive first so the server can validate the batch before accepting file bytes.

## Key Changes
- Add artifact storage configuration separate from the main run-store path:
  - default backend: local object store rooted under the existing storage directory
  - optional backend: S3 with bucket, region, prefix, optional endpoint override, and path-style toggle
- Refactor server startup so `ArtifactStore` is constructed from artifact-storage config instead of being hardwired to `LocalFileSystem` in `serve.rs`.
- Extend `ArtifactStore` with streaming writes:
  - add `put_stream(...)` that writes directly to the configured object store using `object_store::buffered::BufWriter`
  - use the same deterministic object key layout as today so retries are idempotent
- Replace the buffered server artifact upload handler with a streaming implementation:
  - `application/octet-stream`: requires `filename` query param, streams request body into `ArtifactStore`
  - `multipart/form-data`: requires a JSON `manifest` part first, followed by file parts; the manifest is the canonical source of artifact paths and optional checksums/content types
- Add multipart request types to the OpenAPI spec:
  - `ArtifactBatchUploadManifest`
  - `ArtifactBatchUploadEntry`
  - one entry per file part, keyed by part name and relative artifact path
- Validate both octet-stream `filename` query params and multipart manifest paths with the existing relative-path rules; reject traversal, empty segments, duplicate manifest paths, duplicate part names, missing parts, and unexpected parts.
- Compute and verify `sha256` during upload when provided by the client. If omitted, accept the upload without checksum enforcement. Native backend checksum features such as S3-specific checksum headers are optional optimizations, not part of the v1 contract.
- Enforce explicit server-side limits:
  - maximum single artifact size
  - maximum artifacts per multipart request
  - maximum total multipart request bytes
  - reject uploads that exceed these limits before durable commit when possible, and abort active multipart writes when the limit is crossed mid-stream
- For multipart requests, return non-2xx on the first failed file and leave already-written objects in place. Retries are safe because object keys are deterministic; v1 does not attempt cross-object rollback.
- Treat concurrent uploads to the same `{run_id, stage_id, path}` as idempotent-safe retries. Deterministic object keys mean duplicate concurrent uploads may race, but the final durable object must be equivalent regardless of which writer wins.
- Update worker subprocess behavior:
  - after a stage captures artifacts locally, the worker uploads them to the server over the internal artifact route
  - single-file uploads may use raw octet-stream; batch upload should use multipart when multiple artifacts exist for the stage
  - the worker emits `artifact.captured` only after the server confirms durable upload
  - on repeated upload failure, the worker emits a warning-style run notice and continues
- Update read paths so `ArtifactStore` is the only source for list/download; no run-scratch fallback is required.
- Extend the worker spawn contract so the server provides:
  - internal server address or Unix-socket target
  - a short-lived bearer token scoped to artifact upload routes for that run
- Accept that interrupted multipart uploads may leave orphaned objects in v1. Record this as operational debt and add a later cleanup pass or age-based GC policy for abandoned artifact objects.

## Public Interfaces
- `POST /api/v1/runs/{id}/stages/{stageId}/artifacts` accepts both `application/octet-stream` and `multipart/form-data`.
- Multipart wire format:
  - one `manifest` JSON part first
  - one file part per manifest entry
  - manifest fields: `part`, `path`, optional `sha256`, optional `expected_bytes`, optional `content_type`
- Settings gain artifact object-store configuration, with local as the default and S3 as an explicit opt-in backend.

## Test Plan
- Server integration: single-file octet-stream upload stores objects in local object store and returns `204`.
- Server integration: multipart batch upload with manifest stores every artifact under the expected object keys and returns `204`.
- Validation: invalid artifact path, invalid octet-stream `filename`, duplicate path, duplicate part name, missing file part, unexpected file part, malformed manifest, and manifest-not-first all return `400`.
- Integrity: checksum mismatch returns error and does not mark the artifact as captured.
- Limits: oversized single artifact, oversized batch byte total, and too many multipart entries all fail with the configured limit response and abort the active upload.
- Retry behavior: partial multipart failure can be retried safely and produces the correct final artifact set.
- Concurrency: concurrent uploads to the same run/stage/path converge safely to one correct durable object.
- Read path: list/download returns artifacts only from `ArtifactStore`.
- Worker integration: worker uploads captured artifacts through the server route and emits `artifact.captured` only after success.
- Failure behavior: repeated upload failure emits a warning notice and the run completes without failing.
- Backend coverage: large artifact upload works against an S3-compatible backend such as MinIO and uses streaming/multipart object-store writes without buffering the full file in memory.

## Assumptions
- Scope is limited to `ArtifactStore` stage artifacts. Large run blobs written by `RunDatabase::write_blob()` are unchanged.
- Runs are expected to have durable artifacts in object storage; scratch copies are only local cache/state.
- The server remains the sole durable artifact writer and owns all artifact object-store credentials.
- Non-fatal artifact-upload failure is the chosen default for v1; if strict durability becomes required later, it can be added as a separate policy change.
- The artifact object key remains scoped by run id, stage id, and relative artifact path, so same-key retries are naturally idempotent.
