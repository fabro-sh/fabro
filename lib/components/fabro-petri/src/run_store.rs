//! Petri's run store over Fabro's SQLite database: the Petri store crate's
//! `RunStore` and `RunLogs`, implemented on the pool Fabro's other stores
//! share.
//!
//! # Tables
//!
//! - `petri_runs` is a run's existence and its writer lease: one row per run
//!   key, with the owner holding the lease and when it took it. This is the
//!   lease row the integration plan describes as "a row in `runs`". Petri opens
//!   runs by keys of its own, with no Fabro run row behind them, and `runs` has
//!   columns only a Fabro run can fill, so the lease lives in a table of its
//!   own. The create handler inserts the Fabro `runs` row separately.
//! - `petri_records` holds every record of every log, keyed by `(run_id, log,
//!   seq)`: `recorded_at` lifted out for indexing, and the record itself as
//!   JSON, stored and read back unchanged.
//! - `blobs` is Fabro's content-addressed blob table, shared with
//!   [`BlobStore`]. Petri's digest is the same SHA-256 hex.
//!
//! # The log column
//!
//! `log` is the `LogId` rendered with its `Display`: `coordinator`,
//! `resources`, or `execution <n>` for execution `n`. [`log_id_text`] and
//! [`parse_log_id`] are the two directions, and a test pins the strings.
//!
//! # The lease
//!
//! `Create` inserts the run row and takes the lease in one statement, and
//! refuses an existing key with `Exists`. `Write` takes the lease of an
//! existing key when nobody holds it or when the same owner holds it (a retry
//! after a lost reply gets the same lease), and refuses a live different
//! owner with `Leased`. `Read` takes no lease and never blocks a writer. A
//! same-owner reopen in this process shares the live handle, so the lease
//! lasts while any handle of the owner does.
//!
//! The lease ends when the last handle drops, by an operator's
//! [`SqliteRunStore::release_lease`], or when the server observes the
//! worker's exit and calls the same method. Never by timeout. Dropping a
//! handle spawns the release on the current Tokio runtime, because sqlx has
//! no synchronous path; the store awaits every spawned release before its
//! next `open`, so a drop followed by an open observes the release. With no
//! runtime at drop, the row stays leased until an operator releases it, and
//! the drop says so in the log.
//!
//! Every write checks, inside its own transaction, that the handle's owner
//! still holds the lease, and fails with `StaleOwner` otherwise.
//!
//! # Appends
//!
//! One `BEGIN IMMEDIATE` transaction per batch. A record at a seq below the
//! log's head must equal the stored record as a JSON value, and is then
//! accepted without a second insert (a lost-reply retry is safe). A different
//! record at a taken seq, or a seq past the head, is `Conflict`, and the
//! batch stores nothing. A committed transaction is durable past a process
//! crash: Fabro's pool runs SQLite in WAL mode with `synchronous = NORMAL`.

use std::collections::HashMap;
use std::error::Error;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{fmt, mem, ptr};

use fabro_db::DbPool;
use fabro_store::BlobStore;
use fabro_types::BlobHash;
use petri_store::{
    Access, Digest, ExecutionId, LogId, OwnerId, Record, RunKey, RunLogs, RunStore, StoreError,
};
use serde_json::Value;
use sqlx::{Executor, Sqlite};
use tokio::runtime::Handle;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

const EXECUTION_LOG_PREFIX: &str = "execution ";

/// The `log` column value of a log id: its `Display`.
pub fn log_id_text(log: &LogId) -> String {
    log.to_string()
}

/// The log id a `log` column value names, or `None` when the text is not
/// one [`log_id_text`] produces.
pub fn parse_log_id(text: &str) -> Option<LogId> {
    match text {
        "coordinator" => Some(LogId::Coordinator),
        "resources" => Some(LogId::Resources),
        other => other
            .strip_prefix(EXECUTION_LOG_PREFIX)?
            .parse::<u64>()
            .ok()
            .map(|id| LogId::Execution(ExecutionId::new(id))),
    }
}

/// Petri's run store over Fabro's SQLite database.
pub struct SqliteRunStore {
    shared: Arc<Shared>,
}

impl fmt::Debug for SqliteRunStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SqliteRunStore")
            .field("database", &self.shared.database)
            .finish_non_exhaustive()
    }
}

/// What the store and every handle it opens share.
struct Shared {
    pool:     DbPool,
    blobs:    BlobStore,
    /// The database file, for locators.
    database: String,
    /// The writer handle alive in this process per run, so a same-owner
    /// reopen shares it and the lease lasts while any handle does.
    live:     Mutex<HashMap<RunKey, Weak<SqliteRunLogs>>>,
    /// The releases dropped handles spawned, awaited before the next open.
    releases: Mutex<Vec<JoinHandle<()>>>,
}

impl SqliteRunStore {
    /// A store over a pool whose migrations have run.
    #[must_use]
    pub fn new(pool: DbPool) -> Self {
        let database = pool.connect_options().get_filename().display().to_string();
        Self {
            shared: Arc::new(Shared {
                blobs: BlobStore::new(pool.clone()),
                pool,
                database,
                live: Mutex::default(),
                releases: Mutex::default(),
            }),
        }
    }

    /// End the writer lease of `key` from outside, as an operator does, or
    /// as the server does when it observes the worker that held it exit.
    /// The holder's handles turn stale, and the next `Write` open takes the
    /// run. `NotFound` when the store does not hold the key.
    pub async fn release_lease(&self, key: &RunKey) -> Result<(), StoreError> {
        self.shared.drain_releases().await;
        let result = sqlx::query(
            "UPDATE petri_runs SET owner_id = NULL, acquired_at_ms = NULL WHERE run_id = ?",
        )
        .bind(key.as_str())
        .execute(&self.shared.pool)
        .await
        .map_err(|cause| self.shared.backend(key, "release the run's lease", cause))?;
        if result.rows_affected() == 0 {
            return Err(self.shared.not_found(key));
        }
        lock(&self.shared.live).remove(key);
        debug!(run_id = %key, "Petri run lease released from outside");
        Ok(())
    }

    /// The owner holding the writer lease of `key`, if any. `NotFound` when
    /// the store does not hold the key.
    pub async fn owner(&self, key: &RunKey) -> Result<Option<OwnerId>, StoreError> {
        self.shared.drain_releases().await;
        let holder: Option<Option<String>> =
            sqlx::query_scalar("SELECT owner_id FROM petri_runs WHERE run_id = ?")
                .bind(key.as_str())
                .fetch_optional(&self.shared.pool)
                .await
                .map_err(|cause| self.shared.backend(key, "read the run's lease", cause))?;
        match holder {
            None => Err(self.shared.not_found(key)),
            Some(holder) => Ok(holder.map(OwnerId::new)),
        }
    }

    /// The writer handle for `owner`, once the lease is taken: the live one
    /// when this owner already holds a handle here, else a new one.
    fn writer(&self, key: &RunKey, owner: OwnerId) -> Arc<dyn RunLogs> {
        let mut live = lock(&self.shared.live);
        if let Some(handle) = live.get(key).and_then(Weak::upgrade) {
            if handle.owner.as_ref() == Some(&owner) {
                return handle;
            }
        }
        let handle = Arc::new(SqliteRunLogs {
            shared: self.shared.clone(),
            key:    key.clone(),
            owner:  Some(owner),
        });
        live.insert(key.clone(), Arc::downgrade(&handle));
        handle
    }
}

impl Shared {
    fn locator(&self, key: &RunKey) -> String {
        format!("sqlite database {}, run `{key}`", self.database)
    }

    fn backend(
        &self,
        key: &RunKey,
        action: &'static str,
        cause: impl Into<Box<dyn Error + Send + Sync>>,
    ) -> StoreError {
        StoreError::backend(self.locator(key), action, cause)
    }

    fn not_found(&self, key: &RunKey) -> StoreError {
        StoreError::NotFound {
            key:     key.clone(),
            locator: self.locator(key),
        }
    }

    /// Await every release a dropped handle spawned, so what follows sees
    /// the lease as the drops left it.
    async fn drain_releases(&self) {
        let pending = mem::take(&mut *lock(&self.releases));
        for release in pending {
            // A release task never panics: it reports its own failure.
            let _ = release.await;
        }
    }

    /// Take the lease of an existing run for `owner`, or share it when the
    /// same owner holds it.
    async fn take_lease(&self, key: &RunKey, owner: &OwnerId) -> Result<(), StoreError> {
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|cause| self.backend(key, "take the run's lease", cause))?;
        let holder: Option<Option<String>> =
            sqlx::query_scalar("SELECT owner_id FROM petri_runs WHERE run_id = ?")
                .bind(key.as_str())
                .fetch_optional(&mut *tx)
                .await
                .map_err(|cause| self.backend(key, "take the run's lease", cause))?;
        match holder {
            None => return Err(self.not_found(key)),
            Some(Some(holder)) if holder != owner.as_str() => {
                return Err(StoreError::Leased {
                    locator: self.locator(key),
                    owner:   OwnerId::new(holder),
                });
            }
            Some(Some(_)) => {
                debug!(run_id = %key, owner = %owner, "Petri run lease shared with its holder");
            }
            Some(None) => {
                sqlx::query(
                    "UPDATE petri_runs SET owner_id = ?, acquired_at_ms = ? WHERE run_id = ?",
                )
                .bind(owner.as_str())
                .bind(now_ms())
                .bind(key.as_str())
                .execute(&mut *tx)
                .await
                .map_err(|cause| self.backend(key, "take the run's lease", cause))?;
                debug!(run_id = %key, owner = %owner, "Petri run lease taken");
            }
        }
        tx.commit()
            .await
            .map_err(|cause| self.backend(key, "take the run's lease", cause))
    }

    /// Whether `owner` still holds the lease of `key`, read through
    /// `executor` so a write's check sits in the write's own transaction.
    async fn check_owner<'c, E>(
        &self,
        executor: E,
        key: &RunKey,
        owner: &OwnerId,
    ) -> Result<(), StoreError>
    where
        E: Executor<'c, Database = Sqlite>,
    {
        let holder: Option<Option<String>> =
            sqlx::query_scalar("SELECT owner_id FROM petri_runs WHERE run_id = ?")
                .bind(key.as_str())
                .fetch_optional(executor)
                .await
                .map_err(|cause| self.backend(key, "check the run's lease", cause))?;
        match holder {
            Some(Some(holder)) if holder == owner.as_str() => Ok(()),
            _ => Err(StoreError::StaleOwner),
        }
    }

    /// End the lease of `key` when `owner` still holds it: what a dropped
    /// handle does. A lease that already moved is left alone.
    async fn release_owner(&self, key: &RunKey, owner: &OwnerId) {
        let released = sqlx::query(
            "UPDATE petri_runs SET owner_id = NULL, acquired_at_ms = NULL \
             WHERE run_id = ? AND owner_id = ?",
        )
        .bind(key.as_str())
        .bind(owner.as_str())
        .execute(&self.pool)
        .await;
        match released {
            Ok(result) if result.rows_affected() == 1 => {
                debug!(run_id = %key, owner = %owner, "Petri run lease released at drop");
            }
            Ok(_) => {
                debug!(run_id = %key, owner = %owner, "Petri run lease had already moved at drop");
            }
            Err(error) => {
                warn!(
                    run_id = %key,
                    owner = %owner,
                    error = %error,
                    "Petri run lease not released at drop; release it from outside"
                );
            }
        }
    }
}

#[async_trait::async_trait]
impl RunStore for SqliteRunStore {
    async fn open(&self, key: &RunKey, access: Access) -> Result<Arc<dyn RunLogs>, StoreError> {
        let shared = &self.shared;
        shared.drain_releases().await;
        match access {
            Access::Create { owner } => {
                let now = now_ms();
                let result = sqlx::query(
                    "INSERT INTO petri_runs (run_id, created_at_ms, owner_id, acquired_at_ms) \
                     VALUES (?, ?, ?, ?) ON CONFLICT(run_id) DO NOTHING",
                )
                .bind(key.as_str())
                .bind(now)
                .bind(owner.as_str())
                .bind(now)
                .execute(&shared.pool)
                .await
                .map_err(|cause| shared.backend(key, "create the run", cause))?;
                if result.rows_affected() == 0 {
                    return Err(StoreError::Exists {
                        key:     key.clone(),
                        locator: shared.locator(key),
                    });
                }
                debug!(run_id = %key, owner = %owner, "Petri run created");
                Ok(self.writer(key, owner))
            }
            Access::Write { owner } => {
                shared.take_lease(key, &owner).await?;
                Ok(self.writer(key, owner))
            }
            Access::Read => {
                let exists: bool =
                    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM petri_runs WHERE run_id = ?)")
                        .bind(key.as_str())
                        .fetch_one(&shared.pool)
                        .await
                        .map_err(|cause| shared.backend(key, "open the run", cause))?;
                if !exists {
                    return Err(shared.not_found(key));
                }
                Ok(Arc::new(SqliteRunLogs {
                    shared: shared.clone(),
                    key:    key.clone(),
                    owner:  None,
                }))
            }
        }
    }
}

/// One run in the database, opened. A writer handle carries the owner it
/// was opened with; a reader handle refuses every mutation.
struct SqliteRunLogs {
    shared: Arc<Shared>,
    key:    RunKey,
    owner:  Option<OwnerId>,
}

impl SqliteRunLogs {
    fn owner(&self) -> Result<&OwnerId, StoreError> {
        self.owner.as_ref().ok_or(StoreError::ReadOnly)
    }

    fn backend(
        &self,
        action: &'static str,
        cause: impl Into<Box<dyn Error + Send + Sync>>,
    ) -> StoreError {
        self.shared.backend(&self.key, action, cause)
    }

    /// A stored `record_json` back into the record it was.
    fn decode(&self, json: &str) -> Result<Record, StoreError> {
        let value: Value = serde_json::from_str(json)
            .map_err(|cause| self.backend("decode a stored record", cause))?;
        Record::from_value(value).map_err(|cause| self.backend("decode a stored record", cause))
    }

    /// A seq as SQLite stores it.
    fn column(&self, value: u64) -> Result<i64, StoreError> {
        i64::try_from(value).map_err(|cause| self.backend("encode a record", cause))
    }
}

impl Drop for SqliteRunLogs {
    fn drop(&mut self) {
        let Some(owner) = self.owner.clone() else {
            return;
        };
        {
            let mut live = lock(&self.shared.live);
            let this: *const Self = self;
            if live
                .get(&self.key)
                .is_some_and(|weak| ptr::eq(weak.as_ptr(), this))
            {
                live.remove(&self.key);
            }
        }
        match Handle::try_current() {
            Ok(runtime) => {
                let shared = self.shared.clone();
                let key = self.key.clone();
                let release = runtime.spawn(async move {
                    shared.release_owner(&key, &owner).await;
                });
                lock(&self.shared.releases).push(release);
            }
            Err(_) => {
                warn!(
                    run_id = %self.key,
                    owner = %owner,
                    "Petri run lease not released at drop: no async runtime; release it from outside"
                );
            }
        }
    }
}

#[async_trait::async_trait]
impl RunLogs for SqliteRunLogs {
    fn locator(&self) -> String {
        self.shared.locator(&self.key)
    }

    async fn append(&self, log: &LogId, records: &[Record]) -> Result<(), StoreError> {
        let owner = self.owner()?;
        let mut tx = self
            .shared
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|cause| self.backend("begin an append", cause))?;
        self.shared.check_owner(&mut *tx, &self.key, owner).await?;
        let log_text = log_id_text(log);
        let head: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(seq) + 1, 0) FROM petri_records WHERE run_id = ? AND log = ?",
        )
        .bind(self.key.as_str())
        .bind(&log_text)
        .fetch_one(&mut *tx)
        .await
        .map_err(|cause| self.backend("read the log's head", cause))?;
        let mut next =
            u64::try_from(head).map_err(|cause| self.backend("read the log's head", cause))?;
        for record in records {
            let conflict = || StoreError::Conflict {
                log: *log,
                seq: record.seq,
            };
            if record.seq < next {
                let stored: Option<String> = sqlx::query_scalar(
                    "SELECT record_json FROM petri_records WHERE run_id = ? AND log = ? AND seq = ?",
                )
                .bind(self.key.as_str())
                .bind(&log_text)
                .bind(self.column(record.seq)?)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|cause| self.backend("read a stored record", cause))?;
                let same = match stored {
                    Some(json) => self.decode(&json)? == *record,
                    None => false,
                };
                if same {
                    continue;
                }
                return Err(conflict());
            }
            if record.seq != next {
                return Err(conflict());
            }
            let json = serde_json::to_string(&record.record)
                .map_err(|cause| self.backend("encode a record", cause))?;
            sqlx::query(
                "INSERT INTO petri_records (run_id, log, seq, recorded_at, record_json) \
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(self.key.as_str())
            .bind(&log_text)
            .bind(self.column(record.seq)?)
            .bind(self.column(record.recorded_at)?)
            .bind(json)
            .execute(&mut *tx)
            .await
            .map_err(|cause| self.backend("append a record", cause))?;
            next += 1;
        }
        tx.commit()
            .await
            .map_err(|cause| self.backend("commit an append", cause))
    }

    async fn read(&self, log: &LogId) -> Result<Vec<Record>, StoreError> {
        self.read_from(log, 0).await
    }

    async fn read_from(&self, log: &LogId, seq: u64) -> Result<Vec<Record>, StoreError> {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT record_json FROM petri_records WHERE run_id = ? AND log = ? AND seq >= ? ORDER \
             BY seq",
        )
        .bind(self.key.as_str())
        .bind(log_id_text(log))
        .bind(i64::try_from(seq).unwrap_or(i64::MAX))
        .fetch_all(&self.shared.pool)
        .await
        .map_err(|cause| self.backend("read a log", cause))?;
        rows.iter().map(|json| self.decode(json)).collect()
    }

    async fn put_blob(&self, bytes: &[u8]) -> Result<Digest, StoreError> {
        let owner = self.owner()?;
        self.shared
            .check_owner(&self.shared.pool, &self.key, owner)
            .await?;
        self.shared
            .blobs
            .write(bytes)
            .await
            .map_err(|cause| self.backend("store a blob", cause))?;
        Ok(Digest::of(bytes))
    }

    async fn get_blob(&self, digest: Digest) -> Result<Option<Vec<u8>>, StoreError> {
        let hash: BlobHash = digest
            .to_hex()
            .parse()
            .map_err(|cause| self.backend("read a blob", cause))?;
        let bytes = self
            .shared
            .blobs
            .read(&hash)
            .await
            .map_err(|cause| self.backend("read a blob", cause))?;
        Ok(bytes.map(|bytes| bytes.to_vec()))
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Milliseconds since the Unix epoch, as SQLite stores them.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_ids_round_trip_through_their_text() {
        let logs = [
            (LogId::Coordinator, "coordinator"),
            (LogId::Resources, "resources"),
            (LogId::Execution(ExecutionId::new(0)), "execution 0"),
            (LogId::Execution(ExecutionId::new(42)), "execution 42"),
        ];
        for (log, text) in logs {
            assert_eq!(log_id_text(&log), text);
            assert_eq!(parse_log_id(text), Some(log));
        }
        assert_eq!(parse_log_id("execution"), None);
        assert_eq!(parse_log_id("execution x"), None);
        assert_eq!(parse_log_id("engine 1"), None);
    }
}
