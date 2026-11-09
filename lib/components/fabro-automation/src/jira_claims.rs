use std::collections::HashSet;
use std::str::FromStr as _;

use chrono::{DateTime, Utc};
use fabro_db::DbPool;
use fabro_types::RunId;
use sqlx::Row as _;
use sqlx::sqlite::SqliteRow;

use crate::{AutomationId, AutomationStoreError};

/// Records which Jira issue each automation has already started a run for.
#[derive(Clone)]
pub struct JiraIssueRunStore {
    pool: DbPool,
}

impl std::fmt::Debug for JiraIssueRunStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JiraIssueRunStore").finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JiraIssueClaim {
    pub automation_id: AutomationId,
    pub issue_key:     String,
    pub run_id:        RunId,
    pub created_at:    DateTime<Utc>,
}

impl JiraIssueRunStore {
    #[must_use]
    pub fn new(pool: DbPool) -> Self {
        Self { pool }
    }

    /// Ok(true) when inserted, Ok(false) when the key was already claimed.
    pub async fn claim(
        &self,
        automation_id: &AutomationId,
        issue_key: &str,
        run_id: RunId,
        now: DateTime<Utc>,
    ) -> Result<bool, AutomationStoreError> {
        let result = sqlx::query(
            "INSERT OR IGNORE INTO jira_issue_runs (automation_id, issue_key, run_id, created_at)
             VALUES (?, ?, ?, ?)",
        )
        .bind(automation_id.as_str())
        .bind(issue_key)
        .bind(run_id.to_string())
        .bind(now.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn release(
        &self,
        automation_id: &AutomationId,
        issue_key: &str,
    ) -> Result<bool, AutomationStoreError> {
        let result =
            sqlx::query("DELETE FROM jira_issue_runs WHERE automation_id = ? AND issue_key = ?")
                .bind(automation_id.as_str())
                .bind(issue_key)
                .execute(&self.pool)
                .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Marks that the claim's run exists, so it is never treated as stale.
    pub async fn mark_run_created(
        &self,
        automation_id: &AutomationId,
        issue_key: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, AutomationStoreError> {
        let result = sqlx::query(
            "UPDATE jira_issue_runs SET run_created_at = ? WHERE automation_id = ? AND issue_key = ?",
        )
        .bind(now.to_rfc3339())
        .bind(automation_id.as_str())
        .bind(issue_key)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Claims whose run was never marked created and that are older than
    /// `before`.
    pub async fn list_unstarted_before(
        &self,
        automation_id: &AutomationId,
        before: DateTime<Utc>,
    ) -> Result<Vec<JiraIssueClaim>, AutomationStoreError> {
        let rows = sqlx::query(
            "SELECT automation_id, issue_key, run_id, created_at FROM jira_issue_runs
             WHERE automation_id = ? AND run_created_at IS NULL ORDER BY created_at, issue_key",
        )
        .bind(automation_id.as_str())
        .fetch_all(&self.pool)
        .await?;
        let claims: Vec<_> = rows.iter().map(claim_from_row).collect::<Result<_, _>>()?;
        Ok(claims
            .into_iter()
            .filter(|c| c.created_at < before)
            .collect())
    }

    pub async fn claimed_keys(
        &self,
        automation_id: &AutomationId,
    ) -> Result<HashSet<String>, AutomationStoreError> {
        let keys =
            sqlx::query_scalar("SELECT issue_key FROM jira_issue_runs WHERE automation_id = ?")
                .bind(automation_id.as_str())
                .fetch_all(&self.pool)
                .await?;
        Ok(keys.into_iter().collect())
    }

    pub async fn by_run(
        &self,
        run_id: RunId,
    ) -> Result<Option<JiraIssueClaim>, AutomationStoreError> {
        let row = sqlx::query(
            "SELECT automation_id, issue_key, run_id, created_at FROM jira_issue_runs WHERE run_id = ?",
        )
        .bind(run_id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(claim_from_row).transpose()
    }

    pub async fn list(
        &self,
        automation_id: &AutomationId,
    ) -> Result<Vec<JiraIssueClaim>, AutomationStoreError> {
        let rows = sqlx::query(
            "SELECT automation_id, issue_key, run_id, created_at FROM jira_issue_runs
             WHERE automation_id = ? ORDER BY created_at, issue_key",
        )
        .bind(automation_id.as_str())
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(claim_from_row).collect()
    }
}

fn claim_from_row(row: &SqliteRow) -> Result<JiraIssueClaim, AutomationStoreError> {
    let raw_id: String = row.try_get("automation_id")?;
    let issue_key: String = row.try_get("issue_key")?;
    let raw_run_id: String = row.try_get("run_id")?;
    let raw_created_at: String = row.try_get("created_at")?;
    let automation_id =
        AutomationId::from_str(&raw_id).map_err(|source| AutomationStoreError::StoredId {
            value: raw_id,
            source,
        })?;
    let shape_error = || AutomationStoreError::StoredClaimShape {
        automation_id: automation_id.clone(),
        issue_key:     issue_key.clone(),
    };
    let run_id = RunId::from_str(&raw_run_id).map_err(|_| shape_error())?;
    let created_at = DateTime::parse_from_rfc3339(&raw_created_at)
        .map_err(|_| shape_error())?
        .to_utc();
    Ok(JiraIssueClaim {
        automation_id,
        issue_key,
        run_id,
        created_at,
    })
}
