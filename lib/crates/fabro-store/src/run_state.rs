use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::{EventEnvelope, Result, RunSummary, StageId, StoreError};
use fabro_types::run_event::{
    AgentCliStartedProps, AgentSessionStartedProps, CheckpointCompletedProps, RunCompletedProps,
    RunFailedProps, StageCompletedProps, StagePromptProps,
};
use fabro_types::{
    Checkpoint, Conclusion, EventBody, FailureSignature, NodeStatusRecord, Outcome,
    PullRequestRecord, Retro, RunControlAction, RunEvent, RunId, RunRecord, RunStatus,
    RunStatusRecord, SandboxRecord, StageStatus, StageUsage, StartRecord, StatusReason, TokenUsage,
};

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct RunProjection {
    pub run: Option<RunRecord>,
    pub graph_source: Option<String>,
    pub start: Option<StartRecord>,
    pub status: Option<RunStatusRecord>,
    pub pending_control: Option<RunControlAction>,
    pub checkpoint: Option<Checkpoint>,
    pub checkpoints: Vec<(u32, Checkpoint)>,
    pub conclusion: Option<Conclusion>,
    pub retro: Option<Retro>,
    pub retro_prompt: Option<String>,
    pub retro_response: Option<String>,
    pub sandbox: Option<SandboxRecord>,
    pub final_patch: Option<String>,
    pub pull_request: Option<PullRequestRecord>,
    nodes: HashMap<StageId, NodeState>,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct NodeState {
    pub prompt: Option<String>,
    pub response: Option<String>,
    pub status: Option<NodeStatusRecord>,
    pub provider_used: Option<serde_json::Value>,
    pub diff: Option<String>,
    pub script_invocation: Option<serde_json::Value>,
    pub script_timing: Option<serde_json::Value>,
    pub parallel_results: Option<serde_json::Value>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct RunUsage {
    input_tokens: i64,
    output_tokens: i64,
    #[serde(default)]
    reasoning_tokens: Option<i64>,
    #[serde(default)]
    cache_read_tokens: Option<i64>,
    #[serde(default)]
    cache_write_tokens: Option<i64>,
    #[serde(default)]
    cost: Option<f64>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct EventProjectionCache {
    pub last_seq: u32,
    pub state: RunProjection,
}

impl RunProjection {
    pub(crate) fn apply_events(events: &[EventEnvelope]) -> Result<Self> {
        let mut state = Self::default();
        for event in events {
            state.apply_event(event)?;
        }
        Ok(state)
    }

    pub(crate) fn apply_event(&mut self, event: &EventEnvelope) -> Result<()> {
        let stored = RunEvent::from_ref(event.payload.as_value())
            .map_err(|err| StoreError::InvalidEvent(format!("invalid stored event: {err}")))?;
        let ts = stored.ts;
        let run_id = stored.run_id;

        match &stored.body {
            EventBody::RunCreated(props) => {
                let working_directory = PathBuf::from(&props.working_directory);
                let labels = props.labels.clone().into_iter().collect::<HashMap<_, _>>();
                self.run = Some(RunRecord {
                    run_id,
                    settings: props.settings.clone(),
                    graph: props.graph.clone(),
                    workflow_slug: props.workflow_slug.clone(),
                    working_directory,
                    host_repo_path: props.host_repo_path.clone(),
                    repo_origin_url: props.repo_origin_url.clone(),
                    base_branch: props.base_branch.clone(),
                    labels,
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
                self.status = Some(run_status_record(RunStatus::Submitted, props.reason, ts));
            }
            EventBody::RunStarting(props) => {
                self.status = Some(run_status_record(RunStatus::Starting, props.reason, ts));
            }
            EventBody::RunRunning(props) => {
                self.status = Some(run_status_record(RunStatus::Running, props.reason, ts));
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
                self.status = Some(run_status_record(RunStatus::Paused, None, ts));
                self.pending_control = None;
            }
            EventBody::RunUnpaused(_) => {
                self.status = Some(run_status_record(RunStatus::Running, None, ts));
                self.pending_control = None;
            }
            EventBody::RunCompleted(props) => {
                self.status = Some(run_status_record(RunStatus::Succeeded, props.reason, ts));
                self.pending_control = None;
                self.conclusion = Some(conclusion_from_completed(props, ts)?);
                self.final_patch.clone_from(&props.final_patch);
            }
            EventBody::RunFailed(props) => {
                self.status = Some(run_status_record(RunStatus::Failed, props.reason, ts));
                self.pending_control = None;
                self.conclusion = Some(conclusion_from_failed(props, ts));
            }
            EventBody::RunRewound(_) => {
                self.reset_for_rewind();
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
                    provider: props.provider.clone(),
                    working_directory: props.working_directory.clone(),
                    identifier: props.identifier.clone(),
                    host_working_directory: props.host_working_directory.clone(),
                    container_mount_point: props.container_mount_point.clone(),
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
                    .map_err(|err| {
                        StoreError::InvalidEvent(format!("invalid retro payload: {err}"))
                    })?;
            }
            EventBody::PullRequestCreated(props) => {
                self.pull_request = Some(PullRequestRecord {
                    html_url: props.pr_url.clone(),
                    number: props.pr_number,
                    owner: props.owner.clone(),
                    repo: props.repo.clone(),
                    base_branch: props.base_branch.clone(),
                    head_branch: props.head_branch.clone(),
                    title: props.title.clone(),
                });
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
                        StoreError::InvalidEvent(format!("invalid command.started payload: {err}"))
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
                    StoreError::InvalidEvent(format!("invalid command.completed payload: {err}"))
                })?);
            }
            EventBody::ParallelCompleted(props) => {
                let Some(node_id) = stored.node_id.as_deref() else {
                    return Ok(());
                };
                let visit = self.current_visit_for(node_id).unwrap_or(1);
                self.node_mut(node_id, visit).parallel_results =
                    Some(serde_json::to_value(&props.results).map_err(|err| {
                        StoreError::InvalidEvent(format!(
                            "invalid parallel.completed payload: {err}"
                        ))
                    })?);
            }
            _ => {}
        }

        Ok(())
    }

    pub fn node(&self, node: &StageId) -> Option<&NodeState> {
        self.nodes.get(node)
    }

    pub fn iter_nodes(&self) -> impl Iterator<Item = (&StageId, &NodeState)> {
        self.nodes.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn set_node(&mut self, node: StageId, state: NodeState) {
        self.nodes.insert(node, state);
    }

    pub fn list_node_visits(&self, node_id: &str) -> Vec<u32> {
        let mut visits = self
            .nodes
            .keys()
            .filter(|node| node.node_id() == node_id)
            .map(StageId::visit)
            .collect::<Vec<_>>();
        visits.sort_unstable();
        visits.dedup();
        visits
    }

    pub(crate) fn build_summary(&self, run_id: &RunId) -> RunSummary {
        let workflow_name = self.run.as_ref().map(|run| {
            if run.graph.name.is_empty() {
                "unnamed".to_string()
            } else {
                run.graph.name.clone()
            }
        });
        let goal = self.run.as_ref().and_then(|run| {
            let goal = run.graph.goal();
            (!goal.is_empty()).then(|| goal.to_string())
        });
        RunSummary {
            run_id: *run_id,
            workflow_name,
            workflow_slug: self.run.as_ref().and_then(|run| run.workflow_slug.clone()),
            goal,
            labels: self
                .run
                .as_ref()
                .map(|run| run.labels.clone())
                .unwrap_or_default(),
            host_repo_path: self.run.as_ref().and_then(|run| run.host_repo_path.clone()),
            start_time: self.start.as_ref().map(|start| start.start_time),
            status: self.status.as_ref().map(|status| status.status),
            status_reason: self.status.as_ref().and_then(|status| status.reason),
            pending_control: self.pending_control,
            duration_ms: self
                .conclusion
                .as_ref()
                .map(|conclusion| conclusion.duration_ms),
            total_cost: self
                .conclusion
                .as_ref()
                .and_then(|conclusion| conclusion.total_cost),
        }
    }

    fn node_mut(&mut self, node_id: &str, visit: u32) -> &mut NodeState {
        self.nodes.entry(StageId::new(node_id, visit)).or_default()
    }

    fn current_visit_for(&self, node_id: &str) -> Option<u32> {
        self.nodes
            .keys()
            .filter(|node| node.node_id() == node_id)
            .map(StageId::visit)
            .max()
    }

    fn reset_for_rewind(&mut self) {
        self.status = None;
        self.pending_control = None;
        self.checkpoint = None;
        self.checkpoints.clear();
        self.conclusion = None;
        self.retro = None;
        self.retro_prompt = None;
        self.retro_response = None;
        self.sandbox = None;
        self.final_patch = None;
        self.pull_request = None;
        self.nodes.clear();
    }
}

fn run_status_record(
    status: RunStatus,
    reason: Option<StatusReason>,
    updated_at: DateTime<Utc>,
) -> RunStatusRecord {
    RunStatusRecord {
        status,
        reason,
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
    let usage = props.usage.as_ref().map(run_usage_from_token_usage);
    Ok(Conclusion {
        timestamp,
        status: StageStatus::from_str(&props.status).map_err(|err| {
            StoreError::InvalidEvent(format!("invalid completed stage status: {err}"))
        })?,
        duration_ms: props.duration_ms,
        failure_reason: None,
        final_git_commit_sha: props.final_git_commit_sha.clone(),
        stages: Vec::new(),
        total_cost: props.total_cost,
        total_retries: 0,
        total_input_tokens: usage.as_ref().map_or(0, |usage| usage.input_tokens),
        total_output_tokens: usage.as_ref().map_or(0, |usage| usage.output_tokens),
        total_cache_read_tokens: usage
            .as_ref()
            .and_then(|usage| usage.cache_read_tokens)
            .unwrap_or(0),
        total_cache_write_tokens: usage
            .as_ref()
            .and_then(|usage| usage.cache_write_tokens)
            .unwrap_or(0),
        total_reasoning_tokens: usage
            .as_ref()
            .and_then(|usage| usage.reasoning_tokens)
            .unwrap_or(0),
        has_pricing: usage.as_ref().is_some_and(|usage| usage.cost.is_some()),
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
        total_cost: None,
        total_retries: 0,
        total_input_tokens: 0,
        total_output_tokens: 0,
        total_cache_read_tokens: 0,
        total_cache_write_tokens: 0,
        total_reasoning_tokens: 0,
        has_pricing: false,
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

fn stage_outcome_from_props(props: &StageCompletedProps) -> Outcome<Option<StageUsage>> {
    Outcome {
        status: props.status.clone(),
        preferred_label: props.preferred_label.clone(),
        suggested_next_ids: props.suggested_next_ids.clone(),
        context_updates: props
            .context_updates
            .clone()
            .unwrap_or_default()
            .into_iter()
            .collect(),
        jump_to_node: props.jump_to_node.clone(),
        notes: props.notes.clone(),
        failure: props.failure.clone(),
        usage: props.usage.clone(),
        files_touched: props.files_touched.clone(),
        duration_ms: Some(props.duration_ms),
    }
}

fn node_status_from_outcome(
    outcome: &Outcome<Option<StageUsage>>,
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

fn run_usage_from_token_usage(usage: &TokenUsage) -> RunUsage {
    RunUsage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        reasoning_tokens: usage.reasoning_tokens,
        cache_read_tokens: usage.cache_read_tokens,
        cache_write_tokens: usage.cache_write_tokens,
        cost: None,
    }
}
