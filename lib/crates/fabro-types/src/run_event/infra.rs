use serde::{Deserialize, Serialize};

/// Legacy `run.notice` codes paired with the new `metadata.snapshot.failed`
/// event for backward compatibility. Display layers suppress these so the
/// typed event renders without a duplicate raw warning.
pub const NOTICE_CODE_CHECKPOINT_METADATA_WRITE_FAILED: &str = "checkpoint_metadata_write_failed";
pub const NOTICE_CODE_CHECKPOINT_METADATA_PUSH_FAILED: &str = "checkpoint_metadata_push_failed";

#[must_use]
pub fn is_metadata_snapshot_compat_notice_code(code: &str) -> bool {
    matches!(
        code,
        NOTICE_CODE_CHECKPOINT_METADATA_WRITE_FAILED | NOTICE_CODE_CHECKPOINT_METADATA_PUSH_FAILED
    )
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
    pub phase:        MetadataSnapshotPhase,
    pub branch:       String,
    pub duration_ms:  u64,
    pub failure_kind: MetadataSnapshotFailureKind,
    pub error:        String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub causes:       Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_sha:   Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_count:  Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes:        Option<u64>,
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
    pub command:   String,
    pub index:     usize,
    pub exit_code: i32,
    pub stderr:    String,
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
    pub cli_name:    String,
    pub provider:    String,
    pub error:       String,
    pub duration_ms: u64,
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
    pub phase:     String,
    pub command:   String,
    pub index:     usize,
    pub exit_code: i32,
    pub stderr:    String,
}
