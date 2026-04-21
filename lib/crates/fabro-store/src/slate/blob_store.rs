use std::sync::Arc;

use bytes::Bytes;
use fabro_types::RunBlobId;

use crate::Result;
use crate::record::{RawBytesCodec, Record, Repository};

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

impl Record for Blob {
    type Id = RunBlobId;
    type Codec = RawBytesCodec;

    const PREFIX: &'static str = "blobs/sha256";

    fn id(&self) -> Self::Id {
        RunBlobId::new(&self.0)
    }
}

pub struct BlobStore {
    repo: Repository<Blob>,
}

impl std::fmt::Debug for BlobStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlobStore").finish_non_exhaustive()
    }
}

impl BlobStore {
    pub(crate) fn new(db: Arc<slatedb::Db>) -> Self {
        Self {
            repo: Repository::new(db),
        }
    }

    pub async fn write(&self, bytes: &[u8]) -> Result<RunBlobId> {
        let blob = Blob(Bytes::copy_from_slice(bytes));
        let id = blob.id();
        self.repo.put(&blob).await?;
        Ok(id)
    }

    pub async fn read(&self, id: &RunBlobId) -> Result<Option<Bytes>> {
        Ok(self.repo.get(id).await?.map(|blob| blob.0))
    }

    pub async fn exists(&self, id: &RunBlobId) -> Result<bool> {
        self.repo.exists(id).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use bytes::Bytes;
    use fabro_types::RunBlobId;
    use object_store::memory::InMemory;

    use super::BlobStore;
    use crate::Database;
    use crate::keys::SlateKey;

    async fn store() -> Arc<BlobStore> {
        let db = Database::new(
            Arc::new(InMemory::new()),
            "",
            Duration::from_millis(1),
            None,
        );
        db.blobs().await.unwrap()
    }

    #[tokio::test]
    async fn writes_reads_and_checks_existence() {
        let store = store().await;
        let bytes = b"hello world";
        let id = store.write(bytes).await.unwrap();

        assert_eq!(
            store.read(&id).await.unwrap(),
            Some(Bytes::from_static(bytes))
        );
        assert_eq!(store.write(bytes).await.unwrap(), id);
        assert!(store.exists(&id).await.unwrap());
        assert!(!store.exists(&RunBlobId::new(b"missing")).await.unwrap());
    }

    #[tokio::test]
    async fn empty_blobs_round_trip() {
        let store = store().await;
        let id = store.write(b"").await.unwrap();

        assert_eq!(store.read(&id).await.unwrap(), Some(Bytes::new()));
    }

    #[tokio::test]
    async fn raw_db_reads_exact_blob_bytes() {
        let raw_db = Arc::new(
            slatedb::Db::open("blob-store-tests", Arc::new(InMemory::new()))
                .await
                .unwrap(),
        );
        let store = BlobStore::new(Arc::clone(&raw_db));
        let bytes = b"{\"ok\":true}";
        let id = store.write(bytes).await.unwrap();

        let saved = raw_db
            .get(SlateKey::new("blobs").with("sha256").with(id))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(saved.as_ref(), bytes);
    }
}
