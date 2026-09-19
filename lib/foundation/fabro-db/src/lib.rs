use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context as _;
use chrono::{DateTime, Utc};
use sqlx::migrate::{Migrate as _, Migrator};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use tokio::fs;
#[cfg(unix)]
use tokio::task::spawn_blocking;
use tracing::info;

pub type DbPool = sqlx::SqlitePool;

static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// The blob-table migration, exposed so fixtures in other crates can install
/// the production blob schema without a filesystem path into this crate.
pub const BLOBS_MIGRATION_SQL: &str = include_str!("../migrations/2026081301_blobs.sql");

/// The run summary migration, exposed so fixtures in other crates can install
/// the production schema without a filesystem path into this crate.
pub const RUNS_MIGRATION_SQL: &str = include_str!("../migrations/2026071104_runs.sql");

/// The migration that drops the legacy run event log and narrows the `runs`
/// row to its final shape, exposed so fixtures that install
/// [`RUNS_MIGRATION_SQL`] can apply it next and get the production schema.
pub const DROP_RUN_EVENTS_MIGRATION_SQL: &str =
    include_str!("../migrations/2026091803_drop_run_events.sql");

/// The Ask Fabro session record migration, exposed so fixtures in other
/// crates can install the production schema without a filesystem path into
/// this crate.
pub const RUN_SESSION_RECORDS_MIGRATION_SQL: &str =
    include_str!("../migrations/2026091101_run_session_records.sql");

/// The Ask Fabro session events migration (`run_session_events`), exposed so
/// fixtures in other crates can install the production schema without a
/// filesystem path into this crate.
pub const RUN_SESSION_EVENTS_MIGRATION_SQL: &str =
    include_str!("../migrations/2026091802_run_session_events.sql");

/// The Petri run record migration (`petri_runs`, `petri_records`), exposed
/// so fixtures in other crates can install the production schema without a
/// filesystem path into this crate.
pub const PETRI_RECORDS_MIGRATION_SQL: &str =
    include_str!("../migrations/2026091701_petri_records.sql");

/// The Petri projection migration (`platform_records`, `petri_projection`,
/// `petri_stream`), exposed so fixtures in other crates can install the
/// production schema without a filesystem path into this crate.
pub const PETRI_PROJECTION_MIGRATION_SQL: &str =
    include_str!("../migrations/2026091801_petri_projection.sql");

#[derive(Clone)]
pub struct Database {
    pool: DbPool,
}

impl Database {
    pub async fn connect(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await.with_context(|| {
                format!("creating SQLite database directory {}", parent.display())
            })?;
        }

        prepare_private_database_file(path).await?;

        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(Duration::from_secs(5));

        let pool = SqlitePoolOptions::new()
            .connect_with(options)
            .await
            .with_context(|| format!("opening SQLite database {}", path.display()))?;

        Ok(Self { pool })
    }

    pub async fn migrate(&self) -> anyhow::Result<()> {
        let applied = applied_migration_versions(&self.pool).await?;
        self.snapshot_before_new_migrations(&applied)
            .await
            .context("snapshotting SQLite database before migrations")?;
        MIGRATOR
            .run(&self.pool)
            .await
            .context("running SQLite migrations")
    }

    /// Copy the database aside before applying migrations it has not seen.
    ///
    /// A binary downgrade after new migrations have been applied fails sqlx's
    /// startup validation (`migration N was previously applied but is missing
    /// in the resolved migrations`), so the snapshot written to
    /// [`pre_migration_snapshot_path`] is the operator's rollback artifact:
    /// stop the server, replace the database file with the snapshot (and
    /// delete any `-wal`/`-shm` siblings), and the previous binary boots
    /// again. Writes made after the upgrade are lost on rollback, as with any
    /// point-in-time restore.
    ///
    /// The snapshot is only taken when the database has applied migrations
    /// before (a fresh database has nothing worth preserving) and at least
    /// one bundled migration is pending, so the file always holds the state
    /// from immediately before the most recent schema change. Failing to
    /// write the snapshot fails the migration: no rollback artifact, no
    /// schema change.
    async fn snapshot_before_new_migrations(&self, applied: &HashSet<i64>) -> anyhow::Result<()> {
        let has_pending = MIGRATOR
            .iter()
            .any(|migration| !applied.contains(&migration.version));
        if applied.is_empty() || !has_pending {
            return Ok(());
        }

        let connect_options = self.pool.connect_options();
        let database_path = connect_options.get_filename();
        let snapshot_path = pre_migration_snapshot_path(database_path);

        // The snapshot is staged and then renamed into place, so a failure
        // mid-copy never leaves a partial file at the snapshot path.
        let staging_path = append_to_path(&snapshot_path, ".tmp");
        write_snapshot_to_staging(&self.pool, &staging_path)
            .await
            .context("staging the pre-migration snapshot")?;
        remove_file_if_exists(&snapshot_path)
            .await
            .with_context(|| {
                format!(
                    "removing stale pre-migration snapshot {}",
                    snapshot_path.display()
                )
            })?;
        fs::rename(&staging_path, &snapshot_path)
            .await
            .with_context(|| {
                format!(
                    "publishing pre-migration snapshot {}",
                    snapshot_path.display()
                )
            })?;
        #[cfg(unix)]
        {
            let published_path = snapshot_path.clone();
            spawn_blocking(move || sync_parent_directory(&published_path))
                .await
                .context("joining the snapshot directory sync task")?
                .with_context(|| {
                    format!(
                        "syncing the directory of pre-migration snapshot {}",
                        snapshot_path.display()
                    )
                })?;
        }

        info!(
            database = %database_path.display(),
            snapshot = %snapshot_path.display(),
            "Snapshotted SQLite database before applying new migrations"
        );
        Ok(())
    }

    pub async fn health_check(&self) -> anyhow::Result<()> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .context("checking SQLite database health")?;
        Ok(())
    }

    pub fn pool(&self) -> &DbPool {
        &self.pool
    }

    pub fn clone_pool(&self) -> DbPool {
        self.pool.clone()
    }
}

/// Parse an RFC 3339 timestamp column value into UTC.
pub fn parse_rfc3339(value: &str) -> Result<DateTime<Utc>, chrono::ParseError> {
    DateTime::parse_from_rfc3339(value).map(|timestamp| timestamp.with_timezone(&Utc))
}

/// Result of a one-time import of a legacy file or directory into SQLite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportReport {
    pub source_path:   PathBuf,
    pub backup_path:   PathBuf,
    pub imported_rows: usize,
    pub skipped_rows:  usize,
    pub names:         Vec<String>,
}

/// Backup destination for a legacy file or directory after a one-time import
/// into SQLite. `default_name` is used when `source_path` has no file name.
pub fn legacy_backup_path(
    source_path: &Path,
    default_name: &str,
    imported_at: DateTime<Utc>,
) -> PathBuf {
    let timestamp = imported_at.format("%Y%m%dT%H%M%S%fZ");
    let mut file_name = source_path
        .file_name()
        .map_or_else(|| OsString::from(default_name), OsString::from);
    file_name.push(format!(".imported-{timestamp}.bak"));
    source_path.with_file_name(file_name)
}

/// Rollback artifact written by [`Database::migrate`] before applying new
/// migrations: the database path with `.pre-migration.bak` appended.
pub fn pre_migration_snapshot_path(database_path: &Path) -> PathBuf {
    append_to_path(database_path, ".pre-migration.bak")
}

/// Returns `path` with `suffix` appended to its final component, preserving
/// any extension (`fabro.sqlite3` + `-wal` → `fabro.sqlite3-wal`).
pub fn append_to_path(path: &Path, suffix: &str) -> PathBuf {
    let mut path = path.as_os_str().to_os_string();
    path.push(suffix);
    PathBuf::from(path)
}

async fn applied_migration_versions(pool: &DbPool) -> anyhow::Result<HashSet<i64>> {
    let mut conn = pool
        .acquire()
        .await
        .context("acquiring a SQLite connection")?;
    // Migrator::run performs this same ensure + list as its first step, so
    // asking the Migrate trait (rather than querying sqlx's bookkeeping
    // table by hand) cannot drift from what it will actually apply.
    conn.ensure_migrations_table(&MIGRATOR.table_name)
        .await
        .context("ensuring the sqlx migrations table exists")?;
    let applied = conn
        .list_applied_migrations(&MIGRATOR.table_name)
        .await
        .context("listing applied migration versions")?;
    Ok(applied
        .into_iter()
        .map(|migration| migration.version)
        .collect())
}

/// Error writing a consistent single-file SQLite snapshot to a staging path.
#[derive(Debug, thiserror::Error)]
pub enum SnapshotStagingError {
    #[error("removing stale snapshot staging file {path}")]
    RemoveStale {
        path:   PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("creating private snapshot staging area for {path}")]
    CreatePrivateStagingArea {
        path:   PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("snapshot staging path is not valid UTF-8 at {path}")]
    NonUtf8Path { path: PathBuf },
    #[error("writing SQLite snapshot {path}")]
    Write {
        path:   PathBuf,
        #[source]
        source: sqlx::Error,
    },
    #[error("setting private permissions on snapshot staging file {path}")]
    SetPermissions {
        path:   PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("flushing snapshot staging file {path} to disk")]
    Sync {
        path:   PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("publishing private snapshot staging file {path}")]
    Publish {
        path:   PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Writes a consistent single-file copy of the live pool to `staging_path`.
///
/// `VACUUM INTO` produces a snapshot that needs no `-wal`/`-shm` siblings to
/// restore. The copy is first written inside a private same-directory staging
/// area, then restricted to owner-only permissions and flushed before it is
/// exposed at `staging_path`. The caller publishes the staging file into its
/// final path and owns the durability of that rename.
pub async fn write_snapshot_to_staging(
    pool: &DbPool,
    staging_path: &Path,
) -> Result<(), SnapshotStagingError> {
    write_snapshot_to_staging_inner(pool, staging_path, |_| {}).await
}

async fn write_snapshot_to_staging_inner<F>(
    pool: &DbPool,
    staging_path: &Path,
    after_write: F,
) -> Result<(), SnapshotStagingError>
where
    F: FnOnce(&Path),
{
    remove_file_if_exists(staging_path)
        .await
        .map_err(|source| SnapshotStagingError::RemoveStale {
            path: staging_path.to_path_buf(),
            source,
        })?;

    let staging_parent = nonempty_parent(staging_path);
    // SQLite's VACUUM INTO creates its destination with umask-derived
    // permissions. Keep that file behind an owner-only directory until its
    // own mode is restricted, so a traversable database directory never
    // exposes a partially written snapshot.
    let private_staging_area = create_private_staging_area(staging_parent).map_err(|source| {
        SnapshotStagingError::CreatePrivateStagingArea {
            path: staging_path.to_path_buf(),
            source,
        }
    })?;
    let private_staging_path = private_staging_area.path().join("snapshot.sqlite3");
    let staging_target =
        private_staging_path
            .to_str()
            .ok_or_else(|| SnapshotStagingError::NonUtf8Path {
                path: staging_path.to_path_buf(),
            })?;
    sqlx::query("VACUUM INTO ?")
        .bind(staging_target)
        .execute(pool)
        .await
        .map_err(|source| SnapshotStagingError::Write {
            path: staging_path.to_path_buf(),
            source,
        })?;
    after_write(&private_staging_path);
    set_private_permissions(&private_staging_path)
        .await
        .map_err(|source| SnapshotStagingError::SetPermissions {
            path: staging_path.to_path_buf(),
            source,
        })?;
    // The staging file must be durable before the caller renames it into a
    // path that later recovery logic treats as a complete snapshot.
    let sync_result = match fs::File::open(&private_staging_path).await {
        Ok(file) => file.sync_all().await,
        Err(source) => Err(source),
    };
    sync_result.map_err(|source| SnapshotStagingError::Sync {
        path: staging_path.to_path_buf(),
        source,
    })?;
    fs::rename(&private_staging_path, staging_path)
        .await
        .map_err(|source| SnapshotStagingError::Publish {
            path: staging_path.to_path_buf(),
            source,
        })?;
    Ok(())
}

fn create_private_staging_area(parent: &Path) -> std::io::Result<tempfile::TempDir> {
    let mut builder = tempfile::Builder::new();
    builder.prefix(".fabro-snapshot-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    builder.tempdir_in(parent)
}

fn nonempty_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

/// Flushes the directory entry metadata for `path`'s parent so a rename into
/// that directory survives power loss. No-op off Unix, where a directory
/// cannot be opened for syncing.
#[cfg(unix)]
#[expect(
    clippy::disallowed_methods,
    reason = "directory fds have no async open; callers run this on a blocking thread"
)]
pub fn sync_parent_directory(path: &Path) -> std::io::Result<()> {
    std::fs::File::open(nonempty_parent(path))?.sync_all()
}

/// Flushes the directory entry metadata for `path`'s parent so a rename into
/// that directory survives power loss. No-op off Unix, where a directory
/// cannot be opened for syncing.
#[cfg(not(unix))]
pub fn sync_parent_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Removes `path`, treating an already-missing file as success.
async fn remove_file_if_exists(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

/// Restricts `path` to owner-only access (0o600). No-op off Unix.
#[cfg(unix)]
async fn set_private_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await
}

/// Restricts `path` to owner-only access (0o600). No-op off Unix.
#[cfg(not(unix))]
async fn set_private_permissions(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
#[expect(
    clippy::disallowed_methods,
    reason = "SQLite file permissions must be established synchronously before opening the pool"
)]
async fn prepare_private_database_file(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

    let path = path.to_path_buf();
    spawn_blocking(move || {
        std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(&path)
            .with_context(|| format!("creating private SQLite database {}", path.display()))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("setting private SQLite permissions on {}", path.display()))
    })
    .await
    .context("joining SQLite permission setup task")?
}

#[cfg(not(unix))]
async fn prepare_private_database_file(path: &Path) -> anyhow::Result<()> {
    fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(path)
        .await
        .with_context(|| format!("creating SQLite database {}", path.display()))?;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    #[tokio::test]
    async fn snapshot_stays_hidden_until_it_has_private_permissions() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let database_directory = root.path().join("traversable-db");
        fs::create_dir(&database_directory).await?;
        fs::set_permissions(&database_directory, std::fs::Permissions::from_mode(0o755)).await?;

        let database = Database::connect(database_directory.join("fabro.sqlite3")).await?;
        sqlx::query("CREATE TABLE snapshot_secret (value TEXT NOT NULL)")
            .execute(database.pool())
            .await?;
        sqlx::query("INSERT INTO snapshot_secret (value) VALUES ('kept')")
            .execute(database.pool())
            .await?;

        let staging_path = database_directory.join("snapshot.tmp");
        let observed_private_stage = AtomicBool::new(false);
        write_snapshot_to_staging_inner(database.pool(), &staging_path, |private_path| {
            assert!(
                !staging_path.exists(),
                "the traversable parent must not expose the snapshot before chmod"
            );
            let private_directory = private_path
                .parent()
                .expect("private staging file should have a parent");
            let mode = std::fs::metadata(private_directory)
                .expect("private staging directory should exist")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o700, "temporary staging directory must be private");
            observed_private_stage.store(true, Ordering::Relaxed);
        })
        .await?;
        assert!(observed_private_stage.load(Ordering::Relaxed));

        let mode = fs::metadata(&staging_path).await?.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "published staging file must be private");
        let snapshot = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&staging_path)
                    .read_only(true),
            )
            .await?;
        let value: String = sqlx::query_scalar("SELECT value FROM snapshot_secret")
            .fetch_one(&snapshot)
            .await?;
        assert_eq!(value, "kept");
        snapshot.close().await;
        Ok(())
    }
}
