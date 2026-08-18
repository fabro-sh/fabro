use std::str::FromStr as _;

use fabro_db::DbPool;
use fabro_types::ExternalAgentHarness;
use sqlx::sqlite::SqliteRow;
use sqlx::{Row as _, Sqlite, Transaction};

use crate::{
    ApiTrigger, Automation, AutomationDraft, AutomationId, AutomationReplace, AutomationRevision,
    AutomationStoreError, AutomationTarget, AutomationTrigger, AutomationTriggerId, PlaneTrigger,
    ScheduleTrigger,
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
                a.api_enabled,
                a.target_repository,
                a.target_ref,
                a.target_workflow,
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
        let plane_rows = sqlx::query(
            r"
            SELECT automation_id, id, enabled, project_id, ready_state_id, in_progress_state_id,
                   done_state_id, cancelled_state_id, failure_label_id, default_harness,
                   codex_label_id, omp_label_id, poll_interval_seconds, max_concurrency, max_retries
            FROM automation_plane_triggers
            ORDER BY automation_id, id
            ",
        )
        .fetch_all(&self.pool)
        .await?;
        automations_from_rows(&rows, &plane_rows)
    }

    pub async fn get(&self, id: &AutomationId) -> Result<Option<Automation>, AutomationStoreError> {
        let rows = sqlx::query(select_automations_sql!("WHERE a.id = ? ORDER BY t.id"))
            .bind(id.as_str())
            .fetch_all(&self.pool)
            .await?;
        let plane_rows = sqlx::query(
            r"
            SELECT automation_id, id, enabled, project_id, ready_state_id, in_progress_state_id,
                   done_state_id, cancelled_state_id, failure_label_id, default_harness,
                   codex_label_id, omp_label_id, poll_interval_seconds, max_concurrency, max_retries
            FROM automation_plane_triggers
            WHERE automation_id = ?
            ORDER BY id
            ",
        )
        .bind(id.as_str())
        .fetch_all(&self.pool)
        .await?;
        Ok(automations_from_rows(&rows, &plane_rows)?
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
        let mut transaction = self.pool.begin().await?;
        let result = sqlx::query(
            r"
            UPDATE automations SET
                revision = ?,
                name = ?,
                description = ?,
                api_enabled = ?,
                target_repository = ?,
                target_ref = ?,
                target_workflow = ?
            WHERE id = ? AND revision = ?
            ",
        )
        .bind(automation.revision.as_str())
        .bind(&automation.name)
        .bind(automation.description.as_deref())
        .bind(automation.api_enabled())
        .bind(&automation.target.repository)
        .bind(&automation.target.ref_selector)
        .bind(&automation.target.workflow)
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
        sqlx::query("DELETE FROM automation_plane_triggers WHERE automation_id = ?")
            .bind(id.as_str())
            .execute(&mut *transaction)
            .await?;
        insert_schedule_triggers(&mut transaction, &automation).await?;
        insert_plane_triggers(&mut transaction, &automation).await?;
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
    api_enabled:       bool,
    target:            AutomationTarget,
    schedule_triggers: Vec<ScheduleTrigger>,
    plane_triggers:    Vec<PlaneTrigger>,
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
        Ok(Self {
            id,
            revision,
            name: row.try_get("name")?,
            description: row.try_get("description")?,
            api_enabled: row.try_get("api_enabled")?,
            target: AutomationTarget {
                repository:   row.try_get("target_repository")?,
                ref_selector: row.try_get("target_ref")?,
                workflow:     row.try_get("target_workflow")?,
            },
            schedule_triggers: Vec::new(),
            plane_triggers: Vec::new(),
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
        triggers.extend(
            self.plane_triggers
                .into_iter()
                .map(AutomationTrigger::Plane),
        );
        if self.api_enabled {
            triggers.push(AutomationTrigger::Api(ApiTrigger::manual()));
        }
        let id = self.id;
        Automation::from_stored(id.clone(), self.revision, AutomationReplace {
            name: self.name,
            description: self.description,
            target: self.target,
            triggers,
        })
        .map_err(|source| AutomationStoreError::StoredValidation { id, source })
    }
}

fn automations_from_rows(
    rows: &[SqliteRow],
    plane_rows: &[SqliteRow],
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
            current = Some(StoredAutomation::from_row(row)?);
        }
        current
            .as_mut()
            .expect("current automation exists")
            .push_trigger_row(row)?;
    }

    if let Some(automation) = current {
        automations.push(automation.finish()?);
    }

    let mut by_id = automations
        .into_iter()
        .map(|automation| (automation.id.clone(), automation))
        .collect::<std::collections::BTreeMap<_, _>>();

    for row in plane_rows {
        let automation_id_value = row.try_get::<String, _>("automation_id")?;
        let automation_id = AutomationId::new(automation_id_value.clone()).map_err(|source| {
            AutomationStoreError::StoredId {
                value: automation_id_value,
                source,
            }
        })?;
        let Some(automation) = by_id.remove(&automation_id) else {
            continue;
        };
        let trigger = plane_trigger_from_row(row, &automation_id)?;
        let mut replace = AutomationReplace {
            name:        automation.name,
            description: automation.description,
            target:      automation.target,
            triggers:    automation.triggers,
        };
        replace.triggers.push(AutomationTrigger::Plane(trigger));
        let rebuilt = Automation::from_stored(automation_id.clone(), automation.revision, replace)
            .map_err(|source| AutomationStoreError::StoredValidation {
                id: automation_id.clone(),
                source,
            })?;
        by_id.insert(automation_id, rebuilt);
    }

    Ok(by_id.into_values().collect())
}

fn plane_trigger_from_row(
    row: &SqliteRow,
    automation_id: &AutomationId,
) -> Result<PlaneTrigger, AutomationStoreError> {
    let id_value = row.try_get::<String, _>("id")?;
    let id = AutomationTriggerId::new(id_value).map_err(|source| {
        AutomationStoreError::StoredValidation {
            id: automation_id.clone(),
            source,
        }
    })?;
    let harness = row.try_get::<String, _>("default_harness")?;
    let default_harness = harness.parse::<ExternalAgentHarness>().map_err(|_| {
        AutomationStoreError::StoredTriggerShape {
            id: automation_id.clone(),
        }
    })?;
    Ok(PlaneTrigger {
        id,
        enabled: row.try_get("enabled")?,
        project_id: row.try_get("project_id")?,
        ready_state_id: row.try_get("ready_state_id")?,
        in_progress_state_id: row.try_get("in_progress_state_id")?,
        done_state_id: row.try_get("done_state_id")?,
        cancelled_state_id: row.try_get("cancelled_state_id")?,
        failure_label_id: row.try_get("failure_label_id")?,
        default_harness,
        codex_label_id: row.try_get("codex_label_id")?,
        omp_label_id: row.try_get("omp_label_id")?,
        poll_interval_seconds: u64::try_from(row.try_get::<i64, _>("poll_interval_seconds")?)
            .map_err(|_| AutomationStoreError::StoredTriggerShape {
                id: automation_id.clone(),
            })?,
        max_concurrency: usize::try_from(row.try_get::<i64, _>("max_concurrency")?).map_err(
            |_| AutomationStoreError::StoredTriggerShape {
                id: automation_id.clone(),
            },
        )?,
        max_retries: usize::try_from(row.try_get::<i64, _>("max_retries")?).map_err(|_| {
            AutomationStoreError::StoredTriggerShape {
                id: automation_id.clone(),
            }
        })?,
    })
}

pub(crate) async fn insert_automation_ignoring_conflict(
    transaction: &mut Transaction<'_, Sqlite>,
    automation: &Automation,
) -> Result<bool, AutomationStoreError> {
    let result = sqlx::query(
        r"
        INSERT INTO automations (
            id,
            revision,
            name,
            description,
            api_enabled,
            target_repository,
            target_ref,
            target_workflow
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(id) DO NOTHING
        ",
    )
    .bind(automation.id.as_str())
    .bind(automation.revision.as_str())
    .bind(&automation.name)
    .bind(automation.description.as_deref())
    .bind(automation.api_enabled())
    .bind(&automation.target.repository)
    .bind(&automation.target.ref_selector)
    .bind(&automation.target.workflow)
    .execute(&mut **transaction)
    .await?;
    if result.rows_affected() == 0 {
        return Ok(false);
    }
    insert_schedule_triggers(transaction, automation).await?;
    insert_plane_triggers(transaction, automation).await?;
    Ok(true)
}

async fn insert_plane_triggers(
    transaction: &mut Transaction<'_, Sqlite>,
    automation: &Automation,
) -> Result<(), AutomationStoreError> {
    for trigger in automation.plane_triggers() {
        sqlx::query(
            r"
            INSERT INTO automation_plane_triggers (
                automation_id, id, enabled, project_id, ready_state_id, in_progress_state_id,
                done_state_id, cancelled_state_id, failure_label_id, default_harness,
                codex_label_id, omp_label_id, poll_interval_seconds, max_concurrency, max_retries
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ",
        )
        .bind(automation.id.as_str())
        .bind(trigger.id.as_str())
        .bind(trigger.enabled)
        .bind(&trigger.project_id)
        .bind(&trigger.ready_state_id)
        .bind(&trigger.in_progress_state_id)
        .bind(&trigger.done_state_id)
        .bind(&trigger.cancelled_state_id)
        .bind(trigger.failure_label_id.as_deref())
        .bind(trigger.default_harness.as_str())
        .bind(trigger.codex_label_id.as_deref())
        .bind(trigger.omp_label_id.as_deref())
        .bind(i64::try_from(trigger.poll_interval_seconds).unwrap_or(i64::MAX))
        .bind(i64::try_from(trigger.max_concurrency).unwrap_or(i64::MAX))
        .bind(i64::try_from(trigger.max_retries).unwrap_or(i64::MAX))
        .execute(&mut **transaction)
        .await?;
    }
    Ok(())
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
