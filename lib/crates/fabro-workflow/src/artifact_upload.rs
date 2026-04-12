use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use fabro_store::ArtifactStore;
use fabro_types::StageId;

use crate::artifact_snapshot::CapturedArtifactInfo;

#[async_trait]
pub trait StageArtifactUploader: Send + Sync {
    async fn upload_stage_artifacts(
        &self,
        stage_id: &StageId,
        artifact_capture_dir: &Path,
        artifacts: &[CapturedArtifactInfo],
    ) -> Result<()>;
}

pub enum ArtifactSink {
    Store(ArtifactStore),
    Uploader(Arc<dyn StageArtifactUploader>),
}
