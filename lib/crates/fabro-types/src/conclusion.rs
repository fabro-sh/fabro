use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::outcome::StageOutcome;
use crate::{BilledTokenCounts, RunDiff, RunFailure, RunTiming, StageTiming};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageSummary {
    pub stage_id:           String,
    pub stage_label:        String,
    /// Per-node timing summed across every visit of the node within this
    /// conclusion. `wall_time_ms` is the sum of visit wall times.
    pub timing:             StageTiming,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billing_usd_micros: Option<i64>,
    pub retries:            u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conclusion {
    pub timestamp:            DateTime<Utc>,
    pub status:               StageOutcome,
    /// Run-level timing. `wall_time_ms` is the run's clock duration; active
    /// fields sum work across stage visits and can exceed wall time.
    pub timing:               RunTiming,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure:              Option<RunFailure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_git_commit_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stages:               Vec<StageSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billing:              Option<BilledTokenCounts>,
    #[serde(default)]
    pub total_retries:        u32,
    #[serde(default)]
    pub diff:                 RunDiff,
}
