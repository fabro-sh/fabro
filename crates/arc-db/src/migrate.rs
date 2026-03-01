use sqlx::SqlitePool;

const CURRENT_VERSION: i64 = 1;

const MIGRATION_001: &str = include_str!("../migrations/001_create_workflow_runs.sql");

/// Apply all pending migrations to the database.
///
/// Uses `PRAGMA user_version` to track which migrations have been applied.
pub async fn initialize_db(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let row: (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(pool)
        .await?;
    let from_version = row.0;

    if from_version < CURRENT_VERSION {
        let mut tx = pool.begin().await?;

        if from_version < 1 {
            sqlx::query(MIGRATION_001).execute(&mut *tx).await?;
        }

        sqlx::query(&format!("PRAGMA user_version = {CURRENT_VERSION}"))
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;
    }

    Ok(())
}
