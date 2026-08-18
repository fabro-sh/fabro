use chrono::{DateTime, Utc};
use fabro_db::DbPool;
use fabro_types::{ExternalAgentHarness, PlaneDispatch, PlaneDispatchStatus};
use sqlx::Row as _;
use sqlx::sqlite::SqliteRow;

use crate::{AutomationId, AutomationTriggerId};

#[derive(Debug, thiserror::Error)]
pub enum PlaneDispatchStoreError {
    #[error(
        "plane dispatch not found for automation {automation_id}, trigger {trigger_id}, issue {issue_id}"
    )]
    NotFound {
        automation_id: String,
        trigger_id:    String,
        issue_id:      String,
    },
    #[error("stored plane dispatch row is invalid")]
    InvalidRow {
        #[source]
        source: anyhow::Error,
    },
    #[error("database error")]
    Db {
        #[from]
        source: sqlx::Error,
    },
}

/// Durable side-effect flags so Plane comments/state writes stay idempotent.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PlaneDispatchEffects {
    pub claimed_state_applied:    bool,
    pub claim_comment_posted:     bool,
    pub success_state_applied:    bool,
    pub success_comment_posted:   bool,
    pub failure_comment_posted:   bool,
    pub failure_label_applied:    bool,
    pub cancelled_state_applied:  bool,
    pub cancelled_comment_posted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaneDispatchRecord {
    pub dispatch: PlaneDispatch,
    pub effects:  PlaneDispatchEffects,
}

#[derive(Clone)]
pub struct PlaneDispatchStore {
    pool: DbPool,
}

impl std::fmt::Debug for PlaneDispatchStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlaneDispatchStore").finish_non_exhaustive()
    }
}

impl PlaneDispatchStore {
    #[must_use]
    pub fn new(pool: DbPool) -> Self {
        Self { pool }
    }

    /// Compare-and-swap insert. Returns the stored record; `created` is false
    /// on conflict.
    pub async fn create_pending(
        &self,
        record: &PlaneDispatchRecord,
    ) -> Result<(PlaneDispatchRecord, bool), PlaneDispatchStoreError> {
        let result = sqlx::query(
            r"
            INSERT INTO plane_dispatches (
                automation_id, trigger_id, issue_id, issue_identifier, issue_title, issue_url,
                status, harness, attempt, run_ids, current_run_id, pull_request_url, last_error,
                claimed_at, completed_at, created_at, updated_at,
                claimed_state_applied, claim_comment_posted, success_state_applied,
                success_comment_posted, failure_comment_posted, failure_label_applied,
                cancelled_state_applied, cancelled_comment_posted
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(automation_id, trigger_id, issue_id) DO NOTHING
            ",
        )
        .bind(&record.dispatch.automation_id)
        .bind(&record.dispatch.trigger_id)
        .bind(&record.dispatch.issue_id)
        .bind(&record.dispatch.issue_identifier)
        .bind(record.dispatch.issue_title.as_deref())
        .bind(record.dispatch.issue_url.as_deref())
        .bind(record.dispatch.status.as_str())
        .bind(record.dispatch.harness.as_str())
        .bind(i64::from(record.dispatch.attempt))
        .bind(serde_json::to_string(&record.dispatch.run_ids).unwrap_or_else(|_| "[]".to_string()))
        .bind(record.dispatch.current_run_id.as_deref())
        .bind(record.dispatch.pull_request_url.as_deref())
        .bind(record.dispatch.last_error.as_deref())
        .bind(record.dispatch.claimed_at.map(|ts| ts.to_rfc3339()))
        .bind(record.dispatch.completed_at.map(|ts| ts.to_rfc3339()))
        .bind(record.dispatch.created_at.to_rfc3339())
        .bind(record.dispatch.updated_at.to_rfc3339())
        .bind(record.effects.claimed_state_applied)
        .bind(record.effects.claim_comment_posted)
        .bind(record.effects.success_state_applied)
        .bind(record.effects.success_comment_posted)
        .bind(record.effects.failure_comment_posted)
        .bind(record.effects.failure_label_applied)
        .bind(record.effects.cancelled_state_applied)
        .bind(record.effects.cancelled_comment_posted)
        .execute(&self.pool)
        .await?;

        let stored = self
            .get(
                &record.dispatch.automation_id,
                &record.dispatch.trigger_id,
                &record.dispatch.issue_id,
            )
            .await?
            .ok_or_else(|| PlaneDispatchStoreError::NotFound {
                automation_id: record.dispatch.automation_id.clone(),
                trigger_id:    record.dispatch.trigger_id.clone(),
                issue_id:      record.dispatch.issue_id.clone(),
            })?;
        Ok((stored, result.rows_affected() == 1))
    }

    pub async fn get(
        &self,
        automation_id: &str,
        trigger_id: &str,
        issue_id: &str,
    ) -> Result<Option<PlaneDispatchRecord>, PlaneDispatchStoreError> {
        let row = sqlx::query(
            r"
            SELECT * FROM plane_dispatches
            WHERE automation_id = ? AND trigger_id = ? AND issue_id = ?
            ",
        )
        .bind(automation_id)
        .bind(trigger_id)
        .bind(issue_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| record_from_row(&row)).transpose()
    }

    pub async fn list_for_automation(
        &self,
        automation_id: &AutomationId,
    ) -> Result<Vec<PlaneDispatchRecord>, PlaneDispatchStoreError> {
        let rows = sqlx::query(
            r"
            SELECT * FROM plane_dispatches
            WHERE automation_id = ?
            ORDER BY updated_at DESC
            ",
        )
        .bind(automation_id.as_str())
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(record_from_row).collect()
    }

    pub async fn list_nonterminal(
        &self,
        automation_id: &AutomationId,
        trigger_id: &AutomationTriggerId,
    ) -> Result<Vec<PlaneDispatchRecord>, PlaneDispatchStoreError> {
        let rows = sqlx::query(
            r"
            SELECT * FROM plane_dispatches
            WHERE automation_id = ? AND trigger_id = ?
              AND status IN ('pending', 'claimed', 'running', 'retry_pending')
            ORDER BY created_at
            ",
        )
        .bind(automation_id.as_str())
        .bind(trigger_id.as_str())
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(record_from_row).collect()
    }

    pub async fn list_existing_issue_ids(
        &self,
        automation_id: &AutomationId,
        trigger_id: &AutomationTriggerId,
    ) -> Result<Vec<String>, PlaneDispatchStoreError> {
        let rows = sqlx::query(
            r"
            SELECT issue_id FROM plane_dispatches
            WHERE automation_id = ? AND trigger_id = ?
            ",
        )
        .bind(automation_id.as_str())
        .bind(trigger_id.as_str())
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|row| row.try_get::<String, _>("issue_id"))
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub async fn save(&self, record: &PlaneDispatchRecord) -> Result<(), PlaneDispatchStoreError> {
        let result = sqlx::query(
            r"
            UPDATE plane_dispatches SET
                issue_identifier = ?,
                issue_title = ?,
                issue_url = ?,
                status = ?,
                harness = ?,
                attempt = ?,
                run_ids = ?,
                current_run_id = ?,
                pull_request_url = ?,
                last_error = ?,
                claimed_at = ?,
                completed_at = ?,
                updated_at = ?,
                claimed_state_applied = ?,
                claim_comment_posted = ?,
                success_state_applied = ?,
                success_comment_posted = ?,
                failure_comment_posted = ?,
                failure_label_applied = ?,
                cancelled_state_applied = ?,
                cancelled_comment_posted = ?
            WHERE automation_id = ? AND trigger_id = ? AND issue_id = ?
            ",
        )
        .bind(&record.dispatch.issue_identifier)
        .bind(record.dispatch.issue_title.as_deref())
        .bind(record.dispatch.issue_url.as_deref())
        .bind(record.dispatch.status.as_str())
        .bind(record.dispatch.harness.as_str())
        .bind(i64::from(record.dispatch.attempt))
        .bind(serde_json::to_string(&record.dispatch.run_ids).unwrap_or_else(|_| "[]".to_string()))
        .bind(record.dispatch.current_run_id.as_deref())
        .bind(record.dispatch.pull_request_url.as_deref())
        .bind(record.dispatch.last_error.as_deref())
        .bind(record.dispatch.claimed_at.map(|ts| ts.to_rfc3339()))
        .bind(record.dispatch.completed_at.map(|ts| ts.to_rfc3339()))
        .bind(record.dispatch.updated_at.to_rfc3339())
        .bind(record.effects.claimed_state_applied)
        .bind(record.effects.claim_comment_posted)
        .bind(record.effects.success_state_applied)
        .bind(record.effects.success_comment_posted)
        .bind(record.effects.failure_comment_posted)
        .bind(record.effects.failure_label_applied)
        .bind(record.effects.cancelled_state_applied)
        .bind(record.effects.cancelled_comment_posted)
        .bind(&record.dispatch.automation_id)
        .bind(&record.dispatch.trigger_id)
        .bind(&record.dispatch.issue_id)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(PlaneDispatchStoreError::NotFound {
                automation_id: record.dispatch.automation_id.clone(),
                trigger_id:    record.dispatch.trigger_id.clone(),
                issue_id:      record.dispatch.issue_id.clone(),
            });
        }
        Ok(())
    }
}

fn record_from_row(row: &SqliteRow) -> Result<PlaneDispatchRecord, PlaneDispatchStoreError> {
    let status = parse_status(row.try_get("status")?)?;
    let harness = parse_harness(row.try_get("harness")?)?;
    let run_ids = parse_run_ids(row.try_get("run_ids")?)?;
    Ok(PlaneDispatchRecord {
        dispatch: PlaneDispatch {
            automation_id: row.try_get("automation_id")?,
            trigger_id: row.try_get("trigger_id")?,
            issue_id: row.try_get("issue_id")?,
            issue_identifier: row.try_get("issue_identifier")?,
            issue_title: row.try_get("issue_title")?,
            issue_url: row.try_get("issue_url")?,
            status,
            harness,
            attempt: u32::try_from(row.try_get::<i64, _>("attempt")?).unwrap_or(1),
            run_ids,
            current_run_id: row.try_get("current_run_id")?,
            pull_request_url: row.try_get("pull_request_url")?,
            last_error: row.try_get("last_error")?,
            claimed_at: parse_optional_time(row.try_get("claimed_at")?)?,
            completed_at: parse_optional_time(row.try_get("completed_at")?)?,
            created_at: parse_time(row.try_get("created_at")?)?,
            updated_at: parse_time(row.try_get("updated_at")?)?,
        },
        effects:  PlaneDispatchEffects {
            claimed_state_applied:    row.try_get("claimed_state_applied")?,
            claim_comment_posted:     row.try_get("claim_comment_posted")?,
            success_state_applied:    row.try_get("success_state_applied")?,
            success_comment_posted:   row.try_get("success_comment_posted")?,
            failure_comment_posted:   row.try_get("failure_comment_posted")?,
            failure_label_applied:    row.try_get("failure_label_applied")?,
            cancelled_state_applied:  row.try_get("cancelled_state_applied")?,
            cancelled_comment_posted: row.try_get("cancelled_comment_posted")?,
        },
    })
}

fn parse_status(value: &str) -> Result<PlaneDispatchStatus, PlaneDispatchStoreError> {
    value
        .parse()
        .map_err(|source| PlaneDispatchStoreError::InvalidRow {
            source: anyhow::anyhow!("invalid plane dispatch status {value:?}: {source}"),
        })
}

fn parse_harness(value: &str) -> Result<ExternalAgentHarness, PlaneDispatchStoreError> {
    value
        .parse()
        .map_err(|source| PlaneDispatchStoreError::InvalidRow {
            source: anyhow::anyhow!("invalid plane dispatch harness {value:?}: {source}"),
        })
}

fn parse_run_ids(value: &str) -> Result<Vec<String>, PlaneDispatchStoreError> {
    serde_json::from_str(value).map_err(|source| PlaneDispatchStoreError::InvalidRow {
        source: anyhow::Error::new(source).context("invalid plane dispatch run_ids JSON"),
    })
}

fn parse_time(value: &str) -> Result<DateTime<Utc>, PlaneDispatchStoreError> {
    DateTime::parse_from_rfc3339(value)
        .map(|ts| ts.with_timezone(&Utc))
        .map_err(|source| PlaneDispatchStoreError::InvalidRow {
            source: anyhow::Error::new(source).context("invalid plane dispatch timestamp"),
        })
}

fn parse_optional_time(
    value: Option<&str>,
) -> Result<Option<DateTime<Utc>>, PlaneDispatchStoreError> {
    value.map(parse_time).transpose()
}
