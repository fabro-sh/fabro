use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::outcome::StageStatus;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageSummary {
    pub stage_id: String,
    pub stage_label: String,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    pub retries: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conclusion {
    pub timestamp: DateTime<Utc>,
    pub status: StageStatus,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_git_commit_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stages: Vec<StageSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_cost: Option<f64>,
    #[serde(default)]
    pub total_retries: u32,
    #[serde(default)]
    pub total_input_tokens: i64,
    #[serde(default)]
    pub total_output_tokens: i64,
    #[serde(default)]
    pub total_cache_read_tokens: i64,
    #[serde(default)]
    pub total_cache_write_tokens: i64,
    #[serde(default)]
    pub total_reasoning_tokens: i64,
    #[serde(default)]
    pub has_pricing: bool,
}
