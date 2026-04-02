use std::path::Path;

use serde::{Deserialize, Serialize};

/// Record of a pull request created for a workflow run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestRecord {
    pub html_url: String,
    pub number: u64,
    pub owner: String,
    pub repo: String,
    pub base_branch: String,
    pub head_branch: String,
    pub title: String,
}

impl PullRequestRecord {
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize pull_request.json: {e}"))?;
        std::fs::write(path, json).map_err(|e| format!("Failed to write pull_request.json: {e}"))
    }
}
