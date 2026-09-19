use bytes::Bytes;
use fabro_types::BlobHash;
use sqlx::SqlitePool;

use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blob(pub Bytes);

impl AsRef<[u8]> for Blob {
    fn as_ref(&self) -> &[u8] {
        self.0.as_ref()
    }
}

impl From<Bytes> for Blob {
    fn from(value: Bytes) -> Self {
        Self(value)
    }
}

/// The content-addressed `blobs` table: every blob once, under its
/// SHA-256.
pub struct BlobStore {
    pool: SqlitePool,
}

impl std::fmt::Debug for BlobStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlobStore").finish_non_exhaustive()
    }
}

impl BlobStore {
    /// Creates a blob store backed by a SQLite pool whose migrations have run.
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn write(&self, bytes: &[u8]) -> Result<BlobHash> {
        let blob_hash = BlobHash::new(bytes);
        let result = sqlx::query(
            "INSERT INTO blobs (hash, data) VALUES (?, ?) \
             ON CONFLICT(hash) DO NOTHING",
        )
        .bind(blob_hash.to_string())
        .bind(bytes)
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 1 {
            return Ok(blob_hash);
        }

        let stored: Vec<u8> = sqlx::query_scalar("SELECT data FROM blobs WHERE hash = ?")
            .bind(blob_hash.to_string())
            .fetch_one(&self.pool)
            .await?;
        if stored == bytes {
            Ok(blob_hash)
        } else {
            Err(Error::BlobHashConflict { blob_hash })
        }
    }

    pub async fn read(&self, blob_hash: &BlobHash) -> Result<Option<Bytes>> {
        let stored: Option<Vec<u8>> = sqlx::query_scalar("SELECT data FROM blobs WHERE hash = ?")
            .bind(blob_hash.to_string())
            .fetch_optional(&self.pool)
            .await?;
        let Some(stored) = stored else {
            return Ok(None);
        };
        if BlobHash::new(&stored) != *blob_hash {
            return Err(Error::BlobIntegrity {
                blob_hash: *blob_hash,
            });
        }
        Ok(Some(Bytes::from(stored)))
    }

    pub async fn exists(&self, blob_hash: &BlobHash) -> Result<bool> {
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM blobs WHERE hash = ?)")
            .bind(blob_hash.to_string())
            .fetch_one(&self.pool)
            .await?;
        Ok(exists)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use bytes::Bytes;
    use fabro_types::BlobHash;

    use super::BlobStore;
    use crate::Error;

    type TestResult<T> = std::result::Result<T, Box<dyn std::error::Error>>;

    async fn sqlite_store() -> TestResult<(tempfile::TempDir, fabro_db::Database, BlobStore)> {
        let dir = tempfile::tempdir()?;
        let database = fabro_db::Database::connect(dir.path().join("fabro.sqlite3")).await?;
        database.migrate().await?;
        let store = BlobStore::new(database.clone_pool());
        Ok((dir, database, store))
    }

    #[tokio::test]
    async fn sqlite_writes_reads_and_checks_existence() -> TestResult<()> {
        let (_dir, database, store) = sqlite_store().await?;
        let store = Arc::new(store);

        let binary = [0_u8, 0xff, 0x80, b'a'];
        let (first_write, concurrent_write) =
            tokio::join!(store.write(&binary), store.write(&binary));
        let binary_hash = first_write?;
        assert_eq!(concurrent_write?, binary_hash);
        let empty_hash = store.write(b"").await?;

        assert_eq!(store.write(&binary).await?, binary_hash);
        assert_eq!(
            store.read(&binary_hash).await?,
            Some(Bytes::copy_from_slice(&binary))
        );
        assert_eq!(store.read(&empty_hash).await?, Some(Bytes::new()));
        assert!(store.exists(&binary_hash).await?);
        let missing_hash = BlobHash::new(b"missing");
        assert_eq!(store.read(&missing_hash).await?, None);
        assert!(!store.exists(&missing_hash).await?);

        let row_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM blobs")
            .fetch_one(database.pool())
            .await?;
        assert_eq!(row_count, 2);
        Ok(())
    }

    #[tokio::test]
    async fn sqlite_write_rejects_conflicting_stored_bytes() -> TestResult<()> {
        let (_dir, database, store) = sqlite_store().await?;
        let expected = b"expected";
        let blob_hash = BlobHash::new(expected);
        sqlx::query("INSERT INTO blobs (hash, data) VALUES (?, ?)")
            .bind(blob_hash.to_string())
            .bind(b"different".as_slice())
            .execute(database.pool())
            .await?;

        let error = store
            .write(expected)
            .await
            .expect_err("conflicting bytes should fail");
        assert!(
            matches!(error, Error::BlobHashConflict { blob_hash: value } if value == blob_hash)
        );
        Ok(())
    }

    #[tokio::test]
    async fn sqlite_read_rejects_bytes_that_do_not_match_hash() -> TestResult<()> {
        let (_dir, database, store) = sqlite_store().await?;
        let blob_hash = BlobHash::new(b"expected");
        sqlx::query("INSERT INTO blobs (hash, data) VALUES (?, ?)")
            .bind(blob_hash.to_string())
            .bind(b"different".as_slice())
            .execute(database.pool())
            .await?;

        let error = store
            .read(&blob_hash)
            .await
            .expect_err("mismatched stored bytes should fail");
        assert!(matches!(error, Error::BlobIntegrity { blob_hash: value } if value == blob_hash));
        Ok(())
    }
}
