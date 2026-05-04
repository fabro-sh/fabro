use serde::{Deserialize, Serialize};
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandCompletedProps {
    pub stdout:            String,
    pub stderr:            String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code:         Option<i32>,
    pub duration_ms:       u64,
    pub termination:       CommandTermination,
    #[serde(default)]
    pub stdout_bytes:      u64,
    #[serde(default)]
    pub stderr_bytes:      u64,
    #[serde(default)]
    pub streams_separated: bool,
    #[serde(default)]
    pub live_streaming:    bool,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetroStartedProps {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt:   Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model:    Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetroCompletedProps {
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response:    Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retro:       Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetroFailedProps {
    pub error:            String,
    pub duration_ms:      u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec_output_tail: Option<ExecOutputTail>,
}
