use serde::{Deserialize, Serialize};

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
    GitPushFailed,
    GithubTokenFailed,
    ParallelBaseCheckpointFailed,
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

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    strum::Display,
    strum::EnumString,
    strum::IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum MetadataSnapshotPhase {
    Init,
    Checkpoint,
    Finalize,
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    strum::Display,
    strum::EnumString,
    strum::IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum MetadataSnapshotFailureKind {
    LoadState,
    Write,
    Push,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecOutputTail {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout:           Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr:           Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub stdout_truncated: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub stderr_truncated: bool,
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde skip_serializing_if predicates receive fields by reference"
)]
fn is_false(value: &bool) -> bool {
    !*value
}

impl ExecOutputTail {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.stdout.as_deref().unwrap_or("").is_empty()
            && self.stderr.as_deref().unwrap_or("").is_empty()
    }

    #[must_use]
    pub fn stdout_len(&self) -> usize {
        self.stdout.as_deref().map_or(0, str::len)
    }

    #[must_use]
    pub fn stderr_len(&self) -> usize {
        self.stderr.as_deref().map_or(0, str::len)
    }

    #[must_use]
    pub fn trace_summary(tail: Option<&Self>) -> ExecOutputTailTrace {
        ExecOutputTailTrace {
            present:          tail.is_some(),
            stdout_bytes:     tail.map_or(0, Self::stdout_len),
            stderr_bytes:     tail.map_or(0, Self::stderr_len),
            stdout_truncated: tail.is_some_and(|t| t.stdout_truncated),
            stderr_truncated: tail.is_some_and(|t| t.stderr_truncated),
        }
    }
}

/// Flat view of an `ExecOutputTail` for tracing field expansion.
#[derive(Debug, Clone, Copy)]
pub struct ExecOutputTailTrace {
    pub present:          bool,
    pub stdout_bytes:     usize,
    pub stderr_bytes:     usize,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetadataSnapshotStartedProps {
    pub phase:  MetadataSnapshotPhase,
    pub branch: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetadataSnapshotCompletedProps {
    pub phase:       MetadataSnapshotPhase,
    pub branch:      String,
    pub duration_ms: u64,
    pub entry_count: usize,
    pub bytes:       u64,
    pub commit_sha:  String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetadataSnapshotFailedProps {
    pub phase:            MetadataSnapshotPhase,
    pub branch:           String,
    pub duration_ms:      u64,
    pub failure_kind:     MetadataSnapshotFailureKind,
    pub error:            String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub causes:           Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_sha:       Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_count:      Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes:            Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec_output_tail: Option<ExecOutputTail>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SandboxInitializingProps {
    pub provider: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SandboxReadyProps {
    pub provider:    String,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name:        Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu:         Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory:      Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url:         Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SandboxFailedProps {
    pub provider:    String,
    pub error:       String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub causes:      Vec<String>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SandboxCleanupStartedProps {
    pub provider: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SandboxCleanupCompletedProps {
    pub provider:    String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SandboxCleanupFailedProps {
    pub provider: String,
    pub error:    String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub causes:   Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotNameProps {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotCompletedProps {
    pub name:        String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotFailedProps {
    pub name:   String,
    pub error:  String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub causes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitCloneStartedProps {
    pub url:    String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitCloneCompletedProps {
    pub url:         String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitCloneFailedProps {
    pub url:    String,
    pub error:  String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub causes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SandboxInitializedProps {
    pub working_directory: String,
    pub provider:          String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identifier:        Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_cloned:       Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clone_origin_url:  Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clone_branch:      Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetupStartedProps {
    pub command_count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetupCommandStartedProps {
    pub command: String,
    pub index:   usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetupCommandCompletedProps {
    pub command:     String,
    pub index:       usize,
    pub exit_code:   i32,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetupCompletedProps {
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetupFailedProps {
    pub command:          String,
    pub index:            usize,
    pub exit_code:        i32,
    pub stderr:           String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec_output_tail: Option<ExecOutputTail>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CliEnsureStartedProps {
    pub cli_name: String,
    pub provider: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CliEnsureCompletedProps {
    pub cli_name:          String,
    pub provider:          String,
    pub already_installed: bool,
    pub node_installed:    bool,
    pub duration_ms:       u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CliEnsureFailedProps {
    pub cli_name:         String,
    pub provider:         String,
    pub error:            String,
    pub duration_ms:      u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec_output_tail: Option<ExecOutputTail>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DevcontainerResolvedProps {
    pub dockerfile_lines:        usize,
    pub environment_count:       usize,
    pub lifecycle_command_count: usize,
    pub workspace_folder:        String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DevcontainerLifecycleStartedProps {
    pub phase:         String,
    pub command_count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DevcontainerLifecycleCommandStartedProps {
    pub phase:   String,
    pub command: String,
    pub index:   usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DevcontainerLifecycleCommandCompletedProps {
    pub phase:       String,
    pub command:     String,
    pub index:       usize,
    pub exit_code:   i32,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DevcontainerLifecycleCompletedProps {
    pub phase:       String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DevcontainerLifecycleFailedProps {
    pub phase:            String,
    pub command:          String,
    pub index:            usize,
    pub exit_code:        i32,
    pub stderr:           String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec_output_tail: Option<ExecOutputTail>,
}
