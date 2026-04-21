use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use fabro_types::run_event::{
    AgentCliStartedProps, AgentSessionStartedProps, CheckpointCompletedProps, RunCompletedProps,
    RunFailedProps, StageCompletedProps, StagePromptProps,
};
use fabro_types::{
    BilledModelUsage, BlockedReason, Checkpoint, Conclusion, EventBody, FailureSignature,
    InterviewQuestionRecord, InterviewQuestionType, NodeStatusRecord, Outcome,
    PendingInterviewRecord, PullRequestRecord, RunControlAction, RunId, RunProjection, RunSpec,
    RunStatus, RunStatusRecord, RunSummary, SandboxRecord, StageStatus, StartRecord, StatusReason,
};
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
                let working_directory = PathBuf::from(&props.working_directory);
                let labels = props.labels.clone().into_iter().collect::<HashMap<_, _>>();
                self.spec = Some(RunSpec {
                    run_id,
                    settings: props.settings.clone(),
                    graph: props.graph.clone(),
                    workflow_slug: props.workflow_slug.clone(),
                    working_directory,
                    host_repo_path: props.host_repo_path.clone(),
                    repo_origin_url: props.repo_origin_url.clone(),
                    base_branch: props.base_branch.clone(),
                    labels,
                    provenance: props.provenance.clone(),
                    manifest_blob: props.manifest_blob,
                    definition_blob: None,
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
                self.status = Some(run_status_record(RunStatus::Submitted, props.reason, ts));
            }
            EventBody::RunQueued(_) => {
                self.status = Some(run_status_record(RunStatus::Queued, None, ts));
            }
            EventBody::RunStarting(props) => {
                self.status = Some(run_status_record(RunStatus::Starting, props.reason, ts));
            }
            EventBody::RunRunning(props) => {
                self.status = Some(run_status_record(RunStatus::Running, props.reason, ts));
            }
            EventBody::RunBlocked(props) => {
                let visible_status = if self
                    .status
                    .as_ref()
                    .is_some_and(|status| status.status == RunStatus::Paused)
                {
                    RunStatus::Paused
                } else {
                    RunStatus::Blocked
                };
                self.status = Some(run_status_record_with_blocked_reason(
                    visible_status,
                    None,
                    Some(props.blocked_reason),
                    ts,
                ));
            }
            EventBody::RunUnblocked(_) => {
                self.status = Some(match self.status.as_ref() {
                    Some(status) if status.status == RunStatus::Paused => {
                        run_status_record_with_blocked_reason(
                            RunStatus::Paused,
                            status.status_reason,
                            None,
                            ts,
                        )
                    }
                    Some(status) => run_status_record_with_blocked_reason(
                        if status.status == RunStatus::Blocked {
                            RunStatus::Running
                        } else {
                            status.status
                        },
                        status.status_reason,
                        None,
                        ts,
                    ),
                    None => run_status_record(RunStatus::Running, None, ts),
                });
            }
            EventBody::RunRemoving(props) => {
                self.status = Some(run_status_record(RunStatus::Removing, props.reason, ts));
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
                self.status = Some(run_status_record_with_blocked_reason(
                    RunStatus::Paused,
                    None,
                    self.status
                        .as_ref()
                        .and_then(|status| status.blocked_reason),
                    ts,
                ));
                self.pending_control = None;
            }
            EventBody::RunUnpaused(_) => {
                self.status = Some(run_status_record_with_blocked_reason(
                    RunStatus::Running,
                    None,
                    self.status
                        .as_ref()
                        .and_then(|status| status.blocked_reason),
                    ts,
                ));
                self.pending_control = None;
            }
            EventBody::RunCompleted(props) => {
                self.status = Some(run_status_record(RunStatus::Succeeded, props.reason, ts));
                self.pending_control = None;
                self.conclusion = Some(conclusion_from_completed(props, ts)?);
                self.final_patch.clone_from(&props.final_patch);
                self.pending_interviews.clear();
            }
            EventBody::RunFailed(props) => {
                self.status = Some(run_status_record(RunStatus::Failed, props.reason, ts));
                self.pending_control = None;
                self.conclusion = Some(conclusion_from_failed(props, ts));
                self.final_patch.clone_from(&props.final_patch);
                self.pending_interviews.clear();
            }
            EventBody::RunRewound(_) => {
                self.reset_for_rewind();
            }
            EventBody::RunArchived(_props) => {
                // Idempotent on replay: a second RunArchived (from a concurrent
                // archive race or a tampered log) must not overwrite
                // prior_status — doing so would permanently corrupt the run
                // (unarchive would then restore status=Archived and never
                // recover). We also ignore the archive attempt when current
                // status is not a terminal non-archived — the operations
                // layer is the only legitimate emitter and it validates
                // there, but replay must be defensive.
                let current = self.status.as_ref().map(|record| record.status);
                if matches!(
                    current,
                    Some(RunStatus::Succeeded | RunStatus::Failed | RunStatus::Dead)
                ) {
                    self.prior_status = current;
                    self.status = Some(run_status_record(RunStatus::Archived, None, ts));
                }
            }
            EventBody::RunUnarchived(props)
                if matches!(
                    self.status.as_ref().map(|record| record.status),
                    Some(RunStatus::Archived)
                ) && matches!(
                    props.restored_status,
                    RunStatus::Succeeded | RunStatus::Failed | RunStatus::Dead
                ) =>
            {
                // Defensive: restored_status must be a real terminal status.
                // Accepting e.g. Running here would produce a projection
                // whose status is Running without any RunRunning event in
                // the log, breaking the is_active / is_terminal invariants.
                // The operations layer only ever writes validated terminals,
                // but replay must enforce it.
                self.status = Some(run_status_record(props.restored_status, None, ts));
                self.prior_status = None;
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
                        self.node_mut(node_id, visit).diff = Some(diff);
                    }
                }
                self.checkpoint = Some(checkpoint.clone());
                self.checkpoints.push((event.seq, checkpoint));
            }
            EventBody::SandboxInitialized(props) => {
                self.sandbox = Some(SandboxRecord {
                    provider:               props.provider.clone(),
                    working_directory:      props.working_directory.clone(),
                    identifier:             props.identifier.clone(),
                    host_working_directory: props.host_working_directory.clone(),
                    container_mount_point:  props.container_mount_point.clone(),
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
                            question_type:   InterviewQuestionType::from_wire_name(
                                &props.question_type,
                            ),
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
            EventBody::StagePrompt(props) => {
                let Some(node_id) = stored.node_id.as_deref() else {
                    return Ok(());
                };
                let visit = props.visit;
                self.node_mut(node_id, visit).prompt = Some(props.text.clone());
                self.node_mut(node_id, visit).provider_used = provider_used_from_prompt(props);
            }
            EventBody::PromptCompleted(props) => {
                let Some(node_id) = stored.node_id.as_deref() else {
                    return Ok(());
                };
                let visit = self.current_visit_for(node_id).unwrap_or(1);
                self.node_mut(node_id, visit).response = Some(props.response.clone());
            }
            EventBody::StageCompleted(props) => {
                let Some(node_id) = stored.node_id.as_deref() else {
                    return Ok(());
                };
                let visit = stage_visit(node_id, props.node_visits.as_ref(), self).unwrap_or(1);
                let response = props.response.clone();
                let outcome = stage_outcome_from_props(props);
                let status = node_status_from_outcome(&outcome, ts);
                let node = self.node_mut(node_id, visit);
                node.response = response;
                node.status = Some(status);
            }
            EventBody::StageFailed(props) => {
                let Some(node_id) = stored.node_id.as_deref() else {
                    return Ok(());
                };
                let visit = self.current_visit_for(node_id).unwrap_or(1);
                let failure_reason = props.failure.as_ref().map(|detail| detail.message.clone());
                let node = self.node_mut(node_id, visit);
                node.status = Some(NodeStatusRecord {
                    status: StageStatus::Fail,
                    notes: None,
                    failure_reason,
                    timestamp: ts,
                });
            }
            EventBody::AgentSessionStarted(props) => {
                let Some(node_id) = stored.node_id.as_deref() else {
                    return Ok(());
                };
                self.node_mut(node_id, props.visit).provider_used =
                    Some(provider_used_from_agent_session_started(props));
            }
            EventBody::AgentCliStarted(props) => {
                let Some(node_id) = stored.node_id.as_deref() else {
                    return Ok(());
                };
                self.node_mut(node_id, props.visit).provider_used =
                    Some(provider_used_from_agent_cli_started(props));
            }
            EventBody::CommandStarted(props) => {
                let Some(node_id) = stored.node_id.as_deref() else {
                    return Ok(());
                };
                let visit = self.current_visit_for(node_id).unwrap_or(1);
                self.node_mut(node_id, visit).script_invocation =
                    Some(serde_json::to_value(props).map_err(|err| {
                        Error::InvalidEvent(format!("invalid command.started payload: {err}"))
                    })?);
            }
            EventBody::CommandCompleted(props) => {
                let Some(node_id) = stored.node_id.as_deref() else {
                    return Ok(());
                };
                let visit = self.current_visit_for(node_id).unwrap_or(1);
                let node = self.node_mut(node_id, visit);
                node.stdout = Some(props.stdout.clone());
                node.stderr = Some(props.stderr.clone());
                node.script_timing = Some(serde_json::to_value(props).map_err(|err| {
                    Error::InvalidEvent(format!("invalid command.completed payload: {err}"))
                })?);
            }
            EventBody::ParallelCompleted(props) => {
                let Some(node_id) = stored.node_id.as_deref() else {
                    return Ok(());
                };
                let visit = self.current_visit_for(node_id).unwrap_or(1);
                self.node_mut(node_id, visit).parallel_results =
                    Some(serde_json::to_value(&props.results).map_err(|err| {
                        Error::InvalidEvent(format!("invalid parallel.completed payload: {err}"))
                    })?);
            }
            _ => {}
        }

        Ok(())
    }
}

pub(crate) fn build_summary(state: &RunProjection, run_id: &RunId) -> RunSummary {
    let workflow_name = state.spec.as_ref().map(|spec| {
        if spec.graph.name.is_empty() {
            "unnamed".to_string()
        } else {
            spec.graph.name.clone()
        }
    });
    let goal = state.spec.as_ref().and_then(|spec| {
        let goal = spec.graph.goal();
        (!goal.is_empty()).then(|| goal.to_string())
    });
    RunSummary {
        run_id: *run_id,
        workflow_name,
        workflow_slug: state
            .spec
            .as_ref()
            .and_then(|spec| spec.workflow_slug.clone()),
        goal,
        labels: state
            .spec
            .as_ref()
            .map(|spec| spec.labels.clone())
            .unwrap_or_default(),
        host_repo_path: state
            .spec
            .as_ref()
            .and_then(|spec| spec.host_repo_path.clone()),
        start_time: state.start.as_ref().map(|start| start.start_time),
        status: state
            .status
            .as_ref()
            .map_or(RunStatus::Submitted, |status| status.status),
        status_reason: state
            .status
            .as_ref()
            .and_then(|status| status.status_reason),
        blocked_reason: state
            .status
            .as_ref()
            .and_then(|status| status.blocked_reason),
        pending_control: state.pending_control,
        duration_ms: state
            .conclusion
            .as_ref()
            .map(|conclusion| conclusion.duration_ms),
        total_usd_micros: state
            .conclusion
            .as_ref()
            .and_then(|conclusion| conclusion.billing.as_ref())
            .and_then(|billing| billing.total_usd_micros),
    }
}

fn run_status_record(
    status: RunStatus,
    status_reason: Option<StatusReason>,
    updated_at: DateTime<Utc>,
) -> RunStatusRecord {
    run_status_record_with_blocked_reason(status, status_reason, None, updated_at)
}

fn run_status_record_with_blocked_reason(
    status: RunStatus,
    status_reason: Option<StatusReason>,
    blocked_reason: Option<BlockedReason>,
    updated_at: DateTime<Utc>,
) -> RunStatusRecord {
    RunStatusRecord {
        status,
        status_reason,
        blocked_reason,
        updated_at,
    }
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
        status: StageStatus::from_str(&props.status)
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
        status: StageStatus::Fail,
        duration_ms: props.duration_ms,
        failure_reason: Some(props.error.clone()),
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
        .or_else(|| state.current_visit_for(node_id))
}

fn stage_outcome_from_props(props: &StageCompletedProps) -> Outcome<Option<BilledModelUsage>> {
    Outcome {
        status:             props.status.clone(),
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

fn node_status_from_outcome(
    outcome: &Outcome<Option<BilledModelUsage>>,
    timestamp: DateTime<Utc>,
) -> NodeStatusRecord {
    NodeStatusRecord {
        status: outcome.status.clone(),
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
    use std::collections::HashMap;

    use chrono::Utc;
    use fabro_types::run_event::run::RunFailedProps;
    use fabro_types::run_event::{
        InterviewCompletedProps, InterviewOption, InterviewStartedProps, RunControlEffectProps,
    };
    use fabro_types::settings::SettingsLayer;
    use fabro_types::{
        Checkpoint, EventBody, InterviewQuestionType, NodeState, RunBlobId, RunControlAction,
        RunEvent, fixtures,
    };
    use serde_json::json;

    use super::{RunProjection, RunProjectionReducer, build_summary};
    use crate::{EventEnvelope, StageId};

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

    #[test]
    fn deserialize_projection_defaults_missing_nodes_and_checkpoints() {
        let state: RunProjection = serde_json::from_value(serde_json::json!({
            "pending_control": "pause"
        }))
        .unwrap();

        assert_eq!(state.pending_control, Some(RunControlAction::Pause));
        assert!(state.checkpoints.is_empty());
        assert!(state.is_empty());
    }

    #[test]
    fn deserialize_and_round_trip_projection_preserves_stage_ids_and_pending_control() {
        let state: RunProjection = serde_json::from_value(serde_json::json!({
            "spec": {
                "run_id": "01JW6A7VNFZSFF0SKXJG29Z2M3",
                "settings": { "_version": 1 },
                "graph": { "name": "ship", "nodes": {}, "edges": [], "attrs": {} },
                "workflow_slug": "demo",
                "working_directory": "/tmp/project",
                "host_repo_path": null,
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
            "nodes": {
                "build@2": {
                    "diff": "diff --git a/file b/file",
                    "stdout": "done"
                }
            }
        }))
        .unwrap();

        let stage_id = StageId::new("build", 2);
        let node = state.node(&stage_id).unwrap();
        assert_eq!(node.diff.as_deref(), Some("diff --git a/file b/file"));
        assert_eq!(state.list_node_visits("build"), vec![2]);
        assert_eq!(state.pending_control, Some(RunControlAction::Cancel));

        let round_tripped: RunProjection =
            serde_json::from_value(serde_json::to_value(&state).unwrap()).unwrap();
        let serialized = serde_json::to_value(&state).unwrap();
        let round_tripped_node = round_tripped.node(&stage_id).unwrap();
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
    fn set_node_round_trips_through_json() {
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
        state.set_node(StageId::new("build", 2), NodeState {
            stdout: Some("done".to_string()),
            ..NodeState::default()
        });

        let round_tripped: RunProjection =
            serde_json::from_value(serde_json::to_value(&state).unwrap()).unwrap();

        assert_eq!(
            round_tripped
                .node(&StageId::new("build", 2))
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
        assert_eq!(
            pending.question.question_type,
            InterviewQuestionType::MultipleChoice
        );
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
        assert_eq!(
            state
                .status
                .as_ref()
                .map(|status| status.status.to_string()),
            Some("queued".to_string())
        );

        state
            .apply_event(&test_event(
                2,
                EventBody::RunPaused(RunControlEffectProps::default()),
                None,
            ))
            .unwrap();
        state
            .apply_event(&test_raw_event(
                3,
                "run.blocked",
                &json!({ "blocked_reason": "human_input_required" }),
                None,
            ))
            .unwrap();

        let status_json = serde_json::to_value(state.status.as_ref().unwrap()).unwrap();
        assert_eq!(status_json["status"], "paused");
        assert_eq!(status_json["status_reason"], serde_json::Value::Null);
        assert_eq!(status_json["blocked_reason"], "human_input_required");

        let summary = build_summary(&state, &fixtures::RUN_1);
        let summary_json = serde_json::to_value(summary).unwrap();
        assert_eq!(summary_json["status"], "paused");
        assert_eq!(summary_json["status_reason"], serde_json::Value::Null);
        assert_eq!(summary_json["blocked_reason"], "human_input_required");
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

        let status_json = serde_json::to_value(state.status.as_ref().unwrap()).unwrap();
        assert_eq!(status_json["status"], "running");
        assert_eq!(status_json["blocked_reason"], serde_json::Value::Null);
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

        let status_json = serde_json::to_value(state.status.as_ref().unwrap()).unwrap();
        assert_eq!(status_json["status"], "paused");
        assert_eq!(status_json["blocked_reason"], serde_json::Value::Null);
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

        let status_json = serde_json::to_value(state.status.as_ref().unwrap()).unwrap();
        assert_eq!(status_json["status"], "blocked");
        assert_eq!(status_json["blocked_reason"], "human_input_required");
    }

    #[test]
    fn summary_synthesizes_submitted_when_run_exists_without_status() {
        let mut state = RunProjection::default();
        state.spec = Some(fabro_types::RunSpec {
            run_id:            fixtures::RUN_1,
            settings:          SettingsLayer::default(),
            graph:             fabro_types::Graph::new("test"),
            workflow_slug:     Some("test".to_string()),
            working_directory: std::path::PathBuf::from("/tmp/run"),
            host_repo_path:    Some("/tmp/repo".to_string()),
            repo_origin_url:   None,
            base_branch:       None,
            labels:            HashMap::new(),
            provenance:        None,
            manifest_blob:     None,
            definition_blob:   None,
        });

        let summary_json = serde_json::to_value(build_summary(&state, &fixtures::RUN_1)).unwrap();
        assert_eq!(summary_json["status"], "submitted");
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
                        "settings": SettingsLayer::default(),
                        "graph": {
                            "name": "test",
                            "nodes": {},
                            "edges": [],
                            "attrs": {}
                        },
                        "labels": {},
                        "run_dir": "/tmp/run",
                        "working_directory": "/tmp/run",
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
                    duration_ms:    42,
                    reason:         None,
                    git_commit_sha: Some("abc123".to_string()),
                    final_patch:    Some(patch.to_string()),
                }),
                None,
            ))
            .unwrap();

        assert_eq!(state.final_patch.as_deref(), Some(patch));
    }

    #[test]
    fn legacy_run_failed_event_without_final_patch_replays_as_none() {
        let mut state = RunProjection::default();
        // Existing SlateDB events predating the final_patch field should deserialize
        // cleanly as None — no backfill required.
        state
            .apply_event(&test_raw_event(
                1,
                "run.failed",
                &json!({
                    "error": "boom",
                    "duration_ms": 1,
                }),
                None,
            ))
            .unwrap();

        assert!(state.final_patch.is_none());
    }

    #[test]
    fn run_archived_captures_prior_status_and_sets_archived() {
        use fabro_types::RunStatus;
        use fabro_types::run_event::{RunArchivedProps, RunCompletedProps};

        let mut state = RunProjection::default();
        state
            .apply_event(&test_event(
                1,
                EventBody::RunCompleted(RunCompletedProps {
                    duration_ms:          10,
                    artifact_count:       0,
                    status:               "success".to_string(),
                    reason:               None,
                    total_usd_micros:     None,
                    final_git_commit_sha: None,
                    final_patch:          None,
                    billing:              None,
                }),
                None,
            ))
            .unwrap();
        assert_eq!(
            state.status.as_ref().map(|record| record.status),
            Some(RunStatus::Succeeded)
        );
        assert_eq!(state.prior_status, None);

        state
            .apply_event(&test_event(
                2,
                EventBody::RunArchived(RunArchivedProps { actor: None }),
                None,
            ))
            .unwrap();

        assert_eq!(
            state.status.as_ref().map(|record| record.status),
            Some(RunStatus::Archived)
        );
        assert_eq!(state.prior_status, Some(RunStatus::Succeeded));
    }

    #[test]
    fn run_unarchived_restores_status_and_clears_prior_status() {
        use fabro_types::RunStatus;
        use fabro_types::run_event::{RunArchivedProps, RunCompletedProps, RunUnarchivedProps};

        let mut state = RunProjection::default();
        state
            .apply_event(&test_event(
                1,
                EventBody::RunCompleted(RunCompletedProps {
                    duration_ms:          10,
                    artifact_count:       0,
                    status:               "success".to_string(),
                    reason:               None,
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
                EventBody::RunArchived(RunArchivedProps { actor: None }),
                None,
            ))
            .unwrap();
        state
            .apply_event(&test_event(
                3,
                EventBody::RunUnarchived(RunUnarchivedProps {
                    actor:           None,
                    restored_status: RunStatus::Succeeded,
                }),
                None,
            ))
            .unwrap();

        assert_eq!(
            state.status.as_ref().map(|record| record.status),
            Some(RunStatus::Succeeded)
        );
        assert_eq!(state.prior_status, None);
    }

    #[test]
    fn run_unarchived_uses_event_payload_even_when_prior_status_differs() {
        // The event payload is authoritative: the unarchive apply arm sets status
        // from `restored_status`, ignoring whatever `prior_status` was captured.
        use fabro_types::RunStatus;
        use fabro_types::run_event::{RunArchivedProps, RunCompletedProps, RunUnarchivedProps};

        let mut state = RunProjection::default();
        state
            .apply_event(&test_event(
                1,
                EventBody::RunCompleted(RunCompletedProps {
                    duration_ms:          10,
                    artifact_count:       0,
                    status:               "success".to_string(),
                    reason:               None,
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
                EventBody::RunArchived(RunArchivedProps { actor: None }),
                None,
            ))
            .unwrap();
        state
            .apply_event(&test_event(
                3,
                EventBody::RunUnarchived(RunUnarchivedProps {
                    actor:           None,
                    restored_status: RunStatus::Failed,
                }),
                None,
            ))
            .unwrap();

        assert_eq!(
            state.status.as_ref().map(|record| record.status),
            Some(RunStatus::Failed)
        );
        assert_eq!(state.prior_status, None);
    }

    #[test]
    fn double_archive_preserves_prior_status() {
        // Two RunArchived events in sequence — possible under a concurrent
        // archive race or a replayed/retried write — must not overwrite
        // prior_status with Archived. Otherwise unarchive would emit
        // restored_status=Archived, the apply arm would set status=Archived +
        // prior_status=None, and the run would be permanently unrecoverable.
        use fabro_types::RunStatus;
        use fabro_types::run_event::{RunArchivedProps, RunCompletedProps};

        let mut state = RunProjection::default();
        state
            .apply_event(&test_event(
                1,
                EventBody::RunCompleted(RunCompletedProps {
                    duration_ms:          10,
                    artifact_count:       0,
                    status:               "success".to_string(),
                    reason:               None,
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
                EventBody::RunArchived(RunArchivedProps { actor: None }),
                None,
            ))
            .unwrap();
        state
            .apply_event(&test_event(
                3,
                EventBody::RunArchived(RunArchivedProps { actor: None }),
                None,
            ))
            .unwrap();

        assert_eq!(
            state.status.as_ref().map(|record| record.status),
            Some(RunStatus::Archived)
        );
        assert_eq!(state.prior_status, Some(RunStatus::Succeeded));
    }

    #[test]
    fn run_unarchived_with_non_terminal_restored_status_is_ignored() {
        // The operations layer never emits this, but a corrupt log or an
        // imported event must not drag the projection into a nonsense state
        // (status=Running without any RunRunning event preceding it).
        use fabro_types::RunStatus;
        use fabro_types::run_event::{RunArchivedProps, RunCompletedProps, RunUnarchivedProps};

        let mut state = RunProjection::default();
        state
            .apply_event(&test_event(
                1,
                EventBody::RunCompleted(RunCompletedProps {
                    duration_ms:          10,
                    artifact_count:       0,
                    status:               "success".to_string(),
                    reason:               None,
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
                EventBody::RunArchived(RunArchivedProps { actor: None }),
                None,
            ))
            .unwrap();
        state
            .apply_event(&test_event(
                3,
                EventBody::RunUnarchived(RunUnarchivedProps {
                    actor:           None,
                    restored_status: RunStatus::Running,
                }),
                None,
            ))
            .unwrap();

        assert_eq!(
            state.status.as_ref().map(|record| record.status),
            Some(RunStatus::Archived)
        );
        assert_eq!(state.prior_status, Some(RunStatus::Succeeded));
    }

    #[test]
    fn run_archived_on_non_terminal_projection_is_ignored() {
        // Same defensive posture on the other side: an event log that
        // somehow contains RunArchived on a Running run must not produce
        // a projection with status=Archived and prior_status=Some(Running).
        use fabro_types::RunStatus;
        use fabro_types::run_event::{RunArchivedProps, RunStatusTransitionProps};

        let mut state = RunProjection::default();
        state
            .apply_event(&test_event(
                1,
                EventBody::RunRunning(RunStatusTransitionProps { reason: None }),
                None,
            ))
            .unwrap();
        state
            .apply_event(&test_event(
                2,
                EventBody::RunArchived(RunArchivedProps { actor: None }),
                None,
            ))
            .unwrap();

        assert_eq!(
            state.status.as_ref().map(|record| record.status),
            Some(RunStatus::Running)
        );
        assert_eq!(state.prior_status, None);
    }
}
