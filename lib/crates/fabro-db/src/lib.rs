mod migrate;
pub mod workflow_run;

use std::path::Path;
use std::time::Duration;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use tracing::debug;

pub use migrate::initialize_db;
pub use workflow_run::WorkflowRun;

/// Connect to a SQLite database at the given path, creating it if it doesn't exist.
pub async fn connect(path: &Path) -> Result<SqlitePool, sqlx::Error> {
    debug!(path = %path.display(), "Connecting to SQLite database");
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(5))
        .foreign_keys(true);

    SqlitePoolOptions::new().connect_with(options).await
}

/// Connect to an in-memory SQLite database (for tests).
pub async fn connect_memory() -> Result<SqlitePool, sqlx::Error> {
    let options = SqliteConnectOptions::new()
        .filename(":memory:")
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(5))
        .foreign_keys(true);

    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[tokio::test]
    async fn connect_memory_returns_working_pool() {
        let pool = connect_memory().await.unwrap();
        let row: (i64,) = sqlx::query_as("SELECT 1").fetch_one(&pool).await.unwrap();
        assert_eq!(row.0, 1);
    }

    #[tokio::test]
    async fn initialize_db_creates_workflow_runs_table() {
        let pool = connect_memory().await.unwrap();
        initialize_db(&pool).await.unwrap();

        let row: (String,) = sqlx::query_as(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='workflow_runs'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.0, "workflow_runs");
    }

    #[tokio::test]
    async fn initialize_db_sets_user_version() {
        let pool = connect_memory().await.unwrap();
        initialize_db(&pool).await.unwrap();

        let row: (i64,) = sqlx::query_as("PRAGMA user_version")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(row.0, 2);
    }

    #[tokio::test]
    async fn initialize_db_is_idempotent() {
        let pool = connect_memory().await.unwrap();
        initialize_db(&pool).await.unwrap();
        initialize_db(&pool).await.unwrap();

        let row: (i64,) = sqlx::query_as("PRAGMA user_version")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(row.0, 2);
    }

    #[tokio::test]
    async fn workflow_run_round_trips_through_sql() {
        let pool = connect_memory().await.unwrap();
        initialize_db(&pool).await.unwrap();

        let now = Utc::now();
        let now_str = now.format("%Y-%m-%d %H:%M:%S").to_string();

        sqlx::query(
            "INSERT INTO workflow_runs (id, title, run_dir, work_dir, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind("run-1")
        .bind("My Run")
        .bind("/tmp/logs")
        .bind("/tmp/work")
        .bind(&now_str)
        .bind(&now_str)
        .execute(&pool)
        .await
        .unwrap();

        let run: WorkflowRun = sqlx::query_as("SELECT * FROM workflow_runs WHERE id = ?")
            .bind("run-1")
            .fetch_one(&pool)
            .await
            .unwrap();

        assert_eq!(run.id, "run-1");
        assert_eq!(run.title, "My Run");
        assert_eq!(run.run_dir, "/tmp/logs");
        assert_eq!(run.work_dir, "/tmp/work");
    }
}
