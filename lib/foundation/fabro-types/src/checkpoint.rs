use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A checkpoint Fabro recorded for a run: when, at which node, and the
/// commit the workspace was checkpointed at, if it was committed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub timestamp:      DateTime<Utc>,
    pub current_node:   String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_commit_sha: Option<String>,
}
