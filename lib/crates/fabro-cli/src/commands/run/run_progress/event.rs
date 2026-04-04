use std::convert::TryFrom;

use chrono::{DateTime, Utc};
use fabro_types::{EventBody, StageUsage, StoredEvent};
use fabro_workflow::event::RunNoticeLevel;
use fabro_workflow::outcome::compute_stage_cost;
use serde_json::Value;

#[derive(Debug, Clone)]
pub(super) struct ProgressUsage {
    pub(super) model: Option<String>,
    pub(super) input_tokens: u64,
    pub(super) output_tokens: u64,
    pub(super) speed: Option<String>,
    pub(super) cost: Option<f64>,
}

impl ProgressUsage {
    pub(super) fn from_stage_usage(usage: &StageUsage) -> Option<Self> {
        Some(Self {
            model: Some(usage.model.clone()),
            input_tokens: u64::try_from(usage.input_tokens).ok()?,
            output_tokens: u64::try_from(usage.output_tokens).ok()?,
            speed: usage.speed.clone(),
            cost: usage.cost,
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

pub(super) fn from_stored_event(stored: &StoredEvent) -> Option<ProgressEvent> {
    let node_id = stored.node_id.clone().unwrap_or_else(|| "?".to_string());
    let node_label = stored.node_label.clone().unwrap_or_else(|| node_id.clone());

    match &stored.body {
        EventBody::RunStarted(props) => Some(ProgressEvent::WorkflowStarted {
            worktree_dir: props.worktree_dir.clone(),
            base_branch: props.base_branch.clone(),
            base_sha: props.base_sha.clone(),
        }),
        EventBody::SandboxInitialized(props) => Some(ProgressEvent::WorkingDirectorySet {
            working_directory: props.working_directory.clone(),
        }),
        EventBody::SandboxInitializing(props) => Some(ProgressEvent::SandboxInitializing {
            provider: props.provider.clone(),
        }),
        EventBody::SandboxReady(props) => Some(ProgressEvent::SandboxReady {
            provider: props.provider.clone(),
            duration_ms: props.duration_ms,
            name: props.name.clone(),
            cpu: props.cpu,
            memory: props.memory,
            url: props.url.clone(),
        }),
        EventBody::SshAccessReady(props) => Some(ProgressEvent::SshAccessReady {
            ssh_command: props.ssh_command.clone(),
        }),
        EventBody::SetupStarted(props) => Some(ProgressEvent::SetupStarted {
            command_count: props.command_count as u64,
        }),
        EventBody::SetupCompleted(props) => Some(ProgressEvent::SetupCompleted {
            duration_ms: props.duration_ms,
        }),
        EventBody::SetupCommandCompleted(props) => Some(ProgressEvent::SetupCommandCompleted {
            command: props.command.clone(),
            command_index: props.index as u64,
            exit_code: i64::from(props.exit_code),
            duration_ms: props.duration_ms,
        }),
        EventBody::CliEnsureStarted(props) => Some(ProgressEvent::CliEnsureStarted {
            cli_name: props.cli_name.clone(),
        }),
        EventBody::CliEnsureCompleted(props) => Some(ProgressEvent::CliEnsureCompleted {
            cli_name: props.cli_name.clone(),
            already_installed: props.already_installed,
            duration_ms: props.duration_ms,
        }),
        EventBody::CliEnsureFailed(props) => Some(ProgressEvent::CliEnsureFailed {
            cli_name: props.cli_name.clone(),
        }),
        EventBody::DevcontainerResolved(props) => Some(ProgressEvent::DevcontainerResolved {
            dockerfile_lines: props.dockerfile_lines as u64,
            environment_count: props.environment_count as u64,
            lifecycle_command_count: props.lifecycle_command_count as u64,
            workspace_folder: props.workspace_folder.clone(),
        }),
        EventBody::DevcontainerLifecycleStarted(props) => {
            Some(ProgressEvent::DevcontainerLifecycleStarted {
                phase: props.phase.clone(),
                command_count: props.command_count as u64,
            })
        }
        EventBody::DevcontainerLifecycleCompleted(props) => {
            Some(ProgressEvent::DevcontainerLifecycleCompleted {
                phase: props.phase.clone(),
                duration_ms: props.duration_ms,
            })
        }
        EventBody::DevcontainerLifecycleFailed(props) => {
            Some(ProgressEvent::DevcontainerLifecycleFailed {
                phase: props.phase.clone(),
                command: props.command.clone(),
                exit_code: i64::from(props.exit_code),
                stderr: props.stderr.clone(),
            })
        }
        EventBody::DevcontainerLifecycleCommandCompleted(props) => {
            Some(ProgressEvent::DevcontainerLifecycleCommandCompleted {
                command: props.command.clone(),
                command_index: props.index as u64,
                exit_code: i64::from(props.exit_code),
                duration_ms: props.duration_ms,
            })
        }
        EventBody::StageStarted(_) => Some(ProgressEvent::StageStarted {
            node_id,
            name: node_label,
            script: None,
        }),
        EventBody::StageCompleted(props) => Some(ProgressEvent::StageCompleted {
            node_id,
            name: node_label,
            duration_ms: props.duration_ms,
            status: props.status.to_string(),
            usage: props
                .usage
                .as_ref()
                .and_then(ProgressUsage::from_stage_usage),
        }),
        EventBody::StageFailed(props) => Some(ProgressEvent::StageFailed {
            node_id,
            name: node_label,
            error: props
                .failure
                .as_ref()
                .map(|failure| failure.message.clone())
                .unwrap_or_else(|| "unknown error".to_string()),
        }),
        EventBody::StageRetrying(props) => Some(ProgressEvent::StageRetrying {
            name: node_label,
            attempt: props.attempt as u64,
            max_attempts: props.max_attempts as u64,
            delay_ms: props.delay_ms,
        }),
        EventBody::ParallelStarted(_) => Some(ProgressEvent::ParallelStarted),
        EventBody::ParallelBranchStarted(_) => {
            Some(ProgressEvent::ParallelBranchStarted { branch: node_id })
        }
        EventBody::ParallelBranchCompleted(props) => Some(ProgressEvent::ParallelBranchCompleted {
            branch: node_id,
            duration_ms: props.duration_ms,
            status: props.status.clone(),
        }),
        EventBody::ParallelCompleted(_) => Some(ProgressEvent::ParallelCompleted),
        EventBody::AgentMessage(props) => Some(ProgressEvent::AssistantMessage {
            stage_node_id: node_id,
            model: props.model.clone(),
        }),
        EventBody::AgentToolStarted(props) => Some(ProgressEvent::ToolCallStarted {
            stage_node_id: node_id,
            tool_name: props.tool_name.clone(),
            tool_call_id: props.tool_call_id.clone(),
            arguments: props.arguments.clone(),
            timestamp: Some(stored.ts),
        }),
        EventBody::AgentToolCompleted(props) => Some(ProgressEvent::ToolCallCompleted {
            stage_node_id: node_id,
            tool_call_id: props.tool_call_id.clone(),
            is_error: props.is_error,
            duration_ms: None,
            timestamp: Some(stored.ts),
        }),
        EventBody::AgentWarning(props) if props.kind == "context_window" => {
            let usage_percent = props
                .details
                .as_object()
                .and_then(|details| details.get("usage_percent"))
                .and_then(Value::as_u64)
                .unwrap_or(0);
            Some(ProgressEvent::ContextWindowWarning {
                stage_node_id: node_id,
                usage_percent,
            })
        }
        EventBody::AgentCompactionStarted(_) => Some(ProgressEvent::CompactionStarted {
            stage_node_id: node_id,
        }),
        EventBody::AgentCompactionCompleted(props) => Some(ProgressEvent::CompactionCompleted {
            stage_node_id: node_id,
            original_turn_count: props.original_turn_count as u64,
            preserved_turn_count: props.preserved_turn_count as u64,
            tracked_file_count: props.tracked_file_count as u64,
        }),
        EventBody::AgentLlmRetry(props) => {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let delay_ms = (props.delay_secs * 1000.0) as u64;
            Some(ProgressEvent::LlmRetry {
                stage_node_id: node_id,
                model: props.model.clone(),
                attempt: props.attempt as u64,
                delay_ms,
                error: display_value(&props.error).unwrap_or_else(|| "unknown error".to_string()),
            })
        }
        EventBody::AgentSubSpawned(props) => Some(ProgressEvent::SubagentSpawned {
            stage_node_id: node_id,
            agent_id: props.agent_id.clone(),
            task: props.task.clone(),
        }),
        EventBody::AgentSubCompleted(props) => Some(ProgressEvent::SubagentCompleted {
            stage_node_id: node_id,
            agent_id: props.agent_id.clone(),
            success: props.success,
            turns_used: props.turns_used as u64,
        }),
        EventBody::EdgeSelected(props) => Some(ProgressEvent::EdgeSelected {
            from_node: props.from_node.clone(),
            to_node: props.to_node.clone(),
            label: props.label.clone(),
            condition: props.condition.clone(),
        }),
        EventBody::LoopRestart(props) => Some(ProgressEvent::LoopRestart {
            from_node: props.from_node.clone(),
            to_node: props.to_node.clone(),
        }),
        EventBody::RetroStarted(_) => Some(ProgressEvent::RetroStarted),
        EventBody::RetroCompleted(props) => Some(ProgressEvent::RetroCompleted {
            duration_ms: props.duration_ms,
        }),
        EventBody::RetroFailed(props) => Some(ProgressEvent::RetroFailed {
            duration_ms: props.duration_ms,
        }),
        EventBody::RunNotice(props) => Some(ProgressEvent::RunNotice {
            level: match props.level {
                fabro_types::RunNoticeLevel::Info => RunNoticeLevel::Info,
                fabro_types::RunNoticeLevel::Warn => RunNoticeLevel::Warn,
                fabro_types::RunNoticeLevel::Error => RunNoticeLevel::Error,
            },
            code: props.code.clone(),
            message: props.message.clone(),
        }),
        EventBody::PullRequestCreated(props) => Some(ProgressEvent::PullRequestCreated {
            pr_url: props.pr_url.clone(),
            draft: props.draft,
        }),
        EventBody::PullRequestFailed(props) => Some(ProgressEvent::PullRequestFailed {
            error: props.error.clone(),
        }),
        _ => None,
    }
}

pub(super) fn from_json_line(line: &str) -> Option<ProgressEvent> {
    let stored = StoredEvent::from_json_str(line).ok()?;
    from_stored_event(&stored)
}

fn display_value(value: &Value) -> Option<String> {
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

#[cfg(test)]
mod tests {
    use fabro_agent::AgentEvent;
    use fabro_types::fixtures;
    use fabro_workflow::event::{WorkflowRunEvent, to_stored_event};

    use super::*;

    #[test]
    fn parse_edge_selected() {
        let stored = to_stored_event(
            &fixtures::RUN_1,
            &WorkflowRunEvent::EdgeSelected {
                from_node: "a".into(),
                to_node: "b".into(),
                label: Some("yes".into()),
                condition: None,
                reason: "condition".into(),
                preferred_label: None,
                suggested_next_ids: Vec::new(),
                stage_status: "success".into(),
                is_jump: false,
            },
        );

        let event = from_stored_event(&stored).unwrap();
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
            response: None,
            attempt: 1,
            max_attempts: 1,
        };

        let stored = to_stored_event(&fixtures::RUN_1, &event);
        let parsed = from_stored_event(&stored).unwrap();
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
            visit: 1,
            event: AgentEvent::ToolCallStarted {
                tool_name: "read_file".into(),
                tool_call_id: "tc1".into(),
                arguments: serde_json::json!({"path": "src/main.rs"}),
            },
            session_id: None,
            parent_session_id: None,
        };

        let stored = to_stored_event(&fixtures::RUN_1, &event);
        let parsed = from_stored_event(&stored).unwrap();
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
    fn parse_tool_call_timestamps_from_jsonl() {
        let started = from_json_line(
            &serde_json::json!({
                "id": "evt_1",
                "ts": "2026-03-30T12:00:00.000Z",
                "run_id": fixtures::RUN_1.to_string(),
                "event": "agent.tool.started",
                "node_id": "code",
                "node_label": "code",
                "properties": {
                    "tool_name": "read_file",
                    "tool_call_id": "tc1",
                    "arguments": {"path": "src/main.rs"},
                    "visit": 1
                }
            })
            .to_string(),
        )
        .unwrap();
        let completed = from_json_line(
            &serde_json::json!({
                "id": "evt_2",
                "ts": "2026-03-30T12:00:00.500Z",
                "run_id": fixtures::RUN_1.to_string(),
                "event": "agent.tool.completed",
                "node_id": "code",
                "node_label": "code",
                "properties": {
                    "tool_name": "read_file",
                    "tool_call_id": "tc1",
                    "output": {"ok": true},
                    "is_error": false,
                    "visit": 1
                }
            })
            .to_string(),
        )
        .unwrap();

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
                duration_ms: None,
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

        let stored = to_stored_event(&fixtures::RUN_1, &event);
        let parsed = from_stored_event(&stored).unwrap();
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

        let stored = to_stored_event(&fixtures::RUN_1, &event);
        let parsed = from_stored_event(&stored).unwrap();
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
