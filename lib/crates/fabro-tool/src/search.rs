use std::collections::HashMap;
use std::sync::Arc;

use fabro_types::{Run, RunId, RunStatusKind};
use futures::future::try_join_all;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common;
use super::common::{FabroToolBackend, RunSummaryResult, ToolError, ToolResult};

const SEARCH_GOAL_PREVIEW_CHARS: usize = 240;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FabroRunSearchParams {
    pub run_ids:        Option<Vec<String>>,
    pub workflow:       Option<String>,
    pub labels:         Option<HashMap<String, String>>,
    pub status:         Option<Vec<String>>,
    pub archived:       Option<bool>,
    pub created_after:  Option<String>,
    pub created_before: Option<String>,
    pub first:          Option<usize>,
    pub after:          Option<String>,
    pub parent_id:      Option<String>,
}

#[derive(Debug)]
pub struct ValidatedSearchRuns {
    pub raw:    FabroRunSearchParams,
    pub status: Option<Vec<RunStatusKind>>,
}

impl TryFrom<FabroRunSearchParams> for ValidatedSearchRuns {
    type Error = ToolError;

    fn try_from(mut params: FabroRunSearchParams) -> Result<Self, Self::Error> {
        if params.first.is_some_and(|first| first > 100) {
            return Err(ToolError::message("first must be <= 100"));
        }
        if let Some(run_ids) = params.run_ids.as_ref() {
            common::validate_len("run_ids", run_ids.len(), 1, 100)?;
        }
        let status = params
            .status
            .as_ref()
            .map(|statuses| {
                statuses
                    .iter()
                    .map(|status| {
                        status.parse::<RunStatusKind>().map_err(|_| {
                            ToolError::message(format!("unknown run status `{status}`"))
                        })
                    })
                    .collect::<ToolResult<Vec<_>>>()
            })
            .transpose()?;
        if let Some(created_after) = params.created_after.as_deref() {
            common::parse_datetime_filter("created_after", created_after)?;
        }
        if let Some(created_before) = params.created_before.as_deref() {
            common::parse_datetime_filter("created_before", created_before)?;
        }
        if let Some(parent_id) = params.parent_id.take() {
            let parent_id = parent_id.trim().to_string();
            if parent_id.is_empty() {
                return Err(ToolError::message("parent_id must not be blank"));
            }
            params.parent_id = Some(parent_id);
        }
        Ok(Self {
            raw: params,
            status,
        })
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SearchRunsResult {
    pub runs:        Vec<SearchRunSummaryResult>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SearchRunSummaryResult {
    pub run_id:              String,
    pub parent_id:           Option<String>,
    pub children_count:      u64,
    pub workflow_name:       Option<String>,
    pub workflow_graph_name: Option<String>,
    pub workflow_slug:       Option<String>,
    pub status:              String,
    pub archived:            bool,
    pub created_at:          String,
    pub started_at:          Option<String>,
    pub completed_at:        Option<String>,
    pub labels:              HashMap<String, String>,
    pub source_directory:    Option<String>,
    pub repo_origin_url:     Option<String>,
    pub goal_preview:        String,
    pub goal_truncated:      bool,
}

pub async fn search_runs(
    backend: Arc<dyn FabroToolBackend>,
    params: ValidatedSearchRuns,
) -> ToolResult<SearchRunsResult> {
    let status = params.status;
    let raw = params.raw;
    let parent_id = if let Some(parent_selector) = raw.parent_id.as_deref() {
        Some(
            backend
                .resolve_run(parent_selector)
                .await
                .map_err(|err| ToolError::from_anyhow(&err))?
                .id,
        )
    } else {
        None
    };
    let runs = if let Some(run_ids) = raw.run_ids.as_ref() {
        resolve_requested_runs(&backend, run_ids).await?
    } else if let Some(parent_id) = parent_id {
        backend
            .list_store_runs_by_parent(parent_id)
            .await
            .map_err(|err| ToolError::from_anyhow(&err))?
    } else {
        backend
            .list_store_runs()
            .await
            .map_err(|err| ToolError::from_anyhow(&err))?
    };
    let page = filter_sort_and_page_runs(runs, &raw, status.as_deref(), parent_id)?;

    Ok(SearchRunsResult {
        runs:        page.runs.iter().map(search_run_summary_result).collect(),
        next_cursor: page.next_cursor,
    })
}

fn search_run_summary_result(run: &Run) -> SearchRunSummaryResult {
    let RunSummaryResult {
        run_id,
        parent_id,
        children_count,
        workflow_name,
        workflow_graph_name,
        workflow_slug,
        status,
        archived,
        created_at,
        started_at,
        completed_at,
        labels,
        source_directory,
        repo_origin_url,
        goal,
    } = common::run_summary_result(run);
    let (goal_preview, goal_truncated) = goal_preview(&goal);

    SearchRunSummaryResult {
        run_id,
        parent_id,
        children_count,
        workflow_name,
        workflow_graph_name,
        workflow_slug,
        status,
        archived,
        created_at,
        started_at,
        completed_at,
        labels,
        source_directory,
        repo_origin_url,
        goal_preview,
        goal_truncated,
    }
}

fn goal_preview(goal: &str) -> (String, bool) {
    let mut chars = goal.chars();
    let mut preview = chars
        .by_ref()
        .take(SEARCH_GOAL_PREVIEW_CHARS)
        .collect::<String>();
    let truncated = chars.next().is_some();
    if truncated {
        preview.push_str("...");
    }
    (preview, truncated)
}

struct RunSearchPage {
    runs:        Vec<Run>,
    next_cursor: Option<String>,
}

fn filter_sort_and_page_runs(
    mut runs: Vec<Run>,
    raw: &FabroRunSearchParams,
    status: Option<&[RunStatusKind]>,
    parent_id: Option<RunId>,
) -> ToolResult<RunSearchPage> {
    if let Some(parent_id) = parent_id {
        runs.retain(|run| run.parent_id == Some(parent_id));
    }
    if let Some(workflow) = raw.workflow.as_deref() {
        runs.retain(|run| {
            run.workflow.name.as_deref() == Some(workflow)
                || run.workflow.graph_name.as_deref() == Some(workflow)
                || run.workflow.slug.as_deref() == Some(workflow)
        });
    }
    if let Some(labels) = raw.labels.as_ref() {
        runs.retain(|run| {
            labels
                .iter()
                .all(|(key, value)| run.labels.get(key) == Some(value))
        });
    }
    if let Some(status) = status {
        runs.retain(|run| {
            status
                .iter()
                .any(|status| *status == run.lifecycle.status.kind())
        });
    }
    let archived = raw.archived.unwrap_or(false);
    runs.retain(|run| run.lifecycle.archived == archived);
    if let Some(created_after) = raw.created_after.as_deref() {
        let cutoff = common::parse_datetime_filter("created_after", created_after)?;
        runs.retain(|run| run.timestamps.created_at >= cutoff);
    }
    if let Some(created_before) = raw.created_before.as_deref() {
        let cutoff = common::parse_datetime_filter("created_before", created_before)?;
        runs.retain(|run| run.timestamps.created_at <= cutoff);
    }

    runs.sort_by(|a, b| {
        let a_sort_time = a.timestamps.started_at.unwrap_or(a.timestamps.created_at);
        let b_sort_time = b.timestamps.started_at.unwrap_or(b.timestamps.created_at);
        b_sort_time.cmp(&a_sort_time).then_with(|| b.id.cmp(&a.id))
    });

    if let Some(after) = raw.after.as_deref() {
        if let Some(position) = runs.iter().position(|run| run.id.to_string() == after) {
            runs = runs.into_iter().skip(position + 1).collect();
        }
    }

    let first = raw.first.unwrap_or(20).min(100);
    let has_more = runs.len() > first;
    let page = runs.into_iter().take(first).collect::<Vec<_>>();
    let next_cursor = has_more
        .then(|| page.last().map(|run| run.id.to_string()))
        .flatten();
    Ok(RunSearchPage {
        runs: page,
        next_cursor,
    })
}

pub fn search_runs_text(result: &SearchRunsResult) -> String {
    format!("found {} Fabro run(s)", result.runs.len())
}

async fn resolve_requested_runs(
    backend: &Arc<dyn FabroToolBackend>,
    run_ids: &[String],
) -> ToolResult<Vec<Run>> {
    let runs = try_join_all(run_ids.iter().map(|run_id| {
        let backend = Arc::clone(backend);
        async move {
            backend
                .resolve_run(run_id)
                .await
                .map_err(|err| ToolError::from_anyhow(&err))
        }
    }))
    .await?;

    let mut unique = HashMap::new();
    for run in runs {
        unique.entry(run.id).or_insert(run);
    }
    Ok(unique.into_values().collect())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use chrono::{TimeZone, Utc};
    use fabro_types::{RunLifecycle, RunLinks, RunOrigin, RunStatus, RunTimestamps, WorkflowRef};

    use super::*;

    #[test]
    fn cursor_is_applied_after_filters() {
        let matching_newer = run("01KRBZW5C00000000000000001", "keep", 30);
        let unrelated_cursor = run("01KRBZW4DW0000000000000002", "skip", 20);
        let matching_older = run("01KRBZW3EF0000000000000003", "keep", 10);

        let result = filter_sort_and_page_runs(
            vec![
                matching_older.clone(),
                unrelated_cursor.clone(),
                matching_newer.clone(),
            ],
            &FabroRunSearchParams {
                run_ids:        None,
                workflow:       None,
                labels:         Some(HashMap::from([("group".to_string(), "keep".to_string())])),
                status:         None,
                archived:       None,
                created_after:  None,
                created_before: None,
                first:          Some(10),
                after:          Some(unrelated_cursor.id.to_string()),
                parent_id:      None,
            },
            None,
            None,
        )
        .expect("filtering should succeed");

        let ids = result.runs.iter().map(|run| run.id).collect::<Vec<_>>();
        assert_eq!(ids, vec![matching_newer.id, matching_older.id]);
    }

    #[test]
    fn omitted_archived_filter_hides_archived_runs_by_default() {
        let active = run("01KRBZW5C00000000000000001", "keep", 30);
        let archived = archived_run("01KRBZW4DW0000000000000002", "keep", 20);

        let result = filter_sort_and_page_runs(
            vec![archived.clone(), active.clone()],
            &FabroRunSearchParams {
                run_ids:        None,
                workflow:       None,
                labels:         None,
                status:         None,
                archived:       None,
                created_after:  None,
                created_before: None,
                first:          Some(10),
                after:          None,
                parent_id:      None,
            },
            None,
            None,
        )
        .expect("filtering should succeed");

        let ids = result.runs.iter().map(|run| run.id).collect::<Vec<_>>();
        assert_eq!(ids, vec![active.id]);
    }

    #[test]
    fn search_summary_uses_bounded_goal_preview() {
        let parent_id = run_id("01KRBZW4DW0000000000000002");
        let mut run = run("01KRBZW5C00000000000000001", "keep", 30);
        run.parent_id = Some(parent_id);
        run.children_count = 4;
        run.goal = format!("{}tail-marker", "a".repeat(300));

        let summary = search_run_summary_result(&run);

        assert_eq!(summary.parent_id, Some(parent_id.to_string()));
        assert_eq!(summary.children_count, 4);
        assert_eq!(summary.workflow_name.as_deref(), Some("Simple"));
        assert_eq!(summary.workflow_graph_name.as_deref(), Some("GraphName"));
        assert!(summary.goal_truncated);
        assert!(summary.goal_preview.len() < run.goal.len());
        assert!(!summary.goal_preview.contains("tail-marker"));
    }

    #[test]
    fn parent_filter_keeps_matching_direct_children_and_composes_with_archived_default() {
        let parent_id = run_id("01KRBZW5000000000000000004");
        let other_parent_id = run_id("01KRBZW4000000000000000005");
        let mut active_child = run("01KRBZW5C00000000000000001", "keep", 30);
        active_child.parent_id = Some(parent_id);
        let mut archived_child = archived_run("01KRBZW4DW0000000000000002", "keep", 20);
        archived_child.parent_id = Some(parent_id);
        let mut unrelated_child = run("01KRBZW3EF0000000000000003", "keep", 10);
        unrelated_child.parent_id = Some(other_parent_id);

        let result = filter_sort_and_page_runs(
            vec![
                archived_child.clone(),
                unrelated_child.clone(),
                active_child.clone(),
            ],
            &FabroRunSearchParams {
                run_ids:        None,
                workflow:       None,
                labels:         None,
                status:         None,
                archived:       None,
                created_after:  None,
                created_before: None,
                first:          Some(10),
                after:          None,
                parent_id:      Some("nightly-parent".to_string()),
            },
            None,
            Some(parent_id),
        )
        .expect("filtering should succeed");

        let ids = result.runs.iter().map(|run| run.id).collect::<Vec<_>>();
        assert_eq!(ids, vec![active_child.id]);
    }

    fn run(id: &str, group: &str, seconds: u32) -> Run {
        run_with_archived(id, group, seconds, false)
    }

    fn archived_run(id: &str, group: &str, seconds: u32) -> Run {
        run_with_archived(id, group, seconds, true)
    }

    fn run_id(raw: &str) -> fabro_types::RunId {
        raw.parse().expect("test run id should parse")
    }

    fn run_with_archived(id: &str, group: &str, seconds: u32, archived: bool) -> Run {
        let created_at = Utc.with_ymd_and_hms(2026, 5, 11, 12, 0, seconds).unwrap();
        Run {
            id:               id.parse().expect("test run id should parse"),
            parent_id:        None,
            children_count:   0,
            title:            "test".to_string(),
            goal:             "test".to_string(),
            workflow:         WorkflowRef {
                slug:       Some("simple".to_string()),
                name:       Some("Simple".to_string()),
                graph_name: Some("GraphName".to_string()),
                node_count: 0,
                edge_count: 0,
            },
            automation:       None,
            repository:       None,
            created_by:       None,
            origin:           RunOrigin::default(),
            labels:           HashMap::from([("group".to_string(), group.to_string())]),
            lifecycle:        RunLifecycle {
                status: RunStatus::Submitted,
                pending_control: None,
                queue_position: None,
                error: None,
                archived,
                archived_at: None,
            },
            sandbox:          None,
            models:           Vec::new(),
            source_directory: None,
            timestamps:       RunTimestamps {
                created_at,
                started_at: None,
                last_event_at: None,
                completed_at: None,
                duration_ms: None,
                elapsed_secs: None,
            },
            billing:          None,
            diff:             None,
            pull_request:     None,
            current_question: None,
            superseded_by:    None,
            links:            RunLinks { web: None },
        }
    }
}
