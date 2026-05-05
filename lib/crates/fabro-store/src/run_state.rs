use std::collections::{BTreeMap, HashMap};
use std::str::FromStr;

use chrono::{DateTime, Utc};
use fabro_types::run_event::{
    AgentCliStartedProps, AgentSessionStartedProps, CheckpointCompletedProps, RunCompletedProps,
    RunFailedProps, StageCompletedProps, StagePromptProps,
};
use fabro_types::{
    BilledModelUsage, Checkpoint, Conclusion, EventBody, FailureSignature, InterviewQuestionRecord,
    Outcome, PendingInterviewRecord, PullRequestRecord, RunControlAction, RunEvent, RunId,
    RunProjection, RunSpec, RunStatus, RunSummary, SandboxRecord, StageCompletion, StageId,
    StageOutcome, StageProjection, StageState, StartRecord, TerminalStatus, first_event_seq,
};
use fabro_util::error::render_with_causes;
use serde_json::Value;

use crate::{Error, EventEnvelope, Result};

#[derive(Debug, Clone, Default)]
pub(crate) struct EventProjectionCache {
    pub last_seq: u32,
    pub state:    RunProjection,
}

pub trait RunProjectionReducer {
    fn apply_events(events: &[EventEnvelope]) -> Result<Self>
    where
        Self: Sized;

    fn apply_event(&mut self, event: &EventEnvelope) -> Result<()>;
}

impl RunProjectionReducer for RunProjection {
    fn apply_events(events: &[EventEnvelope]) -> Result<Self> {
        let mut state = Self::default();
        for event in events {
            state.apply_event(event)?;
        }
        Ok(state)
    }

    fn apply_event(&mut self, event: &EventEnvelope) -> Result<()> {
        let stored = &event.event;
        let ts = stored.ts;
        let run_id = stored.run_id;

        match &stored.body {
            EventBody::RunCreated(props) => {
                let labels = props.labels.clone().into_iter().collect::<HashMap<_, _>>();
                self.spec = Some(RunSpec {
                    run_id,
                    settings: props.settings.clone(),
                    graph: props.graph.clone(),
                    workflow_slug: props.workflow_slug.clone(),
                    source_directory: props.source_directory.clone(),
                    labels,
                    provenance: props.provenance.clone(),
                    manifest_blob: props.manifest_blob,
                    definition_blob: None,
                    git: props.git.clone(),
                    fork_source_ref: props.fork_source_ref.clone(),
                    in_place: props.in_place,
                });
                self.graph_source.clone_from(&props.workflow_source);
            }
            EventBody::RunStarted(props) => {
                self.start = Some(StartRecord {
                    run_id,
                    start_time: ts,
                    run_branch: props.run_branch.clone(),
                    base_sha: props.base_sha.clone(),
                });
            }
            EventBody::RunSubmitted(props) => {
                if let Some(spec) = self.spec.as_mut() {
                    spec.definition_blob = props.definition_blob;
                }
                self.try_apply_status(RunStatus::Submitted, ts)?;
            }
            EventBody::RunQueued(_) => {
                self.try_apply_status(RunStatus::Queued, ts)?;
            }
            EventBody::RunStarting(_) => {
                self.try_apply_status(RunStatus::Starting, ts)?;
            }
            EventBody::RunRunning(_) => {
                self.try_apply_status(RunStatus::Running, ts)?;
            }
            EventBody::RunBlocked(props) => {
                let next = if matches!(self.status, Some(RunStatus::Paused { .. })) {
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
                    Some(RunStatus::Paused {
                        prior_block: Some(_),
                    }) => RunStatus::Paused { prior_block: None },
                    Some(RunStatus::Paused { prior_block: None }) => {
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
                        prior_block: self.status().and_then(RunStatus::blocked_reason),
                    },
                    ts,
                )?;
                self.pending_control = None;
            }
            EventBody::RunUnpaused(_) => {
                let next = match self.status {
                    Some(RunStatus::Paused {
                        prior_block: Some(blocked_reason),
                    }) => RunStatus::Blocked { blocked_reason },
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
                self.final_patch.clone_from(&props.final_patch);
                self.pending_interviews.clear();
            }
            EventBody::RunFailed(props) => {
                self.try_apply_status(
                    RunStatus::Failed {
                        reason: props.reason,
                    },
                    ts,
                )?;
                self.pending_control = None;
                self.conclusion = Some(conclusion_from_failed(props, ts));
                self.final_patch.clone_from(&props.final_patch);
                self.pending_interviews.clear();
            }
            EventBody::RunSupersededBy(props) => {
                self.superseded_by = Some(props.new_run_id);
            }
            EventBody::RunArchived(_props) => {
                if let Some(current) = self.status {
                    if matches!(current, RunStatus::Archived { .. }) {
                        return Ok(());
                    }
                    let Some(prior) = current.terminal_status() else {
                        return Err(fabro_types::InvalidTransition {
                            from: current,
                            to:   RunStatus::Archived {
                                prior: TerminalStatus::Dead,
                            },
                        }
                        .into());
                    };
                    self.try_apply_status(RunStatus::Archived { prior }, ts)?;
                }
            }
            EventBody::RunUnarchived(_props) => {
                if let Some(RunStatus::Archived { prior }) = self.status {
                    self.try_apply_status(prior.into(), ts)?;
                }
            }
            EventBody::CheckpointCompleted(props) => {
                let checkpoint = checkpoint_from_props(props, ts);
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
                    self.stage_entry(node_id, visit, first_event_seq(event.seq))
                        .completion = Some(stage_completion_from_outcome(outcome, ts));
                }
                self.checkpoint = Some(checkpoint.clone());
                self.checkpoints.push((event.seq, checkpoint));
            }
            EventBody::SandboxInitialized(props) => {
                self.sandbox = Some(SandboxRecord {
                    provider:          props.provider.clone(),
                    working_directory: props.working_directory.clone(),
                    identifier:        props.identifier.clone(),
                    repo_cloned:       props.repo_cloned,
                    clone_origin_url:  props.clone_origin_url.clone(),
                    clone_branch:      props.clone_branch.clone(),
                });
            }
            EventBody::RetroStarted(props) => {
                self.retro_prompt.clone_from(&props.prompt);
            }
            EventBody::RetroCompleted(props) => {
                self.retro_response.clone_from(&props.response);
                self.retro = props
                    .retro
                    .clone()
                    .map(serde_json::from_value)
                    .transpose()
                    .map_err(|err| Error::InvalidEvent(format!("invalid retro payload: {err}")))?;
            }
            EventBody::PullRequestCreated(props) => {
                self.pull_request = Some(PullRequestRecord {
                    html_url:    props.pr_url.clone(),
                    number:      props.pr_number,
                    owner:       props.owner.clone(),
                    repo:        props.repo.clone(),
                    base_branch: props.base_branch.clone(),
                    head_branch: props.head_branch.clone(),
                    title:       props.title.clone(),
                });
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
                        },
                        started_at: Some(ts),
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
            EventBody::StageStarted(_) => {
                let Some(stage_id) = stored.stage_id.as_ref() else {
                    return Ok(());
                };
                let stage = self.stage_entry(
                    stage_id.node_id(),
                    stage_id.visit(),
                    first_event_seq(event.seq),
                );
                stage.begin_attempt(ts);
            }
            EventBody::StageRetrying(_) => {
                let Some(stage) = stage_at_stored_or_current_visit(self, stored, event.seq) else {
                    return Ok(());
                };
                stage.state = Some(StageState::Retrying);
            }
            EventBody::StagePrompt(props) => {
                let Some(stage) = stage_at_stored_or_visit(self, stored, props.visit, event.seq)
                else {
                    return Ok(());
                };
                stage.prompt = Some(props.text.clone());
                stage.provider_used = provider_used_from_prompt(props);
            }
            EventBody::PromptCompleted(props) => {
                let Some(stage) = stage_at_stored_or_current_visit(self, stored, event.seq) else {
                    return Ok(());
                };
                stage.response = Some(props.response.clone());
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
                stage.duration_ms = Some(props.duration_ms);
                stage.usage.clone_from(&props.billing);
                stage.state = Some(StageState::from(outcome.status));
            }
            EventBody::StageFailed(props) => {
                let failure_reason = props.failure.as_ref().map(|detail| detail.message.clone());
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
                stage.duration_ms = Some(props.duration_ms);
                stage.usage.clone_from(&props.billing);
                stage.state = Some(StageState::from(outcome));
            }
            EventBody::AgentSessionStarted(props) => {
                let Some(stage) = stage_at_stored_or_visit(self, stored, props.visit, event.seq)
                else {
                    return Ok(());
                };
                stage.provider_used = Some(provider_used_from_agent_session_started(props));
            }
            EventBody::AgentCliStarted(props) => {
                let Some(stage) = stage_at_stored_or_visit(self, stored, props.visit, event.seq)
                else {
                    return Ok(());
                };
                stage.provider_used = Some(provider_used_from_agent_cli_started(props));
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
                stage.stdout = Some(props.stdout.clone());
                stage.stderr = Some(props.stderr.clone());
                stage.stdout_bytes = Some(props.stdout_bytes);
                stage.stderr_bytes = Some(props.stderr_bytes);
                stage.streams_separated = Some(props.streams_separated);
                stage.live_streaming = Some(props.live_streaming);
                stage.termination = Some(props.termination);
                stage.script_timing = Some(script_timing);
            }
            EventBody::ParallelCompleted(props) => {
                let parallel_results = serde_json::to_value(&props.results).map_err(|err| {
                    Error::InvalidEvent(format!("invalid parallel.completed payload: {err}"))
                })?;
                let Some(stage) = stage_at_stored_or_current_visit(self, stored, event.seq) else {
                    return Ok(());
                };
                stage.parallel_results = Some(parallel_results);
            }
            _ => {}
        }

        Ok(())
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

pub(crate) fn build_summary(state: &RunProjection, run_id: &RunId) -> RunSummary {
    let workflow_name = state.spec.as_ref().map(|spec| {
        if spec.graph.name.is_empty() {
            "unnamed".to_string()
        } else {
            spec.graph.name.clone()
        }
    });
    let goal = state
        .spec
        .as_ref()
        .map(|spec| spec.graph.goal().to_string())
        .unwrap_or_default();
    RunSummary::new(
        *run_id,
        workflow_name,
        state
            .spec
            .as_ref()
            .and_then(|spec| spec.workflow_slug.clone()),
        goal,
        state
            .spec
            .as_ref()
            .map(|spec| spec.labels.clone())
            .unwrap_or_default(),
        state
            .spec
            .as_ref()
            .and_then(|spec| spec.source_directory.clone()),
        state.spec.as_ref().is_some_and(|spec| spec.in_place),
        state
            .spec
            .as_ref()
            .and_then(|spec| spec.git.as_ref())
            .map(|git| git.origin_url.clone()),
        state.start.as_ref().map(|start| start.start_time),
        state.status.unwrap_or(RunStatus::Submitted),
        state.pending_control,
        state
            .conclusion
            .as_ref()
            .map(|conclusion| conclusion.duration_ms),
        state
            .conclusion
            .as_ref()
            .and_then(|conclusion| conclusion.billing.as_ref())
            .and_then(|billing| billing.total_usd_micros),
        state.superseded_by,
    )
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

fn conclusion_from_completed(
    props: &RunCompletedProps,
    timestamp: DateTime<Utc>,
) -> Result<Conclusion> {
    Ok(Conclusion {
        timestamp,
        status: StageOutcome::from_str(&props.status)
            .map_err(|err| Error::InvalidEvent(format!("invalid completed stage status: {err}")))?,
        duration_ms: props.duration_ms,
        failure_reason: None,
        final_git_commit_sha: props.final_git_commit_sha.clone(),
        stages: Vec::new(),
        billing: props.billing.clone(),
        total_retries: 0,
    })
}

fn conclusion_from_failed(props: &RunFailedProps, timestamp: DateTime<Utc>) -> Conclusion {
    Conclusion {
        timestamp,
        status: StageOutcome::Failed {
            retry_requested: false,
        },
        duration_ms: props.duration_ms,
        failure_reason: Some(render_with_causes(&props.error, &props.causes)),
        final_git_commit_sha: props.git_commit_sha.clone(),
        stages: Vec::new(),
        billing: None,
        total_retries: 0,
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
        duration_ms:        Some(props.duration_ms),
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
            .map(|failure| failure.message.clone()),
        timestamp,
    }
}

fn provider_used_from_prompt(props: &StagePromptProps) -> Option<Value> {
    let mut provider_used = serde_json::Map::new();
    if let Some(mode) = props.mode.clone() {
        provider_used.insert("mode".to_string(), Value::String(mode));
    }
    if let Some(provider) = props.provider.clone() {
        provider_used.insert("provider".to_string(), Value::String(provider));
    }
    if let Some(model) = props.model.clone() {
        provider_used.insert("model".to_string(), Value::String(model));
    }
    (!provider_used.is_empty()).then_some(Value::Object(provider_used))
}

fn provider_used_from_agent_session_started(props: &AgentSessionStartedProps) -> Value {
    let mut provider_used = serde_json::Map::new();
    provider_used.insert("mode".to_string(), Value::String("agent".to_string()));
    if let Some(provider) = props.provider.clone() {
        provider_used.insert("provider".to_string(), Value::String(provider));
    }
    if let Some(model) = props.model.clone() {
        provider_used.insert("model".to_string(), Value::String(model));
    }
    Value::Object(provider_used)
}

fn provider_used_from_agent_cli_started(props: &AgentCliStartedProps) -> Value {
    let mut provider_used = serde_json::Map::new();
    provider_used.insert("mode".to_string(), Value::String("cli".to_string()));
    provider_used.insert(
        "provider".to_string(),
        Value::String(props.provider.clone()),
    );
    provider_used.insert("model".to_string(), Value::String(props.model.clone()));
    provider_used.insert("command".to_string(), Value::String(props.command.clone()));
    Value::Object(provider_used)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use chrono::Utc;
    use fabro_types::run_event::run::RunFailedProps;
    use fabro_types::run_event::{
        CheckpointCompletedProps, InterviewCompletedProps, InterviewOption, InterviewStartedProps,
        RunControlEffectProps, StageCompletedProps, StageFailedProps, StagePromptProps,
        StageRetryingProps, StageStartedProps,
    };
    use fabro_types::{
        BilledModelUsage, BlockedReason, Checkpoint, EventBody, FailureCategory, FailureDetail,
        FailureReason, Outcome, QuestionType, RunBlobId, RunControlAction, RunEvent, RunStatus,
        StageOutcome, StageState, SuccessReason, TerminalStatus, WorkflowSettings, first_event_seq,
        fixtures,
    };
    use serde_json::json;

    use super::{RunProjection, RunProjectionReducer, build_summary};
    use crate::{Error, EventEnvelope, StageId};

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
                "facts": {
                    "provider": "open_ai"
                }
            },
            "total_usd_micros": input_tokens + output_tokens
        }))
        .unwrap()
    }

    fn usage_json(usage: &BilledModelUsage) -> serde_json::Value {
        serde_json::to_value(usage).unwrap()
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
    fn deserialize_projection_defaults_missing_stages_and_checkpoints() {
        let state: RunProjection = serde_json::from_value(serde_json::json!({
            "pending_control": "pause"
        }))
        .unwrap();

        assert_eq!(state.pending_control, Some(RunControlAction::Pause));
        assert!(state.checkpoints.is_empty());
        assert!(state.is_empty());
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
                "provenance": null,
                "manifest_blob": null,
                "definition_blob": null
            },
            "pending_control": "cancel",
            "checkpoints": [[
                0,
                {
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
            ]],
            "stages": {
                "build@2": {
                    "first_event_seq": 1,
                    "diff": "diff --git a/file b/file",
                    "stdout": "done"
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
        assert_eq!(round_tripped_node.stdout.as_deref(), Some("done"));
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
        let mut state = RunProjection::default();
        state.pending_control = Some(RunControlAction::Unpause);
        state.checkpoints = vec![(7, Checkpoint {
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
        })];
        state.stage_entry("build", 2, first_event_seq(7)).stdout = Some("done".to_string());

        let round_tripped: RunProjection =
            serde_json::from_value(serde_json::to_value(&state).unwrap()).unwrap();

        assert_eq!(
            round_tripped
                .stage(&StageId::new("build", 2))
                .unwrap()
                .stdout
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
        let mut state = RunProjection::default();
        let stage_id = StageId::new("build", 1);

        state
            .apply_event(&test_stage_event(
                3,
                EventBody::StageStarted(StageStartedProps {
                    index:        0,
                    handler_type: "agent".to_string(),
                    attempt:      1,
                    max_attempts: 1,
                }),
                stage_id.clone(),
            ))
            .unwrap();

        let stage = state.stage(&stage_id).unwrap();
        assert_eq!(stage.first_event_seq, first_event_seq(3));
    }

    #[test]
    fn later_stage_events_do_not_overwrite_first_event_seq() {
        let mut state = RunProjection::default();
        let stage_id = StageId::new("build", 1);

        state
            .apply_event(&test_stage_event(
                3,
                EventBody::StageStarted(StageStartedProps {
                    index:        0,
                    handler_type: "agent".to_string(),
                    attempt:      1,
                    max_attempts: 1,
                }),
                stage_id.clone(),
            ))
            .unwrap();
        state
            .apply_event(&test_event(
                4,
                EventBody::StagePrompt(StagePromptProps {
                    visit:    1,
                    text:     "prompt".to_string(),
                    mode:     None,
                    provider: None,
                    model:    None,
                }),
                Some("build"),
            ))
            .unwrap();

        let stage = state.stage(&stage_id).unwrap();
        assert_eq!(stage.first_event_seq, first_event_seq(3));
        assert_eq!(stage.prompt.as_deref(), Some("prompt"));
    }

    #[test]
    fn stage_completed_event_captures_duration_and_usage_per_visit() {
        let mut state = RunProjection::default();
        let usage = test_usage("gpt-5.2", 123, 45);

        state
            .apply_event(&test_event(
                3,
                EventBody::StageCompleted(StageCompletedProps {
                    index: 0,
                    duration_ms: 789,
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
        assert_eq!(stage.duration_ms, Some(789));
        assert_eq!(stage.usage.as_ref(), Some(&usage));
    }

    #[test]
    fn stage_failed_event_captures_duration_and_usage_per_visit() {
        let mut state = RunProjection::default();
        let stage_id = StageId::new("build", 1);
        let usage = test_usage("gpt-5.2", 321, 54);

        state
            .apply_event(&test_stage_event(
                2,
                EventBody::StageStarted(StageStartedProps {
                    index:        0,
                    handler_type: "agent".to_string(),
                    attempt:      1,
                    max_attempts: 1,
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
                        "failure_class": "transient_infra"
                    },
                    "will_retry": false,
                    "duration_ms": 654,
                    "billing": usage_json(&usage)
                }),
                Some("build"),
            ))
            .unwrap();

        let stage = state.stage(&stage_id).unwrap();
        assert_eq!(stage.duration_ms, Some(654));
        assert_eq!(stage.usage.as_ref(), Some(&usage));
    }

    #[test]
    fn two_visits_of_one_node_retain_distinct_usage() {
        let mut state = RunProjection::default();
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
                        duration_ms,
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
        assert_eq!(first_stage.duration_ms, Some(111));
        assert_eq!(first_stage.usage.as_ref(), Some(&first_usage));
        assert_eq!(second_stage.duration_ms, Some(222));
        assert_eq!(second_stage.usage.as_ref(), Some(&second_usage));
    }

    #[test]
    fn stage_completed_prefers_stored_stage_id_over_legacy_node_visits() {
        let mut state = RunProjection::default();
        let usage = test_usage("gpt-5.2", 300, 30);
        let scoped_stage_id = StageId::new("build", 2);

        state
            .apply_event(&test_stage_event(
                3,
                EventBody::StageCompleted(StageCompletedProps {
                    index: 0,
                    duration_ms: 333,
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
        assert_eq!(stage.duration_ms, Some(333));
        assert_eq!(stage.usage.as_ref(), Some(&usage));
        assert_eq!(stage.response.as_deref(), Some("done"));
    }

    #[test]
    fn stage_failed_prefers_stored_stage_id_and_preserves_retry_request() {
        let mut state = RunProjection::default();
        let usage = test_usage("gpt-5.2", 400, 40);
        let scoped_stage_id = StageId::new("build", 2);

        state
            .apply_event(&test_stage_event(
                3,
                EventBody::StageFailed(StageFailedProps {
                    index:       0,
                    failure:     Some(fabro_types::FailureDetail::new(
                        "try again",
                        fabro_types::FailureCategory::TransientInfra,
                    )),
                    will_retry:  true,
                    duration_ms: 444,
                    billing:     Some(usage.clone()),
                }),
                scoped_stage_id.clone(),
            ))
            .unwrap();

        assert!(
            state.stage(&StageId::new("build", 1)).is_none(),
            "current-visit fallback must not override stored stage_id"
        );
        let stage = state.stage(&scoped_stage_id).unwrap();
        assert_eq!(stage.duration_ms, Some(444));
        assert_eq!(stage.usage.as_ref(), Some(&usage));
        let completion = stage.completion.as_ref().unwrap();
        assert_eq!(completion.outcome, StageOutcome::Failed {
            retry_requested: true,
        });
        assert_eq!(completion.failure_reason.as_deref(), Some("try again"));
    }

    #[test]
    fn checkpoint_completed_creates_projection_entry_for_skipped_stage() {
        let mut state = RunProjection::default();
        let stage_id = StageId::new("skip_me", 1);

        state
            .apply_event(&test_event(
                5,
                EventBody::CheckpointCompleted(CheckpointCompletedProps {
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
        let mut state = RunProjection::default();
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
                            key:   "approve".to_string(),
                            label: "Approve".to_string(),
                        },
                        InterviewOption {
                            key:   "revise".to_string(),
                            label: "Revise".to_string(),
                        },
                    ],
                    allow_freeform:  true,
                    timeout_seconds: Some(30.0),
                    context_display: Some("Latest draft".to_string()),
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
        assert!(pending.question.allow_freeform);
        assert_eq!(pending.question.timeout_seconds, Some(30.0));
        assert_eq!(
            pending.question.context_display.as_deref(),
            Some("Latest draft")
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
    fn queued_and_blocked_events_drive_projection_and_summary_fields() {
        let mut state = RunProjection::default();

        state
            .apply_event(&test_raw_event(1, "run.queued", &json!({}), None))
            .unwrap();
        assert_eq!(state.status(), Some(RunStatus::Queued));

        state
            .apply_event(&test_raw_event(2, "run.starting", &json!({}), None))
            .unwrap();
        state
            .apply_event(&test_raw_event(3, "run.running", &json!({}), None))
            .unwrap();
        state
            .apply_event(&test_event(
                4,
                EventBody::RunPaused(RunControlEffectProps::default()),
                None,
            ))
            .unwrap();
        state
            .apply_event(&test_raw_event(
                5,
                "run.blocked",
                &json!({ "blocked_reason": "human_input_required" }),
                None,
            ))
            .unwrap();

        let status_json = serde_json::to_value(state.status().unwrap()).unwrap();
        assert_eq!(
            state.status(),
            Some(RunStatus::Paused {
                prior_block: Some(BlockedReason::HumanInputRequired),
            })
        );
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
            summary_json["status"],
            json!({
                "kind": "paused",
                "prior_block": "human_input_required"
            })
        );
    }

    #[test]
    fn run_unblocked_clears_blocked_reason_and_restores_running() {
        let mut state = RunProjection::default();

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

        assert_eq!(state.status(), Some(RunStatus::Running));
        let status_json = serde_json::to_value(state.status().unwrap()).unwrap();
        assert_eq!(status_json, json!({ "kind": "running" }));
    }

    #[test]
    fn run_unblocked_while_paused_clears_blocked_reason_without_changing_paused_status() {
        let mut state = RunProjection::default();

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

        assert_eq!(
            state.status(),
            Some(RunStatus::Paused { prior_block: None })
        );
        let status_json = serde_json::to_value(state.status().unwrap()).unwrap();
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
        let mut state = RunProjection::default();

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

        assert_eq!(
            state.status(),
            Some(RunStatus::Blocked {
                blocked_reason: BlockedReason::HumanInputRequired,
            })
        );
        let status_json = serde_json::to_value(state.status().unwrap()).unwrap();
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
        let mut state = RunProjection::default();
        state.spec = Some(fabro_types::RunSpec {
            run_id:           fixtures::RUN_1,
            settings:         WorkflowSettings::default(),
            graph:            fabro_types::Graph::new("test"),
            workflow_slug:    Some("test".to_string()),
            source_directory: Some("/tmp/repo".to_string()),
            git:              None,
            labels:           HashMap::new(),
            provenance:       None,
            manifest_blob:    None,
            definition_blob:  None,
            fork_source_ref:  None,
            in_place:         false,
        });

        let summary_json = serde_json::to_value(build_summary(&state, &fixtures::RUN_1)).unwrap();
        assert_eq!(summary_json["status"], json!({ "kind": "submitted" }));
    }

    #[test]
    fn projection_serialization_includes_manifest_and_definition_blob_refs() {
        let manifest_blob = RunBlobId::new(br#"{"version":1}"#).to_string();
        let definition_blob =
            RunBlobId::new(br#"{"version":1,"workflow_path":"workflow.fabro"}"#).to_string();
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
                        "run_dir": "/tmp/run",
                        "source_directory": "/tmp/run",
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
        let mut state = RunProjection::default();
        let patch = "diff --git a/foo.rs b/foo.rs\n@@ -1 +1 @@\n-a\n+b\n";
        state
            .apply_event(&test_event(
                1,
                EventBody::RunFailed(RunFailedProps {
                    error:          "boom".to_string(),
                    causes:         Vec::new(),
                    duration_ms:    42,
                    reason:         FailureReason::WorkflowError,
                    git_commit_sha: Some("abc123".to_string()),
                    final_patch:    Some(patch.to_string()),
                }),
                None,
            ))
            .unwrap();

        assert_eq!(state.final_patch.as_deref(), Some(patch));
    }

    #[test]
    fn run_failed_projection_renders_causes() {
        let mut state = RunProjection::default();
        state
            .apply_event(&test_event(
                1,
                EventBody::RunFailed(RunFailedProps {
                    error:          "Engine error: Failed to initialize sandbox".to_string(),
                    causes:         vec![
                        "Failed to pull Docker image buildpack-deps:noble".to_string(),
                        "connection refused".to_string(),
                    ],
                    duration_ms:    42,
                    reason:         FailureReason::WorkflowError,
                    git_commit_sha: None,
                    final_patch:    None,
                }),
                None,
            ))
            .unwrap();

        assert_eq!(
            state.conclusion.unwrap().failure_reason.as_deref(),
            Some(
                "Engine error: Failed to initialize sandbox\n  caused by: Failed to pull Docker image buildpack-deps:noble\n  caused by: connection refused"
            )
        );
    }

    #[test]
    fn run_archived_captures_prior_status_and_preserves_reason() {
        use fabro_types::run_event::{RunArchivedProps, RunCompletedProps};

        let mut state = RunProjection::default();
        state
            .apply_event(&test_event(
                1,
                EventBody::RunCompleted(RunCompletedProps {
                    duration_ms:          10,
                    artifact_count:       0,
                    status:               "succeeded".to_string(),
                    reason:               SuccessReason::Completed,
                    total_usd_micros:     None,
                    final_git_commit_sha: None,
                    final_patch:          None,
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

        assert_eq!(
            state.status(),
            Some(RunStatus::Archived {
                prior: TerminalStatus::Succeeded {
                    reason: SuccessReason::Completed,
                },
            })
        );
    }

    #[test]
    fn run_superseded_by_populates_projection_and_summary() {
        use fabro_types::run_event::RunSupersededByProps;

        let mut state = RunProjection::default();
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
    fn run_unarchived_restores_prior_status() {
        use fabro_types::run_event::{RunArchivedProps, RunCompletedProps, RunUnarchivedProps};

        let mut state = RunProjection::default();
        state
            .apply_event(&test_event(
                1,
                EventBody::RunCompleted(RunCompletedProps {
                    duration_ms:          10,
                    artifact_count:       0,
                    status:               "succeeded".to_string(),
                    reason:               SuccessReason::PartialSuccess,
                    total_usd_micros:     None,
                    final_git_commit_sha: None,
                    final_patch:          None,
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

        assert_eq!(
            state.status(),
            Some(RunStatus::Succeeded {
                reason: SuccessReason::PartialSuccess,
            })
        );
    }

    #[test]
    fn duplicate_event_noops_without_bumping_status_updated_at() {
        let mut state = RunProjection::default();
        state
            .apply_event(&test_raw_event_at(
                1,
                "2026-04-07T12:00:00Z",
                "run.running",
                &json!({}),
                None,
            ))
            .unwrap();
        let first_updated_at = state.status_updated_at;

        state
            .apply_event(&test_raw_event_at(
                2,
                "2026-04-07T12:01:00Z",
                "run.running",
                &json!({}),
                None,
            ))
            .unwrap();

        assert_eq!(state.status(), Some(RunStatus::Running));
        assert_eq!(state.status_updated_at, first_updated_at);
    }

    #[test]
    fn paused_over_blocked_round_trips_back_to_blocked() {
        let mut state = RunProjection::default();
        state
            .apply_event(&test_raw_event(1, "run.running", &json!({}), None))
            .unwrap();
        state
            .apply_event(&test_raw_event(
                2,
                "run.blocked",
                &json!({ "blocked_reason": "human_input_required" }),
                None,
            ))
            .unwrap();
        state
            .apply_event(&test_event(
                3,
                EventBody::RunPaused(RunControlEffectProps::default()),
                None,
            ))
            .unwrap();
        state
            .apply_event(&test_event(
                4,
                EventBody::RunUnpaused(RunControlEffectProps::default()),
                None,
            ))
            .unwrap();

        assert_eq!(
            state.status(),
            Some(RunStatus::Blocked {
                blocked_reason: BlockedReason::HumanInputRequired,
            })
        );
    }

    #[test]
    fn run_archived_on_non_terminal_projection_is_rejected() {
        use fabro_types::run_event::RunArchivedProps;

        let mut state = RunProjection::default();
        state
            .apply_event(&test_raw_event(1, "run.running", &json!({}), None))
            .unwrap();

        let err = state
            .apply_event(&test_event(
                2,
                EventBody::RunArchived(RunArchivedProps::default()),
                None,
            ))
            .unwrap_err();

        assert!(matches!(err, Error::InvalidTransition(_)));
        assert_eq!(state.status(), Some(RunStatus::Running));
    }

    #[test]
    fn run_unarchived_replayed_on_non_archived_projection_is_ignored() {
        use fabro_types::run_event::{RunCompletedProps, RunUnarchivedProps};

        let mut state = RunProjection::default();
        state
            .apply_event(&test_event(
                1,
                EventBody::RunCompleted(RunCompletedProps {
                    duration_ms:          10,
                    artifact_count:       0,
                    status:               "succeeded".to_string(),
                    reason:               SuccessReason::Completed,
                    total_usd_micros:     None,
                    final_git_commit_sha: None,
                    final_patch:          None,
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

        assert_eq!(
            state.status(),
            Some(RunStatus::Succeeded {
                reason: SuccessReason::Completed,
            })
        );
        assert_eq!(state.status_updated_at, updated_at);
    }

    fn started_props() -> StageStartedProps {
        StageStartedProps {
            index:        0,
            handler_type: "agent".to_string(),
            attempt:      1,
            max_attempts: 3,
        }
    }

    fn failed_props(duration_ms: u64, will_retry: bool) -> StageFailedProps {
        StageFailedProps {
            index: 0,
            failure: Some(FailureDetail::new("boom", FailureCategory::TransientInfra)),
            will_retry,
            duration_ms,
            billing: None,
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
            duration_ms,
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
                "facts": { "provider": "open_ai" }
            },
            "total_usd_micros": 123
        }))
        .expect("billing fixture should deserialize")
    }

    #[test]
    fn stage_started_records_started_at_and_running_state() {
        let mut state = RunProjection::default();
        let stage_id = StageId::new("build", 1);

        state
            .apply_event(&test_stage_event(
                3,
                EventBody::StageStarted(started_props()),
                stage_id.clone(),
            ))
            .unwrap();

        let stage = state.stage(&stage_id).unwrap();
        assert_eq!(stage.state, Some(StageState::Running));
        assert!(stage.started_at.is_some());
        assert_eq!(stage.effective_state(), StageState::Running);
    }

    #[test]
    fn stage_completed_records_duration_usage_and_terminal_state() {
        let mut state = RunProjection::default();
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
        assert_eq!(stage.duration_ms, Some(42));
        assert_eq!(stage.usage.as_ref(), Some(&usage));
        assert_eq!(stage.state, Some(StageState::Succeeded));
        assert_eq!(stage.effective_state(), StageState::Succeeded);
    }

    #[test]
    fn stage_failed_records_duration_and_failed_state() {
        let mut state = RunProjection::default();
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
        assert_eq!(stage.duration_ms, Some(10));
        assert_eq!(stage.state, Some(StageState::Failed));
    }

    #[test]
    fn stage_retrying_sets_retrying_state() {
        let mut state = RunProjection::default();
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
        assert_eq!(stage.state, Some(StageState::Retrying));
    }

    #[test]
    fn stage_started_after_retrying_returns_to_running_and_resets_attempt_data() {
        let mut state = RunProjection::default();
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
        assert_eq!(stage.state, Some(StageState::Running));
        // Prior attempt's terminal data must not leak into the new attempt.
        assert!(stage.completion.is_none());
        assert_eq!(stage.duration_ms, None);
    }
}
