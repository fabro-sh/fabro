use std::convert::TryFrom;

use chrono::{DateTime, Utc};
use fabro_workflow::event::RunNoticeLevel;
use fabro_workflow::outcome::{StageUsage, compute_stage_cost};
use serde_json::{Map, Value};

#[derive(Debug, Clone)]
pub(super) struct ProgressUsage {
    pub(super) model: Option<String>,
    pub(super) input_tokens: u64,
    pub(super) output_tokens: u64,
    pub(super) speed: Option<String>,
    pub(super) cost: Option<f64>,
}

impl ProgressUsage {
    pub(super) fn from_value(value: &Value) -> Option<Self> {
        let Value::Object(fields) = value else {
            return None;
        };

        Some(Self {
            model: string_field(fields, "model"),
            input_tokens: u64_field(fields, "input_tokens"),
            output_tokens: u64_field(fields, "output_tokens"),
            speed: string_field(fields, "speed"),
            cost: f64_field(fields, "cost"),
        })
    }

    pub(super) fn total_tokens(&self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }

    pub(super) fn display_cost(&self) -> Option<f64> {
        self.cost.or_else(|| {
            let model = self.model.clone()?;
            let input_tokens = i64::try_from(self.input_tokens).ok()?;
            let output_tokens = i64::try_from(self.output_tokens).ok()?;
            let usage = StageUsage {
                model,
                input_tokens,
                output_tokens,
                cache_read_tokens: None,
                cache_write_tokens: None,
                reasoning_tokens: None,
                speed: self.speed.clone(),
                cost: None,
            };
            compute_stage_cost(&usage)
        })
    }
}

#[derive(Debug, Clone)]
pub(super) enum ProgressEvent {
    WorkflowStarted {
        worktree_dir: Option<String>,
        base_branch: Option<String>,
        base_sha: Option<String>,
    },
    WorkingDirectorySet {
        working_directory: String,
    },
    SandboxInitializing {
        provider: String,
    },
    SandboxReady {
        provider: String,
        duration_ms: u64,
        name: Option<String>,
        cpu: Option<f64>,
        memory: Option<f64>,
        url: Option<String>,
    },
    SshAccessReady {
        ssh_command: String,
    },
    SetupStarted {
        command_count: u64,
    },
    SetupCompleted {
        duration_ms: u64,
    },
    SetupCommandCompleted {
        command: String,
        command_index: u64,
        exit_code: i64,
        duration_ms: u64,
    },
    CliEnsureStarted {
        cli_name: String,
    },
    CliEnsureCompleted {
        cli_name: String,
        already_installed: bool,
        duration_ms: u64,
    },
    CliEnsureFailed {
        cli_name: String,
    },
    DevcontainerResolved {
        dockerfile_lines: u64,
        environment_count: u64,
        lifecycle_command_count: u64,
        workspace_folder: String,
    },
    DevcontainerLifecycleStarted {
        phase: String,
        command_count: u64,
    },
    DevcontainerLifecycleCompleted {
        phase: String,
        duration_ms: u64,
    },
    DevcontainerLifecycleFailed {
        phase: String,
        command: String,
        exit_code: i64,
        stderr: String,
    },
    DevcontainerLifecycleCommandCompleted {
        command: String,
        command_index: u64,
        exit_code: i64,
        duration_ms: u64,
    },
    StageStarted {
        node_id: String,
        name: String,
        script: Option<String>,
    },
    StageCompleted {
        node_id: String,
        name: String,
        duration_ms: u64,
        status: String,
        usage: Option<ProgressUsage>,
    },
    StageFailed {
        node_id: String,
        name: String,
        error: String,
    },
    StageRetrying {
        name: String,
        attempt: u64,
        max_attempts: u64,
        delay_ms: u64,
    },
    ParallelStarted,
    ParallelBranchStarted {
        branch: String,
    },
    ParallelBranchCompleted {
        branch: String,
        duration_ms: u64,
        status: String,
    },
    ParallelCompleted,
    AssistantMessage {
        stage_node_id: String,
        model: String,
    },
    ToolCallStarted {
        stage_node_id: String,
        tool_name: String,
        tool_call_id: String,
        arguments: Value,
        timestamp: Option<DateTime<Utc>>,
    },
    ToolCallCompleted {
        stage_node_id: String,
        tool_call_id: String,
        is_error: bool,
        duration_ms: Option<u64>,
        timestamp: Option<DateTime<Utc>>,
    },
    ContextWindowWarning {
        stage_node_id: String,
        usage_percent: u64,
    },
    CompactionStarted {
        stage_node_id: String,
    },
    CompactionCompleted {
        stage_node_id: String,
        original_turn_count: u64,
        preserved_turn_count: u64,
        tracked_file_count: u64,
    },
    LlmRetry {
        stage_node_id: String,
        model: String,
        attempt: u64,
        delay_ms: u64,
        error: String,
    },
    SubagentSpawned {
        stage_node_id: String,
        agent_id: String,
        task: String,
    },
    SubagentCompleted {
        stage_node_id: String,
        agent_id: String,
        success: bool,
        turns_used: u64,
    },
    EdgeSelected {
        from_node: String,
        to_node: String,
        label: Option<String>,
        condition: Option<String>,
    },
    LoopRestart {
        from_node: String,
        to_node: String,
    },
    RetroStarted,
    RetroCompleted {
        duration_ms: u64,
    },
    RetroFailed {
        duration_ms: u64,
    },
    RunNotice {
        level: RunNoticeLevel,
        code: String,
        message: String,
    },
    PullRequestCreated {
        pr_url: String,
        draft: bool,
    },
    PullRequestFailed {
        error: String,
    },
}

#[allow(clippy::needless_pass_by_value)]
pub(super) fn from_envelope_fields(
    event_name: &str,
    fields: &Map<String, Value>,
) -> Option<ProgressEvent> {
    match event_name {
        "run.started" => Some(ProgressEvent::WorkflowStarted {
            worktree_dir: prop_string_field(fields, "worktree_dir"),
            base_branch: prop_string_field(fields, "base_branch"),
            base_sha: prop_string_field(fields, "base_sha"),
        }),
        "sandbox.initialized" => Some(ProgressEvent::WorkingDirectorySet {
            working_directory: prop_string_field(fields, "working_directory")?,
        }),
        "sandbox.initializing" => Some(ProgressEvent::SandboxInitializing {
            provider: prop_string_field(fields, "provider")
                .unwrap_or_else(|| "unknown".to_string()),
        }),
        "sandbox.ready" => Some(ProgressEvent::SandboxReady {
            provider: prop_string_field(fields, "provider")
                .unwrap_or_else(|| "unknown".to_string()),
            duration_ms: prop_u64_field(fields, "duration_ms"),
            name: prop_string_field(fields, "name"),
            cpu: prop_f64_field(fields, "cpu"),
            memory: prop_f64_field(fields, "memory"),
            url: prop_string_field(fields, "url"),
        }),
        "ssh.ready" => Some(ProgressEvent::SshAccessReady {
            ssh_command: prop_string_field(fields, "ssh_command")?,
        }),
        "setup.started" => Some(ProgressEvent::SetupStarted {
            command_count: prop_u64_field(fields, "command_count"),
        }),
        "setup.completed" => Some(ProgressEvent::SetupCompleted {
            duration_ms: prop_u64_field(fields, "duration_ms"),
        }),
        "setup.command.completed" => Some(ProgressEvent::SetupCommandCompleted {
            command: prop_string_field(fields, "command").unwrap_or_else(|| "?".to_string()),
            command_index: prop_u64_field(fields, "index"),
            exit_code: prop_i64_field(fields, "exit_code"),
            duration_ms: prop_u64_field(fields, "duration_ms"),
        }),
        "cli.ensure.started" => Some(ProgressEvent::CliEnsureStarted {
            cli_name: prop_string_field(fields, "cli_name").unwrap_or_else(|| "?".to_string()),
        }),
        "cli.ensure.completed" => Some(ProgressEvent::CliEnsureCompleted {
            cli_name: prop_string_field(fields, "cli_name").unwrap_or_else(|| "?".to_string()),
            already_installed: prop_bool_field(fields, "already_installed"),
            duration_ms: prop_u64_field(fields, "duration_ms"),
        }),
        "cli.ensure.failed" => Some(ProgressEvent::CliEnsureFailed {
            cli_name: prop_string_field(fields, "cli_name").unwrap_or_else(|| "?".to_string()),
        }),
        "devcontainer.resolved" => Some(ProgressEvent::DevcontainerResolved {
            dockerfile_lines: prop_u64_field(fields, "dockerfile_lines"),
            environment_count: prop_u64_field(fields, "environment_count"),
            lifecycle_command_count: prop_u64_field(fields, "lifecycle_command_count"),
            workspace_folder: prop_string_field(fields, "workspace_folder")
                .unwrap_or_else(|| "?".to_string()),
        }),
        "devcontainer.lifecycle.started" => Some(ProgressEvent::DevcontainerLifecycleStarted {
            phase: prop_string_field(fields, "phase").unwrap_or_else(|| "?".to_string()),
            command_count: prop_u64_field(fields, "command_count"),
        }),
        "devcontainer.lifecycle.completed" => Some(ProgressEvent::DevcontainerLifecycleCompleted {
            phase: prop_string_field(fields, "phase").unwrap_or_else(|| "?".to_string()),
            duration_ms: prop_u64_field(fields, "duration_ms"),
        }),
        "devcontainer.lifecycle.failed" => Some(ProgressEvent::DevcontainerLifecycleFailed {
            phase: prop_string_field(fields, "phase").unwrap_or_else(|| "?".to_string()),
            command: prop_string_field(fields, "command").unwrap_or_else(|| "?".to_string()),
            exit_code: prop_i64_field(fields, "exit_code"),
            stderr: prop_display_field(fields, "stderr").unwrap_or_default(),
        }),
        "devcontainer.lifecycle.command.completed" => {
            Some(ProgressEvent::DevcontainerLifecycleCommandCompleted {
                command: prop_string_field(fields, "command").unwrap_or_else(|| "?".to_string()),
                command_index: prop_u64_field(fields, "index"),
                exit_code: prop_i64_field(fields, "exit_code"),
                duration_ms: prop_u64_field(fields, "duration_ms"),
            })
        }
        "stage.started" => Some(ProgressEvent::StageStarted {
            node_id: string_field(fields, "node_id").unwrap_or_else(|| "?".to_string()),
            name: string_field(fields, "node_label").unwrap_or_else(|| "?".to_string()),
            script: prop_string_field(fields, "script"),
        }),
        "stage.completed" => Some(ProgressEvent::StageCompleted {
            node_id: string_field(fields, "node_id").unwrap_or_else(|| "?".to_string()),
            name: string_field(fields, "node_label").unwrap_or_else(|| "?".to_string()),
            duration_ms: prop_u64_field(fields, "duration_ms"),
            status: prop_string_field(fields, "status").unwrap_or_else(|| "success".to_string()),
            usage: prop_value(fields, "usage").and_then(ProgressUsage::from_value),
        }),
        "stage.failed" => Some(ProgressEvent::StageFailed {
            node_id: string_field(fields, "node_id").unwrap_or_else(|| "?".to_string()),
            name: string_field(fields, "node_label").unwrap_or_else(|| "?".to_string()),
            error: prop_display_field(fields, "error")
                .unwrap_or_else(|| "unknown error".to_string()),
        }),
        "stage.retrying" => Some(ProgressEvent::StageRetrying {
            name: string_field(fields, "node_label").unwrap_or_else(|| "?".to_string()),
            attempt: prop_u64_field(fields, "attempt"),
            max_attempts: prop_u64_field(fields, "max_attempts"),
            delay_ms: prop_u64_field(fields, "delay_ms"),
        }),
        "parallel.started" => Some(ProgressEvent::ParallelStarted),
        "parallel.branch.started" => Some(ProgressEvent::ParallelBranchStarted {
            branch: string_field(fields, "node_id").unwrap_or_else(|| "?".to_string()),
        }),
        "parallel.branch.completed" => Some(ProgressEvent::ParallelBranchCompleted {
            branch: string_field(fields, "node_id").unwrap_or_else(|| "?".to_string()),
            duration_ms: prop_u64_field(fields, "duration_ms"),
            status: prop_string_field(fields, "status").unwrap_or_else(|| "success".to_string()),
        }),
        "parallel.completed" => Some(ProgressEvent::ParallelCompleted),
        "agent.message" => Some(ProgressEvent::AssistantMessage {
            stage_node_id: string_field(fields, "node_id").unwrap_or_else(|| "?".to_string()),
            model: prop_string_field(fields, "model").unwrap_or_else(|| "?".to_string()),
        }),
        "agent.tool.started" => Some(ProgressEvent::ToolCallStarted {
            stage_node_id: string_field(fields, "node_id").unwrap_or_else(|| "?".to_string()),
            tool_name: prop_string_field(fields, "tool_name").unwrap_or_else(|| "?".to_string()),
            tool_call_id: prop_string_field(fields, "tool_call_id")
                .unwrap_or_else(|| "?".to_string()),
            arguments: prop_value(fields, "arguments")
                .cloned()
                .unwrap_or_else(|| Value::Object(Map::new())),
            timestamp: timestamp_field(fields, "ts"),
        }),
        "agent.tool.completed" => Some(ProgressEvent::ToolCallCompleted {
            stage_node_id: string_field(fields, "node_id").unwrap_or_else(|| "?".to_string()),
            tool_call_id: prop_string_field(fields, "tool_call_id")
                .unwrap_or_else(|| "?".to_string()),
            is_error: prop_bool_field(fields, "is_error"),
            duration_ms: prop_optional_u64_field(fields, "duration_ms"),
            timestamp: timestamp_field(fields, "ts"),
        }),
        "agent.warning"
            if prop_string_field(fields, "kind").as_deref() == Some("context_window") =>
        {
            let usage_percent = prop_value(fields, "details")
                .and_then(Value::as_object)
                .and_then(|details| details.get("usage_percent"))
                .and_then(Value::as_u64)
                .unwrap_or(0);
            Some(ProgressEvent::ContextWindowWarning {
                stage_node_id: string_field(fields, "node_id").unwrap_or_else(|| "?".to_string()),
                usage_percent,
            })
        }
        "agent.compaction.started" => Some(ProgressEvent::CompactionStarted {
            stage_node_id: string_field(fields, "node_id").unwrap_or_else(|| "?".to_string()),
        }),
        "agent.compaction.completed" => Some(ProgressEvent::CompactionCompleted {
            stage_node_id: string_field(fields, "node_id").unwrap_or_else(|| "?".to_string()),
            original_turn_count: prop_u64_field(fields, "original_turn_count"),
            preserved_turn_count: prop_u64_field(fields, "preserved_turn_count"),
            tracked_file_count: prop_u64_field(fields, "tracked_file_count"),
        }),
        "agent.llm.retry" => {
            let delay_secs = prop_f64_field(fields, "delay_secs").unwrap_or(0.0);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let delay_ms = (delay_secs * 1000.0) as u64;
            Some(ProgressEvent::LlmRetry {
                stage_node_id: string_field(fields, "node_id").unwrap_or_else(|| "?".to_string()),
                model: prop_string_field(fields, "model").unwrap_or_else(|| "?".to_string()),
                attempt: prop_u64_field(fields, "attempt"),
                delay_ms,
                error: prop_display_field(fields, "error")
                    .unwrap_or_else(|| "unknown error".to_string()),
            })
        }
        "agent.sub.spawned" => Some(ProgressEvent::SubagentSpawned {
            stage_node_id: string_field(fields, "node_id").unwrap_or_else(|| "?".to_string()),
            agent_id: prop_string_field(fields, "agent_id").unwrap_or_else(|| "?".to_string()),
            task: prop_string_field(fields, "task").unwrap_or_default(),
        }),
        "agent.sub.completed" => Some(ProgressEvent::SubagentCompleted {
            stage_node_id: string_field(fields, "node_id").unwrap_or_else(|| "?".to_string()),
            agent_id: prop_string_field(fields, "agent_id").unwrap_or_else(|| "?".to_string()),
            success: prop_bool_field(fields, "success"),
            turns_used: prop_u64_field(fields, "turns_used"),
        }),
        "edge.selected" => Some(ProgressEvent::EdgeSelected {
            from_node: prop_string_field(fields, "from_node").unwrap_or_else(|| "?".to_string()),
            to_node: prop_string_field(fields, "to_node").unwrap_or_else(|| "?".to_string()),
            label: prop_string_field(fields, "label"),
            condition: prop_string_field(fields, "condition"),
        }),
        "loop.restart" => Some(ProgressEvent::LoopRestart {
            from_node: prop_string_field(fields, "from_node").unwrap_or_else(|| "?".to_string()),
            to_node: prop_string_field(fields, "to_node").unwrap_or_else(|| "?".to_string()),
        }),
        "retro.started" => Some(ProgressEvent::RetroStarted),
        "retro.completed" => Some(ProgressEvent::RetroCompleted {
            duration_ms: prop_u64_field(fields, "duration_ms"),
        }),
        "retro.failed" => Some(ProgressEvent::RetroFailed {
            duration_ms: prop_u64_field(fields, "duration_ms"),
        }),
        "run.notice" => Some(ProgressEvent::RunNotice {
            level: parse_run_notice_level(prop_string_field(fields, "level").as_deref()),
            code: prop_string_field(fields, "code").unwrap_or_default(),
            message: prop_string_field(fields, "message").unwrap_or_default(),
        }),
        "pull_request.created" => Some(ProgressEvent::PullRequestCreated {
            pr_url: prop_string_field(fields, "pr_url").unwrap_or_else(|| "?".to_string()),
            draft: prop_bool_field(fields, "draft"),
        }),
        "pull_request.failed" => Some(ProgressEvent::PullRequestFailed {
            error: prop_display_field(fields, "error")
                .unwrap_or_else(|| "unknown error".to_string()),
        }),
        _ => None,
    }
}

fn parse_run_notice_level(level: Option<&str>) -> RunNoticeLevel {
    match level.unwrap_or("info") {
        "warn" => RunNoticeLevel::Warn,
        "error" => RunNoticeLevel::Error,
        _ => RunNoticeLevel::Info,
    }
}

fn string_field(fields: &Map<String, Value>, key: &str) -> Option<String> {
    fields.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn prop_value<'a>(fields: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    fields
        .get("properties")
        .and_then(Value::as_object)
        .and_then(|properties| properties.get(key))
}

fn prop_string_field(fields: &Map<String, Value>, key: &str) -> Option<String> {
    prop_value(fields, key)
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn prop_display_field(fields: &Map<String, Value>, key: &str) -> Option<String> {
    let value = prop_value(fields, key)?;
    match value {
        Value::Null => None,
        Value::String(value) => Some(value.clone()),
        Value::Object(map) => map
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| {
                map.get("detail")
                    .and_then(Value::as_object)
                    .and_then(|detail| detail.get("message"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .or_else(|| {
                map.get("data")
                    .and_then(Value::as_object)
                    .and_then(|detail| detail.get("message"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .or_else(|| map.get("data").and_then(Value::as_str).map(str::to_owned))
            .or_else(|| Some(value.to_string())),
        _ => Some(value.to_string()),
    }
}

fn u64_field(fields: &Map<String, Value>, key: &str) -> u64 {
    fields.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn prop_u64_field(fields: &Map<String, Value>, key: &str) -> u64 {
    prop_value(fields, key).and_then(Value::as_u64).unwrap_or(0)
}

fn prop_optional_u64_field(fields: &Map<String, Value>, key: &str) -> Option<u64> {
    prop_value(fields, key).and_then(Value::as_u64)
}

fn prop_i64_field(fields: &Map<String, Value>, key: &str) -> i64 {
    prop_value(fields, key).and_then(Value::as_i64).unwrap_or(0)
}

fn f64_field(fields: &Map<String, Value>, key: &str) -> Option<f64> {
    fields.get(key).and_then(Value::as_f64)
}

fn prop_f64_field(fields: &Map<String, Value>, key: &str) -> Option<f64> {
    prop_value(fields, key).and_then(Value::as_f64)
}

fn prop_bool_field(fields: &Map<String, Value>, key: &str) -> bool {
    prop_value(fields, key)
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn timestamp_field(fields: &Map<String, Value>, key: &str) -> Option<DateTime<Utc>> {
    let value = fields.get(key)?.as_str()?;
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use fabro_agent::AgentEvent;
    use fabro_types::fixtures;
    use fabro_workflow::event::{WorkflowRunEvent, canonicalize_event};

    use super::*;

    fn json_map(value: Value) -> Map<String, Value> {
        value.as_object().cloned().expect("json object")
    }

    fn canonical_fields(event: &WorkflowRunEvent) -> (String, Map<String, Value>) {
        let envelope = canonicalize_event(&fixtures::RUN_1, event);
        let event_name = envelope.event.clone();
        let fields = json_map(serde_json::to_value(envelope).expect("serializable envelope"));
        (event_name, fields)
    }

    #[test]
    fn parse_edge_selected() {
        let fields = json_map(serde_json::json!({
            "properties": {
                "from_node": "a",
                "to_node": "b",
                "label": "yes"
            }
        }));

        let event = from_envelope_fields("edge.selected", &fields).unwrap();
        assert!(matches!(
            event,
            ProgressEvent::EdgeSelected {
                from_node,
                to_node,
                label,
                ..
            } if from_node == "a" && to_node == "b" && label.as_deref() == Some("yes")
        ));
    }

    #[test]
    fn round_trip_stage_completed() {
        let event = WorkflowRunEvent::StageCompleted {
            node_id: "plan".into(),
            name: "Plan".into(),
            index: 0,
            duration_ms: 5000,
            status: "success".into(),
            preferred_label: None,
            suggested_next_ids: Vec::new(),
            usage: None,
            failure: None,
            notes: None,
            files_touched: Vec::new(),
            context_updates: None,
            jump_to_node: None,
            context_values: None,
            node_visits: None,
            loop_failure_signatures: None,
            restart_failure_signatures: None,
            attempt: 1,
            max_attempts: 1,
        };

        let (name, fields) = canonical_fields(&event);
        let parsed = from_envelope_fields(&name, &fields).unwrap();
        assert!(matches!(
            parsed,
            ProgressEvent::StageCompleted {
                node_id,
                name,
                duration_ms,
                ..
            } if node_id == "plan" && name == "Plan" && duration_ms == 5000
        ));
    }

    #[test]
    fn round_trip_agent_tool_call() {
        let event = WorkflowRunEvent::Agent {
            stage: "code".into(),
            event: AgentEvent::ToolCallStarted {
                tool_name: "read_file".into(),
                tool_call_id: "tc1".into(),
                arguments: serde_json::json!({"path": "src/main.rs"}),
            },
            session_id: None,
            parent_session_id: None,
        };

        let (name, fields) = canonical_fields(&event);
        let parsed = from_envelope_fields(&name, &fields).unwrap();
        assert!(matches!(
            parsed,
            ProgressEvent::ToolCallStarted {
                stage_node_id,
                tool_name,
                tool_call_id,
                ..
            } if stage_node_id == "code" && tool_name == "read_file" && tool_call_id == "tc1"
        ));
    }

    #[test]
    fn parse_tool_call_timestamps_from_jsonl_envelope() {
        let started_fields = json_map(serde_json::json!({
            "ts": "2026-03-30T12:00:00.000Z",
            "node_id": "code",
            "properties": {
                "tool_name": "read_file",
                "tool_call_id": "tc1",
                "arguments": {"path": "src/main.rs"}
            }
        }));
        let completed_fields = json_map(serde_json::json!({
            "ts": "2026-03-30T12:00:00.500Z",
            "node_id": "code",
            "properties": {
                "tool_call_id": "tc1",
                "is_error": false,
                "duration_ms": 500
            }
        }));

        let started = from_envelope_fields("agent.tool.started", &started_fields).unwrap();
        let completed = from_envelope_fields("agent.tool.completed", &completed_fields).unwrap();

        assert!(matches!(
            started,
            ProgressEvent::ToolCallStarted {
                timestamp: Some(timestamp),
                ..
            } if timestamp == DateTime::parse_from_rfc3339("2026-03-30T12:00:00.000Z")
                .unwrap()
                .with_timezone(&Utc)
        ));
        assert!(matches!(
            completed,
            ProgressEvent::ToolCallCompleted {
                duration_ms: Some(500),
                timestamp: Some(timestamp),
                ..
            } if timestamp == DateTime::parse_from_rfc3339("2026-03-30T12:00:00.500Z")
                .unwrap()
                .with_timezone(&Utc)
        ));
    }

    #[test]
    fn round_trip_sandbox_ready() {
        let event = WorkflowRunEvent::Sandbox {
            event: fabro_agent::SandboxEvent::Ready {
                provider: "daytona".into(),
                duration_ms: 2500,
                name: Some("sandbox-1".into()),
                cpu: Some(4.0),
                memory: Some(8.0),
                url: Some("https://example.test".into()),
            },
        };

        let (name, fields) = canonical_fields(&event);
        let parsed = from_envelope_fields(&name, &fields).unwrap();
        assert!(matches!(
            parsed,
            ProgressEvent::SandboxReady {
                provider,
                duration_ms,
                name,
                ..
            } if provider == "daytona" && duration_ms == 2500 && name.as_deref() == Some("sandbox-1")
        ));
    }

    #[test]
    fn round_trip_run_notice() {
        let event = WorkflowRunEvent::RunNotice {
            level: RunNoticeLevel::Warn,
            code: "sandbox_cleanup_failed".into(),
            message: "sandbox cleanup failed".into(),
        };

        let (name, fields) = canonical_fields(&event);
        let parsed = from_envelope_fields(&name, &fields).unwrap();
        assert!(matches!(
            parsed,
            ProgressEvent::RunNotice {
                level: RunNoticeLevel::Warn,
                code,
                message,
            } if code == "sandbox_cleanup_failed" && message == "sandbox cleanup failed"
        ));
    }
}
