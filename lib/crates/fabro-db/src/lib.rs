use std::path::Path;
use std::time::Duration;

use anyhow::Context as _;
use sqlx::migrate::Migrator;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use tokio::fs;

pub type DbPool = sqlx::SqlitePool;

static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

pub async fn connect(path: impl AsRef<Path>) -> anyhow::Result<DbPool> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .with_context(|| format!("creating SQLite database directory {}", parent.display()))?;
    }

    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_secs(5));

    SqlitePoolOptions::new()
        .connect_with(options)
        .await
        .with_context(|| format!("opening SQLite database {}", path.display()))
}

pub async fn migrate(pool: &DbPool) -> anyhow::Result<()> {
    MIGRATOR
        .run(pool)
        .await
        .context("running SQLite migrations")
}

pub async fn health_check(pool: &DbPool) -> anyhow::Result<()> {
    sqlx::query("SELECT 1")
        .execute(pool)
        .await
        .context("checking SQLite database health")?;
    Ok(())
}
