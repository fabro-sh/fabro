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
    pub source_directory: Option<String>,
    #[serde(default)]
    pub in_place:         bool,
    #[serde(default)]
    pub repo_origin_url:  Option<String>,
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
        source_directory: Option<String>,
        in_place: bool,
        repo_origin_url: Option<String>,
        start_time: Option<DateTime<Utc>>,
        status: RunStatus,
        pending_control: Option<RunControlAction>,
        duration_ms: Option<u64>,
        total_usd_micros: Option<i64>,
        superseded_by: Option<RunId>,
    ) -> Self {
        let title = truncate_goal(&goal);
        let repository = RepositoryReference {
            name: repository_name(repo_origin_url.as_deref(), source_directory.as_deref()),
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
            source_directory,
            in_place,
            repo_origin_url,
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

fn repository_name(repo_origin_url: Option<&str>, source_directory: Option<&str>) -> String {
    repo_origin_url
        .and_then(repository_name_from_origin)
        .or_else(|| {
            source_directory
                .and_then(path_basename)
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| "unknown".to_string())
}

#[expect(
    clippy::disallowed_types,
    reason = "Run summaries parse the origin only to extract an owner/repo label; raw URLs are not logged or returned here."
)]
fn repository_name_from_origin(origin: &str) -> Option<String> {
    if let Some(path) = origin
        .strip_prefix("git@")
        .and_then(|url| url.split_once(':').map(|(_, path)| path))
    {
        return repository_name_from_path(path).map(ToOwned::to_owned);
    }

    let parsed = url::Url::parse(origin).ok()?;
    let path = parsed.path().trim_matches('/');
    repository_name_from_path(path).map(ToOwned::to_owned)
}

fn repository_name_from_path(path: &str) -> Option<&str> {
    let stripped = path.strip_suffix(".git").unwrap_or(path);
    let mut segments = stripped.rsplit('/').filter(|segment| !segment.is_empty());
    let repo = segments.next()?;
    let owner = segments.next();
    if let Some(owner) = owner {
        let start = stripped.len() - owner.len() - repo.len() - 1;
        stripped.get(start..)
    } else {
        Some(repo)
    }
}

fn path_basename(path: &str) -> Option<&str> {
    path.rsplit(['/', '\\']).find(|segment| !segment.is_empty())
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
    fn summary_prefers_origin_name_over_submitter_source_directory() {
        let summary = RunSummary::new(
            fixtures::RUN_1,
            Some("workflow".to_string()),
            Some("workflow".to_string()),
            "ship it".to_string(),
            HashMap::from([("team".to_string(), "core".to_string())]),
            Some("/Users/client/local-checkout".to_string()),
            false,
            Some("https://github.com/fabro-sh/fabro.git".to_string()),
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
            name: "fabro-sh/fabro".to_string(),
        });
        assert_eq!(summary.created_at, fixtures::RUN_1.created_at());
        assert_eq!(summary.elapsed_secs, Some(0.042));
        assert_eq!(
            summary.source_directory.as_deref(),
            Some("/Users/client/local-checkout")
        );

        let value = serde_json::to_value(&summary).unwrap();
        assert!(value.get("host_repo_path").is_none());
        assert_eq!(value["source_directory"], "/Users/client/local-checkout");
        assert_eq!(
            value["repo_origin_url"],
            "https://github.com/fabro-sh/fabro.git"
        );
        let parsed: RunSummary = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, summary);
    }

    #[test]
    fn summary_falls_back_to_source_directory_then_unknown() {
        let source_only = RunSummary::new(
            fixtures::RUN_1,
            None,
            None,
            "ship it".to_string(),
            HashMap::new(),
            Some("/Users/client/local-checkout".to_string()),
            false,
            None,
            None,
            RunStatus::Submitted,
            None,
            None,
            None,
            None,
        );
        assert_eq!(source_only.repository.name, "local-checkout");

        let unknown = RunSummary::new(
            fixtures::RUN_1,
            None,
            None,
            "ship it".to_string(),
            HashMap::new(),
            None,
            false,
            None,
            None,
            RunStatus::Submitted,
            None,
            None,
            None,
            None,
        );
        assert_eq!(unknown.repository.name, "unknown");
    }
}
