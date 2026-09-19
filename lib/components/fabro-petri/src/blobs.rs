//! Petri's `OutputStore` over Fabro's blob table.
//!
//! A stage value above Petri's offload threshold leaves the run context for
//! the run's blob store and is replaced by the reference
//! `blob://sha256/<hex>`, Fabro's spelling; a later step hydrates it back
//! through the same store. Petri's default store is a directory under the
//! run directory. Installed instead is [`RunBlobs`], which carries every
//! blob to Fabro's `blobs` table: in the server process through its
//! [`fabro_store::BlobStore`], and in a run's worker process through the
//! worker's client ([`ClientBlobs`]), whose run blob endpoints the server
//! answers from the same table. Either way the digest is the same SHA-256
//! hex Fabro's [`BlobHash`] renders, so a reference a Petri record carries
//! names a row Fabro's own readers can fetch.

use std::sync::Arc;

use bytes::Bytes;
use fabro_client::Client;
use fabro_types::{BlobHash, RunId};
use petri_attractor_steps::blobs::{BlobError, BlobStore, OutputStore};

/// Fabro's content-addressed blob table, as a run reaches it.
#[async_trait::async_trait]
pub trait Blobs: Send + Sync {
    /// Store `bytes` and return the hash that names them.
    async fn write(&self, bytes: &[u8]) -> anyhow::Result<BlobHash>;

    /// The bytes behind a hash, or `None` when the table has none.
    async fn read(&self, hash: &BlobHash) -> anyhow::Result<Option<Bytes>>;
}

#[async_trait::async_trait]
impl Blobs for fabro_store::BlobStore {
    async fn write(&self, bytes: &[u8]) -> anyhow::Result<BlobHash> {
        Self::write(self, bytes).await.map_err(anyhow::Error::new)
    }

    async fn read(&self, hash: &BlobHash) -> anyhow::Result<Option<Bytes>> {
        Self::read(self, hash).await.map_err(anyhow::Error::new)
    }
}

/// The blob table as a run's worker reaches it: the run's blob endpoints,
/// with the worker's token.
pub struct ClientBlobs {
    client: Client,
    run_id: RunId,
}

impl ClientBlobs {
    #[must_use]
    pub fn new(client: Client, run_id: RunId) -> Self {
        Self { client, run_id }
    }
}

#[async_trait::async_trait]
impl Blobs for ClientBlobs {
    async fn write(&self, bytes: &[u8]) -> anyhow::Result<BlobHash> {
        self.client.write_run_blob(&self.run_id, bytes).await
    }

    async fn read(&self, hash: &BlobHash) -> anyhow::Result<Option<Bytes>> {
        self.client.read_run_blob(&self.run_id, hash).await
    }
}

/// Petri's blob store over Fabro's blob table.
pub struct RunBlobs {
    blobs: Arc<dyn Blobs>,
}

impl RunBlobs {
    #[must_use]
    pub fn new(blobs: Arc<dyn Blobs>) -> Self {
        Self { blobs }
    }

    /// The capability a runtime installs so every offloaded value goes to
    /// the table.
    #[must_use]
    pub fn output_store(blobs: Arc<dyn Blobs>) -> OutputStore {
        OutputStore(Arc::new(Self::new(blobs)))
    }
}

/// The store's refusal, with the cause chain on one line: Petri's error
/// carries text, not a source.
fn refused(digest: &str, error: &anyhow::Error) -> BlobError {
    BlobError::Store {
        digest:  digest.to_string(),
        message: format!("{error:#}"),
    }
}

#[async_trait::async_trait]
impl BlobStore for RunBlobs {
    async fn put(&self, bytes: &[u8]) -> Result<String, BlobError> {
        let expected = BlobHash::new(bytes);
        let hash = self
            .blobs
            .write(bytes)
            .await
            .map_err(|error| refused(&expected.to_string(), &error))?;
        Ok(hash.to_string())
    }

    async fn get(&self, digest: &str) -> Result<Option<Vec<u8>>, BlobError> {
        let Ok(hash) = digest.parse::<BlobHash>() else {
            // Not a digest the table can hold, so nothing is behind it.
            return Ok(None);
        };
        let bytes = self
            .blobs
            .read(&hash)
            .await
            .map_err(|error| refused(digest, &error))?;
        Ok(bytes.map(|bytes| bytes.to_vec()))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use petri_attractor_steps::blobs::{blob_ref, hydrate, offload_above};
    use petri_runtime::ir::Value;

    use super::*;

    /// A table in memory.
    #[derive(Default)]
    struct MemoryBlobs {
        rows: Mutex<Vec<(BlobHash, Vec<u8>)>>,
    }

    #[async_trait::async_trait]
    impl Blobs for MemoryBlobs {
        async fn write(&self, bytes: &[u8]) -> anyhow::Result<BlobHash> {
            let hash = BlobHash::new(bytes);
            self.rows
                .lock()
                .expect("not poisoned")
                .push((hash, bytes.to_vec()));
            Ok(hash)
        }

        async fn read(&self, hash: &BlobHash) -> anyhow::Result<Option<Bytes>> {
            Ok(self
                .rows
                .lock()
                .expect("not poisoned")
                .iter()
                .find(|(stored, _)| stored == hash)
                .map(|(_, bytes)| Bytes::copy_from_slice(bytes)))
        }
    }

    #[tokio::test]
    async fn a_value_round_trips_through_the_table_under_fabros_reference() {
        let table = Arc::new(MemoryBlobs::default());
        let store = RunBlobs::new(table.clone());
        let mut value = Value::String("x".repeat(10));
        let reference = offload_above(&mut value, &store, 0)
            .await
            .expect("offloaded");
        let hex = BlobHash::new(b"xxxxxxxxxx").to_string();
        assert_eq!(reference, blob_ref(&hex));
        assert_eq!(value, Value::String(reference));
        assert_eq!(hydrate(value, &store).await, Value::String("x".repeat(10)));
        assert_eq!(table.rows.lock().expect("not poisoned").len(), 1);
    }

    #[tokio::test]
    async fn an_unknown_digest_and_a_malformed_one_are_absent() {
        let store = RunBlobs::new(Arc::new(MemoryBlobs::default()));
        assert_eq!(store.get(&"a".repeat(64)).await.expect("reads"), None);
        assert_eq!(store.get("not-a-digest").await.expect("reads"), None);
    }
}
