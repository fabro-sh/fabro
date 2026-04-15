use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{BilledModelUsage, FailureDetail, Outcome, StageStatus};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StageStartedProps {
    pub index:        usize,
    pub handler_type: String,
    pub attempt:      usize,
    pub max_attempts: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StageCompletedProps {
    pub index: usize,
    pub duration_ms: u64,
    pub status: StageStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_label: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggested_next_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billing: Option<BilledModelUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<FailureDetail>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files_touched: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_updates: Option<BTreeMap<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jump_to_node: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_values: Option<BTreeMap<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_visits: Option<BTreeMap<String, usize>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_failure_signatures: Option<BTreeMap<String, usize>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart_failure_signatures: Option<BTreeMap<String, usize>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<String>,
    pub attempt: usize,
    pub max_attempts: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StageFailedProps {
    pub index:       usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure:     Option<FailureDetail>,
    pub will_retry:  bool,
    #[serde(default)]
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StageRetryingProps {
    pub index:        usize,
    pub attempt:      usize,
    pub max_attempts: usize,
    pub delay_ms:     u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StagePromptProps {
    pub visit:    u32,
    pub text:     String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode:     Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model:    Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromptCompletedProps {
    pub response: String,
    pub model:    String,
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billing:  Option<BilledModelUsage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckpointCompletedProps {
    pub status: String,
    pub current_node: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub completed_nodes: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub node_retries: BTreeMap<String, u32>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub context_values: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub node_outcomes: BTreeMap<String, Outcome<Option<BilledModelUsage>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_commit_sha: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub loop_failure_signatures: BTreeMap<String, usize>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub restart_failure_signatures: BTreeMap<String, usize>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub node_visits: BTreeMap<String, usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckpointFailedProps {
    pub error: String,
}
