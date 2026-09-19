//! A run notice: something Fabro wants a reader to know about a run that
//! is not the engine's own event, recorded as a `run.notice` platform
//! record.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunNoticeLevel {
    Info,
    Warn,
    Error,
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    strum::Display,
    strum::EnumString,
    strum::IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum RunNoticeCode {
    ArtifactCollectionFailed,
    ArtifactOffloadFailed,
    ArtifactSyncFailed,
    ArtifactUploadFailed,
    CheckpointMetadataDegraded,
    CheckpointMetadataPushFailed,
    CheckpointMetadataWriteFailed,
    DirtyWorktree,
    GitDiffFailed,
    GitIdentityFallback,
    GitPushFailed,
    GithubTokenFailed,
    GithubTokenRefreshLimited,
    ModelFallbackChainEmpty,
    ModelFallbackSkipped,
    PullRequestFailed,
    SandboxCleanupFailed,
    SandboxGitUnavailable,
    SandboxPreserved,
    WorktreeSkippedNoGit,
}

impl RunNoticeCode {
    #[must_use]
    pub fn is_metadata_snapshot_compat(self) -> bool {
        matches!(
            self,
            Self::CheckpointMetadataWriteFailed | Self::CheckpointMetadataPushFailed
        )
    }
}
