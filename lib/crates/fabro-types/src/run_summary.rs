use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    DiffSummary, InterviewQuestionRecord, Principal, PullRequestLink, RepositoryRef,
    RunControlAction, RunId, RunSandbox, RunStatus,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub id:               RunId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id:        Option<RunId>,
    #[serde(default)]
    pub children_count:   u64,
    pub title:            String,
    pub goal:             String,
    pub workflow:         WorkflowRef,
    #[serde(default)]
    pub automation:       Option<AutomationRef>,
    #[serde(default)]
    pub repository:       Option<RepositoryRef>,
    #[serde(default)]
    pub created_by:       Option<Principal>,
    pub origin:           RunOrigin,
    pub labels:           HashMap<String, String>,
    pub lifecycle:        RunLifecycle,
    #[serde(default)]
    pub sandbox:          Option<RunSandbox>,
    pub models:           Vec<RunModel>,
    #[serde(default)]
    pub source_directory: Option<String>,
    pub timestamps:       RunTimestamps,
    #[serde(default)]
    pub billing:          Option<RunBillingSummary>,
    #[serde(default)]
    pub diff:             Option<DiffSummary>,
    #[serde(default)]
    pub pull_request:     Option<PullRequestLink>,
    #[serde(default)]
    pub current_question: Option<InterviewQuestionRecord>,
    #[serde(default)]
    pub superseded_by:    Option<RunId>,
    pub links:            RunLinks,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowRef {
    #[serde(default)]
    pub slug:       Option<String>,
    #[serde(default)]
    pub name:       Option<String>,
    #[serde(default)]
    pub graph_name: Option<String>,
    /// Number of nodes in the workflow graph.
    #[serde(default)]
    pub node_count: i64,
    /// Number of edges in the workflow graph.
    #[serde(default)]
    pub edge_count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutomationRef {
    pub id:   String,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunOrigin {
    pub kind: RunOriginKind,
}

impl Default for RunOrigin {
    fn default() -> Self {
        Self {
            kind: RunOriginKind::Api,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOriginKind {
    Api,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunModel {
    #[serde(default)]
    pub provider: Option<String>,
    pub name:     String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunLifecycle {
    pub status:          RunStatus,
    #[serde(default)]
    pub pending_control: Option<RunControlAction>,
    #[serde(default)]
    pub queue_position:  Option<u32>,
    #[serde(default)]
    pub error:           Option<RunError>,
    pub archived:        bool,
    #[serde(default)]
    pub archived_at:     Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunError {
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunTimestamps {
    pub created_at:    DateTime<Utc>,
    #[serde(default)]
    pub started_at:    Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_event_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub completed_at:  Option<DateTime<Utc>>,
    #[serde(default)]
    pub duration_ms:   Option<u64>,
    #[serde(default)]
    pub elapsed_secs:  Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunBillingSummary {
    #[serde(default)]
    pub total_usd_micros: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunLinks {
    #[serde(default)]
    pub web: Option<String>,
}
