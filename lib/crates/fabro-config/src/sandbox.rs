use std::collections::HashMap;

use anyhow::anyhow;
use serde::{Deserialize, Serialize};

pub use fabro_types::settings::sandbox::{
    DaytonaNetwork, DaytonaSettings, DaytonaSnapshotSettings, DockerfileSource,
    LocalSandboxSettings, SandboxSettings, WorktreeMode,
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
            env: value.env,
        })
    }
}
