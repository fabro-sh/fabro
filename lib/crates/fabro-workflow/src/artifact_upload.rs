use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use fabro_store::ArtifactStore;
use fabro_types::{ArtifactUpload, StageId};

#[async_trait]
pub trait StageArtifactUploader: Send + Sync {
    async fn upload_stage_artifacts(
        &self,
        stage_id: &StageId,
        retry: u32,
        artifact_capture_dir: &Path,
        artifacts: &[ArtifactUpload],
    ) -> Result<()>;
}

pub enum ArtifactSink {
    Store(ArtifactStore),
    Uploader(Arc<dyn StageArtifactUploader>),
}
