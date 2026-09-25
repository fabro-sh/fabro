# Fabro views of a run under Petri

Plan item F2.1 of `fabro-integration.md`. Petri at `a0d2ceb`, Fabro at
`170291b9f`. This matrix lists every Fabro view of a run and where each fact
comes from once Petri's records are the store. It is written before any view
changes. F2.2 (the projection), F2.3 (platform records) and F2.4 (API, CLI,
web) build from it.

F2.2 and F2.3 implement this matrix: `src/projection.rs` is the fold,
`src/projector.rs` the view pass and its wake-up, and `fabro-store`'s
`platform_records` module the platform record kinds and their table. The
crate README names the rows the fold still leaves default.

Sources are named three ways:

- A Petri event, by its `<subject>.<verb>` name from
  `crates/core/execution/EVENTS.md`. `derived.x` is a value Petri adds beside
  the record. `parsed.x` is Petri's reading of a `step.progress.recorded`
  payload. `custom <kind>` is a `step.progress.recorded` payload with that
  `kind`. `envelope <Variant>` is a backend envelope (`kind = "pebble"` or
  `"acp"`) carrying that Pebble `CodingAgentEvent` variant.
- A platform record, by its `kind` in F2.3's `platform_records` table. The
  plan names four kinds: `checkpoint`, `pull_request.created`,
  `notification.sent`, `run.paired`. This matrix adds the kinds a view needs
  beyond those; the "Platform records" section lists them all.
- `derived`: computed from the rows above. `live`: a query against a provider
  at read time, never a stored fact. `gap`: no source; see the Gaps table.

## Identities

| Identity | Definition | Why |
| --- | --- | --- |
| run | Fabro's `RunId`, which is `RunOptions::run_key` and the `run_key` of Petri's run declaration | one key for the store, the sandbox labels and the API |
| stage | `(run, execution, firing)` | a firing is one visit of a node in one execution; two child invocations can share a node name and visit |
| stage label | `StageId` as `node@visit` from `subject.node.name` and `subject.visit` | display only; never a key, never a join column |
| logical stage | `subject.node.meta.kind` and `meta.synthetic` | lowering nodes (`parallel.branch` delegates, synthetic `<fork>.fan_in`) are not stages in a list |
| attempt | `(stage, attempt)` from `subject.attempt` | a retry keeps the firing |
| fork occurrence | `ForkOccurrence {execution, fork, firing, visit, generation}` | one per visit of a fork; a nested fork has its own |
| branch | `BranchRef {fork, index}` under an occurrence; the child invocation's `call.slot` is `branch:<fork>@<firing>:<index>:<target>` | duplicate targets are separate branches |
| invocation | `context.invocation`; `context.parent` is the calling `(execution, firing, attempt, slot)` | a branch or nested workflow is a child invocation |
| question | `Question.id`, unique within the run, plus the asking `(execution, firing, attempt)` | an agent's question and a human gate's share one shape |
| agent session | Pebble's `session_id`, `parent_session_id`, `stream_id`, `seq`, `tool_call_id`, as the envelope carries them | never rewritten |
| event position | `EventId {log, seq, index}` per log; the API cursor is F2.4's `stream_seq` | `EventId` is per log and has no platform variant |
| platform record | `(run_id, seq)` and, where it belongs to a stage, `(execution, firing)` | ties a Fabro fact to a Petri position |

## Run summary

The `Run` type (`fabro-types/src/run_summary.rs`) serves the run list, the
run detail header, the summary panel, `runs ps`, `run wait` and the SSE
toasts. `RunProjection` (`GET /runs/{id}/state`) serves `attach`, `inspect`,
`output`, `diff`, `rewind` and `fork`.

| Fabro fact | Fields | Source | Keyed on |
| --- | --- | --- | --- |
| identity, parent, children | `Run.id`, `parent_id`, `children_count` | `run.started` (`run_key`); parent and children are Fabro's run tree: platform record `run.parent` (`parent_id`) | run |
| title, goal, workflow, automation, repository, principal, origin, labels, source directory, links | `Run.title`, `goal`, `workflow`, `automation`, `repository`, `created_by`, `origin`, `labels`, `source_directory`, `links`, `RunProjection.spec`, `web_url` | platform record `run.created` (the `RunSpec` Fabro built; the goal is also `graph.registered`'s graph `goal` param); `run.title` for a rename | run |
| status: submitted, pending, runnable, starting | `lifecycle.status.kind` before Petri runs | platform record `run.lifecycle {kind, reason}` (Fabro's queue and approval are before `run.started`) | run |
| status: running | `lifecycle.status.kind = running` | `run.started` | run |
| status: blocked (`human_input_required`) | `lifecycle.status.kind = blocked`, `RunProjection.pending_interviews` | derived: any live firing whose last `wait.state.changed` is `awaiting_answer`; see Questions | run |
| status: paused | `lifecycle.status.kind = paused`, `pending_control` | `run.paused`, `run.unpaused`; a pending request is derived from Fabro's own control call until the record lands. The worker mirrors the same state as platform records `run.lifecycle {paused}` and `{unpaused}`, the way the legacy worker reported it, so the server's live status follows too; they fold to the same status | run |
| status: succeeded, failed | `lifecycle.status.kind`, `status.reason`, `lifecycle.error`, `Conclusion.status`, `Conclusion.failure` | `run.finished {status}` (`success`, `failed`, `cancelled`) and the root `invocation.finished {result}` (`status` gives `partial_success`; `failure` gives the message and class) | run |
| status: dead, removing | `lifecycle.status.kind` | platform record `run.lifecycle` (lease lost, delete requested); Petri has no such state | run |
| cancel reason | `FailureReason::cancelled`, `terminated` | `invocation.cancel.requested {reason}` (`interrupt`, `control`, `stall_timeout`); `run.stalled` beside a watchdog cancel | invocation |
| approval | `lifecycle.approval`, `RunProjection.approval` | platform record `run.lifecycle` (approved, denied with reason) | run |
| archived, superseded, retried | `lifecycle.archived`, `archived_at`, `superseded_by`, `retried_from` | platform records `run.archived`, `run.unarchived`, `run.superseded {new_run_id, target}`, `run.created {retried_from}` | run |
| created at | `timestamps.created_at` | platform record `run.created` `recorded_at` | run |
| started at | `timestamps.started_at`, `StartRecord.start_time` | `run.started` `recorded_at` | run |
| last event at | `timestamps.last_event_at`, `RunProjection.last_event_at` | derived: the greatest `recorded_at` across the coordinator log, every execution log and the platform records | run |
| completed at | `timestamps.completed_at`, `Conclusion.timestamp` | `run.finished` `recorded_at` | run |
| current stage | the list row's status text, `Checkpoint.current_node`, `next_node_id` | derived: the newest firing with no `visit.completed` (label from `subject.node`); `route.applied` `derived.target` for the next node | stage |
| wall time | `timing.wall_time_ms`, `Conclusion.timing` | derived: `run.finished` minus `run.started`; live: now minus `run.started` | run |
| inference and tool time | `timing.inference_time_ms`, `tool_time_ms`, `active_time_ms` | derived: the sum of every final `step.finished` `metrics.custom.pebble.inference_ms` and `pebble.tool_ms`; a prompt node's `custom attractor.prompt.completed` `duration_ms` counts as inference | stage |
| model usage | `Run.usage`, `Conclusion.usage`, `Run.size`, `Run.models` | derived: the sum of every final `step.finished` `metrics.custom.pebble.usage` and `prompt.usage` (lithos-llm `Usage`, cost absent when unpriced); `Run.models` from `custom attractor.fallback.plan` `requested` per stage | stage |
| retries | `Conclusion.total_retries`, `StageSummary.retries` | derived: `visit.completed {attempts}` minus one per firing | stage |
| stages summary | `Conclusion.stages` | derived from the Stages section | stage |
| diff | `Run.diff`, `Conclusion.diff`, `Checkpoint`'s diff | platform record `checkpoint {diff_summary, patch_blob}`; the final one is the run's | stage |
| final commit | `Conclusion.final_git_commit_sha` | the last platform record `checkpoint {git_commit_sha}` | stage |
| run branch, base sha | `StartRecord.run_branch`, `base_sha` | platform record `run.branch {run_branch, base_sha}`, positioned on the checkpoint that created the branch | run |
| Git identity | `RunProjection.git_identity` | platform record `git.identity {name, email, source}`, positioned with `run.branch` | run |
| pull request | `Run.pull_request`, `RunProjection.pull_request`, `pull_request_creation` | see Platform | run |
| current question | `Run.current_question` | see Questions | question |
| sandbox | `Run.sandbox`, `RunProjection.sandbox` | see Sandbox | invocation |
| Ask Fabro | `Run.ask_fabro` | live: whether the sandbox is ready and a model is configured | run |
| final output | `run output` reads `Checkpoint.context_values["response.<node>"]` | the root `invocation.finished {result.output}`; a `blob://sha256/…` reference resolves through the store's `get_blob` | run |
| pending control | `RunProjection.pending_control` | derived: a control Fabro sent whose `run.paused`, `run.unpaused` or `cancel.requested` record has not landed | run |

### The `runs` summary row

Decision 1: the row stays, written in the F2.2 view transaction, narrowed to
what the list views and the scheduler query. Every read path selects only
`id`, `summary_json` and a `children_count` subquery; the other columns are
`WHERE` and `ORDER BY` inputs. The scheduler
(`reconcile_incomplete_runs_on_startup`, `list_by_statuses`) filters on
`status` and reads `id` and `lifecycle.pending_control` from the JSON.

| Column | Read by | Source under Petri |
| --- | --- | --- |
| `id` | every query | `run.started` `run_key` |
| `summary_json` | the API, web and CLI lists | the `Run` value, derived as above |
| `status` | scheduler filter, visibility filter, status sort | the status rows above |
| `archived_at_ms` | visibility filter, status sort | platform record `run.archived` |
| `parent_id` | parent filter, children count | platform record `run.parent` |
| `automation_id` | automation filter | platform record `run.created` |
| `created_at_ms` | default sort, elapsed fallback | platform record `run.created` |
| `started_at_ms`, `completed_at_ms` | elapsed sort | `run.started`, `run.finished` |
| `last_event_at_ms` | updated sort | derived, as above |
| `title`, `workflow_name`, `repository_name` | title, workflow, repository sorts | platform record `run.created`, `run.title` |
| `workflow_slug` | run selector resolution (`list_identities`) | platform record `run.created` |
| `diff_additions`, `diff_deletions` | changes sort | the last platform record `checkpoint {diff_summary}` |
| `total_usd_micros` | size sort | the usage row above |
| `source_last_seq` | concurrency guard on the write path | replaced by the F2.2 per-log positions and F2.4's `stream_seq` |
| `diff_files_changed`, `input_tokens`, `output_tokens`, `reasoning_tokens`, `cache_read_tokens`, `cache_write_tokens` | nobody | dropped from the row; the JSON keeps the values |

## Stages

Views: the stage sidebar and popover, the Stages route with its chat,
primary, context and debug sub-tabs, the waterfall, the Usage tab, the stage
artifacts, the command log endpoint, the CLI progress lines and
`run events --pretty`.

A stage is one firing. The list shows firings whose node `meta.kind` is one
of `start`, `exit`, `command`, `agent`, `prompt`, `human`, `conditional`,
`parallel`, `parallel.fan_in`, `stack.manager_loop`, `wait`, and never one
with `meta.synthetic = true` or `meta.kind = parallel.branch`. A branch's own
stages live in the child invocation and list under the fork (see Parallel).

| Fabro fact | Fields | Source | Keyed on |
| --- | --- | --- | --- |
| list, order, node, handler | `RunStage.id`, `name`, `node_id`, `handler`, `StageProjection.handler`, `first_event_seq` | `visit.started` (`subject.node.name`, `meta.kind`, `meta.label`); order is `recorded_at` | stage |
| visit, graph visit, resumed from | `RunStage.visit`, `graph_visit`, `resumed_from_stage_id`, `StageProjection.graph_visit`, `resumed_from_stage_id` | `subject.visit`; `execution.declared {predecessor}` marks a restart; `resumed_from_stage_id` is dropped (Petri resumes the same firing) | stage |
| state: pending | `state = pending` | `visit.started`, `wait.state.changed {awaiting_admission}` | stage |
| state: running | `running` | `admission.decided {admit}`, `step.started`, `wait.state.changed {running, awaiting_answer, cancelling}` | attempt |
| state: retrying | `retrying`, `stage.retrying` lines (attempt, max attempts, delay) | `step.finished` with `derived.final = false`, `retry.scheduled {next_attempt, base_delay}`, `wait.state.changed {awaiting_retry}`, `retry.elapsed` | attempt |
| state: succeeded, partially succeeded, failed, cancelled | `state`, `StageCompletion.outcome`, `failure_reason`, `timestamp` | `visit.completed {outcome, executed, attempts}` (`success`, `partial_success`, `failure`, `timed_out`, `cancelled`); the class and message from the final `step.finished` `outcome.failure` | stage |
| state: skipped | `skipped` | `admission.decided {skip}`, or `visit.completed {executed: false}` with a `skipped` outcome (false precondition) | stage |
| attempts | `stage.started` `attempt`, `max_attempts`; `derived.exhausted` | `subject.attempt` on every event; `visit.completed {attempts}`; `step.finished` `derived.exhausted` | attempt |
| started at, timing | `RunStage.started_at`, `wall_time_ms`, `StageProjection.started_at`, `timing` | `visit.started` `recorded_at` to `visit.completed` `recorded_at`; `step.finished` `metrics.duration_ms` per attempt; inference and tool time from `metrics.custom.pebble.inference_ms`, `pebble.tool_ms` | stage |
| live timing | `live_inference_ms`, `live_tool_ms`, `tool_batch`, `inference`, `acp_started_at` | envelope `LlmRequestStarted`, `LlmFirstOutput`, `AssistantMessage` (the bracket), `ToolCallStarted`, `ToolCallCompleted` (the batch); `step.started` for an ACP node | session |
| usage | `RunStage.usage`, `StageProjection.usage`, `usage_by_model`, `model` | final `step.finished` `metrics.custom.pebble.usage`, `prompt.usage`; per model from `pebble.subagents.sessions[*] {provider, model, usage}` and envelope `SessionStarted` plus `AssistantMessage {usage}` per session | stage, session |
| provider and model | `RunStage.provider_used`, `StageProjection.provider_used`, `model`, `permission_level` | `custom attractor.fallback.plan {requested, routes}` then envelope `SessionStarted {provider, model}`; `custom attractor.prompt {model}`; the node's config in the registered graph (`graph.registered`, blob by digest) for `reasoning_effort`, `speed`, `permission_level` and an ACP node's settings | attempt |
| prompt | `StageProjection.prompt`, the chat tab's `stage.prompt` | `custom attractor.prompt {prompt, sources}`; envelope `SessionStarted` and the first user message on the stream for an agent | attempt |
| response | `StageProjection.response`, `prompt.completed` | `custom attractor.prompt.completed {response, calls, repairs, usage, duration_ms}`; the final `step.finished` `outcome.output` for an agent | attempt |
| output, output bytes, streaming, termination | `StageProjection.output`, `output_bytes`, `live_streaming`, `termination`, `command.started` `script`, `command.completed` `exit_code`, the command log endpoint | `step.started`; `step.progress.recorded` `log {stream, line}` (the live log); `step.finished` `outcome.output`, `metrics.exit_code`, `metrics.duration_ms`; `timed_out` and `cancelled` statuses for `termination`; a `blob://` output through `get_blob`; the script is `subject.node.meta.script` on every event of the stage (the web's command view reads it off the stream) | attempt |
| output loss | the command view's "output truncated" note | the final `step.finished` `metrics.custom` `output.dropped_bytes`, `output.truncated_lines` (what the caps cut) and `output.incomplete` (the capture ended on silence); absent when the output is whole | attempt |
| script invocation and timing | `script_invocation`, `script_timing` | the node config; `metrics.duration_ms` | attempt |
| context updates, routing directive | `stage.completed` `context_updates`, `preferred_label`, `suggested_next_ids`, `jump_to_node` | `step.finished` `outcome.context_updates`; `routing.resolved` (per group the decision, overrides, jumps, blocks, the weighted draw; `derived.groups[].target`) | attempt |
| edge selected, loop restart, the condition that matched | `edge.selected`, `loop.restart`, the decision renderer's `condition`, the `run events --pretty` transition line | `route.applied` (`derived.target`, `transition`, `back`); its `edge` keys `subject.node.meta.edges`, whose entry carries the edge's `condition` as written (absent on an unconditional edge); a restart is `execution.finished {restart}` then `execution.declared {predecessor}` | stage, execution |
| notes | `StageCompletion.notes` | `step.finished` `outcome` notes; `parsed.note {result_prepared, transition}` | attempt |
| files touched | `stage.completed` `files_touched` | Pebble's fold of envelope `ToolCallCompleted` (see Agent activity) | session |
| stage diff | `StageProjection.diff` | platform record `checkpoint {execution, firing, patch_blob}` | stage |
| artifacts | `RunArtifactEntry {stage_id, node_slug, retry, relative_path, size}`, the stage artifact endpoints | platform record `artifact.collected {execution, firing, attempt, path, object (or historical blob), bytes, digest}` from the `transition` hook, new bytes in configured artifact storage, historical blob sources in SQLite; a file unchanged since an earlier capture is not recorded again | attempt |
| checkout | `setup.*` lines, `attractor.checkout` | the root `start` stage's `custom attractor.checkout {repository, commit, depth, files}` and its log lines; `[run.prepare]` commands are `run_prepare_N` stages | stage |
| hook decisions | none today | `parsed.note {kind: hook}` (`HookReport`), `custom attractor.hook` (a point a step asks itself), `parsed.hook_activity`; `run.note.recorded` for run-level points | attempt |
| budget pause | none today | `parsed.budget {state, attempt, remaining_ms, pending_questions}` | attempt |
| nested workflow | `subgraph.started`, `subgraph.completed` | `invocation.declared` with `context.parent`, `invocation.finished {result}`, the child's `execution.declared` and `execution.finished` | invocation |

## Parallel

Views: the parallel-children and fan-in renderers, the sidebar grouping by
`parallel_group_id`, the CLI branch lines.

| Fabro fact | Fields | Source | Keyed on |
| --- | --- | --- | --- |
| fork started, branch count | `parallel.started {branch_count}`, `parallel_group_id` | `fork.started {occurrence, branches}` on the fork node (`BranchRole::fork`); `node.expanded` (`derived.clones[].entry`) for a `for_each` | occurrence |
| branch started | `parallel.branch.started {index, item_label}` | `custom attractor.parallel.branch.started {fork, occurrence, branch, index, item_label, invocation}`; the child's `invocation.declared` (`call.slot`) | occurrence, branch |
| branch stages | `RunStage.parallel_group_id`, `parallel_branch_index`, `StageProjection.parallel_branch_id` | the child invocation's firings; `context.parent` ties them to the delegate's firing; `meta.branch_role` on the child's entry node | occurrence, branch, stage |
| branch completed | `parallel.branch.completed {index, item_label, duration_ms, status}` | `branch.completed {occurrence, result}`; `custom attractor.parallel.branch.completed {status, disposition, started, duration_ms}` (`started: false` is a branch the cancel reached first); the child's `invocation.finished` | occurrence, branch |
| envelopes at the join | `parallel.completed {results, success_count, failure_count}`, `StageProjection.parallel_results` (`ParallelBranchResult {id, index, item_label, status, context_updates}`) | `fork.completed {occurrence, fork, results, disposition}` in branch order; `custom attractor.parallel.completed` on the fan-in with `parallel.results` in its `step.finished` `context_updates` | occurrence |
| cancelled or killed fork | none today | `fork.completed {disposition: cancelled | killed}`; the join's `visit.completed {executed: false}` | occurrence |
| fan-in prompt | the fan-in renderer's per-branch prompt, response, model, tokens | the fan-in's `custom attractor.prompt` and `attractor.prompt.completed` | attempt |
| nested fork, repeated fork | (no distinct view) | each is its own `ForkOccurrence`; a nested fork's events are in the branch's child execution | occurrence |
| empty `for_each` | (no distinct view) | `fork.started` and `fork.completed` with zero branches; the placeholder clone is `synthetic: true` and is not shown | occurrence |

## Questions

Views: the interview dock, `Run.current_question`, `pending_interviews`, the
human Q&A renderer, `attach`'s inline prompt, the questions endpoints, Slack
interviews.

| Fabro fact | Fields | Source | Keyed on |
| --- | --- | --- | --- |
| pending | `pending_interviews[id] {question, started_at}`, `current_question`, `interview.started` | `step.progress.recorded` with `parsed.question` (`id`, `text`, `options[] {key, label, description, preview}`, `default`, `freeform`, `sensitive`, `kind`, `reference {label, url, kind}`, `timeout_ms`, `context`); `wait.state.changed {awaiting_answer}`; pending until a closing row below | question |
| question fields | `InterviewQuestionRecord.id`, `text`, `stage`, `question_type`, `options`, `allow_freeform`, `timeout_seconds`, `review_target` | `parsed.question`: `kind` is `question_type`, `freeform` is `allow_freeform`, `reference` is `review_target`, `timeout_ms` is `timeout_seconds`; `stage` is the subject's label | question |
| option description and preview, context display | `InterviewOption.description`, `preview`, `context_display` | `parsed.question`: each option's `description` and `preview`, the question's `context` (a human gate reads them from its edges' `human.description` and `human.preview` and from the previous stage's response; a native agent's question carries Pebble's); `reference` is `review_target` when Fabro's validation admits it | question |
| answered | `interview.completed {answer, duration_ms}`, the `actor` | `control.requested` with `derived.answer` and `derived.deliverable = true`; a sensitive answer stays `{"$secret": "answer:<id>"}`; `wait.state.changed {running}` follows; duration is `control.requested` minus the question's `recorded_at`; the actor is platform record `interview.answered {question, principal}` | question |
| late answer | none today | `control.requested` with `derived.deliverable = false` | question |
| expired | `interview.timeout` | `parsed.question_expired {question, waited_ms, default}`; the gate's `step.finished` follows (success with the default, else class `retry_requested`) | question |
| interrupted | `interview.interrupted {reason}` | `control.requested` with `derived.answer.cancelled`, or `cancel.requested` and the attempt's `cancelled` status | question |
| agent questions | the same dock | the same `parsed.question` under the agent's stage (Pebble's question tool reaches the same interviewer) | question |
| steer | `run.steer`, `agent.steering.injected`, `agent.steer.buffered`, `agent.steer.dropped` | `control.requested` with a `{"$steer": …}` value; delivered or not by `derived.deliverable`; buffering is Pebble's, on the envelope. A steer the worker refused (no live agent stage, or several) is platform record `run.notice {code: steer_refused}` | stage |
| interrupt | `run.interrupt`, `agent.interrupt.injected`, `agent.round.interrupted` | `control.requested {cancel}` on the firing, envelope `RoundInterrupted` | stage |
| Slack delivery | `NotificationRouteSettings`, the Slack thread | platform record `notification.sent {question, channel, thread}` | question |

## Sandbox

Views: the Sandbox tab (filesystem, services, VNC), the summary panel, the
header's clone branch, `Run.sandbox`, `runs inspect`, `run ssh`, `run cp`,
the CLI setup lines.

Under the plan Petri acquires every scope through the sandbox-driver plugin,
labels it with the run key, and decides retention (`Always` is the Fabro
default). Petri records the binding, the instance and the retention outcome:
`scope.acquired` (an engine record, once per acquisition, before any attempt
in the scope) carries the provider, the provider's id for the sandbox, its
image and snapshot when the provider knows them, the working directory, the
workspace and lease, and the acquisition time; `scope.failed` the error, its
causes and the reserved provider; `scope.released` (a coordinator record,
once per lease the invocation owned, before `run.finished`) the outcome
retention read, whether the sandbox still exists, and any release problem.
The run's sandbox is the root invocation's scope; a child invocation's scope
(a parallel branch) is not the run's. Petri names the host provider `host`,
which is Fabro's `local`; every other kind is spelled the same.

| Fabro fact | Fields | Source | Keyed on |
| --- | --- | --- | --- |
| plan | `RunSandbox.plan {provider, image, snapshot}` | `graph.registered`'s `fabro.environment` and `fabro.launch {sandbox_backend}` params; platform record `run.created` | run |
| binding: isolated or inherited | none today | `invocation.declared {sandbox}` | invocation |
| which: planned, initializing, ready, failed | `RunSandbox.kind`, `sandbox.initializing`, `sandbox.ready {duration_ms, name, url}`, `sandbox.failed {error, causes, duration_ms}` | `planned` from the platform record `run.created`; `initializing` from `run.started`; `ready` from the root invocation's `scope.acquired` (`provider`, `image`, `snapshot`); `failed` from its `scope.failed` (`provider`, `error`, `causes`, `duration_ms`). The ready duration (`scope.acquired` `duration_ms`) is `RunSandboxInstance.ready_duration_ms` | scope |
| where: instance id, working directory, clone, workspace roots | `RunSandboxInstance.runtime {id, working_directory, repo_cloned, clone_origin_url, clone_branch, workspace_root, repos_root, primary_repo_path, primary_repo_link}`, `sandbox.initialized` | `scope.acquired` (`instance` is the id a reconnect attaches by, `working_directory`). The clone fields stay unset: `custom attractor.checkout` records Petri's copy of the bound repository into the workspace (`repository`, `commit`, `depth`, `files`), which is not a clone Fabro made, and the workspace roots are the provider's layout, read live | scope |
| retention | `RunSandboxInstance.retained`; the run-end `sandbox_cleanup` hook | `scope.released {outcome, retained, problems}` of the root invocation's lease: `retained` is set on the instance (and kept as `FoldState.sandbox_retained`), and `Run.sandbox` keeps naming the instance that ran, whether or not it still exists. `run.note.recorded {kind: hook, point: scope_released}` when a hook ran | scope |
| live status, resources, files, services, VNC, preview, SSH | `SandboxStatus`, `SandboxFileEntry`, `SandboxService`, `VncPreviewResponse`, `PreviewUrlResponse`, `SshAccessResponse`, `ssh.ready` | live: the sandbox-driver provider queried by the run label | run |
| setup commands | `setup.started`, `setup.command.completed`, `setup.completed`, `setup.failed`, `cli.ensure.*` | the `run_prepare_N` stages (Stages section); `cli.ensure.*` has no Petri equivalent and is dropped (the image carries the CLI) | stage |

## Agent activity

Views: the chat sub-tab, the insights sidebar (`StageProjection.agent`, a
Pebble `SessionProjection`), the context window endpoint, the CLI tool-call
lines, the pair transcript, the Ask Fabro sessions.

Every Pebble `CodingAgentEvent` the native backend sees is on the stream as
an envelope under the stage's firing, forwarded as recorded. Fabro keeps
feeding Pebble's own fold with those envelopes, so `StageProjection.agent`
keeps its shape.

| Fabro fact | Fields | Source | Keyed on |
| --- | --- | --- | --- |
| sessions | `root_session_id`, `agent.session.activated {thread_id, provider, model, …}`, `agent.session.deactivated` | envelope `SessionStarted {provider, model}` with `session_id`; `custom attractor.thread {thread, fidelity, resolution}` once per native session; the session ends with the attempt's `step.finished` | session |
| route and failover | `route`, `failovers[]`, `failover_stopped`, `prompt.failover` | `custom attractor.fallback.plan {requested, routes, notices}`; envelope `RouteFailover {from, to, attempt, usage, error, continuation}`, `RouteFailoverStopped {route, reason, error}`; `crates/petri/lib/tests/fallback_events.rs` is the rebuild | attempt, session |
| messages, tokens, cost | `messages`, `usage`, `agent.message {text, usage, tool_call_count}`, `prompts` | envelope `AssistantMessage {usage, tool_call_count, …}`; sum per session; the stage total is `pebble.usage` | session |
| tool calls | `tools{name: {calls, errors, open}}`, `agent.tool.started`, `agent.tool.completed`, `files_touched`, `last_file_touched`, `pending_writes` | envelope `ToolCallStarted {tool_name, tool_call_id, arguments}`, `ToolCallCompleted {tool_call_id, is_error, error_kind}`; Fabro's own run tools appear the same way (the `HostTools` capability) | tool call |
| tools available | `agent_tools` (`ToolSummary {name, description, source, category, invoked}`), `agent.tools.available`, the `run events --pretty` tool count line | `custom attractor.tools {session, tools[] {name, description, source, category}}`, once per native session (the node's own, then each child session); the stage's list is the union by name; `source` is Pebble's as recorded; `category` is Pebble's class only for a `subagent` tool, `other` for the rest (the payload carries Petri's origin category, not Pebble's permission class); `invoked` derives from envelope `ToolCallStarted` | session |
| MCP servers | `mcp_servers{}`, `agent.mcp.*` | envelope `McpServerReady {server, tools, startup_ms}`, `McpServerFailed`, `McpServerDisconnected`; `custom attractor.mcp.unavailable {server, error}` | session |
| skills | `skills.available`, `skills.activated` | `custom attractor.skills` (directories and sources), `attractor.skills.warning`; envelope `SkillsDiscovered`, `SkillActivated` | session |
| sub-agents | `subagents[]`, `subagent_counts`, `descendants` | envelope `SubAgentSpawned {agent_id, depth, task}`, `SubAgentTurnStarted`, `SubAgentCompleted`, `SubAgentFailed`, `SubAgentClosed` under the parent session; the child's events under its own session with `parent_session_id`; `pebble.subagents` on `step.finished` | session |
| compactions | `compactions[]`, `agent.compaction.*` | envelope `CompactionStarted`, `CompactionCompleted {usage, …}`, `CompactionFailed`, `CompactionCancelled`; `custom attractor.compaction`; `pebble.compactions`, `pebble.compaction_usage` on `step.finished` | session |
| context window | `context_window`, `StageContextWindow`, the `context_window` warning | envelope `Warning {kind: context_window, details}` and Pebble's fold; `unavailable_reason` derives from the stage's handler | session |
| todos | `todos{}` | envelope `todo.*` events in Pebble's fold | session |
| activity | `activity` (`idle`, `running`, `waiting_for_steer`, `ended`) | envelope `RoundInterrupted`, `AssistantMessage`; `ended` at `step.finished` | session |
| errors and warnings | `AgentErrorData`, `agent.*` errors | envelope `Error {error}`, `Warning`; `step.finished` `outcome.failure`; `custom attractor.hook.warning` | attempt |
| retries inside a request | `inference.retries`, `LlmRetry` lines | envelope `LlmRetry {model, attempt, delay_secs, error}` | session |
| ACP agent | `agent.acp.started {command, config_name}`, `agent.acp.completed {stdout, stderr, stop_reason, duration_ms}`, `agent.acp.cancelled`, `agent.acp.timed_out` | `step.started`, `step.finished` of an `attractor/agent` node with `backend = acp` (`outcome.output`, `metrics.duration_ms`, `timed_out` or `cancelled` status); the agent's ACP messages as envelopes with `kind = "acp"` | attempt |
| hook agents | none today | `parsed.hook_activity {hook, backend, envelope}`; never counted as the stage's | attempt, hook |
| pair session | `run.pair.*`, `agent.pair.user_message`, `agent.pair.system_message`, `PairRecord`, `PairTranscriptEntry` | platform records `run.paired {pair_id, target}`, `pair.ended`, `pair.failed`, `pair.message {message_id, text}`; the delivered text is `control.requested` with `{"$steer": …}`; the assistant and tool entries are the stage's envelopes | stage, pair |
| Ask Fabro sessions | `run.session.*`, `SessionDetail`, `PermissionLevel` | not Petri (decision 3: they run on the Pebble builder directly); stay in Fabro's own session records | session |

## Platform

Views: the header's pull request card and hover, the PR endpoints, the CLI
`pull_request.*` lines, the files-changed tab and commits picker, the
timeline for rewind and fork, Slack notifications, run pairing.

Platform records are the store for every row here. Each carries `run_id`,
`seq`, `recorded_at`, `kind`, `record_json`, and `execution` and `firing`
where it belongs to a stage. The proposed `record_json` fields follow.

| Fabro fact | Fields | Platform record | Keyed on |
| --- | --- | --- | --- |
| checkpoint | `checkpoints[] {seq, checkpoint, diff}`, `checkpoint.completed`, `checkpoint.failed` | `checkpoint {execution, firing, git_commit_sha, diff_summary, patch_blob}` from the `transition` hook after the commit (decision 5: a failed commit is `checkpoint_failed` on the `step.finished`, so `checkpoint.failed` needs no record); `Checkpoint`'s `completed_nodes`, `node_retries`, `node_visits`, `node_outcomes`, `context_values`, `next_node_id` and failure signatures are derived from Petri's engine state at that position | stage |
| Git commits | `git.commit {sha}`, `git.push`, `git.fetch`, `git.reset`, `RunCommit`, `RunCommitsMeta {base_sha, head_sha}` | `checkpoint {git_commit_sha}` per stage; `run.branch {run_branch, base_sha, workspace}` from the first checkpoint's branch creation; push, fetch and reset are `git.push {branch, success, attempts}` only when a view needs them (none does today) | stage, run |
| run diff | `Run.diff`, `Conclusion.diff` | platform record `run.diff {base_sha, head_sha, diff_summary, patch_blob}` from the `run_finished` hook: the run branch's last checkpoint against the base | run |
| files changed | `FileDiff`, `RunFilesMeta` | live: the run branch or the sandbox, from the `checkpoint` shas | run |
| pull request | `pull_request`, `pull_request_creation`, `PullRequestDetails`, `CheckRun`, `pull_request.*` | `pull_request.requested {creation_id, model, force}`, `pull_request.created {number, owner, repo, html_url, head_sha, draft}`, `pull_request.linked`, `pull_request.unlinked`, `pull_request.failed {creation_id, error}`; details and checks are live from GitHub | run |
| notifications | Slack lifecycle and interview messages | `notification.sent {route, event, channel, thread, message_id}` | run, question |
| pairing | `PairRecord`, `RunPairStatusResponse`, the transcript | `run.paired`, `pair.ended`, `pair.failed`, `pair.message` (see Agent activity) | stage, pair |
| timeline, rewind, fork | `TimelineEntryResponse {ordinal, node_name, visit, checkpoint_seq, run_commit_sha}`, `RewindResponse`, `ForkResponse`, `ForkSourceRef` | F5.1: derived from the `checkpoint` records joined to `visit.completed`; `run.superseded {new_run_id, target}` on the source | stage |
| lifecycle before and after the engine | `run.created`, `run.submitted`, `run.start_requested`, `run.pending`, `run.approved`, `run.denied`, `run.runnable`, `run.starting`, `run.removing`, `run.archived`, `run.unarchived`, `run.title.updated`, `run.parent.*`, `run.notice` | `run.created`, `run.lifecycle {kind, reason, source}`, `run.archived`, `run.unarchived`, `run.title {title}`, `run.parent {parent_id}`, `run.notice {level, code, message}` | run |
| metadata snapshots | `metadata.snapshot.*` | dropped: Fabro's meta branch is replaced by the store | run |

## Other Fabro views

| View | Reads | Source |
| --- | --- | --- |
| Events route, waterfall, debug rows, `run events` | `EventEnvelope {seq, ts, event, properties}` | F2.4's stream: `stream_seq`, the item's own identity, the Petri `RunEvent` or platform record; `ts` is `recorded_at` |
| Logs route, `run logs` | the run's text log | every `step.progress.recorded` `log {stream, line}` in `stream_seq` order, prefixed `[node#firing]` |
| Children route | the run list filtered by parent | the `runs` row `parent_id` |
| Usage route, `GET /usage` | `RunUsage {stages, totals, by_model}`, `AggregateUsage` | derived from the Stages usage rows |
| Run settings route, `GET /runs/{id}/settings` | `WorkflowSettings` | platform record `run.created` (the settings text Fabro handed Petri) |
| Graph and graph source | SVG, DOT | `graph.registered` (the blob by digest); Fabro renders |
| Terminal route | a live shell | live: the sandbox |
| `runs inspect` | `parent_id`, `status`, `spec`, `start`, `conclusion`, `checkpoints`, `sandbox` | the rows above; Petri's `inspect_run` document for the engine's view |
| `run wait` | `lifecycle.status`, `conclusion.timing`, `conclusion.usage` | the Run summary rows |
| `run diff` | `StageProjection.diff`, `start.base_sha`, `conclusion.diff.patch` | the `checkpoint` records |
| `run rewind`, `run fork` | the timeline | F5.1 |
| SSE toasts, board events | `event`, `run_id`, `stage_id`, `title`, `archived`, `start_requested` | the same stream; `stage_id` is the display label with `(execution, firing)` beside it |

## `RunProjection` fields

| Field | Fate |
| --- | --- |
| `title` | platform record `run.created`, `run.title` |
| `parent_id` | platform record `run.parent` |
| `spec` | platform record `run.created` |
| `web_url` | derived from the run id |
| `start` | `run.started` `recorded_at`; `run_branch`, `base_sha` from `run.branch` |
| `status` | the Run summary status rows |
| `approval` | platform record `run.lifecycle` |
| `archived_at` | platform record `run.archived` |
| `status_updated_at` | `recorded_at` of the record that last changed the status |
| `last_event_at` | derived |
| `pending_control` | derived (see Run summary) |
| `checkpoints` | platform record `checkpoint`; the `Checkpoint` body is derived on demand |
| `conclusion` | derived: `run.finished`, the root `invocation.finished`, the Stages rows, the last `checkpoint` |
| `sandbox` | the Sandbox rows (two gaps) |
| `pull_request`, `pull_request_creation` | platform records `pull_request.*` |
| `superseded_by` | platform record `run.superseded` |
| `retried_from` | platform record `run.created` |
| `git_identity` | platform record `git.identity` |
| `pending_interviews` | derived: open `parsed.question`s |
| `stages` | the Stages rows, keyed on `(execution, firing)` |

## `StageProjection` fields

| Field | Fate |
| --- | --- |
| `first_event_seq` | the `stream_seq` of the stage's `visit.started` |
| `prompt` | `custom attractor.prompt`, or the agent's first user message on the envelope stream |
| `response` | `custom attractor.prompt.completed`; the agent's `step.finished` `outcome.output` |
| `completion` | `visit.completed` and the final `step.finished` |
| `provider_used` | `custom attractor.fallback.plan`, envelope `SessionStarted`, `custom attractor.prompt`; the node config |
| `diff` | platform record `checkpoint {patch_blob}` |
| `script_invocation`, `script_timing` | the node config; `metrics.duration_ms` |
| `parallel_results` | `fork.completed {results}`, `custom attractor.parallel.completed` |
| `parallel_branch_id` | `BranchRef` under the `ForkOccurrence`; the label derives from it |
| `output`, `output_bytes`, `live_streaming`, `termination` | `step.finished` `outcome.output`, `metrics`; `step.progress.recorded` `log` lines while live |
| `started_at` | `visit.started` `recorded_at` |
| `handler` | `subject.node.meta.kind` |
| `graph_visit` | `subject.visit` |
| `resumed_from_stage_id` | dropped: a resume continues the same firing |
| `timing` | `visit.started` to `visit.completed`; `pebble.inference_ms`, `pebble.tool_ms` |
| `live_inference_ms`, `live_tool_ms`, `tool_batch`, `inference`, `acp_started_at` | envelope brackets (Agent activity); `step.started` for ACP |
| `usage`, `usage_by_model`, `model` | `pebble.usage`, `prompt.usage`, `pebble.subagents.sessions`, envelope `AssistantMessage` per session |
| `permission_level` | the node config |
| `agent_tools` | `custom attractor.tools` per session; `invoked` from envelope `ToolCallStarted` |
| `agent` | Pebble's fold over the stage's envelopes, unchanged |
| `state` | the Stages state rows |

## Coverage of `EVENTS.md`

Every event family, and where it appears. "Not shown" families are stored
and served on the events stream, and no view row reads them.

| Family | Rows |
| --- | --- |
| `run.started` | Run summary: identity, status running, started at |
| `graph.registered` | Run summary: goal; Sandbox: plan; Stages: node config; Other: graph source and settings |
| `invocation.declared` | Run summary (root result), Stages: nested workflow; Parallel: branch started; Sandbox: binding |
| `execution.declared`, `execution.finished` | Stages: visit and restart; edge selected and loop restart; nested workflow |
| `invocation.finished` | Run summary: status, final output; Parallel: branch completed; Stages: nested workflow |
| `invocation.cancel.requested`, `run.stalled` | Run summary: cancel reason |
| `run.paused`, `run.unpaused` | Run summary: status paused |
| `run.note.recorded` | Stages: hook decisions; Sandbox: retention (when a hook ran) |
| `run.finished` | Run summary: status, completed at, wall time |
| `execution.started` | not shown: repeats `execution.declared`'s start |
| `admission.decided` | Stages: state running, state skipped |
| `step.started` | Stages: state running, output; Agent activity: ACP |
| `step.progress.recorded` `log`, `artifact` | Stages: output, artifacts; Other: logs |
| `step.progress.recorded` `custom` (envelopes) | Agent activity, every row |
| `step.finished`, `derived.final`, `derived.exhausted` | Stages: state, attempts, timing, usage, output, context updates; Run summary: timing, usage |
| `routing.resolved` | Stages: routing directive |
| `retry.elapsed`, `retry.scheduled` | Stages: state retrying |
| `cancel.requested`, `kill.requested` | Run summary: cancel reason; Questions: interrupted; Parallel: cancelled fork |
| `control.requested`, `derived.deliverable`, `derived.answer` | Questions: answered, late, interrupted, steer, interrupt; Agent activity: pair |
| `token.emitted` | not shown: the engine's token flow; `visit.started` carries the join's inputs |
| `route.applied` | Stages: edge selected and the condition that matched; Run summary: current stage |
| `node.expanded` | Parallel: fork started (`for_each`) |
| `visit.started`, `visit.completed` | Stages: list, state, timing, retries; Run summary: current stage |
| `wait.state.changed` | Stages: state; Questions: pending; Run summary: blocked |
| `fork.started`, `branch.completed`, `fork.completed` | Parallel, every row |
| `parsed.question`, `parsed.question_expired` | Questions: pending, expired |
| `parsed.note` `result_prepared`, `transition` | Stages: notes |
| `parsed.note` `budget_paused`, `budget_resumed`, `parsed.budget` | Stages: budget pause |
| `parsed.note` `hook`, `hook.activity`, `parsed.hook_activity` | Stages: hook decisions; Agent activity: hook agents |
| `custom attractor.prompt`, `attractor.prompt.completed` | Stages: prompt, response; Parallel: fan-in prompt |
| `custom attractor.thread` | Agent activity: sessions |
| `custom attractor.tools` | Agent activity: tools available |
| `custom attractor.fallback.plan` | Agent activity: route; Stages: provider and model; Run summary: models |
| `custom attractor.mcp.unavailable` | Agent activity: MCP servers |
| `custom attractor.skills`, `attractor.skills.warning` | Agent activity: skills |
| `custom attractor.compaction` | Agent activity: compactions |
| `custom attractor.hook`, `attractor.hook.warning` | Stages: hook decisions; Agent activity: errors and warnings |
| `custom attractor.checkout` | Stages: checkout; Sandbox: where (the clone) |
| `custom attractor.parallel.branch.started`, `attractor.parallel.branch.completed`, `attractor.parallel.completed` | Parallel: branch started, branch completed, envelopes at the join |
| `custom attractor.model.unknown`, `attractor.model.fallbacks` | not shown at run time: these are load diagnostics; Fabro's create handler reports them before a run exists |

Check 1 holds: every family above has a row, or a "not shown" reason
(`execution.started`, `token.emitted`, the two load diagnostics).

## Coverage of Fabro views

Check 2 holds. Every view the survey found is in a section above: the run
list (row mapper, columns, filters, sort, row actions, bulk toolbar); the run
detail shell, header, actions, dock and tabs; the overview with its graph and
summary panel; the stage sidebar, popover, Stages route (chat, primary,
context, debug), the six renderers (conditional decision, parallel children,
fan-in results, human Q&A, wait status, stage summary), the insights sidebar
and context window; the events, logs, artifacts, files changed, children,
sandbox (filesystem, services, VNC), usage, settings, graph source and
terminal routes; the interview dock, steer bar and title editor; the SSE
toasts; the CLI `run`, `resume`, `attach`, `events`, `wait`, `output`,
`diff`, `rewind`, `fork`, `ask`, `steer`, `logs`, `ssh`, `cp`, `runs ps`,
`runs inspect` and the progress renderer (stage, setup and info displays);
the API's run, stage, events, usage, questions, sessions, pair, pull request,
sandbox, checkpoint, timeline, rewind and fork schemas; and the scheduler's
`list_by_statuses`.

Two Fabro event groups have no view and no Petri source, and are dropped with
the old executor: `metadata.snapshot.*` (the meta branch) and
`cli.ensure.*` (the image carries the CLI). `run.session.*` (Ask Fabro) is not
a view of a run's execution and stays on Fabro's session records
(decision 3).

## Gaps

The smallest source for each fact with no Petri event and no named platform
record. A Petri record is proposed where Petri holds the fact; a platform
record where Fabro does.

| Fact | Views | Smallest source |
| --- | --- | --- |
| who answered | `interview.completed` `actor`, Slack attribution | platform record `interview.answered {question, principal, channel}` written by Fabro's interviewer beside its `InterviewReply` |
| run branch and base sha | `StartRecord`, `run diff`, the commits picker | platform record `run.branch {run_branch, base_sha}` written when Fabro creates the run branch, at that checkpoint's stage position |
| Git identity | `git_identity` | platform record `git.identity {name, email, source}` |
| diff summary and patch per checkpoint | `Run.diff`, `Conclusion.diff`, `StageProjection.diff`, the changes sort | `diff_summary` and `patch_blob` on the `checkpoint` platform record |
| lifecycle before the engine, archive, title, parent, supersede, notices | the run list, header, `runs ps`, `run events --pretty` | platform records `run.created`, `run.lifecycle`, `run.archived`, `run.unarchived`, `run.title`, `run.parent`, `run.superseded`, `run.notice` |
| pull request request, link, unlink, failure | the PR card, `pull_request.*` CLI lines, the creation supervisor's recovery query | platform records `pull_request.requested`, `pull_request.linked`, `pull_request.unlinked`, `pull_request.failed` beside the plan's `pull_request.created` |
| pair lifecycle and messages | the pair endpoints and transcript | platform records `pair.ended`, `pair.failed`, `pair.message` beside the plan's `run.paired` |
