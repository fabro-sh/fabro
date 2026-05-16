use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{BilledTokenCounts, ExecOutputTail, RunNoticeLevel};
use crate::status::{BlockedReason, SuccessReason};
use crate::{
    DiffSummary, ForkSourceRef, GitContext, Graph, RunBlobId, RunControlAction, RunFailure, RunId,
    RunProvenance, WorkflowSettings,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunCreatedProps {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title:            Option<String>,
    pub settings:         WorkflowSettings,
    pub graph:            Graph,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_source:  Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_config:  Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels:           BTreeMap<String, String>,
    pub run_dir:          String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_directory: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_slug:    Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub db_prefix:        Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance:       Option<RunProvenance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_blob:    Option<RunBlobId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git:              Option<GitContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork_source_ref:  Option<ForkSourceRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id:        Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_url:          Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunParentLinkedProps {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_parent_id: Option<RunId>,
    pub parent_id:          RunId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunParentUnlinkedProps {
    pub previous_parent_id: RunId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunStartedProps {
    pub name:         String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_branch:  Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_sha:     Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_branch:   Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal:         Option<String>,
}

#[allow(
    clippy::empty_structs_with_brackets,
    reason = "This type must serialize as {} rather than null."
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct RunStatusTransitionProps {}

#[allow(
    clippy::empty_structs_with_brackets,
    reason = "This type must serialize as {} rather than null."
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct RunStatusEffectProps {}

#[allow(
    clippy::empty_structs_with_brackets,
    reason = "This type must serialize as {} rather than null."
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct RunInterruptProps {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSteerProps {
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSubmittedProps {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition_blob: Option<RunBlobId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunControlRequestedProps {
    pub action: RunControlAction,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunBlockedProps {
    pub blocked_reason: BlockedReason,
}

#[allow(
    clippy::empty_structs_with_brackets,
    reason = "This type must serialize as {} rather than null."
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct RunControlEffectProps {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSupersededByProps {
    pub new_run_id:                RunId,
    pub target_checkpoint_ordinal: usize,
    pub target_node_id:            String,
    pub target_visit:              usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunTitleUpdatedProps {
    pub title: String,
}

#[allow(
    clippy::empty_structs_with_brackets,
    reason = "This type must serialize as {} rather than null."
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct RunArchivedProps {}

#[allow(
    clippy::empty_structs_with_brackets,
    reason = "This type must serialize as {} rather than null."
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct RunUnarchivedProps {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunCompletedProps {
    pub duration_ms:          u64,
    pub artifact_count:       usize,
    pub status:               String,
    pub reason:               SuccessReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_usd_micros:     Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_git_commit_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_patch:          Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_summary:         Option<DiffSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billing:              Option<BilledTokenCounts>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunFailedProps {
    pub failure:              RunFailure,
    pub duration_ms:          u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_git_commit_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_patch:          Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_summary:         Option<DiffSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billing:              Option<BilledTokenCounts>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunNoticeProps {
    pub level:            RunNoticeLevel,
    pub code:             String,
    pub message:          String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec_output_tail: Option<ExecOutputTail>,
}
