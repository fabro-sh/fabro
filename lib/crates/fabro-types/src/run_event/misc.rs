use serde::{Deserialize, Serialize, de};
use serde_json::Value;

use super::ExecOutputTail;
use crate::CommandTermination;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterviewOption {
    pub key:   String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParallelStartedProps {
    pub visit:        u32,
    pub branch_count: usize,
    pub join_policy:  String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParallelBranchStartedProps {
    pub index: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParallelBranchCompletedProps {
    pub index:       usize,
    pub duration_ms: u64,
    pub status:      String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha:    Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParallelCompletedProps {
    pub visit:         u32,
    pub duration_ms:   u64,
    pub success_count: usize,
    pub failure_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub results:       Vec<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InterviewStartedProps {
    #[serde(default)]
    pub question_id:     String,
    pub question:        String,
    #[serde(default)]
    pub stage:           String,
    pub question_type:   String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options:         Vec<InterviewOption>,
    #[serde(default)]
    pub allow_freeform:  bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_display: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InterviewCompletedProps {
    #[serde(default)]
    pub question_id: String,
    pub question:    String,
    pub answer:      String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InterviewTimeoutProps {
    #[serde(default)]
    pub question_id: String,
    pub question:    String,
    #[serde(default)]
    pub stage:       String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InterviewInterruptedProps {
    #[serde(default)]
    pub question_id: String,
    pub question:    String,
    #[serde(default)]
    pub stage:       String,
    pub reason:      String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitCommitProps {
    pub sha: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitPushProps {
    pub branch:           String,
    pub success:          bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec_output_tail: Option<ExecOutputTail>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitBranchProps {
    pub branch: String,
    pub sha:    String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitWorktreeAddProps {
    pub path:   String,
    pub branch: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitWorktreeRemoveProps {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitFetchProps {
    pub branch:  String,
    pub success: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitResetProps {
    pub sha: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EdgeSelectedProps {
    pub from_node:          String,
    pub to_node:            String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label:              Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition:          Option<String>,
    pub reason:             String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_label:    Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggested_next_ids: Vec<String>,
    pub stage_status:       String,
    pub is_jump:            bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoopRestartProps {
    pub from_node: String,
    pub to_node:   String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubgraphStartedProps {
    pub start_node: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubgraphCompletedProps {
    pub steps_executed: usize,
    pub status:         String,
    pub duration_ms:    u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StallWatchdogTimeoutProps {
    pub idle_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtifactCapturedProps {
    pub attempt:        u32,
    pub node_slug:      String,
    pub path:           String,
    pub mime:           String,
    pub content_md5:    String,
    pub content_sha256: String,
    pub bytes:          u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SshAccessReadyProps {
    pub ssh_command: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FailoverProps {
    pub from_provider: String,
    pub from_model:    String,
    pub to_provider:   String,
    pub to_model:      String,
    pub error:         String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandStartedProps {
    pub script:     String,
    pub command:    String,
    pub language:   String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CommandCompletedProps {
    pub output:         String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code:      Option<i32>,
    pub duration_ms:    u64,
    pub termination:    CommandTermination,
    #[serde(default)]
    pub output_bytes:   u64,
    #[serde(default)]
    pub live_streaming: bool,
}

impl<'de> Deserialize<'de> for CommandCompletedProps {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            #[serde(default)]
            output:         Option<String>,
            #[serde(default)]
            stdout:         Option<String>,
            #[serde(default)]
            stderr:         Option<String>,
            #[serde(default)]
            exit_code:      Option<i32>,
            duration_ms:    u64,
            termination:    CommandTermination,
            #[serde(default)]
            output_bytes:   Option<u64>,
            #[serde(default)]
            stdout_bytes:   Option<u64>,
            #[serde(default)]
            stderr_bytes:   Option<u64>,
            #[serde(default)]
            live_streaming: bool,
        }

        let wire = Wire::deserialize(deserializer)?;
        let (output, output_bytes) = if let Some(output) = wire.output {
            (output, wire.output_bytes.unwrap_or(0))
        } else {
            let stdout_bytes = wire.stdout_bytes.unwrap_or(0);
            let stderr_bytes = wire.stderr_bytes.unwrap_or(0);
            let legacy_output = if stdout_bytes == 0 && stderr_bytes > 0 && wire.stderr.is_some() {
                wire.stderr
            } else {
                wire.stdout.or(wire.stderr)
            }
            .ok_or_else(|| de::Error::missing_field("output"))?;
            let legacy_bytes = if stdout_bytes == 0 && stderr_bytes > 0 {
                stderr_bytes
            } else {
                stdout_bytes
            };
            (legacy_output, legacy_bytes)
        };

        Ok(Self {
            output,
            exit_code: wire.exit_code,
            duration_ms: wire.duration_ms,
            termination: wire.termination,
            output_bytes,
            live_streaming: wire.live_streaming,
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn command_completed_deserializes_legacy_stdout_stderr_shape() {
        let props: CommandCompletedProps = serde_json::from_value(json!({
            "stdout": "blob://sha256/stdout",
            "stderr": "blob://sha256/stderr",
            "exit_code": 1,
            "duration_ms": 42,
            "termination": "exited",
            "stdout_bytes": 0,
            "stderr_bytes": 12,
            "streams_separated": true,
            "live_streaming": true
        }))
        .unwrap();

        assert_eq!(props.output, "blob://sha256/stderr");
        assert_eq!(props.output_bytes, 12);
        assert_eq!(props.exit_code, Some(1));
        assert_eq!(props.termination, CommandTermination::Exited);
        assert!(props.live_streaming);
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentCliStartedProps {
    pub visit:    u32,
    pub mode:     String,
    pub provider: String,
    pub model:    String,
    pub command:  String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentCliCompletedProps {
    pub stdout:      String,
    pub stderr:      String,
    pub exit_code:   i32,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentCliCancelledProps {
    pub stdout:      String,
    pub stderr:      String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentCliTimedOutProps {
    pub stdout:      String,
    pub stderr:      String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentAcpStartedProps {
    pub visit:    u32,
    pub mode:     String,
    pub provider: String,
    pub model:    String,
    pub command:  String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentAcpCompletedProps {
    pub stdout:      String,
    pub stderr:      String,
    pub stop_reason: String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentAcpCancelledProps {
    pub stdout:      String,
    pub stderr:      String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentAcpTimedOutProps {
    pub stdout:      String,
    pub stderr:      String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PullRequestCreatedProps {
    pub pr_url:      String,
    pub pr_number:   u64,
    pub owner:       String,
    pub repo:        String,
    pub base_branch: String,
    pub head_branch: String,
    pub title:       String,
    pub draft:       bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PullRequestFailedProps {
    pub error: String,
}
