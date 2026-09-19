use std::path::{Path, PathBuf};
use std::sync::Arc;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

#[cfg(test)]
use crate::{AuthCodeStore, AuthSessionStore};
use crate::{BlobStore, Database, RunSummaryStore};

/// Returns an isolated SQLite blob authority backed by its own in-memory
/// database.
///
/// Every call creates a fresh blob table, so tests never observe rows written
/// by other tests in the same process. Reopen-style tests that model one
/// process-wide blob authority across several store handles should call this
/// once and share the result through [`test_database_with_blobs`].
#[must_use]
pub fn test_blob_store() -> Arc<BlobStore> {
    Arc::new(BlobStore::new(lazy_in_memory_pool(&[
        fabro_db::BLOBS_MIGRATION_SQL,
    ])))
}

/// The migrations a run summary fixture installs: the `runs` row, the
/// platform records and the projection tables.
const RUN_SUMMARY_MIGRATIONS: &[&str] = &[
    fabro_db::RUNS_MIGRATION_SQL,
    fabro_db::DROP_RUN_EVENTS_MIGRATION_SQL,
    fabro_db::PETRI_PROJECTION_MIGRATION_SQL,
];

/// Returns an isolated SQLite run-summary store backed by its own in-memory
/// database and the production `runs`, platform record and projection
/// schemas.
#[must_use]
pub fn test_run_summary_store() -> Arc<RunSummaryStore> {
    Arc::new(RunSummaryStore::new(lazy_in_memory_pool(
        RUN_SUMMARY_MIGRATIONS,
    )))
}

/// An isolated in-memory SQLite pool with `migrations` installed on first
/// use, for stores whose schema is not part of the run-history fixtures.
#[must_use]
pub fn in_memory_pool_with(migrations: &'static [&'static str]) -> sqlx::SqlitePool {
    lazy_in_memory_pool(migrations)
}

/// Builds a single-connection in-memory SQLite pool that installs
/// `migrations` on first use.
///
/// The pool connects lazily so synchronous fixture builders can remain
/// synchronous.
fn lazy_in_memory_pool(migrations: &'static [&'static str]) -> sqlx::SqlitePool {
    let options = SqliteConnectOptions::new()
        .filename(":memory:")
        .foreign_keys(true);
    SqlitePoolOptions::new()
        .max_connections(1)
        // A single in-memory test connection never needs reaping. Disabling
        // both timers also keeps this lazy fixture constructible from sync
        // tests, where SQLx has no Tokio runtime for maintenance tasks.
        .max_lifetime(None)
        .idle_timeout(None)
        .after_connect(move |connection, _metadata| {
            Box::pin(async move {
                for migration in migrations {
                    sqlx::raw_sql(*migration).execute(&mut *connection).await?;
                }
                Ok(())
            })
        })
        .connect_lazy_with(options)
}

/// Returns the SQLite file backing [`test_blob_store_at`] for `store_dir`.
#[must_use]
pub fn test_blob_store_path(store_dir: &Path) -> PathBuf {
    fabro_db::append_to_path(store_dir, "-blobs.sqlite3")
}

/// Returns the SQLite file backing [`test_run_summary_store_at`] for
/// `store_dir`.
#[must_use]
pub fn test_run_summary_store_path(store_dir: &Path) -> PathBuf {
    fabro_db::append_to_path(store_dir, "-runs.sqlite3")
}

/// Returns a durable SQLite blob authority stored beside `store_dir`.
///
/// Handles created for the same directory share one blob database file, so
/// reopen-style tests observe blobs across store handles the way production
/// handles share the process-wide blob authority. Tests that reuse a
/// directory must delete [`test_blob_store_path`] (and its `-wal`/`-shm`
/// siblings) when they reset the directory itself.
#[must_use]
pub fn test_blob_store_at(store_dir: &Path) -> Arc<BlobStore> {
    Arc::new(BlobStore::new(lazy_file_pool(
        test_blob_store_path(store_dir),
        "blobs",
        &[fabro_db::BLOBS_MIGRATION_SQL],
    )))
}

/// Returns a durable SQLite run-history authority stored beside `store_dir`.
///
/// Handles created for the same directory share one database file, which lets
/// reopen-style tests model the process-wide SQLite authority used in
/// production.
#[must_use]
pub fn test_run_summary_store_at(store_dir: &Path) -> Arc<RunSummaryStore> {
    Arc::new(RunSummaryStore::new(lazy_file_pool(
        test_run_summary_store_path(store_dir),
        "runs",
        RUN_SUMMARY_MIGRATIONS,
    )))
}

/// Builds a single-connection file-backed SQLite pool that installs
/// `migrations` the first time it opens a database without `probe_table`.
///
/// Like [`lazy_in_memory_pool`], the pool connects lazily so synchronous
/// fixture builders stay synchronous, and the file persists across handles so
/// reopen-style tests share one authority.
fn lazy_file_pool(
    path: PathBuf,
    probe_table: &'static str,
    migrations: &'static [&'static str],
) -> sqlx::SqlitePool {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .foreign_keys(true);
    SqlitePoolOptions::new()
        .max_connections(1)
        .max_lifetime(None)
        .idle_timeout(None)
        .after_connect(move |connection, _metadata| {
            Box::pin(async move {
                let installed: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master \
                     WHERE type = 'table' AND name = ?)",
                )
                .bind(probe_table)
                .fetch_one(&mut *connection)
                .await?;
                if !installed {
                    for migration in migrations {
                        sqlx::raw_sql(*migration).execute(&mut *connection).await?;
                    }
                }
                Ok(())
            })
        })
        .connect_lazy_with(options)
}

/// Builds a test database whose blob and run summary stores are durable
/// beside `store_dir` and shared by reopen-style handles.
#[must_use]
pub fn test_database_at(store_dir: &Path) -> Database {
    Database::new(
        test_blob_store_at(store_dir),
        test_run_summary_store_at(store_dir),
    )
}

/// Builds a run database with its own isolated blob and run summary
/// stores.
#[must_use]
pub fn test_database() -> Database {
    Database::new(test_blob_store(), test_run_summary_store())
}

/// Builds a run database sharing an explicit blob store.
///
/// Use this for reopen-style tests where two store handles must observe the
/// same blob table, mirroring the one blob authority a production process
/// shares across every run handle.
#[must_use]
pub fn test_database_with_blobs(blobs: Arc<BlobStore>) -> Database {
    Database::new(blobs, test_run_summary_store())
}

/// Builds a run database with explicit shared stores.
#[must_use]
pub fn test_database_with_stores(
    blobs: Arc<BlobStore>,
    run_summaries: Arc<RunSummaryStore>,
) -> Database {
    Database::new(blobs, run_summaries)
}

/// Connects to a migrated `fabro.sqlite3` in `directory` and returns its pool.
#[cfg(test)]
async fn sqlite_test_pool(directory: &Path) -> sqlx::SqlitePool {
    let database = fabro_db::Database::connect(directory.join("fabro.sqlite3"))
        .await
        .unwrap();
    database.migrate().await.unwrap();
    database.clone_pool()
}

#[cfg(test)]
pub(crate) async fn sqlite_auth_session_store() -> (tempfile::TempDir, AuthSessionStore) {
    let directory = tempfile::tempdir().unwrap();
    let store = AuthSessionStore::new(sqlite_test_pool(directory.path()).await);
    (directory, store)
}

#[cfg(test)]
pub(crate) async fn sqlite_auth_code_store() -> (tempfile::TempDir, AuthCodeStore) {
    let directory = tempfile::tempdir().unwrap();
    let store = AuthCodeStore::new(sqlite_test_pool(directory.path()).await);
    (directory, store)
}

#[cfg(test)]
pub(crate) async fn sqlite_run_summary_store() -> (tempfile::TempDir, RunSummaryStore) {
    let directory = tempfile::tempdir().unwrap();
    let store = sqlite_run_summary_store_at(directory.path()).await;
    (directory, store)
}

#[cfg(test)]
pub(crate) async fn sqlite_run_summary_store_at(directory: &Path) -> RunSummaryStore {
    RunSummaryStore::new(sqlite_test_pool(directory).await)
}
