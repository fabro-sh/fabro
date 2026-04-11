use serde::{Deserialize, Serialize};

/// Record of a pull request created for a workflow run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestRecord {
    pub html_url:    String,
    pub number:      u64,
    pub owner:       String,
    pub repo:        String,
    pub base_branch: String,
    pub head_branch: String,
    pub title:       String,
}
