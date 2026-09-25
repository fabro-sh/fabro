//! Captured workspace files go to the server's configured artifact store.
//! Engine values and patches keep using the separate blob capability.

use fabro_client::Client;
use fabro_store::ArtifactStore;
use fabro_types::{BlobHash, RunId};

#[derive(Debug, thiserror::Error)]
pub enum ArtifactWriteError {
    #[error("artifact storage failed")]
    Store(#[from] fabro_store::Error),
    #[error("artifact upload failed")]
    Upload(#[source] anyhow::Error),
}

/// Run-bound storage for captured files, keyed by the `digest` the caller
/// computed over `bytes`. Implementations publish complete objects before
/// returning, preserve errors, and permit concurrent and repeated writes of
/// identical content.
#[async_trait::async_trait]
pub trait ArtifactWriter: Send + Sync {
    async fn write(&self, digest: &BlobHash, bytes: &[u8]) -> Result<(), ArtifactWriteError>;
}

pub struct StoreArtifactWriter {
    store:  ArtifactStore,
    run_id: RunId,
}

impl StoreArtifactWriter {
    #[must_use]
    pub fn new(store: ArtifactStore, run_id: RunId) -> Self {
        Self { store, run_id }
    }
}

#[async_trait::async_trait]
impl ArtifactWriter for StoreArtifactWriter {
    async fn write(&self, digest: &BlobHash, bytes: &[u8]) -> Result<(), ArtifactWriteError> {
        self.store
            .put_capture(&self.run_id, digest, bytes)
            .await
            .map_err(ArtifactWriteError::from)
    }
}

pub struct ClientArtifactWriter {
    client: Client,
    run_id: RunId,
}

impl ClientArtifactWriter {
    #[must_use]
    pub fn new(client: Client, run_id: RunId) -> Self {
        Self { client, run_id }
    }
}

#[async_trait::async_trait]
impl ArtifactWriter for ClientArtifactWriter {
    async fn write(&self, digest: &BlobHash, bytes: &[u8]) -> Result<(), ArtifactWriteError> {
        self.client
            .write_run_artifact_content(&self.run_id, digest, bytes)
            .await
            .map_err(ArtifactWriteError::Upload)
    }
}
