//! The `runs` row: the summary the run list, the board and the scheduler
//! read, written by a Petri run's projector from its projection, beside
//! the platform records and the projection tables over the same pool.

use std::fmt::Write as _;
use std::sync::{Arc, LazyLock, PoisonError, RwLock};

use chrono::{DateTime, Utc};
use fabro_types::{Run, RunId, RunSize, RunStatusKind, RunTiming, timing};
use sqlx::query::Query;
use sqlx::sqlite::{SqliteArguments, SqliteConnection, SqliteRow};
use sqlx::{QueryBuilder, Row as _, Sqlite, SqlitePool};
use strum::VariantArray as _;

use crate::platform_records::{PlatformRecordHook, PlatformRecordStore};
use crate::run_summary::{build_summary, projected_usage};
use crate::{Error, Result, RunProjection};

/// The `runs` row of a Petri run, written by its projector: every column the
/// list views and the scheduler read.
const UPSERT_PETRI_RUN_SQL: &str = r"
INSERT INTO runs (
    id, created_at_ms, started_at_ms, last_event_at_ms, completed_at_ms,
    status, archived_at_ms, parent_id, title, workflow_slug, workflow_name,
    repository_name, automation_id, diff_additions, diff_deletions,
    total_usd_micros, summary_json
) VALUES (
    ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?
)
ON CONFLICT(id) DO UPDATE SET
    created_at_ms = excluded.created_at_ms,
    started_at_ms = excluded.started_at_ms,
    last_event_at_ms = excluded.last_event_at_ms,
    completed_at_ms = excluded.completed_at_ms,
    status = excluded.status,
    archived_at_ms = excluded.archived_at_ms,
    parent_id = excluded.parent_id,
    title = excluded.title,
    workflow_slug = excluded.workflow_slug,
    workflow_name = excluded.workflow_name,
    repository_name = excluded.repository_name,
    automation_id = excluded.automation_id,
    diff_additions = excluded.diff_additions,
    diff_deletions = excluded.diff_deletions,
    total_usd_micros = excluded.total_usd_micros,
    summary_json = excluded.summary_json
";

const SELECT_RUN_SUMMARIES_SQL: &str = r"
SELECT runs.id, runs.summary_json,
       (SELECT COUNT(*) FROM runs AS child WHERE child.parent_id = runs.id) AS children_count
FROM runs";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunSummarySort {
    #[default]
    CreatedAt,
    UpdatedAt,
    Status,
    Elapsed,
    #[serde(rename = "repo")]
    Repository,
    Title,
    Workflow,
    Changes,
    Size,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunSummarySortDirection {
    Asc,
    #[default]
    Desc,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunSummaryVisibility {
    All,
    Default {
        include_archived: bool,
    },
    Selected {
        statuses: Vec<RunStatusKind>,
        archived: bool,
    },
}

impl Default for RunSummaryVisibility {
    fn default() -> Self {
        Self::Default {
            include_archived: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSummaryListQuery {
    pub parent_id:     Option<RunId>,
    pub automation_id: Option<String>,
    pub visibility:    RunSummaryVisibility,
    pub sort:          RunSummarySort,
    pub direction:     RunSummarySortDirection,
    pub limit:         u32,
    pub offset:        u32,
}

impl Default for RunSummaryListQuery {
    fn default() -> Self {
        Self {
            parent_id:     None,
            automation_id: None,
            visibility:    RunSummaryVisibility::default(),
            sort:          RunSummarySort::default(),
            direction:     RunSummarySortDirection::default(),
            limit:         100,
            offset:        0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunSummaryPage {
    pub data:     Vec<Run>,
    pub total:    u64,
    pub has_more: bool,
}

#[derive(Clone)]
pub struct RunSummaryStore {
    pool:          SqlitePool,
    /// Called after a platform record of a run is committed: the
    /// projector's wake-up.
    platform_hook: Arc<RwLock<Option<PlatformRecordHook>>>,
}

impl std::fmt::Debug for RunSummaryStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunSummaryStore").finish_non_exhaustive()
    }
}

impl RunSummaryStore {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
            platform_hook: Arc::new(RwLock::new(None)),
        }
    }

    /// The pool this store's tables live in: the `runs` row, the platform
    /// records and the Petri projection tables. The server's one database;
    /// a test fixture's own.
    #[must_use]
    pub fn pool(&self) -> SqlitePool {
        self.pool.clone()
    }

    /// The platform records over the same pool.
    #[must_use]
    pub fn platform_records(&self) -> PlatformRecordStore {
        PlatformRecordStore::new(self.pool.clone())
    }

    /// Install the wake-up called after a platform record of a run is
    /// committed.
    pub fn set_platform_record_hook(&self, hook: PlatformRecordHook) {
        *self
            .platform_hook
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Some(hook);
    }

    /// Wake the run's projector: a platform record was committed for the run.
    pub fn notify_platform_record(&self, run_id: RunId) {
        let hook = self
            .platform_hook
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(hook) = hook {
            hook(run_id);
        }
    }

    /// The stored projection of a run, as the run's projector last
    /// committed it, or `None` when no view pass has run for it yet.
    pub async fn load_petri_projection(
        &self,
        run_id: &RunId,
    ) -> Result<Option<Arc<RunProjection>>> {
        let json: Option<String> =
            sqlx::query_scalar("SELECT projection_json FROM petri_projection WHERE run_id = ?")
                .bind(run_id.to_string())
                .fetch_optional(&self.pool)
                .await?;
        json.map(|json| Ok(Arc::new(serde_json::from_str(&json)?)))
            .transpose()
    }

    /// Write the `runs` row of a run from its projection, on a connection
    /// the caller holds a transaction on: the columns the list views and
    /// the scheduler read, and the summary JSON.
    pub async fn write_petri_run_row_on_connection(
        connection: &mut SqliteConnection,
        run_id: &RunId,
        projection: &RunProjection,
    ) -> Result<()> {
        let record = PreparedRunSummary::from_projection(run_id, projection);
        bind_run_columns(
            sqlx::query(UPSERT_PETRI_RUN_SQL).bind(run_id.to_string()),
            &record,
        )?
        .execute(connection)
        .await?;
        Ok(())
    }

    /// Whether a run with `run_id` is stored.
    pub async fn contains(&self, run_id: &RunId) -> Result<bool> {
        Ok(
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM runs WHERE id = ?)")
                .bind(run_id.to_string())
                .fetch_one(&self.pool)
                .await?,
        )
    }

    pub(crate) async fn delete_canonical(&self, run_id: &RunId) -> Result<()> {
        // Reserve the write lock up front: a deferred transaction upgraded
        // while another writer is active can fail at once, bypassing the
        // configured busy timeout, so concurrent deletes wait normally.
        let mut transaction = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query("DELETE FROM runs WHERE id = ?")
            .bind(run_id.to_string())
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn get(&self, run_id: &RunId, now: DateTime<Utc>) -> Result<Option<Run>> {
        let mut query = QueryBuilder::<Sqlite>::new(SELECT_RUN_SUMMARIES_SQL);
        query
            .push(" WHERE runs.id = ")
            .push_bind(run_id.to_string());
        let row = query.build().fetch_optional(&self.pool).await?;
        row.map(|row| decode_run_row(&row, now)).transpose()
    }

    /// Every current run row, without the bounded HTTP-list visibility or
    /// pagination semantics.
    pub async fn list_all(&self, now: DateTime<Utc>) -> Result<Vec<Run>> {
        let mut query = QueryBuilder::<Sqlite>::new(SELECT_RUN_SUMMARIES_SQL);
        push_order(
            &mut query,
            RunSummarySort::CreatedAt,
            RunSummarySortDirection::Desc,
            now,
        );
        let rows = query.build().fetch_all(&self.pool).await?;
        decode_run_rows(&rows, now)
    }

    /// Every current run row whose durable status matches one of `statuses`.
    pub async fn list_by_statuses(
        &self,
        statuses: &[RunStatusKind],
        now: DateTime<Utc>,
    ) -> Result<Vec<Run>> {
        if statuses.is_empty() {
            return Ok(Vec::new());
        }
        let mut query = QueryBuilder::<Sqlite>::new(SELECT_RUN_SUMMARIES_SQL);
        query.push(" WHERE status IN (");
        let mut separated = query.separated(", ");
        for status in statuses {
            separated.push_bind(status.to_string());
        }
        separated.push_unseparated(")");
        push_order(
            &mut query,
            RunSummarySort::CreatedAt,
            RunSummarySortDirection::Desc,
            now,
        );
        let rows = query.build().fetch_all(&self.pool).await?;
        decode_run_rows(&rows, now)
    }

    /// Run ids whose latest pull request creation request has no later
    /// record that resolves it. A newer request supersedes the old one;
    /// `created`, `linked` and `unlinked` resolve any pending request;
    /// `failed` resolves only the request whose creation id it names.
    /// Callers still read each candidate's projection to confirm, so this
    /// must never omit a genuinely pending run, but it keeps the set
    /// bounded by in-flight requests rather than by every run that ever
    /// asked for a pull request.
    pub async fn list_pull_request_creation_candidate_run_ids(&self) -> Result<Vec<RunId>> {
        sqlx::query_scalar::<_, String>(
            "SELECT DISTINCT requested.run_id FROM platform_records AS requested \
             WHERE requested.kind = 'pull_request.requested' \
               AND NOT EXISTS ( \
                 SELECT 1 FROM platform_records AS later \
                 WHERE later.run_id = requested.run_id \
                   AND later.seq > requested.seq \
                   AND ( \
                     later.kind IN ( \
                       'pull_request.requested', \
                       'pull_request.created', \
                       'pull_request.linked', \
                       'pull_request.unlinked' \
                     ) \
                     OR ( \
                       later.kind = 'pull_request.failed' \
                       AND json_extract(later.record_json, '$.creation_id') \
                         = json_extract(requested.record_json, '$.creation_id') \
                     ) \
                   ) \
               )",
        )
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(parse_stored_run_id)
        .collect()
    }

    /// Identity fields for every stored run, for selector resolution without
    /// decoding full summaries.
    pub async fn list_identities(&self) -> Result<Vec<RunSummaryIdentity>> {
        let rows = sqlx::query(
            r"
SELECT id, workflow_slug,
       json_extract(summary_json, '$.workflow.name') AS workflow_name,
       json_extract(summary_json, '$.repository.origin_url') AS repository_origin_url
FROM runs",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                let stored_id: String = row.try_get("id")?;
                let id = parse_stored_run_id(stored_id)?;
                Ok(RunSummaryIdentity {
                    id,
                    workflow_slug: row.try_get("workflow_slug")?,
                    workflow_name: row.try_get("workflow_name")?,
                    repository_origin_url: row.try_get("repository_origin_url")?,
                })
            })
            .collect()
    }

    pub async fn list(
        &self,
        query: &RunSummaryListQuery,
        now: DateTime<Utc>,
    ) -> Result<RunSummaryPage> {
        let mut transaction = self.pool.begin().await?;

        let mut count_query = QueryBuilder::<Sqlite>::new("SELECT COUNT(*) FROM runs");
        push_filters(&mut count_query, query);
        let total: i64 = count_query
            .build_query_scalar()
            .fetch_one(&mut *transaction)
            .await?;

        let mut rows_query = QueryBuilder::<Sqlite>::new(SELECT_RUN_SUMMARIES_SQL);
        push_filters(&mut rows_query, query);
        push_order(&mut rows_query, query.sort, query.direction, now);
        rows_query.push(" LIMIT ").push_bind(i64::from(query.limit));
        rows_query
            .push(" OFFSET ")
            .push_bind(i64::from(query.offset));
        let rows = rows_query.build().fetch_all(&mut *transaction).await?;
        transaction.commit().await?;

        let data = decode_run_rows(&rows, now)?;
        let total = u64::try_from(total).expect("COUNT(*) is non-negative");
        let consumed = u64::from(query.offset).saturating_add(data.len() as u64);
        Ok(RunSummaryPage {
            data,
            total,
            has_more: consumed < total,
        })
    }
}

/// Identity fields of a stored run summary, cheap to list for selector
/// resolution.
#[derive(Debug, Clone)]
pub struct RunSummaryIdentity {
    pub id:                    RunId,
    pub workflow_slug:         Option<String>,
    pub workflow_name:         Option<String>,
    pub repository_origin_url: Option<String>,
}

#[derive(Debug)]
struct PreparedRunSummary {
    run:              Run,
    workflow_name:    Option<String>,
    repository_name:  Option<String>,
    total_usd_micros: Option<i64>,
}

impl PreparedRunSummary {
    fn from_projection(run_id: &RunId, projection: &RunProjection) -> Self {
        let mut run = build_summary(projection, run_id);
        if run.timing.is_none() {
            let at = run
                .timestamps
                .last_event_at
                .unwrap_or(run.timestamps.created_at);
            run.timing = projection.live_run_timing(at);
        }
        let usage = projected_usage(projection);
        let workflow_name = run.workflow.display_name().map(str::to_string);
        let repository_name = run
            .repository
            .as_ref()
            .map(|repository| repository.name.clone());

        Self {
            run,
            workflow_name,
            repository_name,
            total_usd_micros: usage.cost.map(|cost| column_count(cost.usd_micros)),
        }
    }
}

/// A usage count as the SQLite read model stores it: the columns are signed,
/// so a count past `i64::MAX` saturates rather than wrapping negative.
fn column_count(count: u64) -> i64 {
    i64::try_from(count).unwrap_or(i64::MAX)
}

/// Binds the `runs` columns after `id`, in the positional order the upsert
/// declares them (`created_at_ms` through `summary_json`).
fn bind_run_columns<'q>(
    query: Query<'q, Sqlite, SqliteArguments>,
    record: &'q PreparedRunSummary,
) -> Result<Query<'q, Sqlite, SqliteArguments>> {
    let run = &record.run;
    let diff = run.diff.unwrap_or_default();
    let summary_json = serde_json::to_string(run)?;
    Ok(query
        .bind(run.timestamps.created_at.timestamp_millis())
        .bind(
            run.timestamps
                .started_at
                .map(|value| value.timestamp_millis()),
        )
        .bind(
            run.timestamps
                .last_event_at
                .unwrap_or(run.timestamps.created_at)
                .timestamp_millis(),
        )
        .bind(
            run.timestamps
                .completed_at
                .map(|value| value.timestamp_millis()),
        )
        .bind(run.lifecycle.status.kind().to_string())
        .bind(
            run.lifecycle
                .archived_at
                .map(|value| value.timestamp_millis()),
        )
        .bind(run.parent_id.map(|value| value.to_string()))
        .bind(&run.title)
        .bind(&run.workflow.slug)
        .bind(&record.workflow_name)
        .bind(&record.repository_name)
        .bind(run.automation.as_ref().map(|automation| &automation.id))
        .bind(diff.additions)
        .bind(diff.deletions)
        .bind(record.total_usd_micros)
        .bind(summary_json))
}

fn push_filters(builder: &mut QueryBuilder<Sqlite>, query: &RunSummaryListQuery) {
    builder.push(" WHERE 1 = 1");
    if let Some(parent_id) = query.parent_id {
        builder
            .push(" AND parent_id = ")
            .push_bind(parent_id.to_string());
    }
    if let Some(automation_id) = &query.automation_id {
        builder
            .push(" AND automation_id = ")
            .push_bind(automation_id.clone());
    }

    match &query.visibility {
        RunSummaryVisibility::All => {}
        RunSummaryVisibility::Default { include_archived } => {
            let not_removing = format!("status <> '{}'", RunStatusKind::Removing);
            if *include_archived {
                builder.push(format!(
                    " AND (archived_at_ms IS NOT NULL OR {not_removing})"
                ));
            } else {
                builder.push(format!(" AND archived_at_ms IS NULL AND {not_removing}"));
            }
        }
        RunSummaryVisibility::Selected { statuses, archived } => {
            builder.push(" AND (");
            let mut has_condition = false;
            if *archived {
                builder.push("archived_at_ms IS NOT NULL");
                has_condition = true;
            }
            if !statuses.is_empty() {
                if has_condition {
                    builder.push(" OR ");
                }
                builder.push("(archived_at_ms IS NULL AND status IN (");
                let mut separated = builder.separated(", ");
                for status in statuses {
                    separated.push_bind(status.to_string());
                }
                separated.push_unseparated("))");
                has_condition = true;
            }
            if !has_condition {
                builder.push("0");
            }
            builder.push(")");
        }
    }
}

/// Status sort rank derived from [`RunStatusKind::board_rank`], so the SQL
/// order and the board column order share one source. Archived runs rank 7,
/// matching the `archived` board column.
static STATUS_RANK_CASE_SQL: LazyLock<String> = LazyLock::new(|| {
    let mut case = String::from("CASE WHEN archived_at_ms IS NOT NULL THEN 7");
    for kind in RunStatusKind::VARIANTS {
        let _ = write!(case, " WHEN status = '{kind}' THEN {}", kind.board_rank());
    }
    case.push_str(" ELSE 9 END");
    case
});

/// Size sort rank derived from [`RunSize::BUCKET_MAX_USD_MICROS`], so the SQL
/// order and the displayed size buckets share one source.
static SIZE_RANK_CASE_SQL: LazyLock<String> = LazyLock::new(|| {
    let mut case = String::from("CASE");
    for (rank, (_, max_usd_micros)) in RunSize::BUCKET_MAX_USD_MICROS.iter().enumerate() {
        let _ = write!(
            case,
            " WHEN COALESCE(total_usd_micros, 0) <= {max_usd_micros} THEN {rank}"
        );
    }
    let _ = write!(case, " ELSE {} END", RunSize::BUCKET_MAX_USD_MICROS.len());
    case
});

fn push_order(
    builder: &mut QueryBuilder<Sqlite>,
    sort: RunSummarySort,
    direction: RunSummarySortDirection,
    now: DateTime<Utc>,
) {
    builder.push(" ORDER BY ");
    match sort {
        RunSummarySort::CreatedAt => builder.push("created_at_ms"),
        RunSummarySort::UpdatedAt => builder.push("last_event_at_ms"),
        RunSummarySort::Status => builder.push(STATUS_RANK_CASE_SQL.as_str()),
        RunSummarySort::Elapsed => builder
            .push("(COALESCE(completed_at_ms, ")
            .push_bind(now.timestamp_millis())
            .push(") - COALESCE(started_at_ms, created_at_ms))"),
        RunSummarySort::Repository => builder.push("COALESCE(repository_name, '') COLLATE NOCASE"),
        RunSummarySort::Title => builder.push("TRIM(title) COLLATE NOCASE"),
        RunSummarySort::Workflow => builder.push("COALESCE(workflow_name, '') COLLATE NOCASE"),
        RunSummarySort::Changes => builder.push("(diff_additions + diff_deletions)"),
        RunSummarySort::Size => builder.push(SIZE_RANK_CASE_SQL.as_str()),
    };
    match direction {
        RunSummarySortDirection::Asc => builder.push(" ASC"),
        RunSummarySortDirection::Desc => builder.push(" DESC"),
    };
    builder.push(", id DESC");
}

fn parse_stored_run_id(stored_id: String) -> Result<RunId> {
    stored_id
        .parse::<RunId>()
        .map_err(|_| Error::RunSummaryMismatch {
            run_id: stored_id,
            field:  "id",
        })
}

fn decode_run_row(row: &SqliteRow, now: DateTime<Utc>) -> Result<Run> {
    let stored_id: String = row.try_get("id")?;
    let summary_json: String = row.try_get("summary_json")?;
    let children_count: i64 = row.try_get("children_count")?;
    let mut run: Run = serde_json::from_str(&summary_json)?;
    if stored_id != run.id.to_string() {
        return Err(Error::RunSummaryMismatch {
            run_id: stored_id,
            field:  "id",
        });
    }
    run.children_count = u64::try_from(children_count).map_err(|_| Error::RunSummaryMismatch {
        run_id: run.id.to_string(),
        field:  "children_count",
    })?;
    overlay_live_wall_time(&mut run, now);
    Ok(run)
}

fn decode_run_rows(rows: &[SqliteRow], now: DateTime<Utc>) -> Result<Vec<Run>> {
    rows.iter().map(|row| decode_run_row(row, now)).collect()
}

fn overlay_live_wall_time(run: &mut Run, now: DateTime<Utc>) {
    if run.timestamps.completed_at.is_some() {
        return;
    }
    let Some(started_at) = run.timestamps.started_at else {
        return;
    };
    let wall_time_ms = timing::elapsed_ms(started_at, now);
    run.timing = Some(
        run.timing
            .unwrap_or_else(|| RunTiming::wall_only(wall_time_ms))
            .with_wall_time(wall_time_ms),
    );
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::Duration;

    use chrono::{DateTime, Utc};
    use fabro_types::{
        AutomationRef, BlockedReason, Conclusion, DiffSummary, FailureReason, Graph, PendingReason,
        PetriAdmission, PullRequestCreationId, RunDiff, RunId, RunProjection, RunSize, RunSpec,
        RunStatus, RunStatusKind, RunTiming, StageOutcome, SuccessReason, WorkflowSettings,
        test_support,
    };
    use lithos_llm::types::{Cost, CostSource, TokenCounts, Usage};
    use strum::VariantArray as _;
    use tokio::time;
    use ulid::Ulid;

    use super::{
        RunSummaryListQuery, RunSummarySort, RunSummarySortDirection, RunSummaryStore,
        RunSummaryVisibility,
    };
    use crate::platform_records::{
        PlatformRecord, PullRequestCreatedRecord, PullRequestFailedRecord,
        PullRequestRequestedRecord,
    };
    use crate::test_support as store_test_support;

    fn dt(value: &str) -> DateTime<Utc> {
        value.parse().unwrap()
    }

    fn run_id(timestamp_ms: u64, random: u128) -> RunId {
        RunId::from(Ulid::from_parts(timestamp_ms, random))
    }

    fn projection(run_id: RunId, title: &str, created_at: DateTime<Utc>) -> RunProjection {
        RunProjection::new(
            title.to_string(),
            RunSpec {
                run_id,
                settings: WorkflowSettings::default(),
                graph: Graph::new("test"),
                graph_source: None,
                workflow_slug: Some("test-workflow".to_string()),
                workflow_version_id: None,
                target: None,
                automation: None,
                source_directory: None,
                labels: HashMap::new(),
                provenance: test_support::test_run_provenance(),
                definition_blob: None,
                spec_blob: None,
                git: None,
                fork_source_ref: None,
                admission: PetriAdmission::default(),
            },
            created_at,
        )
    }

    async fn store() -> (tempfile::TempDir, RunSummaryStore) {
        store_test_support::sqlite_run_summary_store().await
    }

    /// Write the run's row as its projector does.
    async fn write(store: &RunSummaryStore, projection: &RunProjection) {
        let mut transaction = store.pool.begin().await.unwrap();
        RunSummaryStore::write_petri_run_row_on_connection(
            &mut transaction,
            &projection.spec.run_id,
            projection,
        )
        .await
        .unwrap();
        transaction.commit().await.unwrap();
    }

    fn sample_status(kind: RunStatusKind) -> RunStatus {
        match kind {
            RunStatusKind::Submitted => RunStatus::Submitted,
            RunStatusKind::Pending => RunStatus::Pending {
                reason: PendingReason::ApprovalRequired,
            },
            RunStatusKind::Runnable => RunStatus::Runnable,
            RunStatusKind::Starting => RunStatus::Starting,
            RunStatusKind::Running => RunStatus::Running,
            RunStatusKind::Blocked => RunStatus::Blocked {
                blocked_reason: BlockedReason::HumanInputRequired,
            },
            RunStatusKind::Paused => RunStatus::Paused { prior_block: None },
            RunStatusKind::Removing => RunStatus::Removing,
            RunStatusKind::Succeeded => RunStatus::Succeeded {
                reason: SuccessReason::Completed,
            },
            RunStatusKind::Failed => RunStatus::Failed {
                reason: FailureReason::WorkflowError,
            },
            RunStatusKind::Dead => RunStatus::Dead,
        }
    }

    #[tokio::test]
    async fn every_status_kind_writes_within_schema_check() {
        let (_directory, store) = store().await;
        let created_at = dt("2026-07-11T12:00:00Z");
        for (index, kind) in RunStatusKind::VARIANTS.iter().enumerate() {
            let id = run_id(
                created_at.timestamp_millis().cast_unsigned(),
                u128::try_from(index).unwrap() + 1,
            );
            let mut projected = projection(id, "status", created_at);
            projected.status = sample_status(*kind);
            write(&store, &projected).await;
        }
    }

    #[tokio::test]
    async fn a_rewrite_replaces_the_row_and_get_applies_children_count() {
        let (_directory, store) = store().await;
        let created_at = dt("2026-07-11T12:00:00Z");
        let parent_id = run_id(created_at.timestamp_millis().cast_unsigned(), 1);
        let child_id = run_id(created_at.timestamp_millis().cast_unsigned() + 1, 2);

        write(&store, &projection(parent_id, "parent", created_at)).await;
        let mut child = projection(child_id, "first title", created_at);
        child.parent_id = Some(parent_id);
        write(&store, &child).await;
        child.title = "new title".to_string();
        child.last_event_at = created_at + chrono::Duration::seconds(2);
        write(&store, &child).await;

        let parent = store.get(&parent_id, created_at).await.unwrap().unwrap();
        let child = store.get(&child_id, created_at).await.unwrap().unwrap();
        assert_eq!(parent.children_count, 1);
        assert_eq!(child.title, "new title");
        assert!(store.contains(&child_id).await.unwrap());
        assert!(!store.contains(&run_id(1, 99)).await.unwrap());
    }

    #[tokio::test]
    async fn list_filters_sorts_and_paginates_in_sqlite() {
        let (_directory, store) = store().await;
        let created_at = dt("2026-07-11T12:00:00Z");
        let first_id = run_id(created_at.timestamp_millis().cast_unsigned(), 1);
        let second_id = run_id(created_at.timestamp_millis().cast_unsigned() + 1, 2);
        let archived_id = run_id(created_at.timestamp_millis().cast_unsigned() + 2, 3);

        let mut first = projection(first_id, "bravo", created_at);
        first.spec.automation = Some(AutomationRef {
            id:              "nightly".to_string(),
            name:            None,
            trigger_id:      None,
            workflow_source: None,
        });
        let mut second = projection(second_id, "alpha", created_at);
        second.spec.automation = Some(AutomationRef {
            id:              "nightly".to_string(),
            name:            None,
            trigger_id:      None,
            workflow_source: None,
        });
        let mut archived = projection(archived_id, "charlie", created_at);
        archived.archived_at = Some(created_at);
        for projected in [first, second, archived] {
            write(&store, &projected).await;
        }

        let page = store
            .list(
                &RunSummaryListQuery {
                    automation_id: Some("nightly".to_string()),
                    sort: RunSummarySort::Title,
                    direction: RunSummarySortDirection::Asc,
                    limit: 1,
                    ..RunSummaryListQuery::default()
                },
                created_at,
            )
            .await
            .unwrap();
        assert_eq!(page.total, 2);
        assert!(page.has_more);
        assert_eq!(page.data[0].title, "alpha");

        let archived = store
            .list(
                &RunSummaryListQuery {
                    visibility: RunSummaryVisibility::Selected {
                        statuses: Vec::new(),
                        archived: true,
                    },
                    ..RunSummaryListQuery::default()
                },
                created_at,
            )
            .await
            .unwrap();
        assert_eq!(archived.data.len(), 1);
        assert_eq!(archived.data[0].id, archived_id);
    }

    #[tokio::test]
    async fn list_all_is_unbounded_complete_and_newest_first() {
        let (_directory, store) = store().await;
        let created_at = dt("2026-07-11T12:00:00Z");
        let parent_id = run_id(created_at.timestamp_millis().cast_unsigned(), 1);
        let child_id = run_id(created_at.timestamp_millis().cast_unsigned() + 1, 2);
        let archived_id = run_id(created_at.timestamp_millis().cast_unsigned() + 2, 3);
        let removing_id = run_id(created_at.timestamp_millis().cast_unsigned() + 3, 4);
        let tied_low_id = run_id(created_at.timestamp_millis().cast_unsigned() + 4, 5);
        let tied_high_id = run_id(created_at.timestamp_millis().cast_unsigned() + 4, 6);

        let mut projections = vec![projection(parent_id, "parent", created_at)];
        let mut child = projection(
            child_id,
            "child",
            created_at + chrono::Duration::milliseconds(1),
        );
        child.parent_id = Some(parent_id);
        projections.push(child);
        let mut archived = projection(
            archived_id,
            "archived",
            created_at + chrono::Duration::milliseconds(2),
        );
        archived.archived_at = Some(created_at + chrono::Duration::milliseconds(2));
        projections.push(archived);
        let mut removing = projection(
            removing_id,
            "removing",
            created_at + chrono::Duration::milliseconds(3),
        );
        removing.status = RunStatus::Removing;
        projections.push(removing);
        projections.push(projection(
            tied_low_id,
            "tied-low",
            created_at + chrono::Duration::milliseconds(4),
        ));
        projections.push(projection(
            tied_high_id,
            "tied-high",
            created_at + chrono::Duration::milliseconds(4),
        ));
        for index in 0_u64..101 {
            let timestamp_ms = created_at.timestamp_millis().cast_unsigned() + 10 + index;
            projections.push(projection(
                run_id(timestamp_ms, u128::from(index) + 10),
                "bulk",
                DateTime::from_timestamp_millis(timestamp_ms.cast_signed()).unwrap(),
            ));
        }

        let mut expected_ids = Vec::new();
        for projected in projections {
            expected_ids.push(projected.spec.run_id);
            write(&store, &projected).await;
        }
        expected_ids.sort_by(|left, right| {
            right
                .created_at()
                .cmp(&left.created_at())
                .then_with(|| right.cmp(left))
        });

        let listed = store.list_all(created_at).await.unwrap();
        assert_eq!(listed.len(), 107);
        assert_eq!(
            listed.iter().map(|run| run.id).collect::<Vec<_>>(),
            expected_ids
        );
        assert!(listed.iter().any(|run| run.id == archived_id));
        assert!(listed.iter().any(|run| run.id == removing_id));
        assert_eq!(
            listed
                .iter()
                .find(|run| run.id == parent_id)
                .unwrap()
                .children_count,
            1
        );
        let tied = listed
            .iter()
            .filter(|run| run.id == tied_low_id || run.id == tied_high_id)
            .map(|run| run.id)
            .collect::<Vec<_>>();
        assert_eq!(tied, vec![tied_high_id, tied_low_id]);
    }

    #[tokio::test]
    async fn list_by_statuses_is_exact_and_empty_is_empty() {
        let (_directory, store) = store().await;
        let created_at = dt("2026-07-11T12:00:00Z");
        for (index, kind) in RunStatusKind::VARIANTS.iter().enumerate() {
            let id = run_id(
                created_at.timestamp_millis().cast_unsigned() + u64::try_from(index).unwrap(),
                u128::try_from(index).unwrap() + 1,
            );
            let mut projected = projection(id, &kind.to_string(), id.created_at());
            projected.status = sample_status(*kind);
            write(&store, &projected).await;
        }

        let startup_statuses = [
            RunStatusKind::Starting,
            RunStatusKind::Running,
            RunStatusKind::Blocked,
            RunStatusKind::Paused,
            RunStatusKind::Removing,
        ];
        let listed = store
            .list_by_statuses(&startup_statuses, created_at)
            .await
            .unwrap();
        assert_eq!(
            listed
                .iter()
                .map(|run| run.lifecycle.status.kind())
                .collect::<std::collections::HashSet<_>>(),
            startup_statuses.into_iter().collect()
        );
        assert_eq!(listed.len(), startup_statuses.len());
        assert!(
            store
                .list_by_statuses(&[], created_at)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn pull_request_creation_candidates_are_the_runs_with_an_unresolved_request() {
        let (_directory, store) = store().await;
        let created_at = dt("2026-08-27T12:00:00Z");
        let pending_id = run_id(created_at.timestamp_millis().cast_unsigned(), 1);
        let created_id = run_id(created_at.timestamp_millis().cast_unsigned() + 1, 2);
        let failed_id = run_id(created_at.timestamp_millis().cast_unsigned() + 2, 3);
        let renewed_id = run_id(created_at.timestamp_millis().cast_unsigned() + 3, 4);
        let records = store.platform_records();
        let requested = |creation_id: PullRequestCreationId| {
            PlatformRecord::PullRequestRequested(PullRequestRequestedRecord {
                creation_id,
                model: "test-model".to_string(),
                force: false,
            })
        };
        let first = PullRequestCreationId::new();
        let second = PullRequestCreationId::new();

        // A request with nothing after it is pending.
        records
            .append(&pending_id, &requested(first), None)
            .await
            .unwrap();
        // A created pull request resolves the request before it.
        records
            .append(&created_id, &requested(first), None)
            .await
            .unwrap();
        records
            .append(
                &created_id,
                &PlatformRecord::PullRequestCreated(PullRequestCreatedRecord {
                    number:    7,
                    owner:     "acme".to_string(),
                    repo:      "widgets".to_string(),
                    html_url:  "https://github.com/acme/widgets/pull/7".to_string(),
                    head_sha:  None,
                    draft:     false,
                    operation: None,
                }),
                None,
            )
            .await
            .unwrap();
        // A failure resolves only the request it names.
        records
            .append(&failed_id, &requested(first), None)
            .await
            .unwrap();
        records
            .append(
                &failed_id,
                &PlatformRecord::PullRequestFailed(PullRequestFailedRecord {
                    creation_id: Some(second),
                    error:       "boom".to_string(),
                }),
                None,
            )
            .await
            .unwrap();
        // A newer request supersedes the old one and is itself pending.
        records
            .append(&renewed_id, &requested(first), None)
            .await
            .unwrap();
        records
            .append(&renewed_id, &requested(second), None)
            .await
            .unwrap();

        let mut candidates = store
            .list_pull_request_creation_candidate_run_ids()
            .await
            .unwrap();
        candidates.sort();
        let mut expected = vec![pending_id, failed_id, renewed_id];
        expected.sort();
        assert_eq!(candidates, expected);
    }

    #[tokio::test]
    async fn projection_persists_usage_diff_and_derived_size() {
        let (_directory, store) = store().await;
        let created_at = dt("2026-07-11T12:00:00Z");
        let run_id = run_id(created_at.timestamp_millis().cast_unsigned(), 1);
        let mut projection = projection(run_id, "billed", created_at);
        projection.spec.automation = Some(AutomationRef {
            id:              "nightly".to_string(),
            name:            None,
            trigger_id:      None,
            workflow_source: None,
        });
        projection.status = RunStatus::Succeeded {
            reason: SuccessReason::Completed,
        };
        projection.last_event_at = created_at + chrono::Duration::minutes(1);
        projection.conclusion = Some(Conclusion {
            timestamp:            projection.last_event_at,
            status:               StageOutcome::Succeeded,
            timing:               RunTiming::wall_only(60_000),
            failure:              None,
            final_git_commit_sha: None,
            stages:               Vec::new(),
            usage:                Some(Usage {
                tokens: TokenCounts {
                    input:       100,
                    output:      20,
                    reasoning:   5,
                    cache_read:  10,
                    cache_write: 0,
                },
                cost:   Some(Cost {
                    usd_micros: 21_000_000,
                    source:     CostSource::Catalog,
                }),
            }),
            total_retries:        0,
            diff:                 RunDiff {
                patch:   None,
                summary: Some(DiffSummary {
                    files_changed: 2,
                    additions:     10,
                    deletions:     3,
                }),
            },
        });
        write(&store, &projection).await;

        let row = sqlx::query(
            "SELECT created_at_ms, last_event_at_ms, status, title, workflow_slug, \
             automation_id, total_usd_micros, diff_additions, diff_deletions \
             FROM runs WHERE id = ?",
        )
        .bind(run_id.to_string())
        .fetch_one(&store.pool)
        .await
        .unwrap();
        assert_eq!(
            sqlx::Row::get::<i64, _>(&row, "created_at_ms"),
            created_at.timestamp_millis()
        );
        assert_eq!(
            sqlx::Row::get::<i64, _>(&row, "last_event_at_ms"),
            (created_at + chrono::Duration::minutes(1)).timestamp_millis()
        );
        assert_eq!(sqlx::Row::get::<String, _>(&row, "status"), "succeeded");
        assert_eq!(sqlx::Row::get::<String, _>(&row, "title"), "billed");
        assert_eq!(
            sqlx::Row::get::<String, _>(&row, "workflow_slug"),
            "test-workflow"
        );
        assert_eq!(
            sqlx::Row::get::<String, _>(&row, "automation_id"),
            "nightly"
        );
        assert_eq!(
            sqlx::Row::get::<i64, _>(&row, "total_usd_micros"),
            21_000_000
        );
        assert_eq!(sqlx::Row::get::<i64, _>(&row, "diff_additions"), 10);
        assert_eq!(sqlx::Row::get::<i64, _>(&row, "diff_deletions"), 3);

        let run = store.get(&run_id, created_at).await.unwrap().unwrap();
        assert_eq!(run.size, RunSize::S);
    }

    #[tokio::test]
    async fn canonical_delete_waits_for_a_concurrent_writer() {
        let (_directory, store) = store().await;
        let created_at = dt("2026-08-27T12:00:00Z");
        let id = run_id(created_at.timestamp_millis().cast_unsigned(), 5);
        write(&store, &projection(id, "created", created_at)).await;

        let blocker = store.pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let contender = store.clone();
        let delete = tokio::spawn(async move { contender.delete_canonical(&id).await });
        time::sleep(Duration::from_millis(25)).await;
        assert!(
            !delete.is_finished(),
            "delete should wait for the existing writer"
        );

        blocker.commit().await.unwrap();
        delete.await.unwrap().unwrap();
        assert!(!store.contains(&id).await.unwrap());
    }
}
