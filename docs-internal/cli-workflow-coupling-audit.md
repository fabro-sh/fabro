# CLI Workflow Coupling Audit

## Scope

- Production imports under `lib/crates/fabro-cli/src/**` that still reference `fabro_workflow::*` after the server-owned selector/export refactor.
- Test-only imports are listed separately so the remaining architectural debt is explicit.

## Completed In This Change

- Removed the production CLI dependency on `fabro_workflow::run_lookup`.
- Removed the production CLI dependency on `fabro_workflow::run_dump`.
- Added server-owned selector resolution via `GET /api/v1/runs/resolve` and migrated single-run selector flows to it.

## Remaining Production Couplings

| Path | Direct dependency | Why it still exists | Required remediation track |
| --- | --- | --- | --- |
| `lib/crates/fabro-cli/src/commands/run/fork.rs` | `operations::{ForkRunInput, RewindTarget, build_timeline_or_rebuild, fork}` | User-facing CLI still reconstructs run timelines and mutates rewind/fork metadata locally. | Replace with a server API for timeline inspection and fork execution. |
| `lib/crates/fabro-cli/src/commands/run/rewind.rs` | `git::MetadataStore`, `operations::{RewindInput, RewindTarget, RunTimeline, TimelineEntry, build_timeline_or_rebuild, rewind}` | User-facing CLI still performs rewind timeline resolution and metadata mutation locally. | Replace with a server API for rewind preview and rewind execution. |
| `lib/crates/fabro-cli/src/commands/pr/create.rs` | `outcome::StageStatus`, `pull_request::maybe_open_pull_request` | CLI still reconstructs store state and runs PR creation logic from the workflow pipeline directly. | Replace with a server API, or extract PR orchestration into a non-engine shared service crate plus API. |
| `lib/crates/fabro-cli/src/commands/run/runner.rs` | `artifact_snapshot::CapturedArtifactInfo`, `artifact_upload::{ArtifactSink, StageArtifactUploader}`, `event::{Emitter, RunEventSink}`, `operations::{self, StartServices}`, `run_control::RunControlState`, `runtime_store::{RunStoreBackend, RunStoreHandle}` | Hidden worker subprocess path still lives inside the CLI crate and embeds the workflow engine directly. | Re-home worker/runtime code outside the user CLI surface, ideally into a dedicated worker crate or binary. |
| `lib/crates/fabro-cli/src/manifest_builder.rs` | `git::{GitSyncStatus, head_sha, sync_status}` | Manifest submission still relies on git helper logic that happens to live in `fabro_workflow`. | Extract git-sync inspection helpers into a non-workflow shared crate/module. |
| `lib/crates/fabro-cli/src/server_client.rs` | `artifact_snapshot::CapturedArtifactInfo` | The upload client reuses a workflow-owned artifact snapshot DTO. | Extract shared artifact snapshot DTOs into `fabro-store`, `fabro-types`, or a dedicated shared crate. |
| `lib/crates/fabro-cli/src/commands/runs/inspect.rs` | `run_status::RunStatus` | CLI output types still depend on engine-owned run status enums. | Extract shared status types into `fabro-types` or switch to API-generated/public store types. |
| `lib/crates/fabro-cli/src/commands/runs/list.rs` | `run_status::RunStatus` | List rendering still depends on engine-owned run status enums. | Extract shared status types into `fabro-types` or switch to API-generated/public store types. |
| `lib/crates/fabro-cli/src/commands/run/attach.rs` | `outcome::StageStatus`, `run_status::RunStatus` | Attach/replay logic still formats engine-owned terminal status types directly. | Extract shared run/conclusion status types into `fabro-types`. |
| `lib/crates/fabro-cli/src/commands/run/output.rs` | `outcome::StageStatus`, `records::Conclusion` | Human-readable completion output still consumes workflow-owned conclusion/status records. | Extract shared conclusion/status DTOs into `fabro-types` or `fabro-store`. |
| `lib/crates/fabro-cli/src/commands/run/wait.rs` | `records::Conclusion`, `run_status::RunStatus` | Wait output still depends on workflow-owned status/conclusion records. | Extract shared conclusion/status DTOs into `fabro-types` or `fabro-store`. |
| `lib/crates/fabro-cli/src/commands/run/run_progress/stage_display.rs` | `outcome::{StageStatus, format_cost}` | Progress UI still depends on workflow-owned stage status and cost-formatting helper code. | Extract shared stage status types into `fabro-types` and move formatting helpers into `fabro-util`. |
| `lib/crates/fabro-cli/src/commands/run/run_progress/info_display.rs` | `event::RunNoticeLevel` | Progress UI still formats workflow-owned notice levels directly. | Extract shared notice/event enums into `fabro-types`. |
| `lib/crates/fabro-cli/src/commands/run/run_progress/event.rs` | `event::RunNoticeLevel` | Progress event translation still depends on workflow-owned notice levels. | Extract shared notice/event enums into `fabro-types`. |

## Test-Only Couplings

| Path | Direct dependency | Why it still exists | Suggested handling |
| --- | --- | --- | --- |
| `lib/crates/fabro-cli/src/commands/store/dump.rs` test module | `event::{Event, append_event}` | Unit tests synthesize workflow events directly. | Low priority; keep until a lighter-weight event fixture helper exists. |
| `lib/crates/fabro-cli/src/commands/run/wait.rs` test module | `outcome::StageStatus`, `records::Conclusion`, `run_status::RunStatusRecord` | Output tests construct workflow-owned records directly. | Replace with shared fixture builders once status/conclusion DTOs move out. |
| `lib/crates/fabro-cli/src/commands/run/run_progress/mod.rs` test module | `event::{Event, RunNoticeLevel, to_run_event, to_run_event_at}`, `outcome::billed_model_usage_from_llm` | Progress tests build engine events directly. | Replace with shared event fixture helpers after event DTO extraction. |
| `lib/crates/fabro-cli/src/commands/run/run_progress/event.rs` test module | `event::{Event, to_run_event}` | Event rendering tests depend on engine event constructors. | Replace with shared event fixture helpers after event DTO extraction. |
| `lib/crates/fabro-cli/src/commands/run/runner.rs` test module | `artifact_upload::StageArtifactUploader` | Worker tests still reach into workflow upload internals. | Keep with worker re-home work; not worth separating first. |
| `lib/crates/fabro-cli/tests/it/workflow/real_cli.rs` | `context::Context`, `event::Emitter`, `handler::agent::{CodergenBackend, CodergenResult}`, `handler::llm::cli::AgentCliBackend` | Integration test exercises the real workflow engine directly through CLI harnesses. | Accept as engine integration coverage or move under workflow-owned test support later. |
| `lib/crates/fabro-cli/tests/it/scenario/recovery.rs` | `operations::{RunTimeline, build_timeline}` | Scenario test inspects rewind timeline internals directly. | Replace after server-owned rewind/timeline APIs exist. |

## Follow-Up Order

1. Design server APIs for rewind/fork and PR creation so user-facing CLI commands stop importing workflow operations directly.
2. Decide whether the hidden worker path should move to a dedicated worker crate/binary or remain a CLI-internal implementation detail with a stricter boundary.
3. Extract shared status, conclusion, notice, and artifact snapshot types/helpers out of `fabro_workflow`.
4. Extract git sync helpers from `fabro_workflow` so manifest building no longer depends on engine code.
