use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{RunControlAction, RunId, RunStatus};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSummary {
    pub run_id:           RunId,
    pub workflow_name:    Option<String>,
    pub workflow_slug:    Option<String>,
    pub goal:             Option<String>,
    pub labels:           HashMap<String, String>,
    pub host_repo_path:   Option<String>,
    pub start_time:       Option<DateTime<Utc>>,
    pub status:           RunStatus,
    pub pending_control:  Option<RunControlAction>,
    pub duration_ms:      Option<u64>,
    pub total_usd_micros: Option<i64>,
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use chrono::{TimeZone, Utc};

    use super::RunSummary;
    use crate::{BlockedReason, RunControlAction, RunStatus, fixtures};

    #[test]
    fn round_trips_through_serde_json() {
        let summary = RunSummary {
            run_id:           fixtures::RUN_1,
            workflow_name:    Some("workflow".to_string()),
            workflow_slug:    Some("workflow".to_string()),
            goal:             Some("ship it".to_string()),
            labels:           HashMap::from([("team".to_string(), "core".to_string())]),
            host_repo_path:   Some("/tmp/repo".to_string()),
            start_time:       Some(Utc.with_ymd_and_hms(2026, 4, 20, 12, 0, 0).unwrap()),
            status:           RunStatus::Blocked {
                blocked_reason: BlockedReason::HumanInputRequired,
            },
            pending_control:  Some(RunControlAction::Pause),
            duration_ms:      Some(42),
            total_usd_micros: Some(123),
        };

        let value = serde_json::to_value(&summary).unwrap();
        let parsed: RunSummary = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, summary);
    }
}
