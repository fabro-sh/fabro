use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ExternalAgentHarness;

/// Durable lifecycle status of an automated Plane ticket dispatch.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    strum::Display,
    strum::EnumString,
    strum::IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum PlaneDispatchStatus {
    Pending,
    Claimed,
    Running,
    RetryPending,
    Succeeded,
    Failed,
    Cancelled,
}

impl PlaneDispatchStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        self.into()
    }

    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

/// Durable state record of a single Plane ticket automation dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaneDispatch {
    pub automation_id:    String,
    pub trigger_id:       String,
    pub issue_id:         String,
    pub issue_identifier: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_title:      Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_url:        Option<String>,
    pub status:           PlaneDispatchStatus,
    pub harness:          ExternalAgentHarness,
    pub attempt:          u32,
    #[serde(default)]
    pub run_ids:          Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_run_id:   Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error:       Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimed_at:       Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at:     Option<DateTime<Utc>>,
    pub created_at:       DateTime<Utc>,
    pub updated_at:       DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaneDispatchListResponse {
    pub data: Vec<PlaneDispatch>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaneProjectResponse {
    pub id:          String,
    pub name:        String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identifier:  Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaneProjectsResponse {
    pub data: Vec<PlaneProjectResponse>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaneStateResponse {
    pub id:       String,
    pub name:     String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group:    Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color:    Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaneLabelResponse {
    pub id:    String,
    pub name:  String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaneProjectMetadataResponse {
    pub states: Vec<PlaneStateResponse>,
    pub labels: Vec<PlaneLabelResponse>,
}
