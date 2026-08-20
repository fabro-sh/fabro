use fabro_model::ReasoningEffort;
use serde::{Deserialize, Serialize};

use super::ExecOutputTail;
use crate::{
    CommandTermination, ParallelBranchResult, PullRequestCreationId, PullRequestLink, ReviewTarget,
    StageId, StageOutcome,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct InterviewOption {
    pub key:         String,
    pub label:       String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview:     Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParallelStartedProps {
    pub visit:        u32,
    pub branch_count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParallelBranchStartedProps {
    pub index:                 usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_label:            Option<String>,
    /// Graph visit of the branch target for this dispatch. The envelope
    /// `stage_id` ordinal counts executions, so a resumed fan-out's branches
    /// keep visit metadata even though their ordinals advanced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_visit:           Option<u32>,
    /// Prior branch execution superseded by this resumed replay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resumed_from_stage_id: Option<StageId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParallelBranchCompletedProps {
    pub index:       usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_label:  Option<String>,
    pub duration_ms: u64,
    pub status:      StageOutcome,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParallelCompletedProps {
    pub visit:         u32,
    pub duration_ms:   u64,
    pub success_count: usize,
    pub failure_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub results:       Vec<ParallelBranchResult>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_target:   Option<ReviewTarget>,
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
    /// `original_*` and `attempt` are `Option` only because failover events
    /// recorded before model-keyed fallbacks lack them. New events always set
    /// them; stored events are immutable, so absence stays a supported input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    pub from_provider: String,
    pub from_model: String,
    pub to_provider: String,
    pub to_model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_reasoning_effort: Option<ReasoningEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_reasoning_effort: Option<ReasoningEffort>,
    pub error: String,
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
#[serde(deny_unknown_fields)]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentAcpStartedProps {
    pub visit:       u32,
    pub command:     String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_name: Option<String>,
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
pub struct PullRequestCreationRequestedProps {
    pub creation_id: PullRequestCreationId,
    pub model:       String,
    pub force:       bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PullRequestCreatedProps {
    pub pr_url:      String,
    pub pr_number:   u64,
    pub owner:       String,
    pub repo:        String,
    pub base_branch: String,
    pub head_branch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha:    Option<String>,
    pub title:       String,
    pub draft:       bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestLinkedProps {
    pub pull_request: PullRequestLink,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestUnlinkedProps {
    pub pull_request: PullRequestLink,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PullRequestFailedProps {
    /// Set when the failure resolves an explicitly requested creation; absent
    /// for pull request failures in the workflow publish stage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creation_id: Option<PullRequestCreationId>,
    pub error:       String,
}
