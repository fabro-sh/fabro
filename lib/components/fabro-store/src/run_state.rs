use std::collections::{BTreeMap, HashMap};
use std::str::FromStr;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use fabro_types::run_event::{
    AgentLlmStartedProps, CheckpointCompletedProps, RunCompletedProps, RunFailedProps,
    StageCompletedProps, TodoCreatedProps, TodoDeletedProps, TodoUpdatedProps,
};
use fabro_types::settings::run::{EnvironmentProvider, RunEnvironmentSettings};
use fabro_types::{
    ActivatedSkill, AgentControlState, AskFabro, BilledModelUsage, BilledTokenCounts, Checkpoint,
    CheckpointRecord, CommandTermination, Conclusion, EventBody, FailureCategory, FailureSignature,
    InterviewQuestionRecord, McpServerProjection, McpServerStatus, Outcome, PendingInterviewRecord,
    PendingReason, PullRequestCreation, PullRequestCreationStatus, PullRequestLink, RepositoryRef,
    Run, RunApproval, RunApprovalState, RunBillingSummary, RunControlAction, RunDiff, RunEvent,
    RunId, RunLifecycle, RunLinks, RunModel, RunOrigin, RunProjection, RunSandbox,
    RunSandboxFailure, RunSandboxInstance, RunSandboxPlan, RunSandboxRuntime, RunSize, RunSpec,
    RunStatus, RunTimestamps, SandboxProviderKind, StageCompletion, StageHandler, StageId,
    StageInferenceProjection, StageModelUsage, StageOutcome, StageProjection, StageState,
    StartRecord, SubAgentProjection, SubAgentStatus, TodoListKind, TodoListProjection,
    TodoProjection, WorkflowRef, first_event_seq, timing,
};
use fabro_util::error::render_compact_with_causes;

use crate::{Error, EventEnvelope, Result};

#[derive(Debug, Clone, Default)]
pub(crate) struct EventProjectionCache {
    pub last_seq: u32,
    // Arc-shared with the shared projection cache so opening a run does not
    // deep-copy the projection; mutated copy-on-write via `Arc::make_mut`.
    pub state:    Option<Arc<RunProjection>>,
}

pub trait RunProjectionReducer {
    fn apply_events(events: &[EventEnvelope]) -> Result<Self>
    where
        Self: Sized;

    fn apply_event(&mut self, event: &EventEnvelope) -> Result<()>;
}

impl RunProjectionReducer for RunProjection {
    fn apply_events(events: &[EventEnvelope]) -> Result<Self> {
        let Some((first, rest)) = events.split_first() else {
            return Err(Error::InvalidEvent(
                "run projection requires a run.created event".to_string(),
            ));
        };
        let mut state = projection_from_created(first)?;
        for event in rest {
            state.apply_event(event)?;
        }
        Ok(state)
    }

    fn apply_event(&mut self, event: &EventEnvelope) -> Result<()> {
        let stored = &event.event;
        let ts = stored.ts;

        self.last_event_at = ts;

        match &stored.body {
            EventBody::RunCreated(_) => {
                return Err(Error::InvalidEvent(
                    "run.created cannot be applied to an initialized projection".to_string(),
                ));
            }
            EventBody::RunStarted(props) => {
                self.start = Some(StartRecord {
                    start_time: ts,
                    run_branch: props.run_branch.clone(),
                    base_sha:   props.base_sha.clone(),
                });
            }
            EventBody::RunSubmitted(props) => {
                self.spec.definition_blob = props.definition_blob;
            }
            EventBody::RunStartRequested(props) if props.resume => {
                self.try_apply_status(RunStatus::Submitted, ts)?;
                self.conclusion = None;
            }
            EventBody::RunPending(props) => {
                self.try_apply_status(
                    RunStatus::Pending {
                        reason: props.reason,
                    },
                    ts,
                )?;
                if props.reason == PendingReason::ApprovalRequired {
                    self.approval = Some(RunApproval {
                        state:         RunApprovalState::Pending,
                        requested_at:  ts,
                        decided_at:    None,
                        denial_reason: None,
                    });
                }
            }
            EventBody::RunApproved(_) => {
                if let Some(approval) = &mut self.approval {
                    approval.state = RunApprovalState::Approved;
                    approval.decided_at = Some(ts);
                    approval.denial_reason = None;
                }
            }
            EventBody::RunDenied(props) => {
                if let Some(approval) = &mut self.approval {
                    approval.state = RunApprovalState::Denied;
                    approval.decided_at = Some(ts);
                    approval.denial_reason.clone_from(&props.reason);
                }
            }
            EventBody::RunRunnable(_) => {
                self.try_apply_status(RunStatus::Runnable, ts)?;
            }
            EventBody::RunStarting(_) => {
                self.try_apply_status(RunStatus::Starting, ts)?;
            }
            EventBody::RunRunning(_) => {
                self.try_apply_status(RunStatus::Running, ts)?;
            }
            EventBody::RunBlocked(props) => {
                let next = if matches!(self.status, RunStatus::Paused { .. }) {
                    RunStatus::Paused {
                        prior_block: Some(props.blocked_reason),
                    }
                } else {
                    RunStatus::Blocked {
                        blocked_reason: props.blocked_reason,
                    }
                };
                self.try_apply_status(next, ts)?;
            }
            EventBody::RunUnblocked(_) => {
                let next = match self.status {
                    RunStatus::Paused {
                        prior_block: Some(_),
                    } => RunStatus::Paused { prior_block: None },
                    RunStatus::Paused { prior_block: None } => {
                        RunStatus::Paused { prior_block: None }
                    }
                    _ => RunStatus::Running,
                };
                self.try_apply_status(next, ts)?;
            }
            EventBody::RunRemoving(_) => {
                self.try_apply_status(RunStatus::Removing, ts)?;
            }
            EventBody::RunCancelRequested(_) => {
                self.pending_control = Some(RunControlAction::Cancel);
            }
            EventBody::RunPauseRequested(_) => {
                self.pending_control = Some(RunControlAction::Pause);
            }
            EventBody::RunUnpauseRequested(_) => {
                self.pending_control = Some(RunControlAction::Unpause);
            }
            EventBody::RunPaused(_) => {
                self.try_apply_status(
                    RunStatus::Paused {
                        prior_block: self.status.blocked_reason(),
                    },
                    ts,
                )?;
                self.pending_control = None;
            }
            EventBody::RunUnpaused(_) => {
                let next = match self.status {
                    RunStatus::Paused {
                        prior_block: Some(blocked_reason),
                    } => RunStatus::Blocked { blocked_reason },
                    _ => RunStatus::Running,
                };
                self.try_apply_status(next, ts)?;
                self.pending_control = None;
            }
            EventBody::RunCompleted(props) => {
                self.try_apply_status(
                    RunStatus::Succeeded {
                        reason: props.reason,
                    },
                    ts,
                )?;
                self.pending_control = None;
                self.conclusion = Some(conclusion_from_completed(props, ts)?);
                self.pending_interviews.clear();
            }
            EventBody::RunFailed(props) => {
                self.try_apply_status(
                    RunStatus::Failed {
                        reason: props.failure.reason,
                    },
                    ts,
                )?;
                self.pending_control = None;
                self.conclusion = Some(conclusion_from_failed(props, ts));
                self.pending_interviews.clear();
                finalize_unfinished_stages_after_run_failed(self, props, ts);
            }
            EventBody::RunSupersededBy(props) => {
                self.superseded_by = Some(props.new_run_id);
            }
            EventBody::RunParentLinked(props) => {
                self.parent_id = Some(props.parent_id);
            }
            EventBody::RunParentUnlinked(_props) => {
                self.parent_id = None;
            }
            EventBody::RunArchived(_props) => {
                if self.archived_at.is_some() {
                    return Ok(());
                }
                if !self.status.is_terminal() {
                    return Err(fabro_types::InvalidTransition {
                        from: self.status,
                        to:   self.status,
                    }
                    .into());
                }
                self.archived_at = Some(ts);
            }
            EventBody::RunUnarchived(_props) => {
                self.archived_at = None;
            }
            EventBody::RunTitleUpdated(props) => {
                self.title.clone_from(&props.title);
            }
            EventBody::CheckpointCompleted(props) => {
                let checkpoint = checkpoint_from_props(props, ts);
                if let Some(stage_id) = stored.stage_id.as_ref() {
                    // Envelope-first: the diff and any skipped-stage synthesis
                    // attach to the exact execution recorded on the event.
                    // Historical `node_outcomes` must not create or collide
                    // with a newer execution ordinal.
                    apply_checkpoint_to_stage(self, stage_id, props, &checkpoint, event.seq, ts);
                } else {
                    // Legacy fallback for events without a stored stage id:
                    // resolve the visit from the checkpointed `node_visits`
                    // and synthesize skipped stages from historical outcomes.
                    if let Some(node_id) = stored.node_id.as_deref() {
                        let visit = checkpoint
                            .node_visits
                            .get(node_id)
                            .and_then(|visit| u32::try_from(*visit).ok())
                            .unwrap_or(1);
                        if let Some(diff) = props.diff.clone() {
                            self.stage_entry(node_id, visit, first_event_seq(event.seq))
                                .diff = Some(diff);
                        }
                    }
                    for (node_id, outcome) in &checkpoint.node_outcomes {
                        if outcome.status != StageOutcome::Skipped {
                            continue;
                        }
                        let visit = checkpoint
                            .node_visits
                            .get(node_id)
                            .and_then(|visit| u32::try_from(*visit).ok())
                            .unwrap_or(1);
                        if self
                            .stage(&fabro_types::StageId::new(node_id, visit))
                            .is_some()
                        {
                            continue;
                        }
                        let stage = self.stage_entry(node_id, visit, first_event_seq(event.seq));
                        stage.completion = Some(stage_completion_from_outcome(outcome, ts));
                        stage.state = StageState::Skipped;
                    }
                }
                self.checkpoints.push(CheckpointRecord {
                    seq: event.seq,
                    checkpoint,
                    diff: diff_from_checkpoint_props(props),
                });
            }
            EventBody::SandboxInitializing(_) => {
                let plan = sandbox_plan_from_projection_or_settings(self);
                self.sandbox = Some(RunSandbox::initializing(plan));
            }
            EventBody::SandboxFailed(props) => {
                let plan = sandbox_plan_from_projection_or_settings(self);
                self.sandbox = Some(RunSandbox::failed(plan, RunSandboxFailure {
                    provider:    props.provider.clone(),
                    error:       props.error.clone(),
                    causes:      props.causes.clone(),
                    duration_ms: props.duration_ms,
                }));
            }
            EventBody::SandboxInitialized(props) => {
                let plan = sandbox_plan_from_projection_or_settings(self);
                self.sandbox = Some(RunSandbox::ready(plan, RunSandboxInstance {
                    provider: props.provider,
                    image:    props.image.clone(),
                    snapshot: props.snapshot.clone(),
                    runtime:  RunSandboxRuntime {
                        id:                props.id.clone(),
                        working_directory: props.working_directory.clone(),
                        repo_cloned:       props.repo_cloned,
                        clone_origin_url:  props.clone_origin_url.clone(),
                        clone_branch:      props.clone_branch.clone(),
                        workspace_root:    props.workspace_root.clone(),
                        repos_root:        props.repos_root.clone(),
                        primary_repo_path: props.primary_repo_path.clone(),
                        primary_repo_link: props.primary_repo_link.clone(),
                    },
                }));
            }
            EventBody::PullRequestCreationRequested(props) => {
                self.pull_request_creation = Some(PullRequestCreation {
                    id:           props.creation_id,
                    status:       PullRequestCreationStatus::Pending,
                    model:        props.model.clone(),
                    force:        props.force,
                    requested_at: ts,
                    updated_at:   ts,
                    pull_request: None,
                    error:        None,
                });
            }
            EventBody::PullRequestCreated(props) => {
                let pull_request = PullRequestLink {
                    owner:  props.owner.clone(),
                    repo:   props.repo.clone(),
                    number: props.pr_number,
                };
                self.pull_request = Some(pull_request.clone());
                if let Some(creation) = self
                    .pull_request_creation
                    .as_mut()
                    .filter(|creation| creation.is_pending())
                {
                    creation.succeed(pull_request, ts);
                }
            }
            EventBody::PullRequestLinked(props) => {
                self.pull_request = Some(props.pull_request.clone());
                if let Some(creation) = self
                    .pull_request_creation
                    .as_mut()
                    .filter(|creation| creation.is_pending())
                {
                    creation.succeed(props.pull_request.clone(), ts);
                }
            }
            EventBody::PullRequestUnlinked(_) => {
                self.pull_request = None;
                // Clear the creation record too: a lingering `Succeeded`
                // record would point at a pull request that is no longer
                // linked, and it would block a later explicit creation.
                self.pull_request_creation = None;
            }
            EventBody::PullRequestFailed(props) => {
                // Only a failure that names the pending creation resolves it;
                // publish-stage failures carry no creation id and must not
                // fail an unrelated explicit creation.
                if let Some(creation) = self.pull_request_creation.as_mut().filter(|creation| {
                    Some(creation.id) == props.creation_id && creation.is_pending()
                }) {
                    creation.fail(props.error.clone(), ts);
                }
            }
            EventBody::InterviewStarted(props) => {
                if props.question_id.is_empty() {
                    return Ok(());
                }
                self.pending_interviews
                    .insert(props.question_id.clone(), PendingInterviewRecord {
                        question:   InterviewQuestionRecord {
                            id:              props.question_id.clone(),
                            text:            props.question.clone(),
                            stage:           props.stage.clone(),
                            question_type:   props.question_type.parse().unwrap_or_default(),
                            options:         props.options.clone(),
                            allow_freeform:  props.allow_freeform,
                            timeout_seconds: props.timeout_seconds,
                            context_display: props.context_display.clone(),
                            review_target:   props.review_target.clone(),
                        },
                        started_at: ts,
                    });
            }
            EventBody::InterviewCompleted(props) if !props.question_id.is_empty() => {
                self.pending_interviews.remove(&props.question_id);
            }
            EventBody::InterviewTimeout(props) if !props.question_id.is_empty() => {
                self.pending_interviews.remove(&props.question_id);
            }
            EventBody::InterviewInterrupted(props) if !props.question_id.is_empty() => {
                self.pending_interviews.remove(&props.question_id);
            }
            EventBody::StageStarted(props) => {
                let Some(stage_id) = stored.stage_id.as_ref() else {
                    return Ok(());
                };
                // A `stage.started` for a new `StageId` creates a new
                // projection, so an older execution's terminal projection
                // stays immutable. `begin_attempt` on an existing entry
                // remains the compatibility path for automatic retries and
                // legacy histories that repeat one `StageId`.
                let is_new = self.stage(stage_id).is_none();
                let stage = stage_at_stored_stage_id(self, stage_id, event.seq);
                stage.begin_attempt(
                    ts,
                    StageHandler::from_handler_type(Some(&props.handler_type)),
                );
                if is_new {
                    stage.graph_visit = props.graph_visit;
                    stage
                        .resumed_from_stage_id
                        .clone_from(&props.resumed_from_stage_id);
                }
            }
            EventBody::StageRetrying(_) => {
                let Some(stage) = stage_at_stored_or_current_visit(self, stored, event.seq) else {
                    return Ok(());
                };
                stage.state = StageState::Retrying;
                stage.agent_control = AgentControlState::Running;
            }
            EventBody::StagePrompt(props) => {
                let Some(stage) = stage_at_stored_or_visit(self, stored, props.visit, event.seq)
                else {
                    return Ok(());
                };
                stage.prompt = Some(props.text.clone());
                stage.provider_used = StageModelUsage::from_prompt_props(props);
            }
            EventBody::PromptCompleted(props) => {
                let Some(stage) = stage_at_stored_or_current_visit(self, stored, event.seq) else {
                    return Ok(());
                };
                stage.response = Some(props.response.clone());
                if let Some(billing) = &props.billing {
                    stage.usage.replace_with_billed_usage(billing);
                    stage.model = Some(billing.model().clone());
                }
            }
            EventBody::StageCompleted(props) => {
                let response = props.response.clone();
                let outcome = stage_outcome_from_props(props);
                let completion = stage_completion_from_outcome(&outcome, ts);
                let Some(stage) =
                    stage_at_completed_visit(self, stored, props.node_visits.as_ref(), event.seq)
                else {
                    return Ok(());
                };
                stage.response = response;
                stage.completion = Some(completion);
                stage.set_authoritative_timing(props.timing);
                if let Some(billing) = &props.billing {
                    stage.usage.replace_with_billed_usage(billing);
                    stage.model = Some(billing.model().clone());
                }
                stage.state = StageState::from(outcome.status);
                stage.agent_control = AgentControlState::Running;
            }
            EventBody::StageFailed(props) => {
                let failure_reason = props.failure.as_ref().map(|detail| detail.message.clone());
                let failure_category = props.failure.as_ref().map(|detail| detail.category);
                let Some(stage) = stage_at_stored_or_current_visit(self, stored, event.seq) else {
                    return Ok(());
                };
                let outcome = StageOutcome::Failed {
                    retry_requested: props.will_retry,
                };
                stage.completion = Some(StageCompletion {
                    outcome,
                    notes: None,
                    failure_reason,
                    timestamp: ts,
                });
                stage.set_authoritative_timing(props.timing);
                if let Some(billing) = &props.billing {
                    stage.usage.replace_with_billed_usage(billing);
                    stage.model = Some(billing.model().clone());
                }
                stage.state =
                    stage_state_from_failure(props.will_retry, failure_category, stage.termination);
                stage.agent_control = AgentControlState::Running;
            }
            EventBody::AgentMessage(props) => {
                let Some(stage) = stage_at_stored_or_visit(self, stored, props.visit, event.seq)
                else {
                    return Ok(());
                };
                stage.usage.add_counts(&props.billing);
                stage.model = Some(props.model.clone());
                if let Some(context_window) = &props.context_window {
                    let mut context_window = context_window.clone();
                    context_window.event_seq = Some(event.seq);
                    stage.context_window = Some(context_window);
                }
                close_inference_bracket(self, stored, props.visit, event.seq, ts);
            }
            EventBody::AgentLlmStarted(props) => {
                open_inference_bracket(self, stored, props, event.seq, ts);
            }
            EventBody::AgentLlmFirstOutput(props) => {
                let Some(inference) =
                    matching_inference_bracket(self, stored, props.visit, event.seq)
                else {
                    return Ok(());
                };
                inference.first_output_at = Some(ts);
                inference.first_output_kind = Some(props.kind);
            }
            EventBody::AgentLlmRetry(props) => {
                let Some(inference) =
                    matching_inference_bracket(self, stored, props.visit, event.seq)
                else {
                    return Ok(());
                };
                inference.retries = inference.retries.saturating_add(1);
                // A retry discards whatever the failed attempt produced.
                // Replay is driven purely by events, so resetting the
                // in-process latch is not enough: without this the projection
                // keeps asserting output the agent already threw away.
                inference.first_output_at = None;
                inference.first_output_kind = None;
            }
            EventBody::AgentError(props) => {
                close_inference_bracket(self, stored, props.visit, event.seq, ts);
            }
            EventBody::AgentSessionEnded(_) => {
                close_active_brackets_for_session(self, stored, ts);
            }
            EventBody::AgentSessionActivated(props) => {
                let Some(stage) = stage_at_stored_or_visit(self, stored, props.visit, event.seq)
                else {
                    return Ok(());
                };
                stage.provider_used = Some(StageModelUsage::from_agent_session_activated(props));
                stage.permission_level = props.permission_level;
            }
            EventBody::AgentRoundInterrupted(props) => {
                let Some(stage) = stage_at_stored_or_visit(self, stored, props.visit, event.seq)
                else {
                    return Ok(());
                };
                stage.agent_control = AgentControlState::WaitingForSteer;
                close_inference_bracket(self, stored, props.visit, event.seq, ts);
            }
            EventBody::AgentSteeringInjected(props) => {
                let Some(stage) = stage_at_stored_or_visit(self, stored, props.visit, event.seq)
                else {
                    return Ok(());
                };
                stage.agent_control = AgentControlState::Running;
            }
            EventBody::AgentSessionDeactivated(props) => {
                let Some(stage) = stage_at_stored_or_visit(self, stored, props.visit, event.seq)
                else {
                    return Ok(());
                };
                stage.agent_control = AgentControlState::Running;
            }
            EventBody::AgentToolsAvailable(props) => {
                let Some(stage) = stage_at_stored_or_visit(self, stored, props.visit, event.seq)
                else {
                    return Ok(());
                };
                stage.agent_tools.clone_from(&props.tools);
            }
            EventBody::AgentAcpStarted(props) => {
                let Some(stage) = stage_at_stored_or_visit(self, stored, props.visit, event.seq)
                else {
                    return Ok(());
                };
                stage.open_acp_inference(ts);
                // `provider_used` is intentionally sourced from the subsequent
                // `AgentSessionActivated` event, which carries the canonical
                // provider/model. ACP runs without a steering hub never emit
                // activation and legitimately leave it unset.
            }
            EventBody::CommandStarted(props) => {
                let script_invocation = serde_json::to_value(props).map_err(|err| {
                    Error::InvalidEvent(format!("invalid command.started payload: {err}"))
                })?;
                let Some(stage) = stage_at_stored_or_current_visit(self, stored, event.seq) else {
                    return Ok(());
                };
                stage.script_invocation = Some(script_invocation);
            }
            EventBody::CommandCompleted(props) => {
                let script_timing = serde_json::to_value(props).map_err(|err| {
                    Error::InvalidEvent(format!("invalid command.completed payload: {err}"))
                })?;
                let Some(stage) = stage_at_stored_or_current_visit(self, stored, event.seq) else {
                    return Ok(());
                };
                stage.output = Some(props.output.clone());
                stage.output_bytes = Some(props.output_bytes);
                stage.live_streaming = Some(props.live_streaming);
                stage.termination = Some(props.termination);
                stage.script_timing = Some(script_timing);
            }
            EventBody::AgentAcpCompleted(props) => {
                let Some(stage) = stage_at_current_visit(self, stored, event.seq) else {
                    return Ok(());
                };
                stage.close_acp_inference(props.duration_ms);
                apply_agent_terminal(
                    "agent.acp",
                    stage,
                    props,
                    merge_agent_process_output(&props.stdout, &props.stderr),
                    CommandTermination::Exited,
                )?;
            }
            EventBody::AgentAcpCancelled(props) => {
                let Some(stage) = stage_at_current_visit(self, stored, event.seq) else {
                    return Ok(());
                };
                stage.close_acp_inference(props.duration_ms);
                apply_agent_terminal(
                    "agent.acp",
                    stage,
                    props,
                    merge_agent_process_output(&props.stdout, &props.stderr),
                    CommandTermination::Cancelled,
                )?;
            }
            EventBody::AgentAcpTimedOut(props) => {
                let Some(stage) = stage_at_current_visit(self, stored, event.seq) else {
                    return Ok(());
                };
                stage.close_acp_inference(props.duration_ms);
                apply_agent_terminal(
                    "agent.acp",
                    stage,
                    props,
                    merge_agent_process_output(&props.stdout, &props.stderr),
                    CommandTermination::TimedOut,
                )?;
            }
            EventBody::ParallelCompleted(props) => {
                let Some(stage) = stage_at_stored_or_current_visit(self, stored, event.seq) else {
                    return Ok(());
                };
                stage.parallel_results = Some(props.results.clone());
            }
            EventBody::ParallelBranchStarted(props) => {
                // Branches bypass the engine's StageStarted/StageCompleted
                // lifecycle. Seed started_at so the branch stage drives a live
                // wall-clock timer while it runs (the entry is created Running).
                let handler = stored
                    .node_id
                    .as_deref()
                    .and_then(|node_id| self.spec().graph.nodes.get(node_id))
                    .map(|node| StageHandler::from_handler_type(node.handler_type()));
                let is_new = stored
                    .stage_id
                    .as_ref()
                    .is_none_or(|stage_id| self.stage(stage_id).is_none());
                let Some(stage) = stage_at_stored_or_current_visit(self, stored, event.seq) else {
                    return Ok(());
                };
                if stage.started_at.is_none() {
                    stage.started_at = Some(ts);
                }
                if stage.handler.is_none() {
                    stage.handler = handler;
                }
                if is_new {
                    stage.graph_visit = props.graph_visit;
                    stage
                        .resumed_from_stage_id
                        .clone_from(&props.resumed_from_stage_id);
                    stage
                        .parallel_branch_id
                        .clone_from(&stored.parallel_branch_id);
                }
                stage.state = StageState::Running;
            }
            EventBody::ParallelBranchCompleted(props) => {
                // A branch never emits its own StageCompleted, so finalize it
                // here; otherwise the stage spins Running forever after the run
                // (and the fan-in) is done.
                let Some(stage) = stage_at_stored_or_current_visit(self, stored, event.seq) else {
                    return Ok(());
                };
                stage.completion = Some(StageCompletion {
                    outcome:        props.status,
                    notes:          None,
                    failure_reason: None,
                    timestamp:      ts,
                });
                stage.timing = Some(fabro_types::StageTiming::wall_only(props.duration_ms));
                stage.state = StageState::from(props.status);
                stage.agent_control = AgentControlState::Running;
            }
            EventBody::TodoCreated(props) => {
                if !should_project_root_agent_todo_event(stored, props.list_kind) {
                    return Ok(());
                }
                let Some(stage) = stage_at_stored_or_current_visit(self, stored, event.seq) else {
                    return Ok(());
                };
                apply_todo_created(stage, props);
            }
            EventBody::TodoUpdated(props) => {
                if !should_project_root_agent_todo_event(stored, props.list_kind) {
                    return Ok(());
                }
                let Some(stage) = stage_at_stored_or_current_visit(self, stored, event.seq) else {
                    return Ok(());
                };
                apply_todo_updated(stage, props);
            }
            EventBody::TodoDeleted(props) => {
                if !should_project_root_agent_todo_event(stored, props.list_kind) {
                    return Ok(());
                }
                let Some(stage) = stage_at_stored_or_current_visit(self, stored, event.seq) else {
                    return Ok(());
                };
                apply_todo_deleted(stage, props);
            }
            EventBody::AgentSubSpawned(props) => {
                let Some(stage) = stage_at_stored_or_visit(self, stored, props.visit, event.seq)
                else {
                    return Ok(());
                };
                stage.subagents.push(SubAgentProjection {
                    agent_id: props.agent_id.clone(),
                    depth:    props.depth,
                    task:     props.task.clone(),
                    status:   SubAgentStatus::Running,
                });
            }
            // A reused subagent stays one projected row: the spawn task and
            // generation 1 identify it, and every later generation only moves
            // its status. The per-turn task and generation stay in the event
            // log for consumers that need each turn.
            EventBody::AgentSubTurnStarted(props) => {
                set_subagent_status(
                    self,
                    stored,
                    props.visit,
                    event.seq,
                    &props.agent_id,
                    SubAgentStatus::Running,
                );
            }
            EventBody::AgentSubCompleted(props) => {
                set_subagent_status(
                    self,
                    stored,
                    props.visit,
                    event.seq,
                    &props.agent_id,
                    SubAgentStatus::Completed {
                        success:    props.success,
                        turns_used: props.turns_used,
                    },
                );
            }
            EventBody::AgentSubFailed(props) => {
                set_subagent_status(
                    self,
                    stored,
                    props.visit,
                    event.seq,
                    &props.agent_id,
                    SubAgentStatus::Failed {
                        error: props.error.clone(),
                    },
                );
            }
            EventBody::AgentSubClosed(props) => {
                set_subagent_status(
                    self,
                    stored,
                    props.visit,
                    event.seq,
                    &props.agent_id,
                    SubAgentStatus::Closed,
                );
            }
            EventBody::AgentSkillsDiscovered(props) => {
                let Some(stage) = stage_at_stored_or_visit(self, stored, props.visit, event.seq)
                else {
                    return Ok(());
                };
                stage.skills.available.clone_from(&props.skills);
            }
            EventBody::AgentSkillActivated(props) => {
                let Some(stage) = stage_at_stored_or_visit(self, stored, props.visit, event.seq)
                else {
                    return Ok(());
                };
                stage.skills.activated.push(ActivatedSkill {
                    name:   props.skill_name.clone(),
                    source: props.source,
                });
            }
            EventBody::AgentMcpReady(props) => {
                let Some(stage) = stage_at_stored_or_visit(self, stored, props.visit, event.seq)
                else {
                    return Ok(());
                };
                upsert_mcp_server(stage, McpServerProjection {
                    server_name: props.server_name.clone(),
                    tool_count:  props.tool_count,
                    status:      McpServerStatus::Ready {
                        tools: props.tools.clone(),
                    },
                    invoked:     false,
                });
            }
            EventBody::AgentMcpFailed(props) => {
                let Some(stage) = stage_at_stored_or_visit(self, stored, props.visit, event.seq)
                else {
                    return Ok(());
                };
                upsert_mcp_server(stage, McpServerProjection {
                    server_name: props.server_name.clone(),
                    tool_count:  0,
                    status:      McpServerStatus::Failed {
                        error: props.error.clone(),
                    },
                    invoked:     false,
                });
            }
            EventBody::AgentToolStarted(props) => {
                let root_session_id = if stored.parent_session_id.is_none() {
                    stored.session_id.clone()
                } else {
                    None
                };
                let Some(stage) = stage_at_stored_or_visit(self, stored, props.visit, event.seq)
                else {
                    return Ok(());
                };
                if let Some(tool) = stage
                    .agent_tools
                    .iter_mut()
                    .find(|tool| tool.name == props.tool_name)
                {
                    tool.invoked = true;
                }
                if let Some(server) = mcp_server_from_tool_name(&props.tool_name) {
                    if let Some(projection) = stage
                        .mcp_servers
                        .iter_mut()
                        .find(|p| mcp_name_eq(&p.server_name, server))
                    {
                        projection.invoked = true;
                    }
                }
                // A subagent's tools run inside the root session's tool call,
                // so the root batch already covers them. Timing them again
                // would double-count that span.
                if let Some(session_id) = root_session_id {
                    stage.open_tool_call(session_id, props.tool_call_id.clone(), ts);
                }
            }
            EventBody::AgentToolCompleted(props) => {
                if stored.parent_session_id.is_some() {
                    return Ok(());
                }
                let Some(session_id) = stored.session_id.as_deref() else {
                    return Ok(());
                };
                let Some(stage) = stage_at_stored_or_visit(self, stored, props.visit, event.seq)
                else {
                    return Ok(());
                };
                stage.close_tool_call(session_id, &props.tool_call_id, ts);
            }
            _ => {}
        }

        Ok(())
    }
}

/// Decide whether a TODO event should mutate
/// `StageProjection.root_agent_todos`.
///
/// OpenAI plan lists are scoped per agent session (`openai_plan:<session_id>`),
/// so a child/subagent session emits its own list events on the same stage.
/// The root-agent projection excludes those child plans, while the underlying
/// events remain in the run event log. Kimi todo lists
/// (`kimi_todos:<session_id>`) are scoped the same way. Anthropic task lists
/// are root-scoped (`anthropic_tasks:<root_session_id>`) and intentionally
/// shared with subagents, so they always project.
fn should_project_root_agent_todo_event(stored: &RunEvent, list_kind: TodoListKind) -> bool {
    match list_kind {
        TodoListKind::OpenAiPlan | TodoListKind::KimiTodos => stored.parent_session_id.is_none(),
        TodoListKind::AnthropicTasks => true,
    }
}

fn apply_todo_created(stage: &mut StageProjection, props: &TodoCreatedProps) {
    if stage
        .root_agent_todos
        .as_ref()
        .is_none_or(|list| list.list_id != props.list_id || list.kind != props.list_kind)
    {
        stage.root_agent_todos = Some(TodoListProjection::new(
            props.list_kind,
            props.list_id.clone(),
        ));
    }
    let list = stage
        .root_agent_todos
        .as_mut()
        .expect("todo list was just inserted");
    list.upsert(TodoProjection {
        id:          props.todo_id.clone(),
        status:      props.status,
        order:       props.order,
        subject:     props.subject.clone(),
        description: props.description.clone(),
        active_form: props.active_form.clone(),
        owner:       props.owner.clone(),
        blocks:      props.blocks.clone(),
        blocked_by:  props.blocked_by.clone(),
        metadata:    props.metadata.clone(),
    });
}

fn apply_todo_updated(stage: &mut StageProjection, props: &TodoUpdatedProps) {
    if let Some(list) = stage
        .root_agent_todos
        .as_mut()
        .filter(|list| list.list_id == props.list_id)
    {
        list.apply_patch(&props.todo_id, &fabro_types::TodoPatch::from_props(props));
    }
}

fn apply_todo_deleted(stage: &mut StageProjection, props: &TodoDeletedProps) {
    let Some(list) = stage
        .root_agent_todos
        .as_mut()
        .filter(|list| list.list_id == props.list_id)
    else {
        return;
    };
    list.remove(&props.todo_id);
    if list.items.is_empty() {
        stage.root_agent_todos = None;
    }
}

/// Move an already-projected subagent to a new lifecycle status. Every
/// subagent event after the spawn updates the same row, so reuse shows one
/// agent returning to running rather than a second agent appearing.
fn set_subagent_status(
    state: &mut RunProjection,
    stored: &RunEvent,
    visit: u32,
    seq: u32,
    agent_id: &str,
    status: SubAgentStatus,
) {
    let Some(stage) = stage_at_stored_or_visit(state, stored, visit, seq) else {
        return;
    };
    if let Some(subagent) = subagent_mut(stage, agent_id) {
        subagent.status = status;
    }
}

fn subagent_mut<'a>(
    stage: &'a mut StageProjection,
    agent_id: &str,
) -> Option<&'a mut SubAgentProjection> {
    stage
        .subagents
        .iter_mut()
        .find(|subagent| subagent.agent_id == agent_id)
}

fn upsert_mcp_server(stage: &mut StageProjection, mut server: McpServerProjection) {
    if let Some(existing) = stage
        .mcp_servers
        .iter_mut()
        .find(|existing| existing.server_name == server.server_name)
    {
        // Status/tool-count may flip (Ready → Failed across reconnects); keep
        // the sticky `invoked` flag so a server still reads as "used" after
        // its ready/failed state changes.
        server.invoked = server.invoked || existing.invoked;
        *existing = server;
    } else {
        stage.mcp_servers.push(server);
    }
}

/// Extract the `<server>` segment from an `mcp__<server>__<tool>` qualified
/// tool name. Returns `None` for non-MCP tools or malformed names.
fn mcp_server_from_tool_name(tool_name: &str) -> Option<&str> {
    let rest = tool_name.strip_prefix("mcp__")?;
    let idx = rest.find("__")?;
    let server = &rest[..idx];
    (!server.is_empty()).then_some(server)
}

/// Match an MCP server projection name against a server segment parsed from a
/// qualified tool name. Tool names use `fabro_mcp::qualified_tool_name`, which
/// sanitizes non-alphanumeric characters in the server name; normalize the
/// stored projection name the same way before comparing.
fn mcp_name_eq(projection_name: &str, parsed_from_tool: &str) -> bool {
    fn normalize(s: &str) -> String {
        s.chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    }
    normalize(projection_name) == parsed_from_tool
}

fn projection_from_created(event: &EventEnvelope) -> Result<RunProjection> {
    let stored = &event.event;
    let EventBody::RunCreated(props) = &stored.body else {
        return Err(Error::InvalidEvent(format!(
            "run projection must start with run.created, got {}",
            stored.body.event_name()
        )));
    };

    let labels = props.labels.clone().into_iter().collect::<HashMap<_, _>>();
    let title = props
        .title
        .clone()
        .unwrap_or_else(|| fabro_types::infer_run_title(props.graph.goal()));
    let spec = RunSpec {
        run_id: stored.run_id,
        settings: props.settings.clone(),
        graph: props.graph.clone(),
        graph_source: props.workflow_source.clone(),
        workflow_slug: props.workflow_slug.clone(),
        automation: props.automation.clone(),
        source_directory: props.source_directory.clone(),
        labels,
        provenance: props.provenance.clone(),
        manifest_blob: props.manifest_blob,
        definition_blob: None,
        git: props.git.clone(),
        fork_source_ref: props.fork_source_ref.clone(),
    };

    let mut projection = RunProjection::new(title, spec, stored.ts);
    projection.parent_id = props.parent_id;
    projection.retried_from = props.retried_from;
    projection.web_url.clone_from(&props.web_url);
    projection.sandbox = Some(RunSandbox::planned(sandbox_plan(
        &projection.spec.settings.run.environment,
    )));
    Ok(projection)
}

fn sandbox_plan_from_projection_or_settings(state: &RunProjection) -> RunSandboxPlan {
    state.sandbox.as_ref().map_or_else(
        || sandbox_plan(&state.spec.settings.run.environment),
        |sandbox| sandbox.plan().clone(),
    )
}

fn sandbox_plan(settings: &RunEnvironmentSettings) -> RunSandboxPlan {
    let provider = SandboxProviderKind::from(settings.provider);
    RunSandboxPlan {
        provider,
        image: (settings.provider == EnvironmentProvider::Docker)
            .then(|| settings.image.docker.clone())
            .flatten()
            .filter(|image| !image.is_empty()),
        snapshot: None,
    }
}

fn stage_at_visit<'a>(
    state: &'a mut RunProjection,
    stored: &RunEvent,
    visit: u32,
    seq: u32,
) -> Option<&'a mut StageProjection> {
    if visit == 0 {
        return None;
    }
    let node_id = stored.node_id.as_deref()?;
    Some(state.stage_entry(node_id, visit, first_event_seq(seq)))
}

fn stage_at_current_visit<'a>(
    state: &'a mut RunProjection,
    stored: &RunEvent,
    seq: u32,
) -> Option<&'a mut StageProjection> {
    let node_id = stored.node_id.as_deref()?;
    let visit = state.current_visit_for(node_id).unwrap_or(1);
    Some(state.stage_entry(node_id, visit, first_event_seq(seq)))
}

fn stage_at_stored_stage_id<'a>(
    state: &'a mut RunProjection,
    stage_id: &StageId,
    seq: u32,
) -> &'a mut StageProjection {
    state.stage_entry(stage_id.node_id(), stage_id.visit(), first_event_seq(seq))
}

fn stage_at_stored_or_visit<'a>(
    state: &'a mut RunProjection,
    stored: &RunEvent,
    visit: u32,
    seq: u32,
) -> Option<&'a mut StageProjection> {
    if let Some(stage_id) = stored.stage_id.as_ref() {
        return Some(stage_at_stored_stage_id(state, stage_id, seq));
    }
    stage_at_visit(state, stored, visit, seq)
}

/// Open the inference bracket for the stage this event names.
///
/// Only root-session events open a bracket: forwarded child events carry a
/// parent session and must not overwrite the parent's bracket. The session id
/// is copied from the envelope so every later transition can be gated on it.
fn open_inference_bracket(
    state: &mut RunProjection,
    stored: &RunEvent,
    props: &AgentLlmStartedProps,
    seq: u32,
    ts: DateTime<Utc>,
) {
    if stored.parent_session_id.is_some() {
        return;
    }
    let Some(session_id) = stored.session_id.clone() else {
        return;
    };
    let Some(stage) = stage_at_stored_or_visit(state, stored, props.visit, seq) else {
        return;
    };
    stage.inference = Some(StageInferenceProjection {
        session_id,
        started_at: ts,
        requested_model: props.requested_model.clone(),
        first_output_at: None,
        first_output_kind: None,
        retries: 0,
    });
}

/// Resolve the stage owning a bracket this event is allowed to mutate,
/// borrowing the whole projection so the caller can also fold elapsed time
/// into the stage's live accumulators.
///
/// Returns `None` for child-session events, for a stage with no open bracket,
/// and for a bracket belonging to a different session (which is what
/// post-failover events look like).
fn matching_inference_stage<'a>(
    state: &'a mut RunProjection,
    stored: &RunEvent,
    visit: u32,
    seq: u32,
) -> Option<&'a mut StageProjection> {
    if stored.parent_session_id.is_some() {
        return None;
    }
    let session_id = stored.session_id.as_deref()?;
    let stage = stage_at_stored_or_visit(state, stored, visit, seq)?;
    let opened_here = stage
        .inference
        .as_ref()
        .is_some_and(|inference| inference.session_id == session_id);
    opened_here.then_some(stage)
}

/// Resolve the open inference bracket this event is allowed to mutate.
fn matching_inference_bracket<'a>(
    state: &'a mut RunProjection,
    stored: &RunEvent,
    visit: u32,
    seq: u32,
) -> Option<&'a mut StageInferenceProjection> {
    matching_inference_stage(state, stored, visit, seq)?
        .inference
        .as_mut()
}

/// Close the bracket on a stage-addressed terminal event, folding its elapsed
/// time into the stage's live inference accumulator.
fn close_inference_bracket(
    state: &mut RunProjection,
    stored: &RunEvent,
    visit: u32,
    seq: u32,
    ts: DateTime<Utc>,
) {
    let Some(stage) = matching_inference_stage(state, stored, visit, seq) else {
        return;
    };
    close_bracket_on_stage(stage, ts);
}

/// Take the open bracket and add its span to `live_inference_ms`.
///
/// Retries inside the bracket are deliberately included: the in-process
/// stopwatch counts a retried attempt's elapsed time as inference, and
/// `agent.llm.retry` keeps the bracket open rather than reopening it.
fn close_bracket_on_stage(stage: &mut StageProjection, ts: DateTime<Utc>) {
    let Some(inference) = stage.inference.take() else {
        return;
    };
    stage.accumulate_inference_ms(timing::elapsed_ms(inference.started_at, ts));
}

/// Close every active bracket opened by the session that just ended.
///
/// `agent.session.ended` is the only ordering-safe backstop for terminal
/// cancel and wall-clock timeout, which tear the session down through
/// `discard_session` without emitting a message, error, or interrupt. It is
/// emitted after the forwarder drains queued agent events, unlike
/// `agent.session.deactivated`, which is emitted before the drain and so can
/// be followed by a queued `agent.llm.started` that would re-open the bracket.
///
/// The tradeoff is that it carries no stage identity — its props are empty and
/// its envelope has only session ids. So the close takes ordering from the
/// event and identity from the projection, scanning for brackets this session
/// opened. Implemented as a normal stage lookup it would find no target and
/// silently no-op, leaving the bracket open forever on exactly the path it
/// exists to cover.
fn close_active_brackets_for_session(
    state: &mut RunProjection,
    stored: &RunEvent,
    ts: DateTime<Utc>,
) {
    if stored.parent_session_id.is_some() {
        return;
    }
    let Some(session_id) = stored.session_id.as_deref() else {
        return;
    };
    for (_, stage) in state.iter_stages_unordered_mut() {
        let opened_here = stage
            .inference
            .as_ref()
            .is_some_and(|inference| inference.session_id == session_id);
        if opened_here {
            close_bracket_on_stage(stage, ts);
        }
        stage.close_tool_batch_for_session(session_id, ts);
    }
}

fn stage_at_stored_or_current_visit<'a>(
    state: &'a mut RunProjection,
    stored: &RunEvent,
    seq: u32,
) -> Option<&'a mut StageProjection> {
    if let Some(stage_id) = stored.stage_id.as_ref() {
        return Some(stage_at_stored_stage_id(state, stage_id, seq));
    }
    stage_at_current_visit(state, stored, seq)
}

/// Apply a `checkpoint.completed` event to the exact execution named by the
/// envelope `StageId`: attach the checkpoint diff, and for a skipped
/// checkpoint finalize that execution as `Skipped`. A synthetic skipped
/// projection is created only for a first-attempt skip that never
/// materialized a stage; an existing non-terminal (e.g. `Retrying`)
/// projection keeps its identity and becomes terminal, while an older
/// terminal execution stays immutable.
fn apply_checkpoint_to_stage(
    state: &mut RunProjection,
    stage_id: &StageId,
    props: &CheckpointCompletedProps,
    checkpoint: &Checkpoint,
    seq: u32,
    ts: DateTime<Utc>,
) {
    let node_id = stage_id.node_id();
    let skipped_completion = checkpoint
        .node_outcomes
        .get(node_id)
        .filter(|outcome| outcome.status == StageOutcome::Skipped)
        .map(|outcome| stage_completion_from_outcome(outcome, ts));
    if props.diff.is_none() && skipped_completion.is_none() {
        return;
    }

    let is_new = state.stage(stage_id).is_none();
    let stage = stage_at_stored_stage_id(state, stage_id, seq);
    if is_new {
        stage.graph_visit = props.graph_visit;
        stage
            .resumed_from_stage_id
            .clone_from(&props.resumed_from_stage_id);
    }
    if let Some(diff) = props.diff.clone() {
        stage.diff = Some(diff);
    }
    if let Some(completion) = skipped_completion {
        if !stage.state.is_terminal() {
            stage.completion = Some(completion);
            stage.state = StageState::Skipped;
        }
    }
}

fn stage_at_completed_visit<'a>(
    state: &'a mut RunProjection,
    stored: &RunEvent,
    node_visits: Option<&BTreeMap<String, usize>>,
    seq: u32,
) -> Option<&'a mut StageProjection> {
    if let Some(stage_id) = stored.stage_id.as_ref() {
        return Some(stage_at_stored_stage_id(state, stage_id, seq));
    }
    let node_id = stored.node_id.as_deref()?;
    let visit = stage_visit(node_id, node_visits, state).unwrap_or(1);
    Some(state.stage_entry(node_id, visit, first_event_seq(seq)))
}

pub(crate) fn build_summary(state: &RunProjection, run_id: &RunId) -> Run {
    let goal = state.spec.graph.goal().to_string();
    let diff_summary = state
        .conclusion
        .as_ref()
        .and_then(|conclusion| conclusion.diff.summary)
        .or_else(|| {
            state
                .checkpoints
                .iter()
                .rev()
                .find_map(|checkpoint| checkpoint.diff.summary)
        });

    let current_question = state
        .pending_interviews
        .iter()
        .min_by(|(left_id, left), (right_id, right)| {
            left.started_at
                .cmp(&right.started_at)
                .then_with(|| left_id.cmp(right_id))
        })
        .map(|(_, record)| record.question.clone());
    let models = run_models(state);
    let created_by = state.spec.provenance.subject.clone();
    let source_directory = state.spec.source_directory.clone();
    let repo_origin_url = state.spec.git.as_ref().map(|git| git.origin_url.clone());
    let start_time = state.start.as_ref().map(|start| start.start_time);
    let completed_at = state
        .conclusion
        .as_ref()
        .map(|conclusion| conclusion.timestamp);
    let run_timing = state
        .conclusion
        .as_ref()
        .map(|conclusion| conclusion.timing);
    let total_usd_micros = projected_billing(state).total_usd_micros;

    Run {
        id: *run_id,
        parent_id: state.parent_id,
        children_count: 0,
        title: state.title().into_owned(),
        goal,
        workflow: WorkflowRef {
            slug:       state.spec.workflow_slug.clone(),
            name:       state.spec.workflow_name().map(ToOwned::to_owned),
            graph_name: state.spec.graph_name().map(ToOwned::to_owned),
            node_count: i64::try_from(state.spec.graph.nodes.len())
                .expect("graph node count should fit in i64"),
            edge_count: i64::try_from(state.spec.graph.edges.len())
                .expect("graph edge count should fit in i64"),
        },
        automation: state.spec.automation.clone(),
        repository: Some(RepositoryRef::from_origin_and_source(
            repo_origin_url,
            source_directory.as_deref(),
        )),
        created_by,
        origin: RunOrigin::default(),
        labels: state.spec.labels.clone(),
        lifecycle: RunLifecycle {
            status:          state.status,
            approval:        state.approval.clone(),
            pending_control: state.pending_control,
            queue_position:  None,
            error:           None,
            archived:        state.archived_at.is_some(),
            archived_at:     state.archived_at,
        },
        sandbox: state.sandbox.clone(),
        models,
        source_directory,
        timestamps: RunTimestamps {
            created_at: run_id.created_at(),
            started_at: start_time,
            last_event_at: Some(state.last_event_at),
            completed_at,
        },
        timing: run_timing,
        billing: total_usd_micros.map(|total_usd_micros| RunBillingSummary {
            total_usd_micros: Some(total_usd_micros),
        }),
        size: RunSize::from_total_usd_micros(total_usd_micros),
        ask_fabro: AskFabro::default(),
        diff: diff_summary,
        pull_request: state.pull_request.clone(),
        current_question,
        superseded_by: state.superseded_by,
        retried_from: state.retried_from,
        links: RunLinks {
            web: state.web_url.clone(),
        },
    }
}

pub(crate) fn projected_billing(state: &RunProjection) -> BilledTokenCounts {
    if let Some(billing) = state
        .conclusion
        .as_ref()
        .and_then(|conclusion| conclusion.billing.as_ref())
    {
        return billing.clone();
    }

    let mut billing = BilledTokenCounts::default();
    for (stage_id, stage) in state.iter_stages() {
        if !state.is_boundary_stage(stage_id.node_id()) {
            billing.add_counts(&stage.usage);
        }
    }
    billing
}

fn run_models(state: &RunProjection) -> Vec<RunModel> {
    let mut models = state
        .iter_stages()
        .filter_map(|(_, stage)| stage.model.as_ref())
        .map(|model| RunModel {
            provider: Some(model.provider.to_string()),
            name:     model.model_id.to_string(),
        })
        .collect::<Vec<_>>();
    models.sort_by(|left, right| {
        left.provider
            .cmp(&right.provider)
            .then_with(|| left.name.cmp(&right.name))
    });
    models.dedup_by(|left, right| left.provider == right.provider && left.name == right.name);
    models
}

fn checkpoint_from_props(props: &CheckpointCompletedProps, timestamp: DateTime<Utc>) -> Checkpoint {
    let loop_failure_signatures = props
        .loop_failure_signatures
        .clone()
        .into_iter()
        .map(|(key, value)| (FailureSignature(key), value))
        .collect();
    let restart_failure_signatures = props
        .restart_failure_signatures
        .clone()
        .into_iter()
        .map(|(key, value)| (FailureSignature(key), value))
        .collect();

    Checkpoint {
        timestamp,
        current_node: props.current_node.clone(),
        completed_nodes: props.completed_nodes.clone(),
        node_retries: props.node_retries.clone().into_iter().collect(),
        context_values: props.context_values.clone().into_iter().collect(),
        node_outcomes: props.node_outcomes.clone().into_iter().collect(),
        next_node_id: props.next_node_id.clone(),
        git_commit_sha: props.git_commit_sha.clone(),
        loop_failure_signatures,
        restart_failure_signatures,
        node_visits: props.node_visits.clone().into_iter().collect(),
    }
}

fn diff_from_checkpoint_props(props: &CheckpointCompletedProps) -> RunDiff {
    RunDiff {
        patch:   props.diff.clone(),
        summary: props.diff_summary,
    }
}

fn conclusion_from_completed(
    props: &RunCompletedProps,
    timestamp: DateTime<Utc>,
) -> Result<Conclusion> {
    Ok(Conclusion {
        timestamp,
        status: StageOutcome::from_str(&props.status)
            .map_err(|err| Error::InvalidEvent(format!("invalid completed stage status: {err}")))?,
        timing: props.timing,
        failure: None,
        final_git_commit_sha: props.final_git_commit_sha.clone(),
        stages: Vec::new(),
        billing: props.billing.clone(),
        total_retries: 0,
        diff: RunDiff {
            patch:   props.final_patch.clone(),
            summary: props.diff_summary,
        },
    })
}

fn conclusion_from_failed(props: &RunFailedProps, timestamp: DateTime<Utc>) -> Conclusion {
    Conclusion {
        timestamp,
        status: StageOutcome::Failed {
            retry_requested: false,
        },
        timing: props.timing,
        failure: Some(props.failure.clone()),
        final_git_commit_sha: props.final_git_commit_sha.clone(),
        stages: Vec::new(),
        billing: props.billing.clone(),
        total_retries: 0,
        diff: RunDiff {
            patch:   props.final_patch.clone(),
            summary: props.diff_summary,
        },
    }
}

fn finalize_unfinished_stages_after_run_failed(
    state: &mut RunProjection,
    props: &RunFailedProps,
    timestamp: DateTime<Utc>,
) {
    let terminal_state = if props.failure.reason == fabro_types::FailureReason::Cancelled {
        StageState::Cancelled
    } else {
        StageState::Failed
    };

    for (_, stage) in state.iter_stages_unordered_mut() {
        if stage.state.is_terminal() {
            continue;
        }

        // Close any brackets still open so their spans are not dropped on the
        // floor when the live estimate is frozen into `timing` below.
        close_bracket_on_stage(stage, timestamp);
        stage.close_open_acp_inference(timestamp);
        stage.close_open_tool_batch(timestamp);

        // Freeze the live estimate before flipping to a terminal state:
        // `live_timing` reads `effective_state` and would return wall-only
        // once the stage no longer looks in-flight.
        let frozen = stage.live_timing(timestamp);
        stage.state = terminal_state;
        if stage.timing.is_none() && stage.started_at.is_some() {
            stage.set_authoritative_timing(frozen);
        } else {
            stage.clear_live_timing();
        }
    }
}

fn stage_state_from_failure(
    will_retry: bool,
    failure_category: Option<FailureCategory>,
    command_termination: Option<CommandTermination>,
) -> StageState {
    if will_retry {
        StageState::Retrying
    } else if failure_category == Some(FailureCategory::Canceled)
        && command_termination != Some(CommandTermination::Exited)
    {
        StageState::Cancelled
    } else {
        StageState::Failed
    }
}

fn stage_visit(
    node_id: &str,
    node_visits: Option<&BTreeMap<String, usize>>,
    state: &RunProjection,
) -> Option<u32> {
    node_visits
        .and_then(|visits| visits.get(node_id).copied())
        .and_then(|visit| u32::try_from(visit).ok())
        .filter(|visit| *visit > 0)
        .or_else(|| state.current_visit_for(node_id))
}

fn stage_outcome_from_props(props: &StageCompletedProps) -> Outcome<Option<BilledModelUsage>> {
    Outcome {
        status:             props.status,
        preferred_label:    props.preferred_label.clone(),
        suggested_next_ids: props.suggested_next_ids.clone(),
        context_updates:    props
            .context_updates
            .clone()
            .unwrap_or_default()
            .into_iter()
            .collect(),
        jump_to_node:       props.jump_to_node.clone(),
        notes:              props.notes.clone(),
        failure:            props.failure.clone(),
        usage:              props.billing.clone(),
        files_touched:      props.files_touched.clone(),
        timing:             Some(props.timing),
    }
}

fn stage_completion_from_outcome(
    outcome: &Outcome<Option<BilledModelUsage>>,
    timestamp: DateTime<Utc>,
) -> StageCompletion {
    StageCompletion {
        outcome: outcome.status,
        notes: outcome.notes.clone(),
        failure_reason: outcome
            .failure
            .as_ref()
            .map(|failure| render_compact_with_causes(&failure.message, &failure.causes)),
        timestamp,
    }
}

fn apply_agent_terminal(
    event_prefix: &str,
    stage: &mut StageProjection,
    props: &impl serde::Serialize,
    output: String,
    termination: CommandTermination,
) -> Result<()> {
    let script_timing = serde_json::to_value(props).map_err(|err| {
        Error::InvalidEvent(format!("invalid {event_prefix} terminal payload: {err}"))
    })?;
    stage.output = Some(output);
    stage.termination = Some(termination);
    stage.script_timing = Some(script_timing);
    stage.agent_control = AgentControlState::Running;
    Ok(())
}

fn merge_agent_process_output(stdout: &str, stderr: &str) -> String {
    match (stdout.is_empty(), stderr.is_empty()) {
        (true, true) => String::new(),
        (false, true) => stdout.to_string(),
        (true, false) => stderr.to_string(),
        (false, false) => format!("{stdout}\n{stderr}"),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use chrono::{DateTime, Utc};
    use fabro_types::run_event::misc::CommandCompletedProps;
    use fabro_types::run_event::run::RunFailedProps;
    use fabro_types::run_event::{
        AgentAcpCancelledProps, AgentAcpCompletedProps, AgentAcpStartedProps,
        AgentAcpTimedOutProps, AgentMcpFailedProps, AgentMcpReadyProps, AgentMcpToolSummary,
        AgentMessageProps, AgentRoundInterruptedProps, AgentSessionActivatedProps,
        AgentSessionDeactivatedProps, AgentSessionEndedProps, AgentSessionStartedProps,
        AgentSkillActivatedProps, AgentSkillActivationSource, AgentSkillSummary,
        AgentSkillsDiscoveredProps, AgentSteeringInjectedProps, AgentSubClosedProps,
        AgentSubCompletedProps, AgentSubFailedProps, AgentSubSpawnedProps,
        AgentSubTurnStartedProps, AgentToolCategory, AgentToolSource, AgentToolStartedProps,
        AgentToolSummary, AgentToolsAvailableProps, CheckpointCompletedProps,
        InterviewCompletedProps, InterviewOption, InterviewStartedProps,
        ParallelBranchCompletedProps, ParallelBranchStartedProps, RunCompletedProps,
        RunControlEffectProps, StageCompletedProps, StageFailedProps, StagePromptProps,
        StageRetryingProps, StageStartedProps,
    };
    use fabro_types::settings::run::{DockerfileSource, EnvironmentProvider};
    use fabro_types::{
        AgentBackend, AgentControlState, AttrValue, AutomationRef, BilledModelUsage,
        BilledTokenCounts, BlobHash, BlockedReason, Checkpoint, CheckpointRecord,
        CommandTermination, EventBody, FailureCategory, FailureDetail, FailureReason, Graph,
        McpServerStatus, Node, Outcome, ParallelBranchId, PendingReason, PermissionLevel,
        PullRequestCreationStatus, PullRequestLink, QuestionType, ReasoningEffort,
        RunApprovalState, RunBillingSummary, RunControlAction, RunDiff, RunEvent, RunSize, RunSpec,
        RunStatus, Speed, StageContextWindowBreakdownItem, StageContextWindowCategory,
        StageContextWindowCountMethod, StageContextWindowProjection, StageContextWindowStaleness,
        StageContextWindowWarning, StageHandler, StageModelUsage, StageOutcome, StageState,
        StageTiming, SubAgentStatus, SuccessReason, WorkflowSettings, first_event_seq, fixtures,
        test_support,
    };
    use serde_json::json;

    use super::{RunProjection, RunProjectionReducer, build_summary};
    use crate::{Error, EventEnvelope, StageId};

    /// Live accumulation of inference and tool time while a stage is in
    /// flight. The finalized breakdown still arrives with the terminal event
    /// and replaces these; these exist so a long-running stage is not reported
    /// as doing no work.
    mod live_active_accumulation {
        use fabro_types::run_event::{
            AgentLlmFirstOutputProps, AgentLlmRetryProps, AgentLlmStartedProps,
            AgentToolCompletedProps, AgentToolStartedProps,
        };
        use fabro_types::{
            LlmOutputKind, LlmRetryPhase, ModelRef, Speed, StageOutcome, StageProjection,
        };

        use super::*;

        fn stage_id() -> StageId {
            StageId::new("plan", 1)
        }

        fn session_event(seq: u32, ts: &str, session_id: &str, body: EventBody) -> EventEnvelope {
            let mut event = test_stage_event_at(seq, ts, body, stage_id());
            event.event.session_id = Some(session_id.to_string());
            event
        }

        fn agent_event(seq: u32, ts: &str, body: EventBody) -> EventEnvelope {
            session_event(seq, ts, "session-1", body)
        }

        /// An event from a sub-agent session nested under the root session.
        fn child_event(seq: u32, ts: &str, body: EventBody) -> EventEnvelope {
            let mut event = agent_event(seq, ts, body);
            event.event.session_id = Some("session-child".to_string());
            event.event.parent_session_id = Some("session-1".to_string());
            event
        }

        fn llm_started() -> EventBody {
            EventBody::AgentLlmStarted(AgentLlmStartedProps {
                requested_model: ModelRef {
                    provider: "anthropic".parse().unwrap(),
                    model_id: "claude-fable-5".into(),
                    speed:    Some(Speed::Fast),
                },
                visit:           1,
            })
        }

        fn tool_started(tool_call_id: &str) -> EventBody {
            EventBody::AgentToolStarted(AgentToolStartedProps {
                tool_name:         "Bash".to_string(),
                tool_call_id:      tool_call_id.to_string(),
                arguments:         json!({}),
                visit:             1,
                tool_call:         None,
                turn_id:           None,
                parent_message_id: None,
            })
        }

        fn tool_completed(tool_call_id: &str) -> EventBody {
            EventBody::AgentToolCompleted(AgentToolCompletedProps {
                tool_name:    "Bash".to_string(),
                tool_call_id: tool_call_id.to_string(),
                output:       json!("ok"),
                is_error:     false,
                visit:        1,
                tool_result:  None,
                turn_id:      None,
            })
        }

        fn agent_message() -> EventBody {
            EventBody::AgentMessage(live_agent_message_props(live_counts(10, 5)))
        }

        fn started_state() -> RunProjection {
            let mut state = initialized_projection();
            state
                .apply_event(&test_stage_event_at(
                    1,
                    "2026-04-07T12:00:00Z",
                    EventBody::StageStarted(started_props()),
                    stage_id(),
                ))
                .unwrap();
            state
        }

        fn stage(state: &RunProjection) -> &StageProjection {
            state.stage(&stage_id()).unwrap()
        }

        #[test]
        fn closing_an_inference_bracket_accumulates_its_span() {
            let mut state = started_state();
            state
                .apply_event(&agent_event(2, "2026-04-07T12:00:05Z", llm_started()))
                .unwrap();
            state
                .apply_event(&agent_event(
                    3,
                    "2026-04-07T12:00:06Z",
                    EventBody::AgentLlmFirstOutput(AgentLlmFirstOutputProps {
                        kind:  LlmOutputKind::Text,
                        visit: 1,
                    }),
                ))
                .unwrap();
            state
                .apply_event(&agent_event(4, "2026-04-07T12:00:12Z", agent_message()))
                .unwrap();

            // 12:00:05 -> 12:00:12; first_output is a marker, not the close.
            assert_eq!(stage(&state).live_inference_ms, 7_000);
            assert!(stage(&state).inference.is_none());
        }

        #[test]
        fn concurrent_tool_calls_count_once_not_per_call() {
            let mut state = started_state();
            for (seq, id) in [(2, "call-a"), (3, "call-b"), (4, "call-c")] {
                state
                    .apply_event(&agent_event(seq, "2026-04-07T12:00:00Z", tool_started(id)))
                    .unwrap();
            }
            // All three finish 10s later. Summing per-call spans would report
            // 30s; the batch actually occupied 10s of wall time.
            for (seq, id) in [(5, "call-a"), (6, "call-b"), (7, "call-c")] {
                state
                    .apply_event(&agent_event(
                        seq,
                        "2026-04-07T12:00:10Z",
                        tool_completed(id),
                    ))
                    .unwrap();
            }

            assert_eq!(stage(&state).live_tool_ms, 10_000);
            assert!(stage(&state).tool_batch.is_none());
        }

        #[test]
        fn a_batch_stays_open_until_its_last_call_reports() {
            let mut state = started_state();
            state
                .apply_event(&agent_event(
                    2,
                    "2026-04-07T12:00:00Z",
                    tool_started("call-a"),
                ))
                .unwrap();
            state
                .apply_event(&agent_event(
                    3,
                    "2026-04-07T12:00:02Z",
                    tool_started("call-b"),
                ))
                .unwrap();
            state
                .apply_event(&agent_event(
                    4,
                    "2026-04-07T12:00:05Z",
                    tool_completed("call-a"),
                ))
                .unwrap();

            assert_eq!(
                stage(&state).live_tool_ms,
                0,
                "batch must not close while call-b is outstanding"
            );

            state
                .apply_event(&agent_event(
                    5,
                    "2026-04-07T12:00:09Z",
                    tool_completed("call-b"),
                ))
                .unwrap();

            // Measured from the batch open, not from the last call's start.
            assert_eq!(stage(&state).live_tool_ms, 9_000);
        }

        #[test]
        fn successive_batches_accumulate() {
            let mut state = started_state();
            for (seq, ts, body) in [
                (2, "2026-04-07T12:00:00Z", tool_started("call-a")),
                (3, "2026-04-07T12:00:04Z", tool_completed("call-a")),
                (4, "2026-04-07T12:00:10Z", tool_started("call-b")),
                (5, "2026-04-07T12:00:16Z", tool_completed("call-b")),
            ] {
                state.apply_event(&agent_event(seq, ts, body)).unwrap();
            }

            assert_eq!(stage(&state).live_tool_ms, 10_000);
        }

        #[test]
        fn a_duplicate_completion_does_not_drain_the_batch_early() {
            let mut state = started_state();
            state
                .apply_event(&agent_event(
                    2,
                    "2026-04-07T12:00:00Z",
                    tool_started("call-a"),
                ))
                .unwrap();
            state
                .apply_event(&agent_event(
                    3,
                    "2026-04-07T12:00:00Z",
                    tool_started("call-b"),
                ))
                .unwrap();
            // call-a reports twice, as a replayed or duplicated log can.
            state
                .apply_event(&agent_event(
                    4,
                    "2026-04-07T12:00:03Z",
                    tool_completed("call-a"),
                ))
                .unwrap();
            state
                .apply_event(&agent_event(
                    5,
                    "2026-04-07T12:00:04Z",
                    tool_completed("call-a"),
                ))
                .unwrap();

            assert_eq!(stage(&state).live_tool_ms, 0);
            assert!(stage(&state).tool_batch.is_some());
        }

        #[test]
        fn a_foreign_session_completion_does_not_mutate_the_open_batch() {
            let mut state = started_state();
            state
                .apply_event(&agent_event(
                    2,
                    "2026-04-07T12:00:00Z",
                    tool_started("call-a"),
                ))
                .unwrap();
            state
                .apply_event(&session_event(
                    3,
                    "2026-04-07T12:00:05Z",
                    "session-2",
                    tool_completed("call-a"),
                ))
                .unwrap();

            let batch = stage(&state).tool_batch.as_ref().unwrap();
            assert_eq!(batch.session_id, "session-1");
            assert!(batch.open_call_ids.contains("call-a"));
            assert_eq!(stage(&state).live_tool_ms, 0);
        }

        #[test]
        fn a_replacement_session_starts_a_separate_tool_batch() {
            let mut state = started_state();
            state
                .apply_event(&agent_event(
                    2,
                    "2026-04-07T12:00:00Z",
                    tool_started("old-call"),
                ))
                .unwrap();
            state
                .apply_event(&session_event(
                    3,
                    "2026-04-07T12:00:05Z",
                    "session-2",
                    tool_started("new-call"),
                ))
                .unwrap();

            assert_eq!(stage(&state).live_tool_ms, 5_000);
            let batch = stage(&state).tool_batch.as_ref().unwrap();
            assert_eq!(batch.session_id, "session-2");
            assert_eq!(
                batch.open_call_ids,
                ["new-call".to_string()].into_iter().collect()
            );

            // A delayed completion from the old session cannot close the new
            // session's batch even when call ids happen to collide.
            state
                .apply_event(&agent_event(
                    4,
                    "2026-04-07T12:00:07Z",
                    tool_completed("new-call"),
                ))
                .unwrap();
            assert!(stage(&state).tool_batch.is_some());

            state
                .apply_event(&session_event(
                    5,
                    "2026-04-07T12:00:09Z",
                    "session-2",
                    tool_completed("new-call"),
                ))
                .unwrap();
            assert_eq!(stage(&state).live_tool_ms, 9_000);
            assert!(stage(&state).tool_batch.is_none());
        }

        #[test]
        fn subagent_tool_calls_do_not_double_count_against_the_root_batch() {
            let mut state = started_state();
            state
                .apply_event(&agent_event(
                    2,
                    "2026-04-07T12:00:00Z",
                    tool_started("root-call"),
                ))
                .unwrap();
            // The sub-agent's own tools run inside the root call's span.
            state
                .apply_event(&child_event(
                    3,
                    "2026-04-07T12:00:01Z",
                    tool_started("child-call"),
                ))
                .unwrap();
            state
                .apply_event(&child_event(
                    4,
                    "2026-04-07T12:00:02Z",
                    tool_completed("child-call"),
                ))
                .unwrap();
            state
                .apply_event(&agent_event(
                    5,
                    "2026-04-07T12:00:08Z",
                    tool_completed("root-call"),
                ))
                .unwrap();

            assert_eq!(stage(&state).live_tool_ms, 8_000);
        }

        #[test]
        fn session_end_accumulates_every_open_active_bracket() {
            let mut state = started_state();
            state
                .apply_event(&agent_event(2, "2026-04-07T12:00:05Z", llm_started()))
                .unwrap();
            state
                .apply_event(&agent_event(
                    3,
                    "2026-04-07T12:00:07Z",
                    tool_started("call-a"),
                ))
                .unwrap();

            let mut ended = test_stage_event_at(
                4,
                "2026-04-07T12:00:20Z",
                EventBody::AgentSessionEnded(AgentSessionEndedProps {}),
                stage_id(),
            );
            ended.event.session_id = Some("session-1".to_string());
            state.apply_event(&ended).unwrap();

            assert_eq!(stage(&state).live_inference_ms, 15_000);
            assert_eq!(stage(&state).live_tool_ms, 13_000);
            assert!(stage(&state).inference.is_none());
            assert!(stage(&state).tool_batch.is_none());
        }

        #[test]
        fn a_foreign_session_close_leaves_the_bracket_and_accumulator_alone() {
            let mut state = started_state();
            state
                .apply_event(&agent_event(2, "2026-04-07T12:00:05Z", llm_started()))
                .unwrap();

            // Post-failover: a new session emits the message, so the old
            // bracket is not this event's to close or bill.
            let mut foreign =
                test_stage_event_at(3, "2026-04-07T12:00:20Z", agent_message(), stage_id());
            foreign.event.session_id = Some("session-2".to_string());
            state.apply_event(&foreign).unwrap();

            assert_eq!(stage(&state).live_inference_ms, 0);
            assert!(stage(&state).inference.is_some());
        }

        #[test]
        fn a_retry_keeps_accumulating_within_one_bracket() {
            let mut state = started_state();
            state
                .apply_event(&agent_event(2, "2026-04-07T12:00:00Z", llm_started()))
                .unwrap();
            state
                .apply_event(&agent_event(
                    3,
                    "2026-04-07T12:00:04Z",
                    EventBody::AgentLlmRetry(AgentLlmRetryProps {
                        provider:   "anthropic".to_string(),
                        model:      "claude-fable-5".to_string(),
                        attempt:    0,
                        delay_secs: 0.0,
                        error:      json!({ "kind": "stream" }),
                        phase:      Some(LlmRetryPhase::Consume),
                        visit:      1,
                    }),
                ))
                .unwrap();
            state
                .apply_event(&agent_event(4, "2026-04-07T12:00:11Z", agent_message()))
                .unwrap();

            // The whole bracket counts, retry included, matching the
            // in-process stopwatch.
            assert_eq!(stage(&state).live_inference_ms, 11_000);
            assert_eq!(stage(&state).inference, None);
        }

        #[test]
        fn stage_completion_replaces_the_live_estimate_with_finalized_timing() {
            let mut state = started_state();
            state
                .apply_event(&agent_event(2, "2026-04-07T12:00:00Z", llm_started()))
                .unwrap();
            state
                .apply_event(&agent_event(3, "2026-04-07T12:00:09Z", agent_message()))
                .unwrap();
            assert_eq!(stage(&state).live_inference_ms, 9_000);

            state
                .apply_event(&test_stage_event_at(
                    4,
                    "2026-04-07T12:00:10Z",
                    EventBody::StageCompleted(completed_props(10_000, StageOutcome::Succeeded)),
                    stage_id(),
                ))
                .unwrap();

            let stage = stage(&state);
            assert_eq!(
                stage.live_timing(test_dt("2026-04-07T12:30:00Z")),
                stage.timing.unwrap(),
                "a terminal stage reports its finalized breakdown, not a live estimate"
            );
            assert_eq!(stage.live_inference_ms, 0);
            assert_eq!(stage.live_tool_ms, 0);
            assert!(stage.inference.is_none());
            assert!(stage.tool_batch.is_none());
        }

        #[test]
        fn run_failure_freezes_open_work_and_clears_live_bookkeeping() {
            let mut state = started_state();
            state.status = RunStatus::Running;
            state
                .apply_event(&agent_event(2, "2026-04-07T12:00:01Z", llm_started()))
                .unwrap();
            state
                .apply_event(&agent_event(3, "2026-04-07T12:00:04Z", agent_message()))
                .unwrap();
            state
                .apply_event(&agent_event(
                    4,
                    "2026-04-07T12:00:05Z",
                    tool_started("call-a"),
                ))
                .unwrap();

            let mut failed = test_event(
                5,
                EventBody::RunFailed(run_failed_props(FailureReason::WorkflowError)),
                None,
            );
            failed.event.ts = test_dt("2026-04-07T12:00:10Z");
            state.apply_event(&failed).unwrap();

            let stage = stage(&state);
            assert_eq!(
                stage.timing,
                Some(fabro_types::StageTiming::new(10_000, 3_000, 5_000))
            );
            assert_eq!(stage.state, StageState::Failed);
            assert_eq!(stage.live_inference_ms, 0);
            assert_eq!(stage.live_tool_ms, 0);
            assert!(stage.inference.is_none());
            assert!(stage.tool_batch.is_none());
        }
    }

    fn test_event(seq: u32, body: EventBody, node_id: Option<&str>) -> EventEnvelope {
        let event = RunEvent {
            id: format!("evt-{seq}"),
            ts: Utc::now(),
            run_id: fixtures::RUN_1,
            node_id: node_id.map(ToOwned::to_owned),
            node_label: None,
            stage_id: None,
            parallel_group_id: None,
            parallel_branch_id: None,
            session_id: None,
            parent_session_id: None,
            tool_call_id: None,
            actor: None,
            body,
        };

        EventEnvelope { seq, event }
    }

    fn test_stage_event(seq: u32, body: EventBody, stage_id: StageId) -> EventEnvelope {
        let mut event = test_event(seq, body, Some(stage_id.node_id()));
        event.event.stage_id = Some(stage_id);
        event
    }

    fn test_branch_event(
        seq: u32,
        body: EventBody,
        stage_id: StageId,
        branch_id: ParallelBranchId,
    ) -> EventEnvelope {
        let mut event = test_stage_event(seq, body, stage_id);
        event.event.parallel_group_id = Some(branch_id.group().clone());
        event.event.parallel_branch_id = Some(branch_id);
        event
    }

    fn test_stage_event_at(
        seq: u32,
        ts: &str,
        body: EventBody,
        stage_id: StageId,
    ) -> EventEnvelope {
        let mut event = test_stage_event(seq, body, stage_id);
        event.event.ts = test_dt(ts);
        event
    }

    fn test_usage(model_id: &str, input_tokens: i64, output_tokens: i64) -> BilledModelUsage {
        serde_json::from_value(json!({
            "input": {
                "usage": {
                    "model": {
                        "provider": "openai",
                        "model_id": model_id
                    },
                    "tokens": {
                        "input_tokens": input_tokens,
                        "output_tokens": output_tokens
                    }
                },
                "facts": { "algorithm": "openai" }
            },
            "total_usd_micros": input_tokens + output_tokens
        }))
        .unwrap()
    }

    fn usage_json(usage: &BilledModelUsage) -> serde_json::Value {
        serde_json::to_value(usage).unwrap()
    }

    fn usage_counts(usage: &BilledModelUsage) -> BilledTokenCounts {
        BilledTokenCounts::from_billed_usage(std::slice::from_ref(usage))
    }

    fn test_run_spec() -> RunSpec {
        RunSpec {
            graph_source: Some("digraph test {}".to_string()),
            ..test_support::test_run_spec()
        }
    }

    fn initialized_projection() -> RunProjection {
        RunProjection::new("Test run".to_string(), test_run_spec(), Utc::now())
    }

    fn test_dt(value: &str) -> DateTime<Utc> {
        value.parse().unwrap()
    }

    fn running_projection() -> RunProjection {
        let mut state = initialized_projection();
        state
            .apply_event(&test_raw_event(
                1,
                "run.runnable",
                &json!({ "source": "start_requested" }),
                None,
            ))
            .unwrap();
        state
            .apply_event(&test_raw_event(2, "run.starting", &json!({}), None))
            .unwrap();
        state
            .apply_event(&test_raw_event(3, "run.running", &json!({}), None))
            .unwrap();
        state
    }

    #[test]
    fn planned_sandbox_uses_docker_image_and_hides_daytona_snapshot_until_init() {
        let mut docker = WorkflowSettings::default().run.environment;
        docker.provider = EnvironmentProvider::Docker;
        docker.image.docker = Some("ubuntu:24.04".to_string());

        let planned_docker = super::sandbox_plan(&docker);
        assert_eq!(planned_docker.image.as_deref(), Some("ubuntu:24.04"));
        assert_eq!(planned_docker.snapshot, None);

        let mut daytona = WorkflowSettings::default().run.environment;
        daytona.provider = EnvironmentProvider::Daytona;
        daytona.image.dockerfile = Some(DockerfileSource::Inline("FROM ubuntu:24.04".to_string()));

        let planned_daytona = super::sandbox_plan(&daytona);
        assert_eq!(planned_daytona.image, None);
        assert_eq!(planned_daytona.snapshot, None);
    }

    #[test]
    fn sandbox_initialized_updates_image_and_snapshot_projection_fields() {
        let mut state = initialized_projection();
        state
            .apply_event(&test_raw_event(
                1,
                "sandbox.initialized",
                &json!({
                    "provider": "daytona",
                    "id": "fabro-run-sandbox",
                    "working_directory": "/home/daytona/workspace",
                    "snapshot": "fabro-11111111-2222-8333-8444-555555555555"
                }),
                None,
            ))
            .unwrap();

        let sandbox = state.sandbox.expect("sandbox should be projected");
        let instance = sandbox.instance().expect("sandbox should be ready");
        assert_eq!(instance.image, None);
        assert_eq!(
            instance.snapshot.as_deref(),
            Some("fabro-11111111-2222-8333-8444-555555555555")
        );
    }

    #[test]
    fn run_created_without_retried_from_projects_retried_from_none() {
        let event = test_raw_event(
            1,
            "run.created",
            &json!({
                "settings": WorkflowSettings::default(),
                "graph": Graph::new("test"),
                "labels": {},
                "provenance": test_support::test_run_provenance()
            }),
            None,
        );

        let projection = RunProjection::apply_events(&[event]).unwrap();
        assert_eq!(projection.retried_from, None);
        assert_eq!(
            build_summary(&projection, &fixtures::RUN_1).retried_from,
            None
        );
    }

    #[test]
    fn run_created_replay_ignores_unknown_properties() {
        let provenance = test_support::test_run_provenance();
        let event = test_raw_event(
            1,
            "run.created",
            &json!({
                "title": "Historical run",
                "settings": WorkflowSettings::default(),
                "graph": Graph::new("historical"),
                "workflow_source": "digraph historical { start -> exit }",
                "labels": {"team": "platform"},
                "source_directory": "/workspace/project",
                "workflow_slug": "historical",
                "unknown_future_property": {
                    "nested": ["value", 42, true]
                },
                "provenance": provenance
            }),
            None,
        );

        let projection = RunProjection::apply_events(&[event]).unwrap();

        assert_eq!(projection.title(), "Historical run");
        assert_eq!(projection.spec.graph.name, "historical");
        assert_eq!(
            projection.spec.graph_source.as_deref(),
            Some("digraph historical { start -> exit }")
        );
        assert_eq!(
            projection.spec.labels.get("team").map(String::as_str),
            Some("platform")
        );
        assert_eq!(
            projection.spec.source_directory.as_deref(),
            Some("/workspace/project")
        );
        assert_eq!(projection.spec.workflow_slug.as_deref(), Some("historical"));
        assert_eq!(projection.spec.provenance, provenance);
    }

    #[test]
    fn run_created_projects_automation_into_spec_and_summary() {
        let automation = AutomationRef {
            id:         "nightly".to_string(),
            name:       Some("Nightly".to_string()),
            trigger_id: Some("schedule_1".to_string()),
        };
        let event = test_raw_event(
            1,
            "run.created",
            &json!({
                "settings": WorkflowSettings::default(),
                "graph": Graph::new("test"),
                "automation": automation,
                "labels": {},
                "provenance": test_support::test_run_provenance()
            }),
            None,
        );

        let projection = RunProjection::apply_events(&[event]).unwrap();

        assert_eq!(projection.spec.automation, Some(automation.clone()));
        assert_eq!(
            build_summary(&projection, &fixtures::RUN_1).automation,
            Some(automation)
        );
    }

    #[test]
    fn run_created_projects_planned_sandbox_lifecycle() {
        let state = RunProjection::apply_events(&[test_raw_event(
            1,
            "run.created",
            &json!({
                "settings": WorkflowSettings::default(),
                "graph": Graph::new("test"),
                "labels": {},
                "provenance": test_support::test_run_provenance()
            }),
            None,
        )])
        .unwrap();

        let sandbox = serde_json::to_value(state.sandbox.as_ref().unwrap()).unwrap();
        assert_eq!(sandbox["kind"], "planned");
        assert_eq!(sandbox["plan"]["provider"], "local");
        assert!(sandbox.get("instance").is_none());
        assert!(sandbox.get("failure").is_none());
    }

    #[test]
    fn sandbox_lifecycle_events_update_projected_sandbox_state() {
        let mut state = RunProjection::apply_events(&[test_raw_event(
            1,
            "run.created",
            &json!({
                "settings": WorkflowSettings::default(),
                "graph": Graph::new("test"),
                "labels": {},
                "provenance": test_support::test_run_provenance()
            }),
            None,
        )])
        .unwrap();

        state
            .apply_event(&test_raw_event(
                2,
                "sandbox.initializing",
                &json!({ "provider": "docker" }),
                None,
            ))
            .unwrap();
        let initializing = serde_json::to_value(state.sandbox.as_ref().unwrap()).unwrap();
        assert_eq!(initializing["kind"], "initializing");
        assert!(initializing.get("instance").is_none());

        state
            .apply_event(&test_raw_event(
                3,
                "sandbox.initialized",
                &json!({
                    "provider": "docker",
                    "id": "container-abc123",
                    "working_directory": "/workspace",
                    "image": "ubuntu:24.04",
                    "repo_cloned": true,
                    "clone_origin_url": "https://github.com/fabro-sh/fabro.git",
                    "clone_branch": "main"
                }),
                None,
            ))
            .unwrap();
        let ready = serde_json::to_value(state.sandbox.as_ref().unwrap()).unwrap();
        assert_eq!(ready["kind"], "ready");
        assert_eq!(ready["plan"]["provider"], "local");
        assert_eq!(ready["instance"]["provider"], "docker");
        assert_eq!(ready["instance"]["image"], "ubuntu:24.04");
        assert_eq!(ready["instance"]["runtime"]["id"], "container-abc123");
        assert_eq!(
            ready["instance"]["runtime"]["working_directory"],
            "/workspace"
        );

        state
            .apply_event(&test_raw_event(
                4,
                "sandbox.failed",
                &json!({
                    "provider": "docker",
                    "error": "Docker daemon unavailable",
                    "causes": ["connection refused"],
                    "duration_ms": 42
                }),
                None,
            ))
            .unwrap();
        let failed = serde_json::to_value(state.sandbox.as_ref().unwrap()).unwrap();
        assert_eq!(failed["kind"], "failed");
        assert_eq!(failed["failure"]["provider"], "docker");
        assert_eq!(failed["failure"]["error"], "Docker daemon unavailable");
        assert_eq!(failed["failure"]["causes"], json!(["connection refused"]));
        assert_eq!(failed["failure"]["duration_ms"], 42);
        assert!(failed.get("instance").is_none());
    }

    #[test]
    fn run_failed_before_sandbox_events_leaves_sandbox_planned() {
        let state = RunProjection::apply_events(&[
            test_raw_event(
                1,
                "run.created",
                &json!({
                    "settings": WorkflowSettings::default(),
                    "graph": Graph::new("test"),
                    "labels": {},
                    "provenance": test_support::test_run_provenance()
                }),
                None,
            ),
            test_raw_event(
                2,
                "run.runnable",
                &json!({ "source": "start_requested" }),
                None,
            ),
            test_raw_event(3, "run.starting", &json!({}), None),
            test_raw_event(4, "run.running", &json!({}), None),
            test_raw_event(
                5,
                "run.failed",
                &json!({
                    "failure": {
                        "reason": "sandbox_init_failed",
                        "detail": {
                            "message": "Failed before sandbox initialized",
                            "category": "transient_infra"
                        }
                    },
                    "timing": {
                        "wall_time_ms": 1,
                        "inference_time_ms": 0,
                        "tool_time_ms": 0,
                        "active_time_ms": 0
                    }
                }),
                None,
            ),
        ])
        .unwrap();

        let sandbox = serde_json::to_value(state.sandbox.as_ref().unwrap()).unwrap();
        assert_eq!(sandbox["kind"], "planned");
        assert!(sandbox.get("instance").is_none());
        assert!(sandbox.get("failure").is_none());
    }

    fn test_raw_event(
        seq: u32,
        event: &str,
        properties: &serde_json::Value,
        node_id: Option<&str>,
    ) -> EventEnvelope {
        EventEnvelope {
            seq,
            event: RunEvent::from_value(json!({
                "id": format!("evt-{seq}"),
                "ts": Utc::now().to_rfc3339(),
                "run_id": fixtures::RUN_1,
                "event": event,
                "node_id": node_id,
                "properties": properties,
            }))
            .unwrap(),
        }
    }

    fn test_raw_event_at(
        seq: u32,
        ts: &str,
        event: &str,
        properties: &serde_json::Value,
        node_id: Option<&str>,
    ) -> EventEnvelope {
        EventEnvelope {
            seq,
            event: RunEvent::from_value(json!({
                "id": format!("evt-{seq}"),
                "ts": ts,
                "run_id": fixtures::RUN_1,
                "event": event,
                "node_id": node_id,
                "properties": properties,
            }))
            .unwrap(),
        }
    }

    #[test]
    fn live_run_timing_returns_none_before_run_starts() {
        let state = initialized_projection();

        assert_eq!(state.live_run_timing(Utc::now()), None);
    }

    #[test]
    fn live_run_timing_derives_wall_and_completed_stage_active_for_in_flight_run() {
        let mut state = initialized_projection();
        state
            .apply_event(&test_raw_event_at(
                1,
                "2026-04-07T12:00:00Z",
                "run.started",
                &json!({ "name": "Test run" }),
                None,
            ))
            .unwrap();
        state.stage_entry("plan", 1, first_event_seq(2)).timing =
            Some(fabro_types::StageTiming::new(2_000, 700, 300));
        state.stage_entry("code", 1, first_event_seq(3)).timing =
            Some(fabro_types::StageTiming::new(3_000, 20, 80));
        state.stage_entry("running", 1, first_event_seq(4)).timing = None;

        assert_eq!(
            state.live_run_timing(test_dt("2026-04-07T12:00:12.345Z")),
            Some(fabro_types::RunTiming::new(12_345, 720, 380))
        );
    }

    #[test]
    fn live_run_timing_matches_conclusion_timing_at_conclusion_moment() {
        let mut state = initialized_projection();
        let started_at = test_dt("2026-04-07T12:00:00Z");
        let completed_at = test_dt("2026-04-07T12:00:10Z");
        state
            .apply_event(&test_raw_event_at(
                1,
                "2026-04-07T12:00:00Z",
                "run.started",
                &json!({ "name": "Test run" }),
                None,
            ))
            .unwrap();
        state
            .apply_event(&test_raw_event_at(
                2,
                "2026-04-07T12:00:00Z",
                "run.runnable",
                &json!({ "source": "start_requested" }),
                None,
            ))
            .unwrap();
        state
            .apply_event(&test_raw_event_at(
                3,
                "2026-04-07T12:00:00Z",
                "run.starting",
                &json!({}),
                None,
            ))
            .unwrap();
        state
            .apply_event(&test_raw_event_at(
                4,
                "2026-04-07T12:00:01Z",
                "run.running",
                &json!({}),
                None,
            ))
            .unwrap();
        state.stage_entry("plan", 1, first_event_seq(5)).timing =
            Some(fabro_types::StageTiming::new(2_000, 700, 300));
        state.stage_entry("code", 1, first_event_seq(6)).timing =
            Some(fabro_types::StageTiming::new(3_000, 50, 200));

        let conclusion_timing = fabro_types::RunTiming::new(
            u64::try_from(
                completed_at
                    .signed_duration_since(started_at)
                    .num_milliseconds(),
            )
            .unwrap(),
            750,
            500,
        );
        let mut completed = test_event(
            7,
            EventBody::RunCompleted(RunCompletedProps {
                timing:               conclusion_timing,
                artifact_count:       0,
                status:               "succeeded".to_string(),
                reason:               SuccessReason::Completed,
                total_usd_micros:     None,
                final_git_commit_sha: None,
                final_patch:          None,
                diff_summary:         None,
                billing:              None,
            }),
            None,
        );
        completed.event.ts = completed_at;
        state.apply_event(&completed).unwrap();

        assert_eq!(
            state
                .conclusion
                .as_ref()
                .map(|conclusion| conclusion.timing),
            Some(conclusion_timing)
        );
        assert_eq!(state.live_run_timing(completed_at), Some(conclusion_timing));
    }

    #[test]
    fn last_event_at_tracks_most_recent_event_timestamp() {
        let mut state = initialized_projection();
        let later = test_raw_event_at(
            2,
            "2026-04-20T12:05:30Z",
            "run.start_requested",
            &json!({ "resume": false }),
            None,
        );

        state.apply_event(&later).unwrap();

        assert_eq!(state.last_event_at, later.event.ts);
    }

    #[test]
    fn deserialize_and_round_trip_projection_preserves_stages_and_pending_control() {
        let state: RunProjection = serde_json::from_value(serde_json::json!({
            "spec": {
                "run_id": "01JW6A7VNFZSFF0SKXJG29Z2M3",
                "settings": WorkflowSettings::default(),
                "graph": { "name": "ship", "nodes": {}, "edges": [], "attrs": {} },
                "workflow_slug": "demo",
                "source_directory": "/tmp/project",
                "repo_origin_url": null,
                "base_branch": null,
                "labels": {},
                "provenance": test_support::test_run_provenance(),
                "manifest_blob": null,
                "definition_blob": null,
                "git": null,
                "fork_source_ref": null
            },
            "status": { "kind": "submitted" },
            "status_updated_at": "2026-04-07T12:00:00Z",
            "last_event_at": "2026-04-07T12:00:00Z",
            "pending_control": "cancel",
            "checkpoints": [
                {
                    "seq": 0,
                    "diff": {},
                    "checkpoint": {
                        "timestamp": "2026-04-07T12:00:00Z",
                        "current_node": "build",
                        "completed_nodes": ["build"],
                        "node_retries": {},
                        "context_values": {},
                        "node_outcomes": {},
                        "loop_failure_signatures": {},
                        "restart_failure_signatures": {},
                        "node_visits": { "build": 2 }
                    }
                }
            ],
            "pending_interviews": {},
            "stages": {
                "build@2": {
                    "first_event_seq": 1,
                    "diff": "diff --git a/file b/file",
                    "output": "done",
                    "usage": {
                        "input_tokens": 0,
                        "output_tokens": 0,
                        "total_tokens": 0,
                        "reasoning_tokens": 0,
                        "cache_read_tokens": 0,
                        "cache_write_tokens": 0
                    },
                    "state": "running"
                }
            }
        }))
        .unwrap();

        let stage_id = StageId::new("build", 2);
        let node = state.stage(&stage_id).unwrap();
        assert_eq!(node.first_event_seq, first_event_seq(1));
        assert_eq!(node.diff.as_deref(), Some("diff --git a/file b/file"));
        assert_eq!(state.list_node_visits("build"), vec![2]);
        assert_eq!(state.pending_control, Some(RunControlAction::Cancel));

        let round_tripped: RunProjection =
            serde_json::from_value(serde_json::to_value(&state).unwrap()).unwrap();
        let serialized = serde_json::to_value(&state).unwrap();
        let round_tripped_node = round_tripped.stage(&stage_id).unwrap();
        assert_eq!(round_tripped_node.output.as_deref(), Some("done"));
        assert_eq!(round_tripped.list_node_visits("build"), vec![2]);
        assert_eq!(
            round_tripped.pending_control,
            Some(RunControlAction::Cancel)
        );
        assert!(serialized.get("spec").is_some());
        assert!(serialized.get("run").is_none());
    }

    #[test]
    fn stage_entry_round_trips_through_json() {
        let mut state = running_projection();
        state.pending_control = Some(RunControlAction::Unpause);
        state.checkpoints = vec![CheckpointRecord {
            seq:        7,
            checkpoint: Checkpoint {
                timestamp:                  "2026-04-07T12:00:00Z".parse().unwrap(),
                current_node:               "build".to_string(),
                completed_nodes:            vec!["build".to_string()],
                node_retries:               HashMap::new(),
                context_values:             HashMap::new(),
                node_outcomes:              HashMap::new(),
                next_node_id:               None,
                git_commit_sha:             None,
                loop_failure_signatures:    HashMap::new(),
                restart_failure_signatures: HashMap::new(),
                node_visits:                HashMap::from([("build".to_string(), 2usize)]),
            },
            diff:       RunDiff::default(),
        }];
        state.stage_entry("build", 2, first_event_seq(7)).output = Some("done".to_string());

        let round_tripped: RunProjection =
            serde_json::from_value(serde_json::to_value(&state).unwrap()).unwrap();

        assert_eq!(
            round_tripped
                .stage(&StageId::new("build", 2))
                .unwrap()
                .output
                .as_deref(),
            Some("done")
        );
        assert_eq!(round_tripped.list_node_visits("build"), vec![2]);
        assert_eq!(
            round_tripped.pending_control,
            Some(RunControlAction::Unpause)
        );
    }

    #[test]
    fn stage_started_sets_first_event_seq() {
        let mut state = initialized_projection();
        let stage_id = StageId::new("build", 1);

        state
            .apply_event(&test_stage_event(
                3,
                EventBody::StageStarted(StageStartedProps {
                    graph_visit:           None,
                    resumed_from_stage_id: None,
                    index:                 0,
                    handler_type:          "agent".to_string(),
                    attempt:               1,
                    max_attempts:          1,
                }),
                stage_id.clone(),
            ))
            .unwrap();

        let stage = state.stage(&stage_id).unwrap();
        assert_eq!(stage.first_event_seq, first_event_seq(3));
    }

    #[test]
    fn later_stage_events_do_not_overwrite_first_event_seq() {
        let mut state = initialized_projection();
        let stage_id = StageId::new("build", 1);

        state
            .apply_event(&test_stage_event(
                3,
                EventBody::StageStarted(StageStartedProps {
                    graph_visit:           None,
                    resumed_from_stage_id: None,
                    index:                 0,
                    handler_type:          "agent".to_string(),
                    attempt:               1,
                    max_attempts:          1,
                }),
                stage_id.clone(),
            ))
            .unwrap();
        state
            .apply_event(&test_event(
                4,
                EventBody::StagePrompt(StagePromptProps {
                    visit:            1,
                    text:             "prompt".to_string(),
                    mode:             None,
                    provider:         None,
                    model:            None,
                    reasoning_effort: None,
                    speed:            None,
                }),
                Some("build"),
            ))
            .unwrap();

        let stage = state.stage(&stage_id).unwrap();
        assert_eq!(stage.first_event_seq, first_event_seq(3));
        assert_eq!(stage.prompt.as_deref(), Some("prompt"));
    }

    #[test]
    fn parallel_branch_started_projects_identity_without_churning_it() {
        let mut state = initialized_projection();
        let group_id = StageId::new("review_fork", 1);
        let branch_stage_id = StageId::new("review_glm", 1);
        let branch_id = ParallelBranchId::new(group_id.clone(), 0);
        let started = test_branch_event(
            3,
            EventBody::ParallelBranchStarted(ParallelBranchStartedProps {
                index:                 0,
                item_label:            None,
                graph_visit:           Some(1),
                resumed_from_stage_id: None,
            }),
            branch_stage_id.clone(),
            branch_id.clone(),
        );

        state.apply_event(&started).unwrap();

        assert_eq!(
            state
                .stage(&branch_stage_id)
                .unwrap()
                .parallel_branch_id
                .as_ref(),
            Some(&branch_id)
        );

        let reobserved = test_branch_event(
            4,
            EventBody::ParallelBranchStarted(ParallelBranchStartedProps {
                index:                 1,
                item_label:            None,
                graph_visit:           Some(2),
                resumed_from_stage_id: Some(StageId::new("review_glm", 2)),
            }),
            branch_stage_id.clone(),
            ParallelBranchId::new(group_id, 1),
        );

        state.apply_event(&reobserved).unwrap();

        let stage = state.stage(&branch_stage_id).unwrap();
        assert_eq!(stage.parallel_branch_id.as_ref(), Some(&branch_id));
        assert_eq!(stage.graph_visit, Some(1));
        assert!(stage.resumed_from_stage_id.is_none());
    }

    #[test]
    fn parallel_branch_completed_finalizes_branch_stage() {
        // A parallel branch never runs through the engine's StageStarted/
        // StageCompleted lifecycle: its stage entry is created Running by the
        // first branch-scoped event, and only ParallelBranchCompleted marks it
        // terminal. Guards against branches spinning Running forever.
        let mut state = initialized_projection();
        let branch = StageId::new("review_ux", 1);
        let branch_started_at = test_dt("2026-04-07T12:00:00Z");

        state
            .apply_event(&test_stage_event_at(
                3,
                "2026-04-07T12:00:00Z",
                EventBody::ParallelBranchStarted(ParallelBranchStartedProps {
                    index:                 0,
                    item_label:            None,
                    graph_visit:           None,
                    resumed_from_stage_id: None,
                }),
                branch.clone(),
            ))
            .unwrap();
        let stage = state.stage(&branch).unwrap();
        assert_eq!(stage.state, StageState::Running);
        assert_eq!(stage.started_at, Some(branch_started_at));

        state
            .apply_event(&test_stage_event(
                4,
                EventBody::ParallelBranchCompleted(ParallelBranchCompletedProps {
                    index:       0,
                    item_label:  None,
                    duration_ms: 1234,
                    status:      StageOutcome::Succeeded,
                }),
                branch.clone(),
            ))
            .unwrap();

        let stage = state.stage(&branch).unwrap();
        assert_eq!(stage.state, StageState::Succeeded);
        assert_eq!(stage.timing.unwrap().wall_time_ms, 1234);
        assert_eq!(
            stage.completion.as_ref().unwrap().outcome,
            StageOutcome::Succeeded
        );
    }

    #[test]
    fn parallel_branch_completed_folds_failed_status_as_failed() {
        let mut state = initialized_projection();
        let branch = StageId::new("review_ux", 1);

        state
            .apply_event(&test_stage_event(
                3,
                EventBody::ParallelBranchStarted(ParallelBranchStartedProps {
                    index:                 0,
                    item_label:            None,
                    graph_visit:           None,
                    resumed_from_stage_id: None,
                }),
                branch.clone(),
            ))
            .unwrap();
        state
            .apply_event(&test_stage_event(
                4,
                EventBody::ParallelBranchCompleted(ParallelBranchCompletedProps {
                    index:       0,
                    item_label:  None,
                    duration_ms: 500,
                    status:      StageOutcome::Failed {
                        retry_requested: false,
                    },
                }),
                branch.clone(),
            ))
            .unwrap();

        let stage = state.stage(&branch).unwrap();
        assert_eq!(stage.state, StageState::Failed);
        assert_eq!(stage.timing.unwrap().wall_time_ms, 500);
        assert_eq!(
            stage.completion.as_ref().unwrap().outcome,
            StageOutcome::Failed {
                retry_requested: false,
            }
        );
    }

    #[test]
    fn parallel_branch_started_uses_the_graph_handler_for_live_timing() {
        for (handler_type, handler, expected) in [
            (
                "prompt",
                StageHandler::Prompt,
                StageTiming::new(5_000, 5_000, 0),
            ),
            (
                "command",
                StageHandler::Command,
                StageTiming::new(5_000, 0, 5_000),
            ),
        ] {
            let mut spec = test_run_spec();
            let mut node = Node::new("review");
            node.attrs.insert(
                "type".to_string(),
                AttrValue::String(handler_type.to_string()),
            );
            spec.graph.nodes.insert(node.id.clone(), node);
            let mut state = RunProjection::new("Test run".to_string(), spec, Utc::now());
            let branch = StageId::new("review", 1);

            state
                .apply_event(&test_stage_event_at(
                    3,
                    "2026-04-07T12:00:00Z",
                    EventBody::ParallelBranchStarted(ParallelBranchStartedProps {
                        index:                 0,
                        item_label:            None,
                        graph_visit:           None,
                        resumed_from_stage_id: None,
                    }),
                    branch.clone(),
                ))
                .unwrap();

            let stage = state.stage(&branch).unwrap();
            assert_eq!(stage.handler, Some(handler));
            assert_eq!(stage.live_timing(test_dt("2026-04-07T12:00:05Z")), expected);
        }
    }

    fn start_stage(state: &mut RunProjection, stage_id: &StageId) {
        state
            .apply_event(&test_stage_event(
                3,
                EventBody::StageStarted(StageStartedProps {
                    graph_visit:           None,
                    resumed_from_stage_id: None,
                    index:                 0,
                    handler_type:          "agent".to_string(),
                    attempt:               1,
                    max_attempts:          1,
                }),
                stage_id.clone(),
            ))
            .unwrap();
    }

    #[test]
    fn agent_session_activated_updates_stage_provider_used() {
        let mut state = initialized_projection();
        let stage_id = StageId::new("code", 1);
        start_stage(&mut state, &stage_id);

        state
            .apply_event(&test_stage_event(
                4,
                EventBody::AgentSessionActivated(AgentSessionActivatedProps {
                    thread_id:        Some("thread-1".to_string()),
                    provider:         Some("openai".to_string()),
                    model:            Some("gpt-5.4".to_string()),
                    reasoning_effort: Some(ReasoningEffort::High),
                    speed:            Some(Speed::Fast),
                    permission_level: None,
                    capabilities:     vec![fabro_types::SessionCapability::Steer],
                    visit:            1,
                }),
                stage_id.clone(),
            ))
            .unwrap();

        let stage = state.stage(&stage_id).unwrap();
        let provider_used = stage.provider_used.as_ref().unwrap();
        assert_eq!(provider_used.mode, StageModelUsage::MODE_AGENT);
        assert_eq!(provider_used.provider.as_deref(), Some("openai"));
        assert_eq!(provider_used.model.as_deref(), Some("gpt-5.4"));
        assert_eq!(provider_used.reasoning_effort, Some(ReasoningEffort::High));
        assert_eq!(provider_used.speed, Some(Speed::Fast));
    }

    #[test]
    fn object_lifecycle_session_events_do_not_update_stage_provider_used() {
        let mut state = initialized_projection();
        let stage_id = StageId::new("code", 1);
        start_stage(&mut state, &stage_id);

        state
            .apply_event(&test_event(
                4,
                EventBody::AgentSessionStarted(AgentSessionStartedProps {
                    provider: Some("openai".to_string()),
                    model:    Some("gpt-5.4".to_string()),
                }),
                None,
            ))
            .unwrap();
        state
            .apply_event(&test_event(
                5,
                EventBody::AgentSessionEnded(AgentSessionEndedProps {}),
                None,
            ))
            .unwrap();

        let stage = state.stage(&stage_id).unwrap();
        assert!(stage.provider_used.is_none());
    }

    #[test]
    fn agent_acp_started_alone_leaves_stage_provider_used_unset() {
        // `agent.acp.started` no longer writes `provider_used`; the canonical
        // source is the subsequent `agent.session.activated` event. ACP runs
        // without a steering hub never activate and so legitimately leave
        // `provider_used` unset.
        let mut state = initialized_projection();
        let stage_id = StageId::new("code", 1);
        start_stage(&mut state, &stage_id);

        state
            .apply_event(&test_stage_event(
                4,
                EventBody::AgentAcpStarted(AgentAcpStartedProps {
                    visit:       1,
                    command:     "python fake_agent.py".to_string(),
                    config_name: Some("fake".to_string()),
                }),
                stage_id.clone(),
            ))
            .unwrap();

        let stage = state.stage(&stage_id).unwrap();
        assert!(stage.provider_used.is_none());
    }

    #[test]
    fn acp_session_activation_records_provider_used_with_acp_mode() {
        let mut state = initialized_projection();
        let stage_id = StageId::new("code", 1);
        start_stage(&mut state, &stage_id);

        state
            .apply_event(&test_stage_event(
                4,
                EventBody::AgentAcpStarted(AgentAcpStartedProps {
                    visit:       1,
                    command:     "python fake_agent.py".to_string(),
                    config_name: Some("fake".to_string()),
                }),
                stage_id.clone(),
            ))
            .unwrap();
        state
            .apply_event(&test_stage_event(
                5,
                EventBody::AgentSessionActivated(AgentSessionActivatedProps {
                    thread_id:        None,
                    provider:         Some(AgentBackend::Acp.to_string()),
                    model:            Some("fake".to_string()),
                    reasoning_effort: None,
                    speed:            None,
                    permission_level: None,
                    capabilities:     vec![fabro_types::SessionCapability::Steer],
                    visit:            1,
                }),
                stage_id.clone(),
            ))
            .unwrap();

        let stage = state.stage(&stage_id).unwrap();
        let provider_used = stage.provider_used.as_ref().unwrap();
        assert_eq!(provider_used.mode, StageModelUsage::MODE_ACP);
        assert_eq!(provider_used.provider.as_deref(), Some("acp"));
        assert_eq!(provider_used.model.as_deref(), Some("fake"));
    }

    #[test]
    fn acp_events_accumulate_live_inference_time() {
        let mut state = initialized_projection();
        let stage_id = StageId::new("code", 1);

        state
            .apply_event(&test_stage_event_at(
                3,
                "2026-04-07T12:00:00Z",
                EventBody::StageStarted(StageStartedProps {
                    graph_visit:           None,
                    resumed_from_stage_id: None,
                    index:                 0,
                    handler_type:          "agent".to_string(),
                    attempt:               1,
                    max_attempts:          1,
                }),
                stage_id.clone(),
            ))
            .unwrap();
        state
            .apply_event(&test_stage_event_at(
                4,
                "2026-04-07T12:00:05Z",
                EventBody::AgentAcpStarted(AgentAcpStartedProps {
                    visit:       1,
                    command:     "python fake_agent.py".to_string(),
                    config_name: Some("fake".to_string()),
                }),
                stage_id.clone(),
            ))
            .unwrap();

        let stage = state.stage(&stage_id).unwrap();
        assert_eq!(
            stage.live_timing(test_dt("2026-04-07T12:00:15Z")),
            StageTiming::new(15_000, 10_000, 0)
        );

        state
            .apply_event(&test_stage_event_at(
                5,
                "2026-04-07T12:00:17Z",
                EventBody::AgentAcpCompleted(AgentAcpCompletedProps {
                    stdout:      "done".to_string(),
                    stderr:      String::new(),
                    stop_reason: "end_turn".to_string(),
                    duration_ms: 12_000,
                }),
                stage_id.clone(),
            ))
            .unwrap();

        let stage = state.stage(&stage_id).unwrap();
        assert_eq!(
            stage.live_timing(test_dt("2026-04-07T12:00:20Z")),
            StageTiming::new(20_000, 12_000, 0)
        );
    }

    #[test]
    fn agent_acp_completed_updates_stage_output_projection() {
        let mut state = initialized_projection();
        let stage_id = StageId::new("code", 1);
        start_stage(&mut state, &stage_id);

        state
            .apply_event(&test_stage_event(
                4,
                EventBody::AgentAcpCompleted(AgentAcpCompletedProps {
                    stdout:      "done".to_string(),
                    stderr:      "warn".to_string(),
                    stop_reason: "end_turn".to_string(),
                    duration_ms: 42,
                }),
                stage_id.clone(),
            ))
            .unwrap();

        let stage = state.stage(&stage_id).unwrap();
        assert_eq!(stage.output.as_deref(), Some("done\nwarn"));
        assert_eq!(stage.termination, Some(CommandTermination::Exited));
        assert_eq!(
            stage.script_timing.as_ref().unwrap()["stop_reason"],
            serde_json::json!("end_turn")
        );
    }

    #[test]
    fn agent_acp_cancelled_and_timed_out_update_terminal_projection() {
        let mut cancelled = initialized_projection();
        let cancelled_stage_id = StageId::new("cancelled", 1);
        start_stage(&mut cancelled, &cancelled_stage_id);

        cancelled
            .apply_event(&test_stage_event(
                4,
                EventBody::AgentAcpCancelled(AgentAcpCancelledProps {
                    stdout:      "partial".to_string(),
                    stderr:      "cancelled".to_string(),
                    duration_ms: 7,
                }),
                cancelled_stage_id.clone(),
            ))
            .unwrap();

        let stage = cancelled.stage(&cancelled_stage_id).unwrap();
        assert_eq!(stage.output.as_deref(), Some("partial\ncancelled"));
        assert_eq!(stage.termination, Some(CommandTermination::Cancelled));

        let mut timed_out = initialized_projection();
        let timed_out_stage_id = StageId::new("timed_out", 1);
        start_stage(&mut timed_out, &timed_out_stage_id);

        timed_out
            .apply_event(&test_stage_event(
                4,
                EventBody::AgentAcpTimedOut(AgentAcpTimedOutProps {
                    stdout:      "partial".to_string(),
                    stderr:      "timeout".to_string(),
                    duration_ms: 99,
                }),
                timed_out_stage_id.clone(),
            ))
            .unwrap();

        let stage = timed_out.stage(&timed_out_stage_id).unwrap();
        assert_eq!(stage.output.as_deref(), Some("partial\ntimeout"));
        assert_eq!(stage.termination, Some(CommandTermination::TimedOut));
    }

    #[test]
    fn stage_completed_event_captures_duration_and_usage_per_visit() {
        let mut state = initialized_projection();
        let usage = test_usage("gpt-5.2", 123, 45);

        state
            .apply_event(&test_event(
                3,
                EventBody::StageCompleted(StageCompletedProps {
                    index: 0,
                    timing: fabro_types::StageTiming::wall_only(789),
                    status: StageOutcome::Succeeded,
                    preferred_label: None,
                    suggested_next_ids: Vec::new(),
                    billing: Some(usage.clone()),
                    failure: None,
                    notes: None,
                    files_touched: Vec::new(),
                    context_updates: None,
                    jump_to_node: None,
                    context_values: None,
                    node_visits: None,
                    loop_failure_signatures: None,
                    restart_failure_signatures: None,
                    response: Some("done".to_string()),
                    attempt: 1,
                    max_attempts: 1,
                }),
                Some("build"),
            ))
            .unwrap();

        let stage = state.stage(&StageId::new("build", 1)).unwrap();
        assert_eq!(stage.timing.map(|t| t.wall_time_ms), Some(789));
        assert_eq!(stage.usage, usage_counts(&usage));
        assert_eq!(stage.model.as_ref(), Some(usage.model()));
    }

    #[test]
    fn stage_failed_event_captures_duration_and_usage_per_visit() {
        let mut state = initialized_projection();
        let stage_id = StageId::new("build", 1);
        let usage = test_usage("gpt-5.2", 321, 54);

        state
            .apply_event(&test_stage_event(
                2,
                EventBody::StageStarted(StageStartedProps {
                    graph_visit:           None,
                    resumed_from_stage_id: None,
                    index:                 0,
                    handler_type:          "agent".to_string(),
                    attempt:               1,
                    max_attempts:          1,
                }),
                stage_id.clone(),
            ))
            .unwrap();
        state
            .apply_event(&test_raw_event(
                3,
                "stage.failed",
                &json!({
                    "index": 0,
                    "failure": {
                        "message": "provider failed",
                        "category": "transient_infra"
                    },
                    "will_retry": false,
                    "timing": {"wall_time_ms": 654, "inference_time_ms": 0, "tool_time_ms": 0, "active_time_ms": 0},
                    "billing": usage_json(&usage)
                }),
                Some("build"),
            ))
            .unwrap();

        let stage = state.stage(&stage_id).unwrap();
        assert_eq!(stage.timing.map(|t| t.wall_time_ms), Some(654));
        assert_eq!(stage.usage, usage_counts(&usage));
        assert_eq!(stage.model.as_ref(), Some(usage.model()));
    }

    #[test]
    fn two_visits_of_one_node_retain_distinct_usage() {
        let mut state = initialized_projection();
        let first_usage = test_usage("gpt-5.2", 100, 10);
        let second_usage = test_usage("gpt-5.2", 200, 20);

        for (seq, visit, duration_ms, usage) in [
            (3, 1usize, 111, first_usage.clone()),
            (4, 2usize, 222, second_usage.clone()),
        ] {
            state
                .apply_event(&test_event(
                    seq,
                    EventBody::StageCompleted(StageCompletedProps {
                        index: 0,
                        timing: fabro_types::StageTiming::wall_only(duration_ms),
                        status: StageOutcome::Succeeded,
                        preferred_label: None,
                        suggested_next_ids: Vec::new(),
                        billing: Some(usage),
                        failure: None,
                        notes: None,
                        files_touched: Vec::new(),
                        context_updates: None,
                        jump_to_node: None,
                        context_values: None,
                        node_visits: Some(BTreeMap::from([("build".to_string(), visit)])),
                        loop_failure_signatures: None,
                        restart_failure_signatures: None,
                        response: None,
                        attempt: 1,
                        max_attempts: 1,
                    }),
                    Some("build"),
                ))
                .unwrap();
        }

        let first_stage = state.stage(&StageId::new("build", 1)).unwrap();
        let second_stage = state.stage(&StageId::new("build", 2)).unwrap();
        assert_eq!(first_stage.timing.map(|t| t.wall_time_ms), Some(111));
        assert_eq!(first_stage.usage, usage_counts(&first_usage));
        assert_eq!(first_stage.model.as_ref(), Some(first_usage.model()));
        assert_eq!(second_stage.timing.map(|t| t.wall_time_ms), Some(222));
        assert_eq!(second_stage.usage, usage_counts(&second_usage));
        assert_eq!(second_stage.model.as_ref(), Some(second_usage.model()));
    }

    #[test]
    fn stage_completed_prefers_stored_stage_id_over_legacy_node_visits() {
        let mut state = initialized_projection();
        let usage = test_usage("gpt-5.2", 300, 30);
        let scoped_stage_id = StageId::new("build", 2);

        state
            .apply_event(&test_stage_event(
                3,
                EventBody::StageCompleted(StageCompletedProps {
                    index: 0,
                    timing: fabro_types::StageTiming::wall_only(333),
                    status: StageOutcome::Succeeded,
                    preferred_label: None,
                    suggested_next_ids: Vec::new(),
                    billing: Some(usage.clone()),
                    failure: None,
                    notes: None,
                    files_touched: Vec::new(),
                    context_updates: None,
                    jump_to_node: None,
                    context_values: None,
                    node_visits: Some(BTreeMap::from([("build".to_string(), 1usize)])),
                    loop_failure_signatures: None,
                    restart_failure_signatures: None,
                    response: Some("done".to_string()),
                    attempt: 1,
                    max_attempts: 1,
                }),
                scoped_stage_id.clone(),
            ))
            .unwrap();

        assert!(
            state.stage(&StageId::new("build", 1)).is_none(),
            "legacy node_visits must not override stored stage_id"
        );
        let stage = state.stage(&scoped_stage_id).unwrap();
        assert_eq!(stage.timing.map(|t| t.wall_time_ms), Some(333));
        assert_eq!(stage.usage, usage_counts(&usage));
        assert_eq!(stage.model.as_ref(), Some(usage.model()));
        assert_eq!(stage.response.as_deref(), Some("done"));
    }

    #[test]
    fn stage_failed_prefers_stored_stage_id_and_preserves_retry_request() {
        let mut state = initialized_projection();
        let usage = test_usage("gpt-5.2", 400, 40);
        let scoped_stage_id = StageId::new("build", 2);

        state
            .apply_event(&test_stage_event(
                3,
                EventBody::StageFailed(StageFailedProps {
                    index:      0,
                    failure:    Some(fabro_types::FailureDetail::new(
                        "try again",
                        fabro_types::FailureCategory::TransientInfra,
                    )),
                    will_retry: true,
                    timing:     fabro_types::StageTiming::wall_only(444),
                    billing:    Some(usage.clone()),
                }),
                scoped_stage_id.clone(),
            ))
            .unwrap();

        assert!(
            state.stage(&StageId::new("build", 1)).is_none(),
            "current-visit fallback must not override stored stage_id"
        );
        let stage = state.stage(&scoped_stage_id).unwrap();
        assert_eq!(stage.timing.map(|t| t.wall_time_ms), Some(444));
        assert_eq!(stage.usage, usage_counts(&usage));
        assert_eq!(stage.model.as_ref(), Some(usage.model()));
        let completion = stage.completion.as_ref().unwrap();
        assert_eq!(completion.outcome, StageOutcome::Failed {
            retry_requested: true,
        });
        assert_eq!(completion.failure_reason.as_deref(), Some("try again"));
    }

    #[test]
    fn checkpoint_completed_creates_projection_entry_for_skipped_stage() {
        let mut state = initialized_projection();
        let stage_id = StageId::new("skip_me", 1);

        state
            .apply_event(&test_event(
                5,
                EventBody::CheckpointCompleted(CheckpointCompletedProps {
                    graph_visit: None,
                    resumed_from_stage_id: None,
                    status: "running".to_string(),
                    current_node: "next".to_string(),
                    completed_nodes: vec!["skip_me".to_string()],
                    node_retries: BTreeMap::new(),
                    context_values: BTreeMap::new(),
                    node_outcomes: BTreeMap::from([(
                        "skip_me".to_string(),
                        Outcome::skipped("condition was false"),
                    )]),
                    next_node_id: Some("next".to_string()),
                    git_commit_sha: None,
                    loop_failure_signatures: BTreeMap::new(),
                    restart_failure_signatures: BTreeMap::new(),
                    node_visits: BTreeMap::from([("skip_me".to_string(), 1usize)]),
                    diff: None,
                    diff_summary: None,
                }),
                None,
            ))
            .unwrap();

        let stage = state.stage(&stage_id).unwrap();
        assert_eq!(stage.first_event_seq, first_event_seq(5));
        let completion = stage.completion.as_ref().unwrap();
        assert_eq!(completion.outcome, StageOutcome::Skipped);
        assert_eq!(completion.notes.as_deref(), Some("condition was false"));
    }

    #[test]
    fn interview_events_populate_and_clear_pending_interviews() {
        let mut state = initialized_projection();
        state
            .apply_event(&test_event(
                1,
                EventBody::InterviewStarted(InterviewStartedProps {
                    question_id:     "q-1".to_string(),
                    question:        "Approve deploy?".to_string(),
                    stage:           "gate".to_string(),
                    question_type:   "multiple_choice".to_string(),
                    options:         vec![
                        InterviewOption {
                            key:         "approve".to_string(),
                            label:       "Approve".to_string(),
                            description: Some("Ship it".to_string()),
                            preview:     Some("deploy --prod".to_string()),
                        },
                        InterviewOption {
                            key:         "revise".to_string(),
                            label:       "Revise".to_string(),
                            description: None,
                            preview:     None,
                        },
                    ],
                    allow_freeform:  true,
                    timeout_seconds: Some(30.0),
                    context_display: Some("Latest draft".to_string()),
                    review_target:   Some(
                        fabro_types::ReviewTarget::new(
                            "Quarry review exercise",
                            "https://quarry.lithos.computer/tmp/0123456789abcdef0123456789abcdef",
                            fabro_types::ReviewTargetKind::Document,
                        )
                        .unwrap(),
                    ),
                }),
                Some("gate"),
            ))
            .unwrap();

        let pending = state
            .pending_interviews
            .get("q-1")
            .expect("pending interview should be present");
        assert_eq!(pending.question.id, "q-1");
        assert_eq!(pending.question.stage, "gate");
        assert_eq!(pending.question.question_type, QuestionType::MultipleChoice);
        assert_eq!(pending.question.options.len(), 2);
        assert_eq!(
            pending.question.options[0].description.as_deref(),
            Some("Ship it")
        );
        assert_eq!(
            pending.question.options[0].preview.as_deref(),
            Some("deploy --prod")
        );
        assert!(pending.question.allow_freeform);
        assert_eq!(pending.question.timeout_seconds, Some(30.0));
        assert_eq!(
            pending.question.context_display.as_deref(),
            Some("Latest draft")
        );
        assert_eq!(
            pending
                .question
                .review_target
                .as_ref()
                .map(fabro_types::ReviewTarget::url),
            Some("https://quarry.lithos.computer/tmp/0123456789abcdef0123456789abcdef")
        );

        state
            .apply_event(&test_event(
                2,
                EventBody::InterviewCompleted(InterviewCompletedProps {
                    question_id: "q-1".to_string(),
                    question:    "Approve deploy?".to_string(),
                    answer:      "approve".to_string(),
                    duration_ms: 42,
                }),
                Some("gate"),
            ))
            .unwrap();

        assert!(
            state.pending_interviews.is_empty(),
            "completed interview should clear pending state"
        );
    }

    #[test]
    fn pending_runnable_and_blocked_events_drive_projection_and_summary_fields() {
        let mut state = initialized_projection();

        state
            .apply_event(&test_raw_event(
                1,
                "run.pending",
                &json!({ "reason": "approval_required" }),
                None,
            ))
            .unwrap();
        assert_eq!(state.status(), RunStatus::Pending {
            reason: PendingReason::ApprovalRequired,
        });
        assert_eq!(
            state.approval.as_ref().map(|approval| approval.state),
            Some(RunApprovalState::Pending)
        );

        state
            .apply_event(&test_raw_event(2, "run.approved", &json!({}), None))
            .unwrap();
        state
            .apply_event(&test_raw_event(
                3,
                "run.runnable",
                &json!({ "source": "approved" }),
                None,
            ))
            .unwrap();
        assert_eq!(state.status(), RunStatus::Runnable);
        assert_eq!(
            state.approval.as_ref().map(|approval| approval.state),
            Some(RunApprovalState::Approved)
        );

        state
            .apply_event(&test_raw_event(4, "run.starting", &json!({}), None))
            .unwrap();
        state
            .apply_event(&test_raw_event(5, "run.running", &json!({}), None))
            .unwrap();
        state
            .apply_event(&test_event(
                6,
                EventBody::RunPaused(RunControlEffectProps::default()),
                None,
            ))
            .unwrap();
        state
            .apply_event(&test_raw_event(
                7,
                "run.blocked",
                &json!({ "blocked_reason": "human_input_required" }),
                None,
            ))
            .unwrap();

        let status_json = serde_json::to_value(state.status()).unwrap();
        assert_eq!(state.status(), RunStatus::Paused {
            prior_block: Some(BlockedReason::HumanInputRequired),
        });
        assert_eq!(
            status_json,
            json!({
                "kind": "paused",
                "prior_block": "human_input_required"
            })
        );

        let summary = build_summary(&state, &fixtures::RUN_1);
        let summary_json = serde_json::to_value(summary).unwrap();
        assert_eq!(
            summary_json["lifecycle"]["status"],
            json!({
                "kind": "paused",
                "prior_block": "human_input_required"
            })
        );
        assert_eq!(
            summary_json["lifecycle"]["approval"]["state"],
            json!("approved")
        );
    }

    #[test]
    fn approval_denial_projection_records_decision_then_failure() {
        let mut state = initialized_projection();

        state
            .apply_event(&test_raw_event_at(
                1,
                "2026-05-23T12:00:00Z",
                "run.start_requested",
                &json!({ "resume": false }),
                None,
            ))
            .unwrap();
        assert_eq!(state.status(), RunStatus::Submitted);
        assert!(state.approval.is_none());

        state
            .apply_event(&test_raw_event_at(
                2,
                "2026-05-23T12:00:01Z",
                "run.pending",
                &json!({ "reason": "approval_required" }),
                None,
            ))
            .unwrap();
        let approval = state.approval.as_ref().expect("approval should be pending");
        assert_eq!(state.status(), RunStatus::Pending {
            reason: PendingReason::ApprovalRequired,
        });
        assert_eq!(approval.state, RunApprovalState::Pending);
        assert_eq!(
            approval.requested_at.to_rfc3339(),
            "2026-05-23T12:00:01+00:00"
        );
        assert_eq!(approval.decided_at, None);

        state
            .apply_event(&test_raw_event_at(
                3,
                "2026-05-23T12:00:02Z",
                "run.denied",
                &json!({ "reason": "Not approved for execution" }),
                None,
            ))
            .unwrap();
        let approval = state.approval.as_ref().expect("approval should be denied");
        assert_eq!(state.status(), RunStatus::Pending {
            reason: PendingReason::ApprovalRequired,
        });
        assert_eq!(approval.state, RunApprovalState::Denied);
        assert_eq!(
            approval.denial_reason.as_deref(),
            Some("Not approved for execution")
        );
        assert_eq!(
            approval.decided_at.map(|ts| ts.to_rfc3339()).as_deref(),
            Some("2026-05-23T12:00:02+00:00")
        );

        state
            .apply_event(&test_raw_event(
                4,
                "run.failed",
                &json!({
                    "failure": {
                        "reason": "approval_denied",
                        "detail": {
                            "message": "Not approved for execution",
                            "category": "deterministic"
                        }
                    },
                    "timing": {
                        "wall_time_ms": 0,
                        "inference_time_ms": 0,
                        "tool_time_ms": 0,
                        "active_time_ms": 0
                    }
                }),
                None,
            ))
            .unwrap();

        assert_eq!(state.status(), RunStatus::Failed {
            reason: FailureReason::ApprovalDenied,
        });
        let summary_json = serde_json::to_value(build_summary(&state, &fixtures::RUN_1)).unwrap();
        assert_eq!(
            summary_json["lifecycle"]["approval"],
            json!({
                "state": "denied",
                "requested_at": "2026-05-23T12:00:01Z",
                "decided_at": "2026-05-23T12:00:02Z",
                "denial_reason": "Not approved for execution"
            })
        );
        assert_eq!(
            summary_json["lifecycle"]["status"],
            json!({ "kind": "failed", "reason": "approval_denied" })
        );
    }

    #[test]
    fn runnable_projection_without_approval_has_null_summary_approval() {
        let mut state = initialized_projection();
        state
            .apply_event(&test_raw_event(
                1,
                "run.runnable",
                &json!({ "source": "start_requested" }),
                None,
            ))
            .unwrap();

        assert_eq!(state.status(), RunStatus::Runnable);
        assert!(state.approval.is_none());
        let summary_json = serde_json::to_value(build_summary(&state, &fixtures::RUN_1)).unwrap();
        assert_eq!(
            summary_json["lifecycle"]["status"],
            json!({ "kind": "runnable" })
        );
        assert!(summary_json["lifecycle"]["approval"].is_null());
    }

    #[test]
    fn run_unblocked_clears_blocked_reason_and_restores_running() {
        let mut state = running_projection();

        state
            .apply_event(&test_raw_event(
                1,
                "run.blocked",
                &json!({ "blocked_reason": "human_input_required" }),
                None,
            ))
            .unwrap();
        state
            .apply_event(&test_raw_event(2, "run.unblocked", &json!({}), None))
            .unwrap();

        assert_eq!(state.status(), RunStatus::Running);
        let status_json = serde_json::to_value(state.status()).unwrap();
        assert_eq!(status_json, json!({ "kind": "running" }));
    }

    #[test]
    fn run_unblocked_while_paused_clears_blocked_reason_without_changing_paused_status() {
        let mut state = running_projection();

        state
            .apply_event(&test_raw_event(
                1,
                "run.blocked",
                &json!({ "blocked_reason": "human_input_required" }),
                None,
            ))
            .unwrap();
        state
            .apply_event(&test_event(
                2,
                EventBody::RunPaused(RunControlEffectProps::default()),
                None,
            ))
            .unwrap();
        state
            .apply_event(&test_raw_event(3, "run.unblocked", &json!({}), None))
            .unwrap();

        assert_eq!(state.status(), RunStatus::Paused { prior_block: None });
        let status_json = serde_json::to_value(state.status()).unwrap();
        assert_eq!(
            status_json,
            json!({
                "kind": "paused",
                "prior_block": null
            })
        );
    }

    #[test]
    fn unpause_to_still_blocked_yields_visible_blocked_after_event_sequence() {
        let mut state = running_projection();

        state
            .apply_event(&test_raw_event(
                1,
                "run.blocked",
                &json!({ "blocked_reason": "human_input_required" }),
                None,
            ))
            .unwrap();
        state
            .apply_event(&test_event(
                2,
                EventBody::RunPaused(RunControlEffectProps::default()),
                None,
            ))
            .unwrap();
        state
            .apply_event(&test_event(
                3,
                EventBody::RunUnpaused(RunControlEffectProps::default()),
                None,
            ))
            .unwrap();
        state
            .apply_event(&test_raw_event(
                4,
                "run.blocked",
                &json!({ "blocked_reason": "human_input_required" }),
                None,
            ))
            .unwrap();

        assert_eq!(state.status(), RunStatus::Blocked {
            blocked_reason: BlockedReason::HumanInputRequired,
        });
        let status_json = serde_json::to_value(state.status()).unwrap();
        assert_eq!(
            status_json,
            json!({
                "kind": "blocked",
                "blocked_reason": "human_input_required"
            })
        );
    }

    #[test]
    fn summary_synthesizes_submitted_when_run_exists_without_status() {
        let mut state = initialized_projection();
        state.spec = fabro_types::RunSpec {
            workflow_slug: Some("test".to_string()),
            source_directory: Some("/tmp/repo".to_string()),
            ..test_support::test_run_spec()
        };

        let summary_json = serde_json::to_value(build_summary(&state, &fixtures::RUN_1)).unwrap();
        assert_eq!(
            summary_json["lifecycle"]["status"],
            json!({ "kind": "submitted" })
        );
    }

    #[test]
    fn summary_preserves_absent_workflow_name_and_reports_graph_name() {
        let mut state = initialized_projection();
        state.spec = fabro_types::RunSpec {
            graph: fabro_types::Graph::new("GraphName"),
            workflow_slug: Some("release-flow".to_string()),
            source_directory: Some("/tmp/repo".to_string()),
            ..test_support::test_run_spec()
        };

        let summary = build_summary(&state, &fixtures::RUN_1);

        assert_eq!(summary.workflow.name, None);
        assert_eq!(summary.workflow.graph_name.as_deref(), Some("GraphName"));
        assert_eq!(summary.workflow.slug.as_deref(), Some("release-flow"));
    }

    #[test]
    fn summary_uses_explicit_workflow_name() {
        let mut state = initialized_projection();
        state.spec.settings.workflow.name = Some("Ship workflow".to_string());

        let summary = build_summary(&state, &fixtures::RUN_1);

        assert_eq!(summary.workflow.name.as_deref(), Some("Ship workflow"));
        assert_eq!(summary.workflow.graph_name.as_deref(), Some("test"));
    }

    #[test]
    fn run_created_title_populates_projection_and_summary() {
        let event = test_raw_event(
            1,
            "run.created",
            &json!({
                "title": "Explicit title",
                "settings": WorkflowSettings::default(),
                "graph": {
                    "name": "test",
                    "nodes": {},
                    "edges": [],
                    "attrs": { "goal": { "String": "Goal title" } }
                },
                "labels": {},
                "provenance": test_support::test_run_provenance()
            }),
            None,
        );

        let state = RunProjection::apply_events(&[event]).unwrap();
        assert_eq!(state.title, "Explicit title");
        assert_eq!(
            build_summary(&state, &fixtures::RUN_1).title,
            "Explicit title"
        );
    }

    #[test]
    fn run_created_without_title_infers_projection_title() {
        let event = test_raw_event(
            1,
            "run.created",
            &json!({
                "settings": WorkflowSettings::default(),
                "graph": {
                    "name": "test",
                    "nodes": {},
                    "edges": [],
                    "attrs": { "goal": { "String": "## Plan: Legacy title\n\nDetails" } }
                },
                "labels": {},
                "provenance": test_support::test_run_provenance()
            }),
            None,
        );

        let state = RunProjection::apply_events(&[event]).unwrap();
        assert_eq!(state.title, "Legacy title");
        assert_eq!(
            build_summary(&state, &fixtures::RUN_1).title,
            "Legacy title"
        );
    }

    #[test]
    fn run_title_updated_changes_projection_and_summary() {
        let events = vec![
            test_raw_event(
                1,
                "run.created",
                &json!({
                    "title": "Original title",
                    "settings": WorkflowSettings::default(),
                    "graph": {
                        "name": "test",
                        "nodes": {},
                        "edges": [],
                        "attrs": { "goal": { "String": "Goal title" } }
                    },
                    "labels": {},
                    "provenance": test_support::test_run_provenance()
                }),
                None,
            ),
            test_raw_event(
                2,
                "run.title.updated",
                &json!({ "title": "Renamed title" }),
                None,
            ),
        ];

        let state = RunProjection::apply_events(&events).unwrap();
        assert_eq!(state.title, "Renamed title");
        assert_eq!(
            build_summary(&state, &fixtures::RUN_1).title,
            "Renamed title"
        );
    }

    #[test]
    fn projection_serialization_includes_manifest_and_definition_blob_refs() {
        let manifest_blob = BlobHash::new(br#"{"version":1}"#).to_string();
        let definition_blob =
            BlobHash::new(br#"{"version":1,"workflow_path":"workflow.fabro"}"#).to_string();
        let events = vec![
            EventEnvelope {
                seq:   1,
                event: RunEvent::from_value(json!({
                    "id": "evt-run-created",
                    "ts": "2026-04-07T12:00:00Z",
                    "run_id": fixtures::RUN_1,
                    "event": "run.created",
                    "properties": {
                        "settings": WorkflowSettings::default(),
                        "graph": {
                            "name": "test",
                            "nodes": {},
                            "edges": [],
                            "attrs": {}
                        },
                        "labels": {},
                        "source_directory": "/tmp/run",
                        "provenance": test_support::test_run_provenance(),
                        "manifest_blob": manifest_blob
                    }
                }))
                .unwrap(),
            },
            EventEnvelope {
                seq:   2,
                event: RunEvent::from_value(json!({
                    "id": "evt-run-submitted",
                    "ts": "2026-04-07T12:00:01Z",
                    "run_id": fixtures::RUN_1,
                    "event": "run.submitted",
                    "properties": {
                        "definition_blob": definition_blob
                    }
                }))
                .unwrap(),
            },
        ];

        let state = RunProjection::apply_events(&events).unwrap();
        let value = serde_json::to_value(&state).unwrap();

        assert_eq!(
            value["spec"]["manifest_blob"],
            events[0].event.properties().unwrap()["manifest_blob"]
        );
        assert_eq!(
            value["spec"]["definition_blob"],
            events[1].event.properties().unwrap()["definition_blob"]
        );
    }

    #[test]
    fn run_failed_with_final_patch_populates_projection() {
        let mut state = running_projection();
        let patch = "diff --git a/foo.rs b/foo.rs\n@@ -1 +1 @@\n-a\n+b\n";
        state
            .apply_event(&test_event(
                1,
                EventBody::RunFailed(RunFailedProps {
                    failure:              fabro_types::RunFailure {
                        reason: FailureReason::WorkflowError,
                        detail: FailureDetail::new("boom", FailureCategory::Deterministic),
                    },
                    timing:               fabro_types::RunTiming::wall_only(42),
                    final_git_commit_sha: Some("abc123".to_string()),
                    final_patch:          Some(patch.to_string()),
                    diff_summary:         None,
                    billing:              None,
                }),
                None,
            ))
            .unwrap();

        assert_eq!(
            state
                .conclusion
                .as_ref()
                .and_then(|conclusion| conclusion.diff.patch.as_deref()),
            Some(patch)
        );
    }

    #[test]
    fn patch_bearing_events_roll_up_diff_summary_without_blanking_prior_value() {
        let mut state = running_projection();
        state
            .apply_event(&test_raw_event(
                3,
                "checkpoint.completed",
                &json!({
                    "status": "running",
                    "current_node": "build",
                    "completed_nodes": ["build"],
                    "diff_summary": {
                        "files_changed": 2,
                        "additions": 10,
                        "deletions": 3
                    }
                }),
                Some("build"),
            ))
            .unwrap();
        assert_eq!(
            serde_json::to_value(build_summary(&state, &fixtures::RUN_1)).unwrap()["diff"],
            json!({
                "files_changed": 2,
                "additions": 10,
                "deletions": 3
            })
        );

        state
            .apply_event(&test_raw_event(
                4,
                "checkpoint.completed",
                &json!({
                    "status": "running",
                    "current_node": "review",
                    "completed_nodes": ["build", "review"]
                }),
                Some("review"),
            ))
            .unwrap();
        assert_eq!(
            serde_json::to_value(build_summary(&state, &fixtures::RUN_1)).unwrap()["diff"]["files_changed"],
            2
        );

        state
            .apply_event(&test_raw_event(
                5,
                "run.completed",
                &json!({
                    "timing": {"wall_time_ms": 42, "inference_time_ms": 0, "tool_time_ms": 0, "active_time_ms": 0},
                    "artifact_count": 0,
                    "status": "succeeded",
                    "reason": "completed",
                    "diff_summary": {
                        "files_changed": 4,
                        "additions": 18,
                        "deletions": 7
                    }
                }),
                None,
            ))
            .unwrap();
        assert_eq!(
            serde_json::to_value(build_summary(&state, &fixtures::RUN_1)).unwrap()["diff"],
            json!({
                "files_changed": 4,
                "additions": 18,
                "deletions": 7
            })
        );

        let mut failed_state = running_projection();
        failed_state
            .apply_event(&test_raw_event(
                3,
                "run.failed",
                &json!({
                    "failure": {
                        "reason": "workflow_error",
                        "detail": {
                            "message": "boom",
                            "category": "deterministic"
                        }
                    },
                    "timing": {"wall_time_ms": 42, "inference_time_ms": 0, "tool_time_ms": 0, "active_time_ms": 0},
                    "diff_summary": {
                        "files_changed": 5,
                        "additions": 20,
                        "deletions": 8
                    }
                }),
                None,
            ))
            .unwrap();
        assert_eq!(
            serde_json::to_value(build_summary(&failed_state, &fixtures::RUN_1)).unwrap()["diff"],
            json!({
                "files_changed": 5,
                "additions": 20,
                "deletions": 8
            })
        );
    }

    #[test]
    fn run_failed_projection_renders_causes() {
        let mut state = running_projection();
        state
            .apply_event(&test_event(
                1,
                EventBody::RunFailed(RunFailedProps {
                    failure:              fabro_types::RunFailure {
                        reason: FailureReason::WorkflowError,
                        detail: {
                            let mut detail = FailureDetail::new(
                                "Failed to initialize sandbox",
                                FailureCategory::TransientInfra,
                            );
                            detail.causes = vec![
                                "Failed to pull Docker image buildpack-deps:noble".to_string(),
                                "connection refused".to_string(),
                            ];
                            detail
                        },
                    },
                    timing:               fabro_types::RunTiming::wall_only(42),
                    final_git_commit_sha: None,
                    final_patch:          None,
                    diff_summary:         None,
                    billing:              None,
                }),
                None,
            ))
            .unwrap();

        let failure = state.conclusion.unwrap().failure.unwrap();
        assert_eq!(failure.detail.message, "Failed to initialize sandbox");
        assert_eq!(failure.detail.causes, vec![
            "Failed to pull Docker image buildpack-deps:noble".to_string(),
            "connection refused".to_string(),
        ]);
    }

    #[test]
    fn run_failed_projection_uses_nested_failure_reason_and_conclusion() {
        let mut state = running_projection();
        let failure = fabro_types::RunFailure {
            reason: FailureReason::SandboxInitFailed,
            detail: {
                let mut detail = FailureDetail::new(
                    "Failed to initialize sandbox",
                    FailureCategory::TransientInfra,
                );
                detail.causes = vec!["connection refused".to_string()];
                detail.system_actor = Some(fabro_types::SystemActorKind::Engine);
                detail.signature = Some(fabro_types::FailureSignature(
                    "init|transient_infra|docker".to_string(),
                ));
                detail
            },
        };
        state
            .apply_event(&test_event(
                1,
                EventBody::RunFailed(RunFailedProps {
                    failure:              failure.clone(),
                    timing:               fabro_types::RunTiming::wall_only(42),
                    final_git_commit_sha: Some("abc123".to_string()),
                    final_patch:          None,
                    diff_summary:         None,
                    billing:              None,
                }),
                None,
            ))
            .unwrap();

        assert_eq!(state.status, RunStatus::Failed {
            reason: FailureReason::SandboxInitFailed,
        });
        let conclusion = state.conclusion.unwrap();
        assert_eq!(conclusion.failure, Some(failure));
        assert_eq!(conclusion.final_git_commit_sha.as_deref(), Some("abc123"));
    }

    #[test]
    fn resume_start_request_clears_prior_terminal_conclusion() {
        let mut state = running_projection();
        let failed_at = "2026-05-24T22:01:19Z";

        state
            .apply_event(&test_raw_event_at(
                4,
                failed_at,
                "run.failed",
                &serde_json::to_value(run_failed_props(FailureReason::Cancelled)).unwrap(),
                None,
            ))
            .unwrap();
        assert!(state.conclusion.is_some());
        assert_eq!(
            build_summary(&state, &fixtures::RUN_1)
                .timestamps
                .completed_at,
            Some(test_dt(failed_at))
        );

        state
            .apply_event(&test_raw_event_at(
                5,
                "2026-05-24T22:36:50Z",
                "run.start_requested",
                &json!({ "resume": true }),
                None,
            ))
            .unwrap();
        state
            .apply_event(&test_raw_event_at(
                6,
                "2026-05-24T22:36:51Z",
                "run.runnable",
                &json!({ "source": "start_requested" }),
                None,
            ))
            .unwrap();
        state
            .apply_event(&test_raw_event_at(
                7,
                "2026-05-24T22:36:52Z",
                "run.starting",
                &json!({}),
                None,
            ))
            .unwrap();
        state
            .apply_event(&test_raw_event_at(
                8,
                "2026-05-24T22:36:53Z",
                "run.running",
                &json!({}),
                None,
            ))
            .unwrap();

        let summary = build_summary(&state, &fixtures::RUN_1);
        assert_eq!(state.status, RunStatus::Running);
        assert!(state.conclusion.is_none());
        assert_eq!(summary.timestamps.completed_at, None);
        assert_eq!(summary.timing, None);
    }

    #[test]
    fn run_archived_captures_prior_status_and_preserves_reason() {
        use fabro_types::run_event::{RunArchivedProps, RunCompletedProps};

        let mut state = running_projection();
        state
            .apply_event(&test_event(
                1,
                EventBody::RunCompleted(RunCompletedProps {
                    timing:               fabro_types::RunTiming::wall_only(10),
                    artifact_count:       0,
                    status:               "succeeded".to_string(),
                    reason:               SuccessReason::Completed,
                    total_usd_micros:     None,
                    final_git_commit_sha: None,
                    final_patch:          None,
                    diff_summary:         None,
                    billing:              None,
                }),
                None,
            ))
            .unwrap();
        state
            .apply_event(&test_event(
                2,
                EventBody::RunArchived(RunArchivedProps::default()),
                None,
            ))
            .unwrap();

        assert_eq!(state.status(), RunStatus::Succeeded {
            reason: SuccessReason::Completed,
        });
        assert!(state.archived_at.is_some());
    }

    #[test]
    fn run_superseded_by_populates_projection_and_summary() {
        use fabro_types::run_event::RunSupersededByProps;

        let mut state = running_projection();
        state
            .apply_event(&test_event(
                1,
                EventBody::RunSupersededBy(RunSupersededByProps {
                    new_run_id:                fixtures::RUN_2,
                    target_checkpoint_ordinal: 2,
                    target_node_id:            "build".to_string(),
                    target_visit:              1,
                }),
                None,
            ))
            .unwrap();

        assert_eq!(state.superseded_by, Some(fixtures::RUN_2));

        let summary = build_summary(&state, &fixtures::RUN_1);
        assert_eq!(summary.superseded_by, Some(fixtures::RUN_2));
    }

    #[test]
    fn pull_request_created_populates_projection_and_summary() {
        use fabro_types::run_event::PullRequestCreatedProps;

        let mut state = running_projection();
        state
            .apply_event(&test_event(
                1,
                EventBody::PullRequestCreated(PullRequestCreatedProps {
                    pr_url:      "https://github.com/fabro-sh/fabro/pull/123".to_string(),
                    pr_number:   123,
                    owner:       "fabro-sh".to_string(),
                    repo:        "fabro".to_string(),
                    base_branch: "main".to_string(),
                    head_branch: "fabro/run/demo".to_string(),
                    head_sha:    Some("final-sha".to_string()),
                    title:       "Add run PR chip".to_string(),
                    draft:       false,
                }),
                None,
            ))
            .unwrap();

        let pull_request = state
            .pull_request
            .as_ref()
            .expect("projection should store pull request");
        assert_eq!(
            pull_request.html_url(),
            "https://github.com/fabro-sh/fabro/pull/123"
        );
        assert_eq!(pull_request.number, 123);

        let summary = build_summary(&state, &fixtures::RUN_1);
        assert_eq!(summary.pull_request, state.pull_request);
    }

    #[test]
    fn pull_request_creation_projects_failure_retry_and_success() {
        use fabro_types::run_event::{
            PullRequestCreatedProps, PullRequestCreationRequestedProps, PullRequestFailedProps,
        };

        let mut state = running_projection();
        let first_id = "01KYYK70WTZT2E551P3H5P0059".parse().unwrap();
        state
            .apply_event(&test_event(
                1,
                EventBody::PullRequestCreationRequested(PullRequestCreationRequestedProps {
                    creation_id: first_id,
                    model:       "kimi-k3".to_string(),
                    force:       false,
                }),
                None,
            ))
            .unwrap();
        assert!(state.pull_request_creation.as_ref().unwrap().is_pending());

        state
            .apply_event(&test_event(
                2,
                EventBody::PullRequestFailed(PullRequestFailedProps {
                    creation_id: None,
                    error:       "publish stage failure".to_string(),
                }),
                None,
            ))
            .unwrap();
        assert!(
            state.pull_request_creation.as_ref().unwrap().is_pending(),
            "a failure without a creation id must not resolve the creation"
        );

        state
            .apply_event(&test_event(
                3,
                EventBody::PullRequestFailed(PullRequestFailedProps {
                    creation_id: Some(first_id),
                    error:       "provider unavailable".to_string(),
                }),
                None,
            ))
            .unwrap();
        let failed = state.pull_request_creation.as_ref().unwrap();
        assert_eq!(failed.status, PullRequestCreationStatus::Failed);
        assert_eq!(failed.error.as_deref(), Some("provider unavailable"));

        let retry_id = "01KYYK70WTZT2E551P3H5P0060".parse().unwrap();
        state
            .apply_event(&test_event(
                4,
                EventBody::PullRequestCreationRequested(PullRequestCreationRequestedProps {
                    creation_id: retry_id,
                    model:       "claude-sonnet-4-6".to_string(),
                    force:       true,
                }),
                None,
            ))
            .unwrap();
        assert_eq!(state.pull_request_creation.as_ref().unwrap().id, retry_id);

        state
            .apply_event(&test_event(
                5,
                EventBody::PullRequestCreated(PullRequestCreatedProps {
                    pr_url:      "https://github.com/fabro-sh/fabro/pull/123".to_string(),
                    pr_number:   123,
                    owner:       "fabro-sh".to_string(),
                    repo:        "fabro".to_string(),
                    base_branch: "main".to_string(),
                    head_branch: "fabro/run/demo".to_string(),
                    head_sha:    Some("final-sha".to_string()),
                    title:       "Create asynchronously".to_string(),
                    draft:       true,
                }),
                None,
            ))
            .unwrap();
        let succeeded = state.pull_request_creation.as_ref().unwrap();
        assert_eq!(succeeded.status, PullRequestCreationStatus::Succeeded);
        assert_eq!(succeeded.pull_request.as_ref().unwrap().number, 123);
        assert_eq!(succeeded.pull_request, state.pull_request);
        assert!(succeeded.error.is_none());
    }

    #[test]
    fn pull_request_linked_replaces_and_unlinked_clears_projection() {
        use fabro_types::run_event::{
            PullRequestCreatedProps, PullRequestLinkedProps, PullRequestUnlinkedProps,
        };

        let mut state = running_projection();
        let github_pull_request = PullRequestLink {
            owner:  "fabro-sh".to_string(),
            repo:   "fabro".to_string(),
            number: 123,
        };
        let replacement_pull_request = PullRequestLink {
            owner:  "acme".to_string(),
            repo:   "widgets".to_string(),
            number: 42,
        };

        state
            .apply_event(&test_event(
                1,
                EventBody::PullRequestCreated(PullRequestCreatedProps {
                    pr_url:      github_pull_request.html_url(),
                    pr_number:   github_pull_request.number,
                    owner:       github_pull_request.owner.clone(),
                    repo:        github_pull_request.repo.clone(),
                    base_branch: "main".to_string(),
                    head_branch: "fabro/run/demo".to_string(),
                    head_sha:    Some("final-sha".to_string()),
                    title:       "Add run PR chip".to_string(),
                    draft:       false,
                }),
                None,
            ))
            .unwrap();
        assert_eq!(state.pull_request, Some(github_pull_request.clone()));

        state
            .apply_event(&test_event(
                2,
                EventBody::PullRequestLinked(PullRequestLinkedProps {
                    pull_request: replacement_pull_request.clone(),
                }),
                None,
            ))
            .unwrap();
        assert_eq!(state.pull_request, Some(replacement_pull_request.clone()));

        state
            .apply_event(&test_event(
                3,
                EventBody::PullRequestUnlinked(PullRequestUnlinkedProps {
                    pull_request: replacement_pull_request,
                }),
                None,
            ))
            .unwrap();
        assert_eq!(state.pull_request, None);

        state
            .apply_event(&test_event(
                4,
                EventBody::PullRequestLinked(PullRequestLinkedProps {
                    pull_request: github_pull_request.clone(),
                }),
                None,
            ))
            .unwrap();
        assert_eq!(state.pull_request, Some(github_pull_request));
    }

    #[test]
    fn run_unarchived_restores_prior_status() {
        use fabro_types::run_event::{RunArchivedProps, RunCompletedProps, RunUnarchivedProps};

        let mut state = running_projection();
        state
            .apply_event(&test_event(
                1,
                EventBody::RunCompleted(RunCompletedProps {
                    timing:               fabro_types::RunTiming::wall_only(10),
                    artifact_count:       0,
                    status:               "succeeded".to_string(),
                    reason:               SuccessReason::PartialSuccess,
                    total_usd_micros:     None,
                    final_git_commit_sha: None,
                    final_patch:          None,
                    diff_summary:         None,
                    billing:              None,
                }),
                None,
            ))
            .unwrap();
        state
            .apply_event(&test_event(
                2,
                EventBody::RunArchived(RunArchivedProps::default()),
                None,
            ))
            .unwrap();
        state
            .apply_event(&test_event(
                3,
                EventBody::RunUnarchived(RunUnarchivedProps::default()),
                None,
            ))
            .unwrap();

        assert_eq!(state.status(), RunStatus::Succeeded {
            reason: SuccessReason::PartialSuccess,
        });
    }

    #[test]
    fn duplicate_event_noops_without_bumping_status_updated_at() {
        let mut state = initialized_projection();
        state
            .apply_event(&test_raw_event_at(
                1,
                "2026-04-07T12:00:00Z",
                "run.runnable",
                &json!({ "source": "start_requested" }),
                None,
            ))
            .unwrap();
        state
            .apply_event(&test_raw_event_at(
                2,
                "2026-04-07T12:00:30Z",
                "run.starting",
                &json!({}),
                None,
            ))
            .unwrap();
        state
            .apply_event(&test_raw_event_at(
                3,
                "2026-04-07T12:01:00Z",
                "run.running",
                &json!({}),
                None,
            ))
            .unwrap();
        let first_updated_at = state.status_updated_at;

        state
            .apply_event(&test_raw_event_at(
                4,
                "2026-04-07T12:02:00Z",
                "run.running",
                &json!({}),
                None,
            ))
            .unwrap();

        assert_eq!(state.status(), RunStatus::Running);
        assert_eq!(state.status_updated_at, first_updated_at);
    }

    #[test]
    fn paused_over_blocked_round_trips_back_to_blocked() {
        let mut state = running_projection();
        state
            .apply_event(&test_raw_event(
                3,
                "run.blocked",
                &json!({ "blocked_reason": "human_input_required" }),
                None,
            ))
            .unwrap();
        state
            .apply_event(&test_event(
                4,
                EventBody::RunPaused(RunControlEffectProps::default()),
                None,
            ))
            .unwrap();
        state
            .apply_event(&test_event(
                5,
                EventBody::RunUnpaused(RunControlEffectProps::default()),
                None,
            ))
            .unwrap();

        assert_eq!(state.status(), RunStatus::Blocked {
            blocked_reason: BlockedReason::HumanInputRequired,
        });
    }

    #[test]
    fn run_archived_on_non_terminal_projection_is_rejected() {
        use fabro_types::run_event::RunArchivedProps;

        let mut state = running_projection();

        let err = state
            .apply_event(&test_event(
                3,
                EventBody::RunArchived(RunArchivedProps::default()),
                None,
            ))
            .unwrap_err();

        assert!(matches!(err, Error::InvalidTransition(_)));
        assert_eq!(state.status(), RunStatus::Running);
    }

    #[test]
    fn run_unarchived_replayed_on_non_archived_projection_is_ignored() {
        use fabro_types::run_event::{RunCompletedProps, RunUnarchivedProps};

        let mut state = running_projection();
        state
            .apply_event(&test_event(
                1,
                EventBody::RunCompleted(RunCompletedProps {
                    timing:               fabro_types::RunTiming::wall_only(10),
                    artifact_count:       0,
                    status:               "succeeded".to_string(),
                    reason:               SuccessReason::Completed,
                    total_usd_micros:     None,
                    final_git_commit_sha: None,
                    final_patch:          None,
                    diff_summary:         None,
                    billing:              None,
                }),
                None,
            ))
            .unwrap();
        let updated_at = state.status_updated_at;

        state
            .apply_event(&test_event(
                2,
                EventBody::RunUnarchived(RunUnarchivedProps::default()),
                None,
            ))
            .unwrap();

        assert_eq!(state.status(), RunStatus::Succeeded {
            reason: SuccessReason::Completed,
        });
        assert_eq!(state.status_updated_at, updated_at);
    }

    fn started_props() -> StageStartedProps {
        StageStartedProps {
            graph_visit:           None,
            resumed_from_stage_id: None,
            index:                 0,
            handler_type:          "agent".to_string(),
            attempt:               1,
            max_attempts:          3,
        }
    }

    fn failed_props(duration_ms: u64, will_retry: bool) -> StageFailedProps {
        StageFailedProps {
            index: 0,
            failure: Some(FailureDetail::new("boom", FailureCategory::TransientInfra)),
            will_retry,
            timing: fabro_types::StageTiming::wall_only(duration_ms),
            billing: None,
        }
    }

    fn canceled_failed_props(duration_ms: u64, will_retry: bool) -> StageFailedProps {
        StageFailedProps {
            index: 0,
            failure: Some(FailureDetail::new("cancelled", FailureCategory::Canceled)),
            will_retry,
            timing: fabro_types::StageTiming::wall_only(duration_ms),
            billing: None,
        }
    }

    fn run_failed_props(reason: FailureReason) -> RunFailedProps {
        let category = if reason == FailureReason::Cancelled {
            FailureCategory::Canceled
        } else {
            FailureCategory::Deterministic
        };

        RunFailedProps {
            failure:              fabro_types::RunFailure {
                reason,
                detail: FailureDetail::new("run failed", category),
            },
            timing:               fabro_types::RunTiming::wall_only(42),
            final_git_commit_sha: None,
            final_patch:          None,
            diff_summary:         None,
            billing:              None,
        }
    }

    fn retrying_props() -> StageRetryingProps {
        StageRetryingProps {
            index:        0,
            attempt:      2,
            max_attempts: 3,
            delay_ms:     0,
        }
    }

    fn completed_props(duration_ms: u64, status: StageOutcome) -> StageCompletedProps {
        StageCompletedProps {
            index: 0,
            timing: fabro_types::StageTiming::wall_only(duration_ms),
            status,
            preferred_label: None,
            suggested_next_ids: Vec::new(),
            billing: None,
            failure: None,
            notes: None,
            files_touched: Vec::new(),
            context_updates: None,
            jump_to_node: None,
            context_values: None,
            node_visits: None,
            loop_failure_signatures: None,
            restart_failure_signatures: None,
            response: None,
            attempt: 1,
            max_attempts: 3,
        }
    }

    fn billed_usage() -> BilledModelUsage {
        serde_json::from_value(json!({
            "input": {
                "usage": {
                    "model": {
                        "provider": "openai",
                        "model_id": "gpt-test"
                    },
                    "tokens": {
                        "input_tokens": 10,
                        "output_tokens": 5,
                        "reasoning_tokens": 2,
                        "cache_read_tokens": 3,
                        "cache_write_tokens": 4
                    }
                },
                "facts": { "algorithm": "openai" }
            },
            "total_usd_micros": 123
        }))
        .expect("billing fixture should deserialize")
    }

    fn live_agent_message_props(billing: BilledTokenCounts) -> AgentMessageProps {
        AgentMessageProps {
            text: "assistant text".to_string(),
            model: billed_usage().model().clone(),
            billing,
            cost_source: None,
            tool_call_count: 0,
            visit: 1,
            message: None,
            context_window: None,
            reasoning: None,
        }
    }

    fn live_counts(input_tokens: i64, output_tokens: i64) -> BilledTokenCounts {
        BilledTokenCounts {
            input_tokens,
            output_tokens,
            total_tokens: input_tokens + output_tokens,
            reasoning_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            total_usd_micros: None,
        }
    }

    #[test]
    fn stage_started_records_started_at_and_running_state() {
        let mut state = initialized_projection();
        let stage_id = StageId::new("build", 1);

        state
            .apply_event(&test_stage_event(
                3,
                EventBody::StageStarted(started_props()),
                stage_id.clone(),
            ))
            .unwrap();

        let stage = state.stage(&stage_id).unwrap();
        assert_eq!(stage.state, StageState::Running);
        assert!(stage.started_at.is_some());
        assert_eq!(stage.effective_state(), StageState::Running);
    }

    #[test]
    fn agent_message_accumulates_live_usage_on_stage_projection() {
        let mut state = initialized_projection();
        let stage_id = StageId::new("build", 1);
        let model = billed_usage().model().clone();

        state
            .apply_event(&test_stage_event(
                1,
                EventBody::StageStarted(started_props()),
                stage_id.clone(),
            ))
            .unwrap();
        state
            .apply_event(&test_stage_event(
                2,
                EventBody::AgentMessage(live_agent_message_props(live_counts(10, 5))),
                stage_id.clone(),
            ))
            .unwrap();
        state
            .apply_event(&test_stage_event(
                3,
                EventBody::AgentMessage(live_agent_message_props(live_counts(20, 7))),
                stage_id.clone(),
            ))
            .unwrap();

        let stage = state.stage(&stage_id).unwrap();
        assert_eq!(stage.usage, live_counts(30, 12));
        assert_eq!(stage.model, Some(model));
    }

    #[test]
    fn stage_completed_replaces_live_usage_with_terminal_billing() {
        let mut state = initialized_projection();
        let stage_id = StageId::new("build", 1);
        let usage = billed_usage();

        state
            .apply_event(&test_stage_event(
                1,
                EventBody::StageStarted(started_props()),
                stage_id.clone(),
            ))
            .unwrap();
        state
            .apply_event(&test_stage_event(
                2,
                EventBody::AgentMessage(live_agent_message_props(live_counts(100, 50))),
                stage_id.clone(),
            ))
            .unwrap();
        let mut props = completed_props(42, StageOutcome::Succeeded);
        props.billing = Some(usage.clone());
        state
            .apply_event(&test_stage_event(
                3,
                EventBody::StageCompleted(props),
                stage_id.clone(),
            ))
            .unwrap();

        let stage = state.stage(&stage_id).unwrap();
        assert_eq!(stage.usage, usage_counts(&usage));
        assert_eq!(stage.model.as_ref(), Some(usage.model()));
    }

    #[test]
    fn stage_completed_without_billing_preserves_live_usage() {
        let mut state = initialized_projection();
        let stage_id = StageId::new("build", 1);
        let model = billed_usage().model().clone();

        state
            .apply_event(&test_stage_event(
                1,
                EventBody::StageStarted(started_props()),
                stage_id.clone(),
            ))
            .unwrap();
        state
            .apply_event(&test_stage_event(
                2,
                EventBody::AgentMessage(live_agent_message_props(live_counts(10, 5))),
                stage_id.clone(),
            ))
            .unwrap();
        state
            .apply_event(&test_stage_event(
                3,
                EventBody::StageCompleted(completed_props(42, StageOutcome::Succeeded)),
                stage_id.clone(),
            ))
            .unwrap();

        let stage = state.stage(&stage_id).unwrap();
        assert_eq!(stage.usage, live_counts(10, 5));
        assert_eq!(stage.model, Some(model));
    }

    #[test]
    fn summary_size_tracks_current_projected_usage_before_terminal_conclusion() {
        let mut state = initialized_projection();
        let stage_id = StageId::new("build", 1);
        let usage = test_usage("gpt-5.2", 10_000_001, 10_000_000);

        state
            .apply_event(&test_stage_event(
                1,
                EventBody::StageStarted(started_props()),
                stage_id.clone(),
            ))
            .unwrap();
        let mut props = completed_props(42, StageOutcome::Succeeded);
        props.billing = Some(usage);
        state
            .apply_event(&test_stage_event(
                2,
                EventBody::StageCompleted(props),
                stage_id,
            ))
            .unwrap();

        let summary = build_summary(&state, &fixtures::RUN_1);
        assert_eq!(summary.size, RunSize::S);
        assert_eq!(
            summary.billing,
            Some(RunBillingSummary {
                total_usd_micros: Some(20_000_001),
            })
        );
    }

    #[test]
    fn stage_failed_replaces_live_usage_with_terminal_billing() {
        let mut state = initialized_projection();
        let stage_id = StageId::new("build", 1);
        let usage = billed_usage();

        state
            .apply_event(&test_stage_event(
                1,
                EventBody::StageStarted(started_props()),
                stage_id.clone(),
            ))
            .unwrap();
        state
            .apply_event(&test_stage_event(
                2,
                EventBody::AgentMessage(live_agent_message_props(live_counts(100, 50))),
                stage_id.clone(),
            ))
            .unwrap();
        let mut props = failed_props(42, false);
        props.billing = Some(usage.clone());
        state
            .apply_event(&test_stage_event(
                3,
                EventBody::StageFailed(props),
                stage_id.clone(),
            ))
            .unwrap();

        let stage = state.stage(&stage_id).unwrap();
        assert_eq!(stage.usage, usage_counts(&usage));
        assert_eq!(stage.model.as_ref(), Some(usage.model()));
    }

    #[test]
    fn stage_started_resets_live_usage_for_new_attempt() {
        let mut state = initialized_projection();
        let stage_id = StageId::new("build", 1);

        state
            .apply_event(&test_stage_event(
                1,
                EventBody::StageStarted(started_props()),
                stage_id.clone(),
            ))
            .unwrap();
        state
            .apply_event(&test_stage_event(
                2,
                EventBody::AgentMessage(live_agent_message_props(live_counts(10, 5))),
                stage_id.clone(),
            ))
            .unwrap();
        state
            .apply_event(&test_stage_event(
                3,
                EventBody::StageStarted(started_props()),
                stage_id.clone(),
            ))
            .unwrap();

        let stage = state.stage(&stage_id).unwrap();
        assert!(stage.usage.is_zero());
        assert_eq!(stage.model, None);
        assert_eq!(stage.state, StageState::Running);
    }

    #[test]
    fn stage_completed_records_duration_usage_and_terminal_state() {
        let mut state = initialized_projection();
        let stage_id = StageId::new("build", 1);
        let usage = billed_usage();

        state
            .apply_event(&test_stage_event(
                1,
                EventBody::StageStarted(started_props()),
                stage_id.clone(),
            ))
            .unwrap();
        let mut props = completed_props(42, StageOutcome::Succeeded);
        props.billing = Some(usage.clone());
        state
            .apply_event(&test_event(
                2,
                EventBody::StageCompleted(props),
                Some("build"),
            ))
            .unwrap();

        let stage = state.stage(&stage_id).unwrap();
        assert_eq!(stage.timing.map(|t| t.wall_time_ms), Some(42));
        assert_eq!(stage.usage, usage_counts(&usage));
        assert_eq!(stage.model.as_ref(), Some(usage.model()));
        assert_eq!(stage.state, StageState::Succeeded);
        assert_eq!(stage.effective_state(), StageState::Succeeded);
    }

    #[test]
    fn stage_failed_records_duration_and_failed_state() {
        let mut state = initialized_projection();
        let stage_id = StageId::new("build", 1);

        state
            .apply_event(&test_stage_event(
                1,
                EventBody::StageStarted(started_props()),
                stage_id.clone(),
            ))
            .unwrap();
        state
            .apply_event(&test_event(
                2,
                EventBody::StageFailed(failed_props(10, false)),
                Some("build"),
            ))
            .unwrap();

        let stage = state.stage(&stage_id).unwrap();
        assert_eq!(stage.timing.map(|t| t.wall_time_ms), Some(10));
        assert_eq!(stage.state, StageState::Failed);
    }

    #[test]
    fn stage_failed_canceled_without_retry_records_cancelled_state() {
        let mut state = initialized_projection();
        let stage_id = StageId::new("build", 1);

        state
            .apply_event(&test_stage_event(
                1,
                EventBody::StageStarted(started_props()),
                stage_id.clone(),
            ))
            .unwrap();
        state
            .apply_event(&test_event(
                2,
                EventBody::StageFailed(canceled_failed_props(10, false)),
                Some("build"),
            ))
            .unwrap();

        let stage = state.stage(&stage_id).unwrap();
        assert_eq!(stage.timing.map(|t| t.wall_time_ms), Some(10));
        assert_eq!(stage.state, StageState::Cancelled);
    }

    #[test]
    fn exited_command_with_canceled_failure_category_records_failed_state() {
        let mut state = initialized_projection();
        let stage_id = StageId::new("build", 1);

        state
            .apply_event(&test_stage_event(
                1,
                EventBody::StageStarted(started_props()),
                stage_id.clone(),
            ))
            .unwrap();
        state
            .apply_event(&test_stage_event(
                2,
                EventBody::CommandCompleted(CommandCompletedProps {
                    output:         "blob://sha256/test".to_string(),
                    exit_code:      Some(100),
                    duration_ms:    10,
                    termination:    CommandTermination::Exited,
                    output_bytes:   42,
                    live_streaming: true,
                }),
                stage_id.clone(),
            ))
            .unwrap();
        state
            .apply_event(&test_event(
                3,
                EventBody::StageFailed(StageFailedProps {
                    index:      0,
                    failure:    Some(FailureDetail::new(
                        "Script failed with exit code: 100\n\nCancelling due to test failure",
                        FailureCategory::Canceled,
                    )),
                    will_retry: false,
                    timing:     fabro_types::StageTiming::wall_only(10),
                    billing:    None,
                }),
                Some("build"),
            ))
            .unwrap();

        let stage = state.stage(&stage_id).unwrap();
        assert_eq!(stage.termination, Some(CommandTermination::Exited));
        assert_eq!(stage.state, StageState::Failed);
    }

    #[test]
    fn run_failed_cancelled_finalizes_running_stage_as_cancelled() {
        let mut state = running_projection();
        let stage_id = StageId::new("build", 1);

        state
            .apply_event(&test_stage_event_at(
                4,
                "2026-04-07T12:00:00Z",
                EventBody::StageStarted(started_props()),
                stage_id.clone(),
            ))
            .unwrap();
        state
            .apply_event(&test_raw_event_at(
                5,
                "2026-04-07T12:00:05Z",
                "run.failed",
                &serde_json::to_value(run_failed_props(FailureReason::Cancelled)).unwrap(),
                None,
            ))
            .unwrap();

        let stage = state.stage(&stage_id).unwrap();
        assert_eq!(stage.state, StageState::Cancelled);
        assert_eq!(
            stage.timing,
            Some(fabro_types::StageTiming::wall_only(5_000))
        );
    }

    #[test]
    fn stage_started_records_execution_identity_metadata() {
        let mut state = running_projection();
        let stage_id = StageId::new("work", 2);

        state
            .apply_event(&test_stage_event(
                4,
                EventBody::StageStarted(StageStartedProps {
                    graph_visit: Some(1),
                    resumed_from_stage_id: Some(StageId::new("work", 1)),
                    ..started_props()
                }),
                stage_id.clone(),
            ))
            .unwrap();

        let stage = state.stage(&stage_id).unwrap();
        assert_eq!(stage.graph_visit, Some(1));
        assert_eq!(stage.resumed_from_stage_id, Some(StageId::new("work", 1)));
    }

    #[test]
    fn retry_stage_started_preserves_execution_identity_metadata() {
        let mut state = running_projection();
        let stage_id = StageId::new("work", 2);

        state
            .apply_event(&test_stage_event(
                4,
                EventBody::StageStarted(StageStartedProps {
                    graph_visit: Some(1),
                    resumed_from_stage_id: Some(StageId::new("work", 1)),
                    ..started_props()
                }),
                stage_id.clone(),
            ))
            .unwrap();
        // A malformed retry event for the same StageId cannot rewrite the
        // first attempt's immutable execution identity.
        state
            .apply_event(&test_stage_event(
                5,
                EventBody::StageStarted(StageStartedProps {
                    attempt: 2,
                    graph_visit: Some(99),
                    resumed_from_stage_id: Some(StageId::new("other", 7)),
                    ..started_props()
                }),
                stage_id.clone(),
            ))
            .unwrap();

        let stage = state.stage(&stage_id).unwrap();
        assert_eq!(stage.graph_visit, Some(1));
        assert_eq!(stage.resumed_from_stage_id, Some(StageId::new("work", 1)));
    }

    #[test]
    fn cancelled_execution_stays_immutable_when_resumed_execution_starts() {
        let mut state = running_projection();
        let first = StageId::new("work", 1);
        let second = StageId::new("work", 2);

        state
            .apply_event(&test_stage_event_at(
                4,
                "2026-04-07T12:00:00Z",
                EventBody::StageStarted(started_props()),
                first.clone(),
            ))
            .unwrap();
        state
            .apply_event(&test_raw_event_at(
                5,
                "2026-04-07T12:00:05Z",
                "run.failed",
                &serde_json::to_value(run_failed_props(FailureReason::Cancelled)).unwrap(),
                None,
            ))
            .unwrap();
        state
            .apply_event(&test_raw_event(
                6,
                "run.start_requested",
                &json!({ "resume": true }),
                None,
            ))
            .unwrap();
        state
            .apply_event(&test_raw_event(
                7,
                "run.runnable",
                &json!({ "source": "start_requested" }),
                None,
            ))
            .unwrap();
        state
            .apply_event(&test_raw_event(8, "run.starting", &json!({}), None))
            .unwrap();
        state
            .apply_event(&test_raw_event(9, "run.running", &json!({}), None))
            .unwrap();
        state
            .apply_event(&test_stage_event(
                10,
                EventBody::StageStarted(StageStartedProps {
                    graph_visit: Some(1),
                    resumed_from_stage_id: Some(first.clone()),
                    ..started_props()
                }),
                second.clone(),
            ))
            .unwrap();

        // The cancelled execution keeps its terminal projection untouched...
        let cancelled = state.stage(&first).unwrap();
        assert_eq!(cancelled.state, StageState::Cancelled);
        assert_eq!(
            cancelled.timing,
            Some(fabro_types::StageTiming::wall_only(5_000))
        );
        // ...while the reexecution runs as a distinct stage linked back to it.
        let resumed = state.stage(&second).unwrap();
        assert_eq!(resumed.state, StageState::Running);
        assert_eq!(resumed.graph_visit, Some(1));
        assert_eq!(resumed.resumed_from_stage_id, Some(first));
    }

    #[test]
    fn run_failed_after_resume_preserves_earlier_terminal_executions() {
        let mut state = running_projection();
        let done = StageId::new("verify", 1);
        let active = StageId::new("work", 2);

        state
            .apply_event(&test_stage_event_at(
                4,
                "2026-04-07T12:00:00Z",
                EventBody::StageStarted(started_props()),
                done.clone(),
            ))
            .unwrap();
        state
            .apply_event(&test_stage_event_at(
                5,
                "2026-04-07T12:00:03Z",
                EventBody::StageCompleted(completed_props(3_000, StageOutcome::Succeeded)),
                done.clone(),
            ))
            .unwrap();
        state
            .apply_event(&test_stage_event_at(
                6,
                "2026-04-07T12:00:04Z",
                EventBody::StageStarted(started_props()),
                active.clone(),
            ))
            .unwrap();
        state
            .apply_event(&test_raw_event_at(
                7,
                "2026-04-07T12:00:09Z",
                "run.failed",
                &serde_json::to_value(run_failed_props(FailureReason::Cancelled)).unwrap(),
                None,
            ))
            .unwrap();

        let terminal = state.stage(&done).unwrap();
        assert_eq!(terminal.state, StageState::Succeeded);
        assert_eq!(
            terminal.timing,
            Some(fabro_types::StageTiming::wall_only(3_000))
        );
        assert_eq!(state.stage(&active).unwrap().state, StageState::Cancelled);
    }

    #[test]
    fn checkpoint_completed_targets_envelope_stage_id_after_ordinal_divergence() {
        let mut state = running_projection();
        let execution = StageId::new("work", 2);

        state
            .apply_event(&test_stage_event(
                4,
                EventBody::StageStarted(StageStartedProps {
                    graph_visit: Some(1),
                    ..started_props()
                }),
                execution.clone(),
            ))
            .unwrap();
        // Checkpointed graph state still says visit 1 for `work`, and carries
        // a historical skipped outcome for another node. Neither may create
        // or mutate a projection at a stale ordinal.
        state
            .apply_event(&test_stage_event(
                5,
                EventBody::CheckpointCompleted(CheckpointCompletedProps {
                    graph_visit: Some(1),
                    resumed_from_stage_id: None,
                    status: "success".to_string(),
                    current_node: "work".to_string(),
                    completed_nodes: vec!["old_skip".to_string(), "work".to_string()],
                    node_retries: BTreeMap::new(),
                    context_values: BTreeMap::new(),
                    node_outcomes: BTreeMap::from([(
                        "old_skip".to_string(),
                        Outcome::skipped("historical skip"),
                    )]),
                    next_node_id: None,
                    git_commit_sha: None,
                    loop_failure_signatures: BTreeMap::new(),
                    restart_failure_signatures: BTreeMap::new(),
                    node_visits: BTreeMap::from([
                        ("work".to_string(), 1usize),
                        ("old_skip".to_string(), 1usize),
                    ]),
                    diff: Some("diff for work@2".to_string()),
                    diff_summary: None,
                }),
                execution.clone(),
            ))
            .unwrap();

        assert_eq!(
            state.stage(&execution).unwrap().diff.as_deref(),
            Some("diff for work@2")
        );
        assert!(state.stage(&StageId::new("work", 1)).is_none());
        assert!(state.stage(&StageId::new("old_skip", 1)).is_none());
    }

    #[test]
    fn skipped_checkpoint_with_stage_id_creates_synthetic_execution() {
        let mut state = running_projection();
        let execution = StageId::new("gate", 3);

        // First-attempt StageStart-hook skip: no stage.started was emitted,
        // the checkpoint is the first stage-scoped event for this execution.
        state
            .apply_event(&test_stage_event(
                4,
                EventBody::CheckpointCompleted(CheckpointCompletedProps {
                    graph_visit: Some(2),
                    resumed_from_stage_id: Some(StageId::new("gate", 2)),
                    status: "skipped".to_string(),
                    current_node: "gate".to_string(),
                    completed_nodes: vec!["gate".to_string()],
                    node_retries: BTreeMap::new(),
                    context_values: BTreeMap::new(),
                    node_outcomes: BTreeMap::from([(
                        "gate".to_string(),
                        Outcome::skipped("skipped by StageStart hook"),
                    )]),
                    next_node_id: None,
                    git_commit_sha: None,
                    loop_failure_signatures: BTreeMap::new(),
                    restart_failure_signatures: BTreeMap::new(),
                    node_visits: BTreeMap::from([("gate".to_string(), 2usize)]),
                    diff: None,
                    diff_summary: None,
                }),
                execution.clone(),
            ))
            .unwrap();

        let stage = state.stage(&execution).unwrap();
        assert_eq!(stage.state, StageState::Skipped);
        assert_eq!(stage.graph_visit, Some(2));
        assert_eq!(stage.resumed_from_stage_id, Some(StageId::new("gate", 2)));
        assert_eq!(
            stage.completion.as_ref().unwrap().notes.as_deref(),
            Some("skipped by StageStart hook")
        );
    }

    #[test]
    fn skipped_checkpoint_finalizes_existing_retrying_execution() {
        let mut state = running_projection();
        let execution = StageId::new("gate", 1);

        state
            .apply_event(&test_stage_event(
                4,
                EventBody::StageStarted(started_props()),
                execution.clone(),
            ))
            .unwrap();
        state
            .apply_event(&test_stage_event(
                5,
                EventBody::StageRetrying(StageRetryingProps {
                    index:        0,
                    attempt:      2,
                    max_attempts: 3,
                    delay_ms:     0,
                }),
                execution.clone(),
            ))
            .unwrap();
        assert_eq!(state.stage(&execution).unwrap().state, StageState::Retrying);

        // StageStart hook skipped the retry; the checkpoint finalizes the
        // existing execution instead of allocating a new projection.
        state
            .apply_event(&test_stage_event(
                6,
                EventBody::CheckpointCompleted(CheckpointCompletedProps {
                    graph_visit: Some(1),
                    resumed_from_stage_id: None,
                    status: "skipped".to_string(),
                    current_node: "gate".to_string(),
                    completed_nodes: vec!["gate".to_string()],
                    node_retries: BTreeMap::new(),
                    context_values: BTreeMap::new(),
                    node_outcomes: BTreeMap::from([(
                        "gate".to_string(),
                        Outcome::skipped("skipped on retry"),
                    )]),
                    next_node_id: None,
                    git_commit_sha: None,
                    loop_failure_signatures: BTreeMap::new(),
                    restart_failure_signatures: BTreeMap::new(),
                    node_visits: BTreeMap::from([("gate".to_string(), 1usize)]),
                    diff: None,
                    diff_summary: None,
                }),
                execution.clone(),
            ))
            .unwrap();

        let stage = state.stage(&execution).unwrap();
        assert_eq!(stage.state, StageState::Skipped);
        assert_eq!(stage.first_event_seq, first_event_seq(4));
        assert!(state.stage(&StageId::new("gate", 2)).is_none());
    }

    #[test]
    fn skipped_checkpoint_never_reopens_an_older_terminal_execution() {
        let mut state = running_projection();
        let execution = StageId::new("gate", 1);

        state
            .apply_event(&test_stage_event(
                4,
                EventBody::StageStarted(started_props()),
                execution.clone(),
            ))
            .unwrap();
        state
            .apply_event(&test_stage_event(
                5,
                EventBody::StageCompleted(completed_props(2_000, StageOutcome::Succeeded)),
                execution.clone(),
            ))
            .unwrap();
        state
            .apply_event(&test_stage_event(
                6,
                EventBody::CheckpointCompleted(CheckpointCompletedProps {
                    graph_visit: Some(1),
                    resumed_from_stage_id: None,
                    status: "skipped".to_string(),
                    current_node: "gate".to_string(),
                    completed_nodes: vec!["gate".to_string()],
                    node_retries: BTreeMap::new(),
                    context_values: BTreeMap::new(),
                    node_outcomes: BTreeMap::from([(
                        "gate".to_string(),
                        Outcome::skipped("late skip"),
                    )]),
                    next_node_id: None,
                    git_commit_sha: None,
                    loop_failure_signatures: BTreeMap::new(),
                    restart_failure_signatures: BTreeMap::new(),
                    node_visits: BTreeMap::from([("gate".to_string(), 1usize)]),
                    diff: None,
                    diff_summary: None,
                }),
                execution.clone(),
            ))
            .unwrap();

        let stage = state.stage(&execution).unwrap();
        assert_eq!(stage.state, StageState::Succeeded);
        assert_eq!(
            stage.completion.as_ref().unwrap().outcome,
            StageOutcome::Succeeded
        );
    }

    #[test]
    fn legacy_stage_started_payload_without_identity_fields_deserializes() {
        let event = test_raw_event(
            4,
            "stage.started",
            &json!({
                "index": 0,
                "handler_type": "agent",
                "attempt": 1,
                "max_attempts": 3
            }),
            Some("work"),
        );

        let EventBody::StageStarted(props) = &event.event.body else {
            panic!("expected stage.started body");
        };
        assert_eq!(props.graph_visit, None);
        assert_eq!(props.resumed_from_stage_id, None);
    }

    #[test]
    fn run_failed_non_cancelled_finalizes_running_stage_as_failed() {
        let mut state = running_projection();
        let stage_id = StageId::new("build", 1);

        state
            .apply_event(&test_stage_event_at(
                4,
                "2026-04-07T12:00:00Z",
                EventBody::StageStarted(started_props()),
                stage_id.clone(),
            ))
            .unwrap();
        state
            .apply_event(&test_raw_event_at(
                5,
                "2026-04-07T12:00:05Z",
                "run.failed",
                &serde_json::to_value(run_failed_props(FailureReason::WorkflowError)).unwrap(),
                None,
            ))
            .unwrap();

        let stage = state.stage(&stage_id).unwrap();
        assert_eq!(stage.state, StageState::Failed);
        assert_eq!(
            stage.timing,
            Some(fabro_types::StageTiming::wall_only(5_000))
        );
    }

    #[test]
    fn run_failed_preserves_already_terminal_stage_projection() {
        let mut state = running_projection();
        let stage_id = StageId::new("build", 1);

        state
            .apply_event(&test_stage_event_at(
                4,
                "2026-04-07T12:00:00Z",
                EventBody::StageStarted(started_props()),
                stage_id.clone(),
            ))
            .unwrap();
        state
            .apply_event(&test_stage_event(
                5,
                EventBody::StageCompleted(completed_props(42, StageOutcome::Succeeded)),
                stage_id.clone(),
            ))
            .unwrap();
        state
            .apply_event(&test_raw_event_at(
                6,
                "2026-04-07T12:00:05Z",
                "run.failed",
                &serde_json::to_value(run_failed_props(FailureReason::Cancelled)).unwrap(),
                None,
            ))
            .unwrap();

        let stage = state.stage(&stage_id).unwrap();
        assert_eq!(stage.state, StageState::Succeeded);
        assert_eq!(stage.timing, Some(fabro_types::StageTiming::wall_only(42)));
    }

    #[test]
    fn stage_retrying_sets_retrying_state() {
        let mut state = initialized_projection();
        let stage_id = StageId::new("build", 1);

        state
            .apply_event(&test_stage_event(
                1,
                EventBody::StageStarted(started_props()),
                stage_id.clone(),
            ))
            .unwrap();
        state
            .apply_event(&test_event(
                2,
                EventBody::StageFailed(failed_props(10, true)),
                Some("build"),
            ))
            .unwrap();
        state
            .apply_event(&test_event(
                3,
                EventBody::StageRetrying(retrying_props()),
                Some("build"),
            ))
            .unwrap();

        let stage = state.stage(&stage_id).unwrap();
        assert_eq!(stage.state, StageState::Retrying);
    }

    #[test]
    fn stage_started_after_retrying_returns_to_running_and_resets_attempt_data() {
        let mut state = initialized_projection();
        let stage_id = StageId::new("build", 1);

        state
            .apply_event(&test_stage_event(
                1,
                EventBody::StageStarted(started_props()),
                stage_id.clone(),
            ))
            .unwrap();
        state
            .apply_event(&test_event(
                2,
                EventBody::StageFailed(failed_props(10, true)),
                Some("build"),
            ))
            .unwrap();
        state
            .apply_event(&test_event(
                3,
                EventBody::StageRetrying(retrying_props()),
                Some("build"),
            ))
            .unwrap();
        state
            .apply_event(&test_stage_event(
                4,
                EventBody::StageStarted(started_props()),
                stage_id.clone(),
            ))
            .unwrap();

        let stage = state.stage(&stage_id).unwrap();
        assert_eq!(stage.state, StageState::Running);
        // Prior attempt's terminal data must not leak into the new attempt.
        assert!(stage.completion.is_none());
        assert_eq!(stage.timing.map(|t| t.wall_time_ms), None);
    }

    mod todo_reducer {
        use fabro_types::run_event::{TodoCreatedProps, TodoDeletedProps, TodoUpdatedProps};
        use fabro_types::{TodoListKind, TodoListProjection, TodoStatus};

        use super::*;

        fn stage_id() -> StageId {
            StageId::new("code", 1)
        }

        fn root_agent_todos<'a>(
            state: &'a RunProjection,
            stage_id: &StageId,
        ) -> &'a TodoListProjection {
            state
                .stage(stage_id)
                .and_then(|stage| stage.root_agent_todos.as_ref())
                .expect("root agent todos present")
        }

        fn child_stage_event(seq: u32, body: EventBody, stage_id: StageId) -> EventEnvelope {
            let mut event = test_stage_event(seq, body, stage_id);
            event.event.session_id = Some(format!("child-session-{seq}"));
            event.event.parent_session_id = Some("root-session".to_string());
            event
        }

        fn created(
            list: &str,
            list_kind: TodoListKind,
            id: &str,
            order: u32,
            subject: &str,
        ) -> EventBody {
            EventBody::TodoCreated(TodoCreatedProps {
                list_id: list.to_string(),
                list_kind,
                todo_id: id.to_string(),
                status: TodoStatus::Pending,
                order,
                subject: subject.to_string(),
                description: String::new(),
                active_form: None,
                owner: None,
                blocks: Vec::new(),
                blocked_by: Vec::new(),
                metadata: BTreeMap::new(),
            })
        }

        fn updated_status(
            list: &str,
            list_kind: TodoListKind,
            id: &str,
            status: TodoStatus,
        ) -> EventBody {
            EventBody::TodoUpdated(TodoUpdatedProps {
                list_id: list.to_string(),
                list_kind,
                todo_id: id.to_string(),
                status: Some(status),
                order: None,
                subject: None,
                description: None,
                active_form: None,
                owner: None,
                add_blocks: None,
                add_blocked_by: None,
                metadata_patch: BTreeMap::new(),
            })
        }

        fn deleted(list: &str, list_kind: TodoListKind, id: &str) -> EventBody {
            EventBody::TodoDeleted(TodoDeletedProps {
                list_id: list.to_string(),
                list_kind,
                todo_id: id.to_string(),
            })
        }

        #[test]
        fn replay_reconstructs_current_list() {
            let mut state = initialized_projection();
            let stage_id = stage_id();
            let list = "openai_plan:ses_a";
            state
                .apply_event(&test_stage_event(
                    1,
                    created(list, TodoListKind::OpenAiPlan, "a", 0, "first"),
                    stage_id.clone(),
                ))
                .unwrap();
            state
                .apply_event(&test_stage_event(
                    2,
                    created(list, TodoListKind::OpenAiPlan, "b", 1, "second"),
                    stage_id.clone(),
                ))
                .unwrap();
            state
                .apply_event(&test_stage_event(
                    3,
                    updated_status(list, TodoListKind::OpenAiPlan, "a", TodoStatus::InProgress),
                    stage_id.clone(),
                ))
                .unwrap();

            let projection = root_agent_todos(&state, &stage_id);
            assert_eq!(projection.list_id, list);
            assert_eq!(projection.items.len(), 2);
            assert_eq!(projection.items[0].id, "a");
            assert_eq!(projection.items[0].status, TodoStatus::InProgress);
            assert_eq!(projection.items[1].id, "b");
        }

        #[test]
        fn deleted_todos_are_absent() {
            let mut state = initialized_projection();
            let stage_id = stage_id();
            let list = "openai_plan:ses_a";
            state
                .apply_event(&test_stage_event(
                    1,
                    created(list, TodoListKind::OpenAiPlan, "a", 0, "first"),
                    stage_id.clone(),
                ))
                .unwrap();
            state
                .apply_event(&test_stage_event(
                    2,
                    created(list, TodoListKind::OpenAiPlan, "b", 1, "second"),
                    stage_id.clone(),
                ))
                .unwrap();
            state
                .apply_event(&test_stage_event(
                    3,
                    deleted(list, TodoListKind::OpenAiPlan, "a"),
                    stage_id.clone(),
                ))
                .unwrap();

            let projection = root_agent_todos(&state, &stage_id);
            assert_eq!(projection.items.len(), 1);
            assert_eq!(projection.items[0].id, "b");
        }

        #[test]
        fn stage_todo_lists_stay_isolated() {
            let mut state = initialized_projection();
            let plan_one = StageId::new("plan_one", 1);
            let plan_two = StageId::new("plan_two", 1);
            let claude = StageId::new("claude", 1);
            state
                .apply_event(&test_stage_event(
                    1,
                    created("openai_plan:s1", TodoListKind::OpenAiPlan, "a", 0, "p1"),
                    plan_one.clone(),
                ))
                .unwrap();
            state
                .apply_event(&test_stage_event(
                    2,
                    created("openai_plan:s2", TodoListKind::OpenAiPlan, "a", 0, "p2"),
                    plan_two.clone(),
                ))
                .unwrap();
            state
                .apply_event(&test_stage_event(
                    3,
                    created(
                        "anthropic_tasks:s_root",
                        TodoListKind::AnthropicTasks,
                        "1",
                        0,
                        "claude task",
                    ),
                    claude.clone(),
                ))
                .unwrap();

            assert_eq!(root_agent_todos(&state, &plan_one).items[0].subject, "p1");
            assert_eq!(root_agent_todos(&state, &plan_two).items[0].subject, "p2");
            assert_eq!(
                root_agent_todos(&state, &claude).items[0].subject,
                "claude task"
            );
        }

        #[test]
        fn root_openai_plan_remains_projected_after_child_plan_events() {
            let mut state = initialized_projection();
            let stage_id = stage_id();
            let root_list = "openai_plan:root_session";
            let child_list = "openai_plan:child_session";
            state
                .apply_event(&test_stage_event(
                    1,
                    created(
                        root_list,
                        TodoListKind::OpenAiPlan,
                        "root-a",
                        0,
                        "root first",
                    ),
                    stage_id.clone(),
                ))
                .unwrap();
            state
                .apply_event(&test_stage_event(
                    2,
                    created(
                        root_list,
                        TodoListKind::OpenAiPlan,
                        "root-b",
                        1,
                        "root second",
                    ),
                    stage_id.clone(),
                ))
                .unwrap();
            state
                .apply_event(&child_stage_event(
                    3,
                    created(
                        child_list,
                        TodoListKind::OpenAiPlan,
                        "child-a",
                        0,
                        "child first",
                    ),
                    stage_id.clone(),
                ))
                .unwrap();
            state
                .apply_event(&child_stage_event(
                    4,
                    created(
                        child_list,
                        TodoListKind::OpenAiPlan,
                        "child-b",
                        1,
                        "child second",
                    ),
                    stage_id.clone(),
                ))
                .unwrap();
            state
                .apply_event(&test_stage_event(
                    5,
                    updated_status(
                        root_list,
                        TodoListKind::OpenAiPlan,
                        "root-a",
                        TodoStatus::Completed,
                    ),
                    stage_id.clone(),
                ))
                .unwrap();
            state
                .apply_event(&test_stage_event(
                    6,
                    updated_status(
                        root_list,
                        TodoListKind::OpenAiPlan,
                        "root-b",
                        TodoStatus::Completed,
                    ),
                    stage_id.clone(),
                ))
                .unwrap();

            let projection = root_agent_todos(&state, &stage_id);
            assert_eq!(projection.list_id, root_list);
            assert_eq!(projection.kind, TodoListKind::OpenAiPlan);
            assert_eq!(projection.items.len(), 2);
            assert_eq!(projection.items[0].id, "root-a");
            assert_eq!(projection.items[0].status, TodoStatus::Completed);
            assert_eq!(projection.items[1].id, "root-b");
            assert_eq!(projection.items[1].status, TodoStatus::Completed);
        }

        #[test]
        fn child_session_whole_lists_do_not_project_when_root_has_no_list() {
            for (kind, child_list) in [
                (TodoListKind::OpenAiPlan, "openai_plan:child_session"),
                (TodoListKind::KimiTodos, "kimi_todos:child_session"),
            ] {
                let mut state = initialized_projection();
                let stage_id = stage_id();
                state
                    .apply_event(&test_stage_event(
                        1,
                        EventBody::StageStarted(started_props()),
                        stage_id.clone(),
                    ))
                    .unwrap();
                state
                    .apply_event(&child_stage_event(
                        2,
                        created(child_list, kind, "c-a", 0, "child work"),
                        stage_id.clone(),
                    ))
                    .unwrap();

                let stage = state.stage(&stage_id).expect("stage projection present");
                assert!(
                    stage.root_agent_todos.is_none(),
                    "a child session's {kind} list must not become the stage's root list"
                );
            }
        }

        #[test]
        fn root_openai_plan_projects_after_earlier_child_plan_event() {
            let mut state = initialized_projection();
            let stage_id = stage_id();
            let root_list = "openai_plan:root_session";
            state
                .apply_event(&test_stage_event(
                    1,
                    EventBody::StageStarted(started_props()),
                    stage_id.clone(),
                ))
                .unwrap();
            state
                .apply_event(&child_stage_event(
                    2,
                    created(
                        "openai_plan:child_session",
                        TodoListKind::OpenAiPlan,
                        "child-a",
                        0,
                        "child work",
                    ),
                    stage_id.clone(),
                ))
                .unwrap();
            state
                .apply_event(&test_stage_event(
                    3,
                    created(
                        root_list,
                        TodoListKind::OpenAiPlan,
                        "root-a",
                        0,
                        "root work",
                    ),
                    stage_id.clone(),
                ))
                .unwrap();

            let projection = root_agent_todos(&state, &stage_id);
            assert_eq!(projection.list_id, root_list);
            assert_eq!(projection.items.len(), 1);
            assert_eq!(projection.items[0].subject, "root work");
        }

        #[test]
        fn anthropic_child_session_task_events_still_project() {
            let mut state = initialized_projection();
            let stage_id = stage_id();
            let list = "anthropic_tasks:root_session";
            state
                .apply_event(&child_stage_event(
                    1,
                    created(
                        list,
                        TodoListKind::AnthropicTasks,
                        "task-a",
                        0,
                        "task first",
                    ),
                    stage_id.clone(),
                ))
                .unwrap();
            state
                .apply_event(&child_stage_event(
                    2,
                    updated_status(
                        list,
                        TodoListKind::AnthropicTasks,
                        "task-a",
                        TodoStatus::Completed,
                    ),
                    stage_id.clone(),
                ))
                .unwrap();

            let projection = root_agent_todos(&state, &stage_id);
            assert_eq!(projection.list_id, list);
            assert_eq!(projection.kind, TodoListKind::AnthropicTasks);
            assert_eq!(projection.items.len(), 1);
            assert_eq!(projection.items[0].id, "task-a");
            assert_eq!(projection.items[0].status, TodoStatus::Completed);
        }

        #[test]
        fn metadata_patch_merges_and_null_deletes() {
            let mut state = initialized_projection();
            let stage_id = stage_id();
            let list = "anthropic_tasks:r";
            state
                .apply_event(&test_stage_event(
                    1,
                    created(list, TodoListKind::AnthropicTasks, "1", 0, "t"),
                    stage_id.clone(),
                ))
                .unwrap();
            let mut meta = BTreeMap::new();
            meta.insert("k1".to_string(), serde_json::json!("v1"));
            meta.insert("k2".to_string(), serde_json::json!("v2"));
            state
                .apply_event(&test_stage_event(
                    2,
                    EventBody::TodoUpdated(TodoUpdatedProps {
                        list_id:        list.to_string(),
                        list_kind:      TodoListKind::AnthropicTasks,
                        todo_id:        "1".to_string(),
                        status:         None,
                        order:          None,
                        subject:        None,
                        description:    None,
                        active_form:    None,
                        owner:          None,
                        add_blocks:     None,
                        add_blocked_by: None,
                        metadata_patch: meta,
                    }),
                    stage_id.clone(),
                ))
                .unwrap();
            let mut delete = BTreeMap::new();
            delete.insert("k1".to_string(), serde_json::Value::Null);
            state
                .apply_event(&test_stage_event(
                    3,
                    EventBody::TodoUpdated(TodoUpdatedProps {
                        list_id:        list.to_string(),
                        list_kind:      TodoListKind::AnthropicTasks,
                        todo_id:        "1".to_string(),
                        status:         None,
                        order:          None,
                        subject:        None,
                        description:    None,
                        active_form:    None,
                        owner:          None,
                        add_blocks:     None,
                        add_blocked_by: None,
                        metadata_patch: delete,
                    }),
                    stage_id.clone(),
                ))
                .unwrap();

            let todo = &root_agent_todos(&state, &stage_id).items[0];
            assert!(!todo.metadata.contains_key("k1"));
            assert_eq!(todo.metadata.get("k2"), Some(&serde_json::json!("v2")));
        }
    }

    mod agent_state_reducer {
        use super::*;

        fn stage_id() -> StageId {
            StageId::new("code", 1)
        }

        #[test]
        fn interrupt_settlement_and_steering_update_agent_control_projection() {
            let mut state = initialized_projection();
            let stage_id = stage_id();

            state
                .apply_event(&test_stage_event(
                    1,
                    EventBody::AgentRoundInterrupted(AgentRoundInterruptedProps {
                        generation: 1,
                        visit:      1,
                    }),
                    stage_id.clone(),
                ))
                .unwrap();
            assert_eq!(
                state.stage(&stage_id).unwrap().agent_control,
                AgentControlState::WaitingForSteer
            );

            state
                .apply_event(&test_stage_event(
                    2,
                    EventBody::AgentSteeringInjected(AgentSteeringInjectedProps {
                        text:  "continue".to_string(),
                        visit: 1,
                    }),
                    stage_id.clone(),
                ))
                .unwrap();
            assert_eq!(
                state.stage(&stage_id).unwrap().agent_control,
                AgentControlState::Running
            );

            state
                .apply_event(&test_stage_event(
                    3,
                    EventBody::AgentRoundInterrupted(AgentRoundInterruptedProps {
                        generation: 2,
                        visit:      1,
                    }),
                    stage_id.clone(),
                ))
                .unwrap();
            state
                .apply_event(&test_stage_event(
                    4,
                    EventBody::AgentSessionDeactivated(AgentSessionDeactivatedProps { visit: 1 }),
                    stage_id.clone(),
                ))
                .unwrap();
            assert_eq!(
                state.stage(&stage_id).unwrap().agent_control,
                AgentControlState::Running
            );

            state
                .apply_event(&test_stage_event(
                    5,
                    EventBody::AgentRoundInterrupted(AgentRoundInterruptedProps {
                        generation: 3,
                        visit:      1,
                    }),
                    stage_id.clone(),
                ))
                .unwrap();
            state
                .apply_event(&test_stage_event(
                    6,
                    EventBody::StageFailed(failed_props(10, false)),
                    stage_id.clone(),
                ))
                .unwrap();
            assert_eq!(
                state.stage(&stage_id).unwrap().agent_control,
                AgentControlState::Running
            );
        }

        #[test]
        fn subagent_events_update_stage_projection() {
            let mut state = initialized_projection();
            let stage_id = stage_id();

            state
                .apply_event(&test_stage_event(
                    1,
                    EventBody::AgentSubSpawned(AgentSubSpawnedProps {
                        agent_id:   "sub-1".to_string(),
                        depth:      1,
                        task:       "write tests".to_string(),
                        generation: 1,
                        visit:      1,
                    }),
                    stage_id.clone(),
                ))
                .unwrap();
            let stage = state.stage(&stage_id).unwrap();
            assert_eq!(stage.subagents.len(), 1);
            assert_eq!(stage.subagents[0].agent_id, "sub-1");
            assert_eq!(stage.subagents[0].depth, 1);
            assert_eq!(stage.subagents[0].task, "write tests");
            assert_eq!(stage.subagents[0].status, SubAgentStatus::Running);

            state
                .apply_event(&test_stage_event(
                    2,
                    EventBody::AgentSubCompleted(AgentSubCompletedProps {
                        agent_id:   "sub-1".to_string(),
                        depth:      1,
                        generation: 1,
                        success:    true,
                        turns_used: 3,
                        visit:      1,
                    }),
                    stage_id.clone(),
                ))
                .unwrap();
            let stage = state.stage(&stage_id).unwrap();
            assert_eq!(stage.subagents[0].status, SubAgentStatus::Completed {
                success:    true,
                turns_used: 3,
            });

            state
                .apply_event(&test_stage_event(
                    3,
                    EventBody::AgentSubTurnStarted(AgentSubTurnStartedProps {
                        agent_id:   "sub-1".to_string(),
                        depth:      1,
                        task:       "fix the review findings".to_string(),
                        generation: 2,
                        visit:      1,
                    }),
                    stage_id.clone(),
                ))
                .unwrap();
            let stage = state.stage(&stage_id).unwrap();
            assert_eq!(stage.subagents.len(), 1);
            assert_eq!(stage.subagents[0].task, "write tests");
            assert_eq!(stage.subagents[0].status, SubAgentStatus::Running);

            state
                .apply_event(&test_stage_event(
                    4,
                    EventBody::AgentSubCompleted(AgentSubCompletedProps {
                        agent_id:   "sub-1".to_string(),
                        depth:      1,
                        generation: 2,
                        success:    true,
                        turns_used: 5,
                        visit:      1,
                    }),
                    stage_id.clone(),
                ))
                .unwrap();
            let stage = state.stage(&stage_id).unwrap();
            assert_eq!(stage.subagents.len(), 1);
            assert_eq!(stage.subagents[0].status, SubAgentStatus::Completed {
                success:    true,
                turns_used: 5,
            });

            state
                .apply_event(&test_stage_event(
                    5,
                    EventBody::AgentSubSpawned(AgentSubSpawnedProps {
                        agent_id:   "sub-2".to_string(),
                        depth:      2,
                        task:       "debug failure".to_string(),
                        generation: 1,
                        visit:      1,
                    }),
                    stage_id.clone(),
                ))
                .unwrap();
            state
                .apply_event(&test_stage_event(
                    6,
                    EventBody::AgentSubFailed(AgentSubFailedProps {
                        agent_id:   "sub-2".to_string(),
                        depth:      2,
                        generation: 1,
                        error:      json!({ "message": "boom" }),
                        visit:      1,
                    }),
                    stage_id.clone(),
                ))
                .unwrap();
            let stage = state.stage(&stage_id).unwrap();
            assert_eq!(stage.subagents[1].status, SubAgentStatus::Failed {
                error: json!({ "message": "boom" }),
            });

            state
                .apply_event(&test_stage_event(
                    7,
                    EventBody::AgentSubClosed(AgentSubClosedProps {
                        agent_id:   "sub-2".to_string(),
                        depth:      2,
                        generation: 1,
                        visit:      1,
                    }),
                    stage_id.clone(),
                ))
                .unwrap();
            let stage = state.stage(&stage_id).unwrap();
            assert_eq!(stage.subagents[1].status, SubAgentStatus::Closed);
        }

        #[test]
        fn skill_events_update_stage_projection() {
            let mut state = initialized_projection();
            let stage_id = stage_id();

            state
                .apply_event(&test_stage_event(
                    1,
                    EventBody::AgentSkillsDiscovered(AgentSkillsDiscoveredProps {
                        provider_profile: "claude".to_string(),
                        source_dirs:      vec![".claude/skills".to_string()],
                        skills:           vec![
                            AgentSkillSummary {
                                name:        "rust".to_string(),
                                description: "Rust help".to_string(),
                            },
                            AgentSkillSummary {
                                name:        "docs".to_string(),
                                description: "Docs help".to_string(),
                            },
                        ],
                        visit:            1,
                    }),
                    stage_id.clone(),
                ))
                .unwrap();
            state
                .apply_event(&test_stage_event(
                    2,
                    EventBody::AgentSkillActivated(AgentSkillActivatedProps {
                        skill_name: "rust".to_string(),
                        source:     AgentSkillActivationSource::Slash,
                        visit:      1,
                    }),
                    stage_id.clone(),
                ))
                .unwrap();
            state
                .apply_event(&test_stage_event(
                    3,
                    EventBody::AgentSkillActivated(AgentSkillActivatedProps {
                        skill_name: "rust".to_string(),
                        source:     AgentSkillActivationSource::Tool,
                        visit:      1,
                    }),
                    stage_id.clone(),
                ))
                .unwrap();

            let stage = state.stage(&stage_id).unwrap();
            assert_eq!(stage.skills.available.len(), 2);
            assert_eq!(stage.skills.available[0].name, "rust");
            assert_eq!(stage.skills.activated.len(), 2);
            assert_eq!(stage.skills.activated[0].name, "rust");
            assert_eq!(
                stage.skills.activated[0].source,
                AgentSkillActivationSource::Slash
            );
            assert_eq!(
                stage.skills.activated[1].source,
                AgentSkillActivationSource::Tool
            );
        }

        #[test]
        fn agent_session_activation_updates_stage_permission_level_projection() {
            fn activated_props(
                permission_level: Option<PermissionLevel>,
            ) -> AgentSessionActivatedProps {
                AgentSessionActivatedProps {
                    thread_id: None,
                    provider: Some("openai".to_string()),
                    model: Some("gpt-5.4".to_string()),
                    reasoning_effort: None,
                    speed: None,
                    permission_level,
                    capabilities: vec![],
                    visit: 1,
                }
            }

            let mut state = initialized_projection();
            let stage_id = stage_id();

            state
                .apply_event(&test_stage_event(
                    1,
                    EventBody::AgentSessionActivated(activated_props(Some(
                        PermissionLevel::ReadOnly,
                    ))),
                    stage_id.clone(),
                ))
                .unwrap();

            let stage = state.stage(&stage_id).unwrap();
            assert_eq!(stage.permission_level, Some(PermissionLevel::ReadOnly));

            let mut legacy_state = initialized_projection();
            legacy_state
                .apply_event(&test_stage_event(
                    1,
                    EventBody::AgentSessionActivated(activated_props(None)),
                    stage_id.clone(),
                ))
                .unwrap();

            let legacy_stage = legacy_state.stage(&stage_id).unwrap();
            assert_eq!(legacy_stage.permission_level, None);
        }

        fn agent_tool(name: &str, category: AgentToolCategory, invoked: bool) -> AgentToolSummary {
            AgentToolSummary {
                name: name.to_string(),
                description: format!("{name} description"),
                source: AgentToolSource::Native,
                category,
                invoked,
            }
        }

        #[test]
        fn agent_tools_available_replaces_stage_agent_tools() {
            let mut state = initialized_projection();
            let stage_id = stage_id();

            state
                .apply_event(&test_stage_event(
                    1,
                    EventBody::AgentToolsAvailable(AgentToolsAvailableProps {
                        tools: vec![
                            agent_tool("read_file", AgentToolCategory::Read, false),
                            agent_tool("apply_patch", AgentToolCategory::Write, false),
                        ],
                        visit: 1,
                    }),
                    stage_id.clone(),
                ))
                .unwrap();
            state
                .apply_event(&test_stage_event(
                    2,
                    EventBody::AgentToolsAvailable(AgentToolsAvailableProps {
                        tools: vec![agent_tool("grep", AgentToolCategory::Read, false)],
                        visit: 1,
                    }),
                    stage_id.clone(),
                ))
                .unwrap();

            let stage = state.stage(&stage_id).unwrap();
            assert_eq!(stage.agent_tools, vec![agent_tool(
                "grep",
                AgentToolCategory::Read,
                false
            )]);
        }

        #[test]
        fn agent_tool_started_marks_only_matching_available_tool_invoked() {
            let mut state = initialized_projection();
            let stage_id = stage_id();

            state
                .apply_event(&test_stage_event(
                    1,
                    EventBody::AgentToolsAvailable(AgentToolsAvailableProps {
                        tools: vec![
                            agent_tool("read_file", AgentToolCategory::Read, false),
                            agent_tool("apply_patch", AgentToolCategory::Write, false),
                        ],
                        visit: 1,
                    }),
                    stage_id.clone(),
                ))
                .unwrap();
            state
                .apply_event(&test_stage_event(
                    2,
                    EventBody::AgentToolStarted(AgentToolStartedProps {
                        tool_name:         "apply_patch".to_string(),
                        tool_call_id:      "call_patch".to_string(),
                        arguments:         serde_json::json!({}),
                        visit:             1,
                        tool_call:         None,
                        turn_id:           None,
                        parent_message_id: None,
                    }),
                    stage_id.clone(),
                ))
                .unwrap();

            let stage = state.stage(&stage_id).unwrap();
            assert!(!stage.agent_tools[0].invoked);
            assert!(stage.agent_tools[1].invoked);
        }

        #[test]
        fn legacy_tool_started_without_available_tools_does_not_synthesize_tool_list() {
            let mut state = initialized_projection();
            let stage_id = stage_id();

            state
                .apply_event(&test_stage_event(
                    1,
                    EventBody::AgentToolStarted(AgentToolStartedProps {
                        tool_name:         "apply_patch".to_string(),
                        tool_call_id:      "call_patch".to_string(),
                        arguments:         serde_json::json!({}),
                        visit:             1,
                        tool_call:         None,
                        turn_id:           None,
                        parent_message_id: None,
                    }),
                    stage_id.clone(),
                ))
                .unwrap();

            let stage = state.stage(&stage_id).unwrap();
            assert!(stage.agent_tools.is_empty());
        }

        #[test]
        fn mcp_server_events_update_stage_projection() {
            let mut state = initialized_projection();
            let stage_id = stage_id();

            state
                .apply_event(&test_stage_event(
                    1,
                    EventBody::AgentMcpReady(AgentMcpReadyProps {
                        server_name: "filesystem".to_string(),
                        tool_count:  2,
                        tools:       vec![
                            AgentMcpToolSummary {
                                name:          "read_file".to_string(),
                                original_name: "read_file".to_string(),
                            },
                            AgentMcpToolSummary {
                                name:          "write_file".to_string(),
                                original_name: "write_file".to_string(),
                            },
                        ],
                        visit:       1,
                    }),
                    stage_id.clone(),
                ))
                .unwrap();
            state
                .apply_event(&test_stage_event(
                    2,
                    EventBody::AgentMcpFailed(AgentMcpFailedProps {
                        server_name: "github".to_string(),
                        error:       "missing token".to_string(),
                        visit:       1,
                    }),
                    stage_id.clone(),
                ))
                .unwrap();
            state
                .apply_event(&test_stage_event(
                    3,
                    EventBody::AgentMcpReady(AgentMcpReadyProps {
                        server_name: "filesystem".to_string(),
                        tool_count:  1,
                        tools:       vec![AgentMcpToolSummary {
                            name:          "read_file".to_string(),
                            original_name: "read_file".to_string(),
                        }],
                        visit:       1,
                    }),
                    stage_id.clone(),
                ))
                .unwrap();

            let stage = state.stage(&stage_id).unwrap();
            assert_eq!(stage.mcp_servers.len(), 2);
            assert_eq!(stage.mcp_servers[0].server_name, "filesystem");
            assert_eq!(stage.mcp_servers[0].tool_count, 1);
            assert_eq!(stage.mcp_servers[0].status, McpServerStatus::Ready {
                tools: vec![AgentMcpToolSummary {
                    name:          "read_file".to_string(),
                    original_name: "read_file".to_string(),
                }],
            });
            assert!(!stage.mcp_servers[0].invoked);
            assert_eq!(stage.mcp_servers[1].server_name, "github");
            assert_eq!(stage.mcp_servers[1].tool_count, 0);
            assert_eq!(stage.mcp_servers[1].status, McpServerStatus::Failed {
                error: "missing token".to_string(),
            });
            assert!(!stage.mcp_servers[1].invoked);
        }

        #[test]
        fn agent_tool_started_marks_matching_mcp_server_as_invoked() {
            let mut state = initialized_projection();
            let stage_id = stage_id();

            state
                .apply_event(&test_stage_event(
                    1,
                    EventBody::AgentMcpReady(AgentMcpReadyProps {
                        server_name: "filesystem".to_string(),
                        tool_count:  1,
                        tools:       vec![AgentMcpToolSummary {
                            name:          "read_file".to_string(),
                            original_name: "read_file".to_string(),
                        }],
                        visit:       1,
                    }),
                    stage_id.clone(),
                ))
                .unwrap();
            state
                .apply_event(&test_stage_event(
                    2,
                    EventBody::AgentMcpReady(AgentMcpReadyProps {
                        server_name: "other".to_string(),
                        tool_count:  0,
                        tools:       vec![],
                        visit:       1,
                    }),
                    stage_id.clone(),
                ))
                .unwrap();
            // Native (non-MCP) tool call: should not touch any MCP server.
            state
                .apply_event(&test_stage_event(
                    3,
                    EventBody::AgentToolStarted(AgentToolStartedProps {
                        tool_name:         "Bash".to_string(),
                        tool_call_id:      "call_bash".to_string(),
                        arguments:         serde_json::json!({}),
                        visit:             1,
                        tool_call:         None,
                        turn_id:           None,
                        parent_message_id: None,
                    }),
                    stage_id.clone(),
                ))
                .unwrap();
            // Qualified MCP tool call: flips matching server's `invoked`.
            state
                .apply_event(&test_stage_event(
                    4,
                    EventBody::AgentToolStarted(AgentToolStartedProps {
                        tool_name:         "mcp__filesystem__read_file".to_string(),
                        tool_call_id:      "call_fs".to_string(),
                        arguments:         serde_json::json!({}),
                        visit:             1,
                        tool_call:         None,
                        turn_id:           None,
                        parent_message_id: None,
                    }),
                    stage_id.clone(),
                ))
                .unwrap();

            let stage = state.stage(&stage_id).unwrap();
            let filesystem = stage
                .mcp_servers
                .iter()
                .find(|s| s.server_name == "filesystem")
                .unwrap();
            assert!(filesystem.invoked, "filesystem should be marked invoked");
            let other = stage
                .mcp_servers
                .iter()
                .find(|s| s.server_name == "other")
                .unwrap();
            assert!(!other.invoked, "unused MCP server should stay un-invoked");
        }

        #[test]
        fn mcp_invoked_flag_survives_status_reread() {
            let mut state = initialized_projection();
            let stage_id = stage_id();

            state
                .apply_event(&test_stage_event(
                    1,
                    EventBody::AgentMcpReady(AgentMcpReadyProps {
                        server_name: "filesystem".to_string(),
                        tool_count:  1,
                        tools:       vec![AgentMcpToolSummary {
                            name:          "read_file".to_string(),
                            original_name: "read_file".to_string(),
                        }],
                        visit:       1,
                    }),
                    stage_id.clone(),
                ))
                .unwrap();
            state
                .apply_event(&test_stage_event(
                    2,
                    EventBody::AgentToolStarted(AgentToolStartedProps {
                        tool_name:         "mcp__filesystem__read_file".to_string(),
                        tool_call_id:      "call_fs".to_string(),
                        arguments:         serde_json::json!({}),
                        visit:             1,
                        tool_call:         None,
                        turn_id:           None,
                        parent_message_id: None,
                    }),
                    stage_id.clone(),
                ))
                .unwrap();
            // Server re-reports Ready (e.g. tool registry refresh): invoked
            // must remain true, not get clobbered back to false.
            state
                .apply_event(&test_stage_event(
                    3,
                    EventBody::AgentMcpReady(AgentMcpReadyProps {
                        server_name: "filesystem".to_string(),
                        tool_count:  2,
                        tools:       vec![
                            AgentMcpToolSummary {
                                name:          "read_file".to_string(),
                                original_name: "read_file".to_string(),
                            },
                            AgentMcpToolSummary {
                                name:          "stat".to_string(),
                                original_name: "stat".to_string(),
                            },
                        ],
                        visit:       1,
                    }),
                    stage_id.clone(),
                ))
                .unwrap();

            let stage = state.stage(&stage_id).unwrap();
            assert!(stage.mcp_servers[0].invoked);
            assert_eq!(stage.mcp_servers[0].tool_count, 2);
        }

        #[test]
        fn agent_messages_replace_latest_context_window_for_matching_stage() {
            let mut state = initialized_projection();
            let stage_id = stage_id();
            let first = context_window_snapshot(10);
            let second = context_window_snapshot(20);

            state
                .apply_event(&test_stage_event(
                    7,
                    EventBody::AgentMessage(agent_message_with_context_window(first)),
                    stage_id.clone(),
                ))
                .unwrap();
            state
                .apply_event(&test_stage_event(
                    8,
                    EventBody::AgentMessage(agent_message_with_context_window(second)),
                    stage_id.clone(),
                ))
                .unwrap();

            let stage = state.stage(&stage_id).unwrap();
            let snapshot = stage.context_window.as_ref().unwrap();
            assert_eq!(snapshot.input_tokens, 20);
            assert_eq!(snapshot.event_seq, Some(8));
        }

        #[test]
        fn agent_message_without_context_window_preserves_existing_context_window() {
            let mut state = initialized_projection();
            let stage_id = stage_id();

            state
                .apply_event(&test_stage_event(
                    7,
                    EventBody::AgentMessage(agent_message_with_context_window(
                        context_window_snapshot(10),
                    )),
                    stage_id.clone(),
                ))
                .unwrap();
            state
                .apply_event(&test_stage_event(
                    8,
                    EventBody::AgentMessage(live_agent_message_props(live_counts(1, 1))),
                    stage_id.clone(),
                ))
                .unwrap();

            let snapshot = state
                .stage(&stage_id)
                .unwrap()
                .context_window
                .as_ref()
                .unwrap();
            assert_eq!(snapshot.input_tokens, 10);
            assert_eq!(snapshot.event_seq, Some(7));
        }

        fn agent_message_with_context_window(
            context_window: StageContextWindowProjection,
        ) -> AgentMessageProps {
            AgentMessageProps {
                context_window: Some(context_window),
                ..live_agent_message_props(live_counts(1, 1))
            }
        }

        fn context_window_snapshot(input_tokens: u64) -> StageContextWindowProjection {
            StageContextWindowProjection {
                provider: "openai".to_string(),
                model: "gpt-5.4".to_string(),
                context_window_tokens: 400_000,
                input_tokens,
                usage_percent: input_tokens as f64 * 100.0 / 400_000.0,
                count_method: StageContextWindowCountMethod::LocalEstimate,
                staleness: StageContextWindowStaleness::Live,
                generated_at: Utc::now(),
                event_seq: None,
                breakdown: vec![StageContextWindowBreakdownItem {
                    category:      StageContextWindowCategory::Conversation,
                    tokens:        input_tokens,
                    usage_percent: input_tokens as f64 * 100.0 / 400_000.0,
                }],
                warnings: vec![StageContextWindowWarning {
                    code:    "local_token_estimate".to_string(),
                    message: "input token count is a local estimate".to_string(),
                }],
            }
        }
    }

    mod inference_bracket_reducer {
        use fabro_types::run_event::{
            AgentErrorProps, AgentLlmFirstOutputProps, AgentLlmRetryProps, AgentLlmStartedProps,
        };
        use fabro_types::{
            LlmOutputKind, LlmRetryPhase, ModelRef, Speed, StageInferenceProjection,
        };

        use super::*;

        const ROOT: &str = "ses_root";

        fn stage_id() -> StageId {
            StageId::new("code", 1)
        }

        /// Stage-addressed event attributed to a root session.
        fn root_event(seq: u32, body: EventBody) -> EventEnvelope {
            let mut event = test_stage_event(seq, body, stage_id());
            event.event.session_id = Some(ROOT.to_string());
            event
        }

        /// Forwarded child-session event: same stage, but with a parent link.
        fn child_event(seq: u32, body: EventBody) -> EventEnvelope {
            let mut event = test_stage_event(seq, body, stage_id());
            event.event.session_id = Some("ses_child".to_string());
            event.event.parent_session_id = Some(ROOT.to_string());
            event
        }

        /// `agent.session.ended` as it is actually stored: session ids only,
        /// no `node_id` and no `stage_id`.
        fn session_ended_event(seq: u32, session_id: &str) -> EventEnvelope {
            let mut event = test_event(
                seq,
                EventBody::AgentSessionEnded(AgentSessionEndedProps {}),
                None,
            );
            event.event.session_id = Some(session_id.to_string());
            event
        }

        fn started() -> EventBody {
            EventBody::AgentLlmStarted(AgentLlmStartedProps {
                requested_model: ModelRef {
                    provider: "anthropic".parse().unwrap(),
                    model_id: "claude-fable-5".into(),
                    speed:    Some(Speed::Fast),
                },
                visit:           1,
            })
        }

        fn first_output(kind: LlmOutputKind) -> EventBody {
            EventBody::AgentLlmFirstOutput(AgentLlmFirstOutputProps { kind, visit: 1 })
        }

        fn retry(phase: LlmRetryPhase) -> EventBody {
            EventBody::AgentLlmRetry(AgentLlmRetryProps {
                provider:   "anthropic".to_string(),
                model:      "claude-fable-5".to_string(),
                attempt:    0,
                delay_secs: 0.0,
                error:      json!({ "kind": "stream" }),
                phase:      Some(phase),
                visit:      1,
            })
        }

        fn open_bracket(state: &RunProjection) -> Option<&StageInferenceProjection> {
            state.stage(&stage_id()).unwrap().inference.as_ref()
        }

        #[test]
        fn started_opens_a_bracket_carrying_the_requested_model() {
            let mut state = initialized_projection();
            state.apply_event(&root_event(1, started())).unwrap();

            let inference = open_bracket(&state).expect("bracket should be open");
            assert_eq!(inference.session_id, ROOT);
            assert_eq!(inference.requested_model.provider.as_str(), "anthropic");
            assert_eq!(
                inference.requested_model.model_id.as_str(),
                "claude-fable-5"
            );
            assert_eq!(inference.requested_model.speed, Some(Speed::Fast));
            assert_eq!(inference.first_output_at, None);
            assert_eq!(inference.first_output_kind, None);
            assert_eq!(inference.retries, 0);
        }

        #[test]
        fn first_output_records_the_observed_kind() {
            let mut state = initialized_projection();
            state.apply_event(&root_event(1, started())).unwrap();
            state
                .apply_event(&root_event(2, first_output(LlmOutputKind::ToolCall)))
                .unwrap();

            let inference = open_bracket(&state).unwrap();
            assert!(inference.first_output_at.is_some());
            assert_eq!(inference.first_output_kind, Some(LlmOutputKind::ToolCall));
        }

        #[test]
        fn message_closes_the_bracket() {
            let mut state = initialized_projection();
            state.apply_event(&root_event(1, started())).unwrap();
            state
                .apply_event(&root_event(2, first_output(LlmOutputKind::Text)))
                .unwrap();
            state
                .apply_event(&root_event(
                    3,
                    EventBody::AgentMessage(live_agent_message_props(live_counts(10, 5))),
                ))
                .unwrap();

            assert!(open_bracket(&state).is_none());
            // The close must not undo the rest of the message's work.
            assert_eq!(state.stage(&stage_id()).unwrap().usage.input_tokens, 10);
        }

        #[test]
        fn error_and_round_interrupt_close_the_bracket() {
            for close in [
                EventBody::AgentError(AgentErrorProps {
                    error: json!({ "message": "boom" }),
                    visit: 1,
                }),
                EventBody::AgentRoundInterrupted(AgentRoundInterruptedProps {
                    generation: 1,
                    visit:      1,
                }),
            ] {
                let mut state = initialized_projection();
                state.apply_event(&root_event(1, started())).unwrap();
                state.apply_event(&root_event(2, close)).unwrap();
                assert!(open_bracket(&state).is_none());
            }
        }

        #[test]
        fn retry_counts_the_attempt_and_clears_observed_output() {
            let mut state = initialized_projection();
            state.apply_event(&root_event(1, started())).unwrap();
            state
                .apply_event(&root_event(2, first_output(LlmOutputKind::Text)))
                .unwrap();
            state
                .apply_event(&root_event(3, retry(LlmRetryPhase::Consume)))
                .unwrap();

            // Replay is event-driven, so the projection must forget output the
            // agent discarded rather than keep asserting it.
            let inference = open_bracket(&state).unwrap();
            assert_eq!(inference.retries, 1);
            assert_eq!(inference.first_output_at, None);
            assert_eq!(inference.first_output_kind, None);

            state
                .apply_event(&root_event(4, first_output(LlmOutputKind::Reasoning)))
                .unwrap();
            state
                .apply_event(&root_event(5, retry(LlmRetryPhase::Open)))
                .unwrap();
            let inference = open_bracket(&state).unwrap();
            assert_eq!(inference.retries, 2);
            assert_eq!(inference.first_output_kind, None);
        }

        #[test]
        fn session_ended_closes_a_bracket_it_cannot_address_by_stage() {
            let mut state = initialized_projection();
            state.apply_event(&root_event(1, started())).unwrap();
            assert!(open_bracket(&state).is_some());

            // Terminal cancel path: no message, error, or interrupt is ever
            // emitted. The envelope carries no node_id or stage_id, so a
            // normal stage lookup would silently no-op and leave the bracket
            // open forever.
            state.apply_event(&session_ended_event(2, ROOT)).unwrap();
            assert!(open_bracket(&state).is_none());
        }

        #[test]
        fn session_ended_from_another_session_leaves_the_bracket_open() {
            let mut state = initialized_projection();
            state.apply_event(&root_event(1, started())).unwrap();
            state
                .apply_event(&session_ended_event(2, "ses_other"))
                .unwrap();
            assert!(open_bracket(&state).is_some());
        }

        #[test]
        fn a_start_queued_behind_deactivation_does_not_leave_a_phantom_bracket() {
            let mut state = initialized_projection();

            // `lease.release()` emits deactivation *before* the forwarder
            // drains queued agent events, so a queued start legitimately
            // arrives after it. Keying the close on deactivation would clear
            // the bracket and then immediately re-open it.
            state
                .apply_event(&root_event(
                    1,
                    EventBody::AgentSessionDeactivated(AgentSessionDeactivatedProps { visit: 1 }),
                ))
                .unwrap();
            state.apply_event(&root_event(2, started())).unwrap();
            state.apply_event(&session_ended_event(3, ROOT)).unwrap();

            assert!(open_bracket(&state).is_none());
        }

        #[test]
        fn child_session_events_do_not_touch_the_root_bracket() {
            let mut state = initialized_projection();
            state.apply_event(&root_event(1, started())).unwrap();
            state
                .apply_event(&root_event(2, first_output(LlmOutputKind::Text)))
                .unwrap();

            // A sub-agent runs its own rounds on the same stage. None of them
            // may open, advance, or close the root session's bracket.
            state.apply_event(&child_event(3, started())).unwrap();
            state
                .apply_event(&child_event(4, first_output(LlmOutputKind::ToolCall)))
                .unwrap();
            state
                .apply_event(&child_event(5, retry(LlmRetryPhase::Open)))
                .unwrap();
            state
                .apply_event(&session_ended_event(6, "ses_child"))
                .unwrap();

            let inference = open_bracket(&state).expect("root bracket should survive");
            assert_eq!(inference.session_id, ROOT);
            assert_eq!(inference.first_output_kind, Some(LlmOutputKind::Text));
            assert_eq!(inference.retries, 0);
        }

        #[test]
        fn transitions_without_an_open_bracket_are_ignored() {
            let mut state = initialized_projection();
            state
                .apply_event(&root_event(1, first_output(LlmOutputKind::Text)))
                .unwrap();
            state
                .apply_event(&root_event(2, retry(LlmRetryPhase::Open)))
                .unwrap();
            assert!(open_bracket(&state).is_none());
        }
    }
}
