use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{Result, StoreError};
use fabro_types::{
    Checkpoint, Conclusion, NodeStatusRecord, Retro, RunId, RunRecord, RunStatus, RunStatusRecord,
    SandboxRecord, StartRecord, StatusReason,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeVisitRef<'a> {
    pub node_id: &'a str,
    pub visit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogRecord {
    pub run_id: RunId,
    pub created_at: DateTime<Utc>,
    pub db_prefix: String,
    pub run_dir: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSummary {
    pub run_id: RunId,
    pub created_at: DateTime<Utc>,
    pub db_prefix: String,
    pub run_dir: Option<String>,
    pub workflow_name: Option<String>,
    pub workflow_slug: Option<String>,
    pub goal: Option<String>,
    pub labels: HashMap<String, String>,
    pub host_repo_path: Option<String>,
    pub start_time: Option<DateTime<Utc>>,
    pub status: Option<RunStatus>,
    pub status_reason: Option<StatusReason>,
    pub duration_ms: Option<u64>,
    pub total_cost: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSnapshot {
    pub run: RunRecord,
    pub start: Option<StartRecord>,
    pub status: Option<RunStatusRecord>,
    pub checkpoint: Option<Checkpoint>,
    pub conclusion: Option<Conclusion>,
    pub retro: Option<Retro>,
    pub graph: Option<String>,
    pub sandbox: Option<SandboxRecord>,
    pub nodes: Vec<NodeSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeSnapshot {
    pub node_id: String,
    pub visit: u32,
    pub prompt: Option<String>,
    pub response: Option<String>,
    pub status: Option<NodeStatusRecord>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventPayload(serde_json::Value);

impl EventPayload {
    pub fn new(value: serde_json::Value, expected_run_id: &RunId) -> Result<Self> {
        let payload = Self(value);
        payload.validate(expected_run_id)?;
        Ok(payload)
    }

    pub fn validate(&self, expected_run_id: &RunId) -> Result<()> {
        let obj = self.0.as_object().ok_or_else(|| {
            StoreError::InvalidEvent("event payload must be a JSON object".into())
        })?;

        for field in ["id", "ts", "run_id", "event"] {
            match obj.get(field) {
                Some(serde_json::Value::String(_)) => {}
                _ => {
                    return Err(StoreError::InvalidEvent(format!(
                        "missing or non-string required field: {field}"
                    )));
                }
            }
        }

        match obj.get("run_id") {
            Some(serde_json::Value::String(run_id)) if run_id == &expected_run_id.to_string() => {
                Ok(())
            }
            Some(serde_json::Value::String(run_id)) => Err(StoreError::InvalidEvent(format!(
                "payload run_id {run_id:?} does not match store run_id {expected_run_id:?}"
            ))),
            _ => Err(StoreError::InvalidEvent(
                "missing or non-string required field: run_id".into(),
            )),
        }
    }

    pub fn into_inner(self) -> serde_json::Value {
        self.0
    }

    pub fn as_value(&self) -> &serde_json::Value {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub seq: u32,
    pub payload: EventPayload,
}
