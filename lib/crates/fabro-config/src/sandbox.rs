use std::collections::HashMap;

use anyhow::anyhow;
use serde::{Deserialize, Serialize};

#[cfg(feature = "exedev")]
pub use fabro_types::settings::sandbox::ExeSettings;
pub use fabro_types::settings::sandbox::{
    DaytonaNetwork, DaytonaSettings, DaytonaSnapshotSettings, DockerfileSource,
    LocalSandboxSettings, SandboxSettings, SshSettings, WorktreeMode,
};

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, crate::Combine)]
pub struct DaytonaConfig {
    pub auto_stop_interval: Option<i32>,
    pub labels: Option<HashMap<String, String>>,
    pub snapshot: Option<DaytonaSnapshotConfig>,
    pub network: Option<DaytonaNetwork>,
    /// Skip git repo detection and cloning during initialization.
    pub skip_clone: Option<bool>,
}

impl TryFrom<DaytonaConfig> for DaytonaSettings {
    type Error = anyhow::Error;

    fn try_from(value: DaytonaConfig) -> Result<Self, Self::Error> {
        Ok(Self {
            auto_stop_interval: value.auto_stop_interval,
            labels: value.labels,
            snapshot: value.snapshot.map(TryInto::try_into).transpose()?,
            network: value.network,
            skip_clone: value.skip_clone.unwrap_or(false),
        })
    }
}

/// Snapshot configuration: when present, the sandbox is created from a snapshot
/// instead of a bare Docker image.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, crate::Combine)]
pub struct DaytonaSnapshotConfig {
    pub name: Option<String>,
    pub cpu: Option<i32>,
    pub memory: Option<i32>,
    pub disk: Option<i32>,
    pub dockerfile: Option<DockerfileSource>,
}

impl TryFrom<DaytonaSnapshotConfig> for DaytonaSnapshotSettings {
    type Error = anyhow::Error;

    fn try_from(value: DaytonaSnapshotConfig) -> Result<Self, Self::Error> {
        Ok(Self {
            name: value
                .name
                .ok_or_else(|| anyhow!("sandbox.daytona.snapshot.name is required"))?,
            cpu: value.cpu,
            memory: value.memory,
            disk: value.disk,
            dockerfile: value.dockerfile,
        })
    }
}

/// Configuration for an exe.dev sandbox (TOML target for `[sandbox.exe]`).
#[cfg(feature = "exedev")]
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, crate::Combine)]
pub struct ExeConfig {
    pub image: Option<String>,
}

#[cfg(feature = "exedev")]
impl From<ExeConfig> for ExeSettings {
    fn from(value: ExeConfig) -> Self {
        Self { image: value.image }
    }
}

/// Configuration for an SSH sandbox (TOML target for `[sandbox.ssh]`).
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, crate::Combine)]
pub struct SshConfig {
    /// SSH destination (e.g. `user@host` or an SSH alias).
    pub destination: Option<String>,
    /// Remote working directory.
    pub working_directory: Option<String>,
    /// Optional path to a custom SSH config file.
    pub config_file: Option<String>,
    /// Base URL for port previews (e.g. `"http://beast"`).
    /// When set, `get_preview_url(port)` returns `"{preview_url_base}:{port}"`.
    pub preview_url_base: Option<String>,
}

impl TryFrom<SshConfig> for SshSettings {
    type Error = anyhow::Error;

    fn try_from(value: SshConfig) -> Result<Self, Self::Error> {
        Ok(Self {
            destination: value
                .destination
                .ok_or_else(|| anyhow!("sandbox.ssh.destination is required"))?,
            working_directory: value
                .working_directory
                .ok_or_else(|| anyhow!("sandbox.ssh.working_directory is required"))?,
            config_file: value.config_file,
            preview_url_base: value.preview_url_base,
        })
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, crate::Combine)]
pub struct LocalSandboxConfig {
    pub worktree_mode: Option<WorktreeMode>,
}

impl From<LocalSandboxConfig> for LocalSandboxSettings {
    fn from(value: LocalSandboxConfig) -> Self {
        Self {
            worktree_mode: value.worktree_mode.unwrap_or_default(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, crate::Combine)]
pub struct SandboxConfig {
    pub provider: Option<String>,
    pub preserve: Option<bool>,
    pub devcontainer: Option<bool>,
    pub local: Option<LocalSandboxConfig>,
    pub daytona: Option<DaytonaConfig>,
    #[cfg(feature = "exedev")]
    pub exe: Option<ExeConfig>,
    pub ssh: Option<SshConfig>,
    pub env: Option<HashMap<String, String>>,
}

impl TryFrom<SandboxConfig> for SandboxSettings {
    type Error = anyhow::Error;

    fn try_from(value: SandboxConfig) -> Result<Self, Self::Error> {
        Ok(Self {
            provider: value.provider,
            preserve: value.preserve,
            devcontainer: value.devcontainer,
            local: value.local.map(Into::into),
            daytona: value.daytona.map(TryInto::try_into).transpose()?,
            #[cfg(feature = "exedev")]
            exe: value.exe.map(Into::into),
            ssh: value.ssh.map(TryInto::try_into).transpose()?,
            env: value.env,
        })
    }
}
