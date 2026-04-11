use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::run_id::RunId;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartRecord {
    pub run_id:     RunId,
    pub start_time: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_sha:   Option<String>,
}
