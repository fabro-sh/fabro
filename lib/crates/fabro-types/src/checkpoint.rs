use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::billing::BilledModelUsage;
use crate::failure_signature::FailureSignature;
use crate::outcome::Outcome;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub timestamp: DateTime<Utc>,
    pub current_node: String,
    pub completed_nodes: Vec<String>,
    pub node_retries: HashMap<String, u32>,
    pub context_values: HashMap<String, Value>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub node_outcomes: HashMap<String, Outcome<Option<BilledModelUsage>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_commit_sha: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub loop_failure_signatures: HashMap<FailureSignature, usize>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub restart_failure_signatures: HashMap<FailureSignature, usize>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub node_visits: HashMap<String, usize>,
}
