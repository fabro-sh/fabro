use std::collections::HashMap;
use std::str::FromStr as _;

use fabro_db::DbPool;
use fabro_types::{GitRunTarget, RunTarget};
use sqlx::sqlite::SqliteRow;
use sqlx::{Row as _, Sqlite, Transaction};

use crate::{
    ApiTrigger, Automation, AutomationDraft, AutomationGitWorkflowSource, AutomationId,
    AutomationReplace, AutomationRevision, AutomationStoreError, AutomationTrigger,
    AutomationTriggerId, JiraTrigger, ScheduleTrigger,
};

/// Shared projection for loading automations with their schedule triggers.
/// A macro rather than a `const` because sqlx requires `&'static str` SQL.
macro_rules! select_automations_sql {
    ($suffix:expr) => {
        concat!(
            "SELECT
                a.id,
                a.revision,
                a.name,
                a.description,
                a.environment_id,
                a.last_error,
                a.api_enabled,
                a.target_repository,
                a.target_branch,
                a.target_tag,
                a.target_sha,
                a.target_workflow,
                a.workflow_source_repository,
                a.workflow_source_branch,
                a.workflow_source_tag,
                a.workflow_source_sha,
                t.id AS trigger_id,
                t.enabled AS trigger_enabled,
                t.expression AS trigger_expression
            FROM automations AS a
            LEFT JOIN automation_triggers AS t ON t.automation_id = a.id
            ",
            $suffix
        )
    };
}

#[derive(Clone)]
pub struct AutomationStore {
    pool: DbPool,
}

impl std::fmt::Debug for AutomationStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AutomationStore").finish_non_exhaustive()
    }
}

impl AutomationStore {
    #[must_use]
    pub fn new(pool: DbPool) -> Self {
        Self { pool }
    }

    pub async fn list(&self) -> Result<Vec<Automation>, AutomationStoreError> {
        let rows = sqlx::query(select_automations_sql!("ORDER BY a.id, t.id"))
            .fetch_all(&self.pool)
            .await?;
        let jira_triggers = load_jira_triggers(&self.pool, None).await?;
        automations_from_rows(&rows, jira_triggers)
    }

    pub async fn get(&self, id: &AutomationId) -> Result<Option<Automation>, AutomationStoreError> {
        let rows = sqlx::query(select_automations_sql!("WHERE a.id = ? ORDER BY t.id"))
            .bind(id.as_str())
            .fetch_all(&self.pool)
            .await?;
        let jira_triggers = load_jira_triggers(&self.pool, Some(id)).await?;
        Ok(automations_from_rows(&rows, jira_triggers)?
            .into_iter()
            .next())
    }

    pub async fn exists(&self, id: &AutomationId) -> Result<bool, AutomationStoreError> {
        let row = sqlx::query("SELECT 1 FROM automations WHERE id = ?")
            .bind(id.as_str())
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.is_some())
    }

    pub async fn references_environment(
        &self,
        environment_id: &str,
    ) -> Result<bool, AutomationStoreError> {
        let row = sqlx::query("SELECT 1 FROM automations WHERE environment_id = ? LIMIT 1")
            .bind(environment_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.is_some())
    }

    pub async fn set_last_error(
        &self,
        id: &AutomationId,
        message: Option<&str>,
    ) -> Result<(), AutomationStoreError> {
        let result = sqlx::query("UPDATE automations SET last_error = ? WHERE id = ?")
            .bind(message)
            .bind(id.as_str())
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(AutomationStoreError::NotFound { id: id.clone() });
        }
        Ok(())
    }

    pub async fn create(&self, draft: AutomationDraft) -> Result<Automation, AutomationStoreError> {
        let (id, replace) = draft.into();
        let (automation, _) = Automation::from_replace(id.clone(), replace)?;
        let mut transaction = self.pool.begin().await?;
        if !insert_automation_ignoring_conflict(&mut transaction, &automation).await? {
            return Err(AutomationStoreError::AlreadyExists { id });
        }
        transaction.commit().await?;
        Ok(automation)
    }

    pub async fn replace(
        &self,
        id: &AutomationId,
        expected: &AutomationRevision,
        draft: AutomationReplace,
    ) -> Result<Automation, AutomationStoreError> {
        let (automation, _) = Automation::from_replace(id.clone(), draft)?;
        let target = stored_git_target(&automation);
        let workflow_source = automation.workflow_source.as_ref();
        let mut transaction = self.pool.begin().await?;
        let result = sqlx::query(
            r"
            UPDATE automations SET
                revision = ?,
                name = ?,
                description = ?,
                environment_id = ?,
                last_error = NULL,
                api_enabled = ?,
                target_repository = ?,
                target_branch = ?,
                target_tag = ?,
                target_sha = ?,
                target_workflow = ?,
                workflow_source_repository = ?,
                workflow_source_branch = ?,
                workflow_source_tag = ?,
                workflow_source_sha = ?
            WHERE id = ? AND revision = ?
            ",
        )
        .bind(automation.revision.as_str())
        .bind(&automation.name)
        .bind(automation.description.as_deref())
        .bind(automation.environment_id.as_deref())
        .bind(automation.api_enabled())
        .bind(&target.repo)
        .bind(&target.branch)
        .bind(target.tag.as_deref())
        .bind(target.sha.as_deref())
        .bind(&automation.workflow)
        .bind(workflow_source.map(|source| source.repo.as_str()))
        .bind(workflow_source.map(|source| source.branch.as_str()))
        .bind(workflow_source.and_then(|source| source.tag.as_deref()))
        .bind(workflow_source.and_then(|source| source.sha.as_deref()))
        .bind(id.as_str())
        .bind(expected.as_str())
        .execute(&mut *transaction)
        .await?;
        if result.rows_affected() == 0 {
            return Err(revision_mismatch_error(&mut transaction, id, expected).await?);
        }

        sqlx::query("DELETE FROM automation_triggers WHERE automation_id = ?")
            .bind(id.as_str())
            .execute(&mut *transaction)
            .await?;
        insert_schedule_triggers(&mut transaction, &automation).await?;
        sqlx::query("DELETE FROM automation_jira_triggers WHERE automation_id = ?")
            .bind(id.as_str())
            .execute(&mut *transaction)
            .await?;
        insert_jira_triggers(&mut transaction, &automation).await?;
        transaction.commit().await?;
        Ok(automation)
    }

    pub async fn delete(
        &self,
        id: &AutomationId,
        expected: &AutomationRevision,
    ) -> Result<(), AutomationStoreError> {
        let mut transaction = self.pool.begin().await?;
        let result = sqlx::query("DELETE FROM automations WHERE id = ? AND revision = ?")
            .bind(id.as_str())
            .bind(expected.as_str())
            .execute(&mut *transaction)
            .await?;
        if result.rows_affected() == 0 {
            return Err(revision_mismatch_error(&mut transaction, id, expected).await?);
        }
        transaction.commit().await?;
        Ok(())
    }
}

struct StoredAutomation {
    id:                AutomationId,
    revision:          AutomationRevision,
    name:              String,
    description:       Option<String>,
    environment_id:    Option<String>,
    last_error:        Option<String>,
    api_enabled:       bool,
    target:            RunTarget,
    workflow:          String,
    workflow_source:   Option<AutomationGitWorkflowSource>,
    schedule_triggers: Vec<ScheduleTrigger>,
    jira_triggers:     Vec<JiraTrigger>,
}

impl StoredAutomation {
    fn from_row(row: &SqliteRow) -> Result<Self, AutomationStoreError> {
        let id_value = row.try_get::<String, _>("id")?;
        let id = AutomationId::new(id_value.clone()).map_err(|source| {
            AutomationStoreError::StoredId {
                value: id_value,
                source,
            }
        })?;
        let revision = AutomationRevision::from_str(&row.try_get::<String, _>("revision")?)
            .map_err(|source| AutomationStoreError::InvalidRevision {
                id: id.clone(),
                source,
            })?;
        let workflow_source = stored_workflow_source(row, &id)?;
        Ok(Self {
            id,
            revision,
            name: row.try_get("name")?,
            description: row.try_get("description")?,
            environment_id: row.try_get("environment_id")?,
            last_error: row.try_get("last_error")?,
            api_enabled: row.try_get("api_enabled")?,
            target: RunTarget::Git(GitRunTarget {
                repo:   row.try_get("target_repository")?,
                branch: row.try_get("target_branch")?,
                tag:    row.try_get("target_tag")?,
                sha:    row.try_get("target_sha")?,
            }),
            workflow: row.try_get("target_workflow")?,
            workflow_source,
            schedule_triggers: Vec::new(),
            jira_triggers: Vec::new(),
        })
    }

    fn push_trigger_row(&mut self, row: &SqliteRow) -> Result<(), AutomationStoreError> {
        let Some(id_value) = row.try_get::<Option<String>, _>("trigger_id")? else {
            return Ok(());
        };
        let id = AutomationTriggerId::new(id_value).map_err(|source| {
            AutomationStoreError::StoredValidation {
                id: self.id.clone(),
                source,
            }
        })?;
        self.schedule_triggers.push(ScheduleTrigger {
            id,
            enabled: row
                .try_get::<Option<bool>, _>("trigger_enabled")?
                .ok_or_else(|| AutomationStoreError::StoredTriggerShape {
                    id: self.id.clone(),
                })?,
            expression: row
                .try_get::<Option<String>, _>("trigger_expression")?
                .ok_or_else(|| AutomationStoreError::StoredTriggerShape {
                    id: self.id.clone(),
                })?,
        });
        Ok(())
    }

    fn finish(self) -> Result<Automation, AutomationStoreError> {
        // `from_stored` canonicalizes trigger order and the manual API trigger.
        let mut triggers = self
            .schedule_triggers
            .into_iter()
            .map(AutomationTrigger::Schedule)
            .collect::<Vec<_>>();
        triggers.extend(self.jira_triggers.into_iter().map(AutomationTrigger::Jira));
        if self.api_enabled {
            triggers.push(AutomationTrigger::Api(ApiTrigger::manual()));
        }
        let id = self.id;
        let mut automation =
            Automation::from_stored(id.clone(), self.revision, AutomationReplace {
                name: self.name,
                description: self.description,
                environment_id: self.environment_id,
                target: self.target,
                workflow: self.workflow,
                workflow_source: self.workflow_source,
                triggers,
            })
            .map_err(|source| AutomationStoreError::StoredValidation { id, source })?;
        automation.last_error = self.last_error;
        Ok(automation)
    }
}

/// Jira triggers live in their own table, so they are loaded beside the
/// schedule join and merged by automation id.
async fn load_jira_triggers(
    pool: &DbPool,
    only: Option<&AutomationId>,
) -> Result<HashMap<String, Vec<JiraTrigger>>, AutomationStoreError> {
    let rows = sqlx::query(
        "SELECT automation_id, id, enabled, jql, in_progress_status, review_status
        FROM automation_jira_triggers
        WHERE ?1 IS NULL OR automation_id = ?1
        ORDER BY automation_id, id",
    )
    .bind(only.map(AutomationId::as_str))
    .fetch_all(pool)
    .await?;
    let mut by_automation = HashMap::<String, Vec<JiraTrigger>>::new();
    for row in &rows {
        let automation_id = row.try_get::<String, _>("automation_id")?;
        let id_value = row.try_get::<String, _>("id")?;
        let id = AutomationTriggerId::new(id_value.clone()).map_err(|source| {
            AutomationStoreError::StoredId {
                value: id_value,
                source,
            }
        })?;
        by_automation
            .entry(automation_id)
            .or_default()
            .push(JiraTrigger {
                id,
                enabled: row.try_get("enabled")?,
                jql: row.try_get("jql")?,
                in_progress_status: row.try_get("in_progress_status")?,
                review_status: row.try_get("review_status")?,
            });
    }
    Ok(by_automation)
}

fn automations_from_rows(
    rows: &[SqliteRow],
    mut jira_triggers: HashMap<String, Vec<JiraTrigger>>,
) -> Result<Vec<Automation>, AutomationStoreError> {
    let mut automations = Vec::new();
    let mut current: Option<StoredAutomation> = None;

    for row in rows {
        let row_id = row.try_get::<String, _>("id")?;
        if current
            .as_ref()
            .is_some_and(|automation| automation.id.as_str() != row_id)
        {
            automations.push(
                current
                    .take()
                    .expect("current automation exists")
                    .finish()?,
            );
        }
        if current.is_none() {
            let mut stored = StoredAutomation::from_row(row)?;
            stored.jira_triggers = jira_triggers.remove(&row_id).unwrap_or_default();
            current = Some(stored);
        }
        current
            .as_mut()
            .expect("current automation exists")
            .push_trigger_row(row)?;
    }

    if let Some(automation) = current {
        automations.push(automation.finish()?);
    }
    Ok(automations)
}

pub(crate) async fn insert_automation_ignoring_conflict(
    transaction: &mut Transaction<'_, Sqlite>,
    automation: &Automation,
) -> Result<bool, AutomationStoreError> {
    let target = stored_git_target(automation);
    let workflow_source = automation.workflow_source.as_ref();
    let result = sqlx::query(
        r"
        INSERT INTO automations (
            id,
            revision,
            name,
            description,
            environment_id,
            api_enabled,
            target_repository,
            target_branch,
            target_tag,
            target_sha,
            target_workflow,
            workflow_source_repository,
            workflow_source_branch,
            workflow_source_tag,
            workflow_source_sha
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(id) DO NOTHING
        ",
    )
    .bind(automation.id.as_str())
    .bind(automation.revision.as_str())
    .bind(&automation.name)
    .bind(automation.description.as_deref())
    .bind(automation.environment_id.as_deref())
    .bind(automation.api_enabled())
    .bind(&target.repo)
    .bind(&target.branch)
    .bind(target.tag.as_deref())
    .bind(target.sha.as_deref())
    .bind(&automation.workflow)
    .bind(workflow_source.map(|source| source.repo.as_str()))
    .bind(workflow_source.map(|source| source.branch.as_str()))
    .bind(workflow_source.and_then(|source| source.tag.as_deref()))
    .bind(workflow_source.and_then(|source| source.sha.as_deref()))
    .execute(&mut **transaction)
    .await?;
    if result.rows_affected() == 0 {
        return Ok(false);
    }
    insert_schedule_triggers(transaction, automation).await?;
    insert_jira_triggers(transaction, automation).await?;
    Ok(true)
}

fn stored_workflow_source(
    row: &SqliteRow,
    id: &AutomationId,
) -> Result<Option<AutomationGitWorkflowSource>, AutomationStoreError> {
    let repository = row.try_get::<Option<String>, _>("workflow_source_repository")?;
    let branch = row.try_get::<Option<String>, _>("workflow_source_branch")?;
    let tag = row.try_get::<Option<String>, _>("workflow_source_tag")?;
    let sha = row.try_get::<Option<String>, _>("workflow_source_sha")?;
    match (repository, branch) {
        (None, None) if tag.is_none() && sha.is_none() => Ok(None),
        (Some(repo), Some(branch)) => Ok(Some(AutomationGitWorkflowSource {
            repo,
            branch,
            tag,
            sha,
        })),
        _ => Err(AutomationStoreError::StoredWorkflowSourceShape { id: id.clone() }),
    }
}

fn stored_git_target(automation: &Automation) -> &GitRunTarget {
    automation
        .git_target()
        .expect("stored automations have already passed Git-only validation")
}

async fn insert_schedule_triggers(
    transaction: &mut Transaction<'_, Sqlite>,
    automation: &Automation,
) -> Result<(), AutomationStoreError> {
    for trigger in automation.schedule_triggers() {
        sqlx::query(
            r"
            INSERT INTO automation_triggers (automation_id, id, enabled, expression)
            VALUES (?, ?, ?, ?)
            ",
        )
        .bind(automation.id.as_str())
        .bind(trigger.id.as_str())
        .bind(trigger.enabled)
        .bind(&trigger.expression)
        .execute(&mut **transaction)
        .await?;
    }
    Ok(())
}

async fn insert_jira_triggers(
    transaction: &mut Transaction<'_, Sqlite>,
    automation: &Automation,
) -> Result<(), AutomationStoreError> {
    for trigger in automation.jira_triggers() {
        sqlx::query(
            r"
            INSERT INTO automation_jira_triggers (
                automation_id, id, enabled, jql, in_progress_status, review_status
            ) VALUES (?, ?, ?, ?, ?, ?)
            ",
        )
        .bind(automation.id.as_str())
        .bind(trigger.id.as_str())
        .bind(trigger.enabled)
        .bind(&trigger.jql)
        .bind(trigger.in_progress_status.as_deref())
        .bind(trigger.review_status.as_deref())
        .execute(&mut **transaction)
        .await?;
    }
    Ok(())
}

async fn current_revision(
    transaction: &mut Transaction<'_, Sqlite>,
    id: &AutomationId,
) -> Result<Option<AutomationRevision>, AutomationStoreError> {
    let current = sqlx::query_scalar::<_, String>("SELECT revision FROM automations WHERE id = ?")
        .bind(id.as_str())
        .fetch_optional(&mut **transaction)
        .await?;
    current
        .map(|revision| {
            AutomationRevision::from_str(&revision).map_err(|source| {
                AutomationStoreError::InvalidRevision {
                    id: id.clone(),
                    source,
                }
            })
        })
        .transpose()
}

async fn revision_mismatch_error(
    transaction: &mut Transaction<'_, Sqlite>,
    id: &AutomationId,
    expected: &AutomationRevision,
) -> Result<AutomationStoreError, AutomationStoreError> {
    let Some(actual) = current_revision(transaction, id).await? else {
        return Err(AutomationStoreError::NotFound { id: id.clone() });
    };
    Ok(AutomationStoreError::StaleRevision {
        id: id.clone(),
        expected: expected.clone(),
        actual,
    })
}
