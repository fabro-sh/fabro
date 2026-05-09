use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffStats {
    pub additions: i64,
    pub deletions: i64,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffSummary {
    pub files_changed: i64,
    pub additions:     i64,
    pub deletions:     i64,
}
