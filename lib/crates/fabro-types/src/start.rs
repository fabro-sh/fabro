use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartRecord {
    pub run_id: String,
    pub start_time: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_sha: Option<String>,
}
