use std::collections::HashMap;

use chrono::{DateTime, Utc};
use fabro_util::text::strip_goal_decoration;
use serde::{Deserialize, Serialize};

use crate::{RepositoryReference, RunControlAction, RunId, RunStatus};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSummary {
    pub run_id:           RunId,
    #[serde(default)]
    pub workflow_name:    Option<String>,
    #[serde(default)]
    pub workflow_slug:    Option<String>,
    pub goal:             String,
    pub title:            String,
    pub labels:           HashMap<String, String>,
    #[serde(default)]
    pub host_repo_path:   Option<String>,
    pub repository:       RepositoryReference,
    #[serde(default)]
    pub start_time:       Option<DateTime<Utc>>,
    pub created_at:       DateTime<Utc>,
    pub status:           RunStatus,
    #[serde(default)]
    pub pending_control:  Option<RunControlAction>,
    #[serde(default)]
    pub duration_ms:      Option<u64>,
    #[serde(default)]
    pub elapsed_secs:     Option<f64>,
    #[serde(default)]
    pub total_usd_micros: Option<i64>,
    #[serde(default)]
    pub superseded_by:    Option<RunId>,
}

impl RunSummary {
    #[allow(
        clippy::too_many_arguments,
        reason = "RunSummary is a flat wire DTO; the constructor centralizes derived fields."
    )]
    pub fn new(
        run_id: RunId,
        workflow_name: Option<String>,
        workflow_slug: Option<String>,
        goal: String,
        labels: HashMap<String, String>,
        host_repo_path: Option<String>,
        start_time: Option<DateTime<Utc>>,
        status: RunStatus,
        pending_control: Option<RunControlAction>,
        duration_ms: Option<u64>,
        total_usd_micros: Option<i64>,
        superseded_by: Option<RunId>,
    ) -> Self {
        let title = truncate_goal(&goal);
        let repository = RepositoryReference {
            name: repository_name(host_repo_path.as_deref()),
        };
        let elapsed_secs = elapsed_secs(duration_ms);
        let created_at = run_id.created_at();

        Self {
            run_id,
            workflow_name,
            workflow_slug,
            goal,
            title,
            labels,
            host_repo_path,
            repository,
            start_time,
            created_at,
            status,
            pending_control,
            duration_ms,
            elapsed_secs,
            total_usd_micros,
            superseded_by,
        }
    }
}

fn truncate_goal(goal: &str) -> String {
    const MAX_LEN: usize = 100;

    let stripped = strip_goal_decoration(goal);
    let char_count = stripped.chars().count();
    if char_count <= MAX_LEN {
        return stripped.to_string();
    }

    let truncated: String = stripped.chars().take(MAX_LEN - 3).collect();
    format!("{truncated}...")
}

fn repository_name(host_repo_path: Option<&str>) -> String {
    host_repo_path
        .and_then(|path| path.rsplit(['/', '\\']).find(|segment| !segment.is_empty()))
        .unwrap_or("unknown")
        .to_string()
}

fn elapsed_secs(duration_ms: Option<u64>) -> Option<f64> {
    duration_ms.map(|ms| ms as f64 / 1000.0)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use chrono::{TimeZone, Utc};

    use super::RunSummary;
    use crate::{BlockedReason, RepositoryReference, RunControlAction, RunStatus, fixtures};

    #[test]
    fn round_trips_through_serde_json() {
        let summary = RunSummary::new(
            fixtures::RUN_1,
            Some("workflow".to_string()),
            Some("workflow".to_string()),
            "ship it".to_string(),
            HashMap::from([("team".to_string(), "core".to_string())]),
            Some("/tmp/repo".to_string()),
            Some(Utc.with_ymd_and_hms(2026, 4, 20, 12, 0, 0).unwrap()),
            RunStatus::Blocked {
                blocked_reason: BlockedReason::HumanInputRequired,
            },
            Some(RunControlAction::Pause),
            Some(42),
            Some(123),
            Some(fixtures::RUN_2),
        );

        assert_eq!(summary.title, "ship it");
        assert_eq!(summary.repository, RepositoryReference {
            name: "repo".to_string(),
        });
        assert_eq!(summary.created_at, fixtures::RUN_1.created_at());
        assert_eq!(summary.elapsed_secs, Some(0.042));

        let value = serde_json::to_value(&summary).unwrap();
        let parsed: RunSummary = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, summary);
    }
}
