use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::outcome::StageStatus;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeStatusRecord {
    pub status:         StageStatus,
    #[serde(default)]
    pub notes:          Option<String>,
    #[serde(default)]
    pub failure_reason: Option<String>,
    pub timestamp:      DateTime<Utc>,
}
