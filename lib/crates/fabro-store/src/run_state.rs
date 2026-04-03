use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::{CatalogRecord, EventEnvelope, NodeVisitRef, Result, RunSummary, StoreError};
use fabro_types::{
    Checkpoint, Conclusion, FailureSignature, NodeStatusRecord, Outcome, PullRequestRecord, Retro,
    RunId, RunRecord, RunStatus, RunStatusRecord, SandboxRecord, StageStatus, StageUsage,
    StartRecord, StatusReason,
};

#[derive(Debug, Clone, Default)]
pub struct RunProjection {
    pub run: Option<RunRecord>,
    pub graph_source: Option<String>,
    pub start: Option<StartRecord>,
    pub status: Option<RunStatusRecord>,
    pub checkpoint: Option<Checkpoint>,
    pub checkpoints: Vec<(u32, Checkpoint)>,
    pub conclusion: Option<Conclusion>,
    pub retro: Option<Retro>,
    pub retro_prompt: Option<String>,
    pub retro_response: Option<String>,
    pub sandbox: Option<SandboxRecord>,
    pub final_patch: Option<String>,
    pub pull_request: Option<PullRequestRecord>,
    pub nodes: HashMap<(String, u32), NodeState>,
    pub last_git_sha: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct NodeState {
    pub prompt: Option<String>,
    pub response: Option<String>,
    pub status: Option<NodeStatusRecord>,
    pub outcome: Option<Outcome<Option<StageUsage>>>,
    pub provider_used: Option<serde_json::Value>,
    pub diff: Option<String>,
    pub script_invocation: Option<serde_json::Value>,
    pub script_timing: Option<serde_json::Value>,
    pub parallel_results: Option<serde_json::Value>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
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
        let value = event.payload.as_value();
        let ts = parse_ts(value)?;
        let event_name = value
            .get("event")
            .and_then(Value::as_str)
            .ok_or_else(|| StoreError::InvalidEvent("event payload missing event name".into()))?;
        let run_id = parse_run_id(value)?;
        let properties = value
            .get("properties")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();

        match event_name {
            "run.created" => {
                let settings = required_json::<fabro_types::Settings>(&properties, "settings")?;
                let graph = required_json::<fabro_types::Graph>(&properties, "graph")?;
                let working_directory =
                    required_string(&properties, "working_directory").map(PathBuf::from)?;
                let labels = optional_json::<BTreeMap<String, String>>(&properties, "labels")?
                    .unwrap_or_default()
                    .into_iter()
                    .collect::<HashMap<_, _>>();
                self.run = Some(RunRecord {
                    run_id,
                    created_at: ts,
                    settings,
                    graph,
                    workflow_slug: optional_string(&properties, "workflow_slug"),
                    working_directory,
                    host_repo_path: optional_string(&properties, "host_repo_path"),
                    base_branch: optional_string(&properties, "base_branch"),
                    labels,
                });
                self.graph_source = optional_string(&properties, "workflow_source");
            }
            "run.started" => {
                self.start = Some(StartRecord {
                    run_id,
                    start_time: ts,
                    run_branch: optional_string(&properties, "run_branch"),
                    base_sha: optional_string(&properties, "base_sha"),
                });
            }
            "run.submitted" => {
                self.status = Some(run_status_record(RunStatus::Submitted, &properties, ts)?);
            }
            "run.starting" => {
                self.status = Some(run_status_record(RunStatus::Starting, &properties, ts)?);
            }
            "run.running" => {
                self.status = Some(run_status_record(RunStatus::Running, &properties, ts)?);
            }
            "run.removing" => {
                self.status = Some(run_status_record(RunStatus::Removing, &properties, ts)?);
            }
            "run.completed" => {
                self.status = Some(run_status_record(RunStatus::Succeeded, &properties, ts)?);
                self.conclusion = Some(conclusion_from_completed(&properties, ts)?);
                self.final_patch = optional_string(&properties, "final_patch");
                self.last_git_sha = optional_string(&properties, "final_git_commit_sha")
                    .or_else(|| self.last_git_sha.clone());
            }
            "run.failed" => {
                self.status = Some(run_status_record(RunStatus::Failed, &properties, ts)?);
                self.conclusion = Some(conclusion_from_failed(&properties, ts));
                self.last_git_sha = optional_string(&properties, "git_commit_sha")
                    .or_else(|| self.last_git_sha.clone());
            }
            "run.rewound" => {
                self.reset_for_rewind();
                self.last_git_sha = optional_string(&properties, "run_commit_sha")
                    .or_else(|| self.last_git_sha.clone());
            }
            "checkpoint.completed" => {
                let checkpoint = checkpoint_from_properties(&properties, ts)?;
                self.last_git_sha = checkpoint
                    .git_commit_sha
                    .clone()
                    .or_else(|| self.last_git_sha.clone());
                if let Some(node_id) = value.get("node_id").and_then(Value::as_str) {
                    let visit = checkpoint
                        .node_visits
                        .get(node_id)
                        .and_then(|visit| u32::try_from(*visit).ok())
                        .unwrap_or(1);
                    if let Some(diff) = optional_string(&properties, "diff") {
                        self.node_mut(node_id, visit).diff = Some(diff);
                    }
                }
                self.checkpoint = Some(checkpoint.clone());
                self.checkpoints.push((event.seq, checkpoint));
            }
            "sandbox.initialized" => {
                self.sandbox = Some(SandboxRecord {
                    provider: required_string(&properties, "provider")?,
                    working_directory: required_string(&properties, "working_directory")?,
                    identifier: optional_string(&properties, "identifier"),
                    host_working_directory: optional_string(&properties, "host_working_directory"),
                    container_mount_point: optional_string(&properties, "container_mount_point"),
                });
            }
            "retro.started" => {
                self.retro_prompt = optional_string(&properties, "prompt");
            }
            "retro.completed" => {
                self.retro_response = optional_string(&properties, "response");
                self.retro = optional_json::<Retro>(&properties, "retro")?;
            }
            "pull_request.created" => {
                self.pull_request = Some(PullRequestRecord {
                    html_url: required_string(&properties, "pr_url")?,
                    number: required_u64(&properties, "pr_number")?,
                    owner: required_string(&properties, "owner")?,
                    repo: required_string(&properties, "repo")?,
                    base_branch: required_string(&properties, "base_branch")?,
                    head_branch: required_string(&properties, "head_branch")?,
                    title: required_string(&properties, "title")?,
                });
            }
            "stage.prompt" => {
                let Some(node_id) = value.get("node_id").and_then(Value::as_str) else {
                    return Ok(());
                };
                let visit = required_u32(&properties, "visit")?;
                self.node_mut(node_id, visit).prompt = optional_string(&properties, "text");
                self.node_mut(node_id, visit).provider_used =
                    provider_used_from_prompt(&properties);
            }
            "prompt.completed" => {
                let Some(node_id) = value.get("node_id").and_then(Value::as_str) else {
                    return Ok(());
                };
                let visit = self.current_visit_for(node_id).unwrap_or(1);
                self.node_mut(node_id, visit).response = optional_string(&properties, "response");
            }
            "stage.completed" => {
                let Some(node_id) = value.get("node_id").and_then(Value::as_str) else {
                    return Ok(());
                };
                let visit = stage_visit(node_id, &properties, self).unwrap_or(1);
                let response = optional_string(&properties, "response");
                let outcome = stage_outcome_from_properties(&properties)?;
                let status = node_status_from_outcome(&outcome, ts);
                let node = self.node_mut(node_id, visit);
                node.response = response;
                node.status = Some(status);
                node.outcome = Some(outcome);
            }
            "stage.failed" => {
                let Some(node_id) = value.get("node_id").and_then(Value::as_str) else {
                    return Ok(());
                };
                let visit = self.current_visit_for(node_id).unwrap_or(1);
                let failure = optional_json::<fabro_types::FailureDetail>(&properties, "failure")?;
                let failure_reason = failure.as_ref().map(|detail| detail.message.clone());
                let node = self.node_mut(node_id, visit);
                node.status = Some(NodeStatusRecord {
                    status: StageStatus::Fail,
                    notes: None,
                    failure_reason: failure_reason.clone(),
                    timestamp: ts,
                });
                node.outcome = Some(Outcome {
                    status: StageStatus::Fail,
                    preferred_label: None,
                    suggested_next_ids: Vec::new(),
                    context_updates: HashMap::new(),
                    jump_to_node: None,
                    notes: None,
                    failure,
                    usage: None,
                    files_touched: Vec::new(),
                    duration_ms: None,
                });
            }
            "agent.session.started" | "agent.cli.started" => {
                let Some(node_id) = value.get("node_id").and_then(Value::as_str) else {
                    return Ok(());
                };
                let visit = required_u32(&properties, "visit")?;
                self.node_mut(node_id, visit).provider_used =
                    Some(provider_used_from_agent_event(event_name, &properties));
            }
            "command.started" => {
                let Some(node_id) = value.get("node_id").and_then(Value::as_str) else {
                    return Ok(());
                };
                let visit = self.current_visit_for(node_id).unwrap_or(1);
                self.node_mut(node_id, visit).script_invocation =
                    Some(Value::Object(properties.clone()));
            }
            "command.completed" => {
                let Some(node_id) = value.get("node_id").and_then(Value::as_str) else {
                    return Ok(());
                };
                let visit = self.current_visit_for(node_id).unwrap_or(1);
                let node = self.node_mut(node_id, visit);
                node.stdout = optional_string(&properties, "stdout");
                node.stderr = optional_string(&properties, "stderr");
                node.script_timing = Some(Value::Object(properties.clone()));
            }
            "parallel.completed" => {
                let Some(node_id) = value.get("node_id").and_then(Value::as_str) else {
                    return Ok(());
                };
                let visit = self.current_visit_for(node_id).unwrap_or(1);
                self.node_mut(node_id, visit).parallel_results = properties.get("results").cloned();
            }
            _ => {}
        }

        Ok(())
    }

    pub fn node(&self, node: &NodeVisitRef<'_>) -> Option<&NodeState> {
        self.nodes.get(&(node.node_id.to_string(), node.visit))
    }

    pub fn list_node_visits(&self, node_id: &str) -> Vec<u32> {
        let mut visits = self
            .nodes
            .keys()
            .filter(|(current_node_id, _)| current_node_id == node_id)
            .map(|(_, visit)| *visit)
            .collect::<Vec<_>>();
        visits.sort_unstable();
        visits.dedup();
        visits
    }

    pub(crate) fn build_summary(&self, catalog: &CatalogRecord) -> RunSummary {
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
            catalog: catalog.clone(),
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
        self.nodes.entry((node_id.to_string(), visit)).or_default()
    }

    fn current_visit_for(&self, node_id: &str) -> Option<u32> {
        self.nodes
            .keys()
            .filter(|(current_node_id, _)| current_node_id == node_id)
            .map(|(_, visit)| *visit)
            .max()
    }

    fn reset_for_rewind(&mut self) {
        self.status = None;
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

fn parse_ts(value: &Value) -> Result<DateTime<Utc>> {
    let ts = value
        .get("ts")
        .and_then(Value::as_str)
        .ok_or_else(|| StoreError::InvalidEvent("event payload missing ts".into()))?;
    chrono::DateTime::parse_from_rfc3339(ts)
        .map(|ts| ts.with_timezone(&Utc))
        .map_err(|err| StoreError::InvalidEvent(format!("invalid event ts: {err}")))
}

fn parse_run_id(value: &Value) -> Result<RunId> {
    let run_id = value
        .get("run_id")
        .and_then(Value::as_str)
        .ok_or_else(|| StoreError::InvalidEvent("event payload missing run_id".into()))?;
    run_id
        .parse()
        .map_err(|err| StoreError::InvalidEvent(format!("invalid run_id: {err}")))
}

fn required_string(properties: &serde_json::Map<String, Value>, key: &str) -> Result<String> {
    properties
        .get(key)
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .ok_or_else(|| StoreError::InvalidEvent(format!("event missing string property {key}")))
}

fn optional_string(properties: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    properties
        .get(key)
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

fn required_u64(properties: &serde_json::Map<String, Value>, key: &str) -> Result<u64> {
    properties
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| StoreError::InvalidEvent(format!("event missing integer property {key}")))
}

fn required_u32(properties: &serde_json::Map<String, Value>, key: &str) -> Result<u32> {
    u32::try_from(required_u64(properties, key)?)
        .map_err(|_| StoreError::InvalidEvent(format!("property {key} does not fit in u32")))
}

fn required_json<T: DeserializeOwned>(
    properties: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<T> {
    let value = properties
        .get(key)
        .cloned()
        .ok_or_else(|| StoreError::InvalidEvent(format!("event missing property {key}")))?;
    serde_json::from_value(value)
        .map_err(|err| StoreError::InvalidEvent(format!("invalid property {key}: {err}")))
}

fn optional_json<T: DeserializeOwned>(
    properties: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<T>> {
    properties
        .get(key)
        .filter(|value| !value.is_null())
        .cloned()
        .map(|value| {
            serde_json::from_value(value)
                .map_err(|err| StoreError::InvalidEvent(format!("invalid property {key}: {err}")))
        })
        .transpose()
}

fn parse_reason(properties: &serde_json::Map<String, Value>) -> Result<Option<StatusReason>> {
    optional_string(properties, "reason")
        .map(|reason| {
            serde_json::from_value(Value::String(reason))
                .map_err(|err| StoreError::InvalidEvent(format!("invalid status reason: {err}")))
        })
        .transpose()
}

fn run_status_record(
    status: RunStatus,
    properties: &serde_json::Map<String, Value>,
    updated_at: DateTime<Utc>,
) -> Result<RunStatusRecord> {
    Ok(RunStatusRecord {
        status,
        reason: parse_reason(properties)?,
        updated_at,
    })
}

fn checkpoint_from_properties(
    properties: &serde_json::Map<String, Value>,
    timestamp: DateTime<Utc>,
) -> Result<Checkpoint> {
    let loop_failure_signatures =
        optional_json::<HashMap<String, usize>>(properties, "loop_failure_signatures")?
            .unwrap_or_default()
            .into_iter()
            .map(|(key, value)| (FailureSignature(key), value))
            .collect();
    let restart_failure_signatures =
        optional_json::<HashMap<String, usize>>(properties, "restart_failure_signatures")?
            .unwrap_or_default()
            .into_iter()
            .map(|(key, value)| (FailureSignature(key), value))
            .collect();

    Ok(Checkpoint {
        timestamp,
        current_node: required_string(properties, "current_node")?,
        completed_nodes: optional_json(properties, "completed_nodes")?.unwrap_or_default(),
        node_retries: optional_json(properties, "node_retries")?.unwrap_or_default(),
        context_values: optional_json(properties, "context_values")?.unwrap_or_default(),
        node_outcomes: optional_json(properties, "node_outcomes")?.unwrap_or_default(),
        next_node_id: optional_string(properties, "next_node_id"),
        git_commit_sha: optional_string(properties, "git_commit_sha"),
        loop_failure_signatures,
        restart_failure_signatures,
        node_visits: optional_json(properties, "node_visits")?.unwrap_or_default(),
    })
}

fn conclusion_from_completed(
    properties: &serde_json::Map<String, Value>,
    timestamp: DateTime<Utc>,
) -> Result<Conclusion> {
    let usage = optional_json::<fabro_types::StageUsage>(properties, "usage")?;
    Ok(Conclusion {
        timestamp,
        status: StageStatus::from_str(&required_string(properties, "status")?).map_err(|err| {
            StoreError::InvalidEvent(format!("invalid completed stage status: {err}"))
        })?,
        duration_ms: required_u64(properties, "duration_ms")?,
        failure_reason: None,
        final_git_commit_sha: optional_string(properties, "final_git_commit_sha"),
        stages: Vec::new(),
        total_cost: properties.get("total_cost").and_then(Value::as_f64),
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

fn conclusion_from_failed(
    properties: &serde_json::Map<String, Value>,
    timestamp: DateTime<Utc>,
) -> Conclusion {
    Conclusion {
        timestamp,
        status: StageStatus::Fail,
        duration_ms: properties
            .get("duration_ms")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        failure_reason: optional_string(properties, "error"),
        final_git_commit_sha: optional_string(properties, "git_commit_sha"),
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
    properties: &serde_json::Map<String, Value>,
    state: &RunProjection,
) -> Option<u32> {
    properties
        .get("node_visits")
        .and_then(|value| serde_json::from_value::<HashMap<String, usize>>(value.clone()).ok())
        .and_then(|visits| visits.get(node_id).copied())
        .and_then(|visit| u32::try_from(visit).ok())
        .or_else(|| state.current_visit_for(node_id))
}

fn stage_outcome_from_properties(
    properties: &serde_json::Map<String, Value>,
) -> Result<Outcome<Option<StageUsage>>> {
    let status = StageStatus::from_str(&required_string(properties, "status")?)
        .map_err(|err| StoreError::InvalidEvent(format!("invalid stage status: {err}")))?;
    Ok(Outcome {
        status,
        preferred_label: optional_string(properties, "preferred_label"),
        suggested_next_ids: optional_json(properties, "suggested_next_ids")?.unwrap_or_default(),
        context_updates: optional_json(properties, "context_updates")?.unwrap_or_default(),
        jump_to_node: optional_string(properties, "jump_to_node"),
        notes: optional_string(properties, "notes"),
        failure: optional_json(properties, "failure")?,
        usage: optional_json(properties, "usage")?,
        files_touched: optional_json(properties, "files_touched")?.unwrap_or_default(),
        duration_ms: properties.get("duration_ms").and_then(Value::as_u64),
    })
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

fn provider_used_from_prompt(properties: &serde_json::Map<String, Value>) -> Option<Value> {
    let mut provider_used = serde_json::Map::new();
    if let Some(mode) = optional_string(properties, "mode") {
        provider_used.insert("mode".to_string(), Value::String(mode));
    }
    if let Some(provider) = optional_string(properties, "provider") {
        provider_used.insert("provider".to_string(), Value::String(provider));
    }
    if let Some(model) = optional_string(properties, "model") {
        provider_used.insert("model".to_string(), Value::String(model));
    }
    (!provider_used.is_empty()).then_some(Value::Object(provider_used))
}

fn provider_used_from_agent_event(
    event_name: &str,
    properties: &serde_json::Map<String, Value>,
) -> Value {
    let mut provider_used = serde_json::Map::new();
    provider_used.insert(
        "mode".to_string(),
        Value::String(if event_name == "agent.cli.started" {
            "cli".to_string()
        } else {
            "agent".to_string()
        }),
    );
    if let Some(provider) = optional_string(properties, "provider") {
        provider_used.insert("provider".to_string(), Value::String(provider));
    }
    if let Some(model) = optional_string(properties, "model") {
        provider_used.insert("model".to_string(), Value::String(model));
    }
    if let Some(command) = optional_string(properties, "command") {
        provider_used.insert("command".to_string(), Value::String(command));
    }
    Value::Object(provider_used)
}
