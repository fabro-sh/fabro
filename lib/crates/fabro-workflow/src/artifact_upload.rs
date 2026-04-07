use std::path::Path;

use anyhow::Result;
use async_trait::async_trait;
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
