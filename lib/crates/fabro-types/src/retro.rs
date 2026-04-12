use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::run_id::RunId;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SmoothnessRating {
    Effortless,
    Smooth,
    Bumpy,
    Struggled,
    Failed,
}

impl fmt::Display for SmoothnessRating {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Effortless => "effortless",
            Self::Smooth => "smooth",
            Self::Bumpy => "bumpy",
            Self::Struggled => "struggled",
            Self::Failed => "failed",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningCategory {
    Repo,
    Code,
    Workflow,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Learning {
    pub category: LearningCategory,
    pub text:     String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrictionKind {
    Retry,
    Timeout,
    WrongApproach,
    ToolFailure,
    Ambiguity,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrictionPoint {
    pub kind:        FrictionKind,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage_id:    Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenItemKind {
    TechDebt,
    FollowUp,
    Investigation,
    TestGap,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenItem {
    pub kind:        OpenItemKind,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageRetro {
    pub stage_id:           String,
    pub stage_label:        String,
    pub status:             String,
    pub duration_ms:        u64,
    pub retries:            u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billing_usd_micros: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes:              Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason:     Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files_touched:      Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregateStats {
    pub total_duration_ms:        u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_billing_usd_micros: Option<i64>,
    pub total_retries:            u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files_touched:            Vec<String>,
    pub stages_completed:         usize,
    pub stages_failed:            usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetroNarrative {
    pub smoothness:      SmoothnessRating,
    pub intent:          String,
    pub outcome:         String,
    #[serde(default)]
    pub learnings:       Vec<Learning>,
    #[serde(default)]
    pub friction_points: Vec<FrictionPoint>,
    #[serde(default)]
    pub open_items:      Vec<OpenItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Retro {
    pub run_id:          RunId,
    pub workflow_name:   String,
    pub goal:            String,
    pub timestamp:       DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smoothness:      Option<SmoothnessRating>,
    pub stages:          Vec<StageRetro>,
    pub stats:           AggregateStats,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent:          Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome:         Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub learnings:       Option<Vec<Learning>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub friction_points: Option<Vec<FrictionPoint>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_items:      Option<Vec<OpenItem>>,
}

impl Retro {
    pub fn apply_narrative(&mut self, narrative: RetroNarrative) {
        self.smoothness = Some(narrative.smoothness);
        self.intent = Some(narrative.intent);
        self.outcome = Some(narrative.outcome);
        self.learnings = if narrative.learnings.is_empty() {
            None
        } else {
            Some(narrative.learnings)
        };
        self.friction_points = if narrative.friction_points.is_empty() {
            None
        } else {
            Some(narrative.friction_points)
        };
        self.open_items = if narrative.open_items.is_empty() {
            None
        } else {
            Some(narrative.open_items)
        };
    }
}
