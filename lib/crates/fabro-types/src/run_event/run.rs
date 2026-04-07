use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{Graph, RunControlAction, Settings, StatusReason};

use super::{RunNoticeLevel, TokenUsage};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunCreatedProps {
    pub settings: Settings,
    pub graph: Graph,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_config: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    pub run_dir: String,
    pub working_directory: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_repo_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_origin_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_slug: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub db_prefix: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunStartedProps {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunStatusTransitionProps {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<StatusReason>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunControlRequestedProps {
    pub action: RunControlAction,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct RunControlEffectProps {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunRewoundProps {
    pub target_checkpoint_ordinal: usize,
    pub target_node_id: String,
    pub target_visit: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_commit_sha: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunCompletedProps {
    pub duration_ms: u64,
    pub artifact_count: usize,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<StatusReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_cost: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_git_commit_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_patch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunFailedProps {
    pub error: String,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<StatusReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_commit_sha: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunNoticeProps {
    pub level: RunNoticeLevel,
    pub code: String,
    pub message: String,
}
