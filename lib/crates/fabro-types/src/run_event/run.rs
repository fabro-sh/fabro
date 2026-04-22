use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{ActorRef, BilledTokenCounts, RunNoticeLevel};
use crate::settings::SettingsLayer;
use crate::status::{BlockedReason, FailureReason, SuccessReason};
use crate::{Graph, RunBlobId, RunControlAction, RunProvenance};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunCreatedProps {
    pub settings:          SettingsLayer,
    pub graph:             Graph,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_source:   Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_config:   Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels:            BTreeMap<String, String>,
    pub run_dir:           String,
    pub working_directory: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_repo_path:    Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_origin_url:   Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_branch:       Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_slug:     Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub db_prefix:         Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance:        Option<RunProvenance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_blob:     Option<RunBlobId>,
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[derive(Default)]
pub struct RunStatusTransitionProps {}

#[allow(
    clippy::empty_structs_with_brackets,
    reason = "This type must serialize as {} rather than null."
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct RunStatusEffectProps {}

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
pub struct RunRewoundProps {
    pub target_checkpoint_ordinal: usize,
    pub target_node_id:            String,
    pub target_visit:              usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_status:           Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_commit_sha:            Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunArchivedProps {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<ActorRef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunUnarchivedProps {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<ActorRef>,
}

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
    pub billing:              Option<BilledTokenCounts>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunFailedProps {
    pub error:          String,
    pub duration_ms:    u64,
    pub reason:         FailureReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_commit_sha: Option<String>,
    // Optional unified-patch text captured at run end. Additive for back-compat:
    // pre-change events replay with `final_patch: None` via serde default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_patch:    Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunNoticeProps {
    pub level:   RunNoticeLevel,
    pub code:    String,
    pub message: String,
}
