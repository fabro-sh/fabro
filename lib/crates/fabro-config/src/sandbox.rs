use std::collections::HashMap;

use anyhow::anyhow;
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, crate::Combine)]
pub struct DaytonaConfig {
    pub auto_stop_interval: Option<i32>,
    pub labels: Option<HashMap<String, String>>,
    pub snapshot: Option<DaytonaSnapshotConfig>,
    pub network: Option<DaytonaNetwork>,
    /// Skip git repo detection and cloning during initialization.
    pub skip_clone: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct DaytonaSettings {
    pub auto_stop_interval: Option<i32>,
    pub labels: Option<HashMap<String, String>>,
    pub snapshot: Option<DaytonaSnapshotSettings>,
    pub network: Option<DaytonaNetwork>,
    /// Skip git repo detection and cloning during initialization.
    #[serde(default)]
    pub skip_clone: bool,
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

/// Network access mode for a Daytona sandbox.
///
/// TOML syntax:
/// ```toml
/// network = "block"                                  # no egress
/// network = "allow_all"                              # full access (default)
/// network = { allow_list = ["208.80.154.232/32"] }   # CIDR allowlist
/// ```
#[derive(Clone, Debug, PartialEq, crate::Combine)]
pub enum DaytonaNetwork {
    Block,
    AllowAll,
    AllowList(Vec<String>),
}

impl Serialize for DaytonaNetwork {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            DaytonaNetwork::Block => serializer.serialize_str("block"),
            DaytonaNetwork::AllowAll => serializer.serialize_str("allow_all"),
            DaytonaNetwork::AllowList(cidrs) => {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("allow_list", cidrs)?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for DaytonaNetwork {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct DaytonaNetworkVisitor;

        impl<'de> Visitor<'de> for DaytonaNetworkVisitor {
            type Value = DaytonaNetwork;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(
                    formatter,
                    r#""block", "allow_all", or {{ allow_list = [...] }}"#
                )
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<DaytonaNetwork, E> {
                match value {
                    "block" => Ok(DaytonaNetwork::Block),
                    "allow_all" => Ok(DaytonaNetwork::AllowAll),
                    other => Err(de::Error::custom(format!(
                        "unknown network mode \"{other}\": expected \"block\" or \"allow_all\""
                    ))),
                }
            }

            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<DaytonaNetwork, M::Error> {
                let Some(key) = map.next_key::<String>()? else {
                    return Err(de::Error::custom(
                        "empty table: expected { allow_list = [...] }",
                    ));
                };

                if key != "allow_list" {
                    return Err(de::Error::custom(format!(
                        "unknown key \"{key}\": expected \"allow_list\""
                    )));
                }

                let cidrs: Vec<String> = map.next_value()?;

                if cidrs.is_empty() {
                    return Err(de::Error::custom("allow_list must not be empty"));
                }

                if let Some(extra) = map.next_key::<String>()? {
                    return Err(de::Error::custom(format!(
                        "unexpected key \"{extra}\": allow_list table must have exactly one key"
                    )));
                }

                Ok(DaytonaNetwork::AllowList(cidrs))
            }
        }

        deserializer.deserialize_any(DaytonaNetworkVisitor)
    }
}

/// Source for a snapshot Dockerfile.
///
/// TOML syntax:
/// ```toml
/// dockerfile = "FROM rust:1.85-slim-bookworm"          # inline content
/// dockerfile = { path = "./Dockerfile" }                # file reference
/// ```
///
/// `Path` variants are resolved to `Inline` during config loading
/// (see `run_config::resolve_dockerfile`), so downstream consumers
/// should only ever see `Inline`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, crate::Combine)]
#[serde(untagged)]
pub enum DockerfileSource {
    Inline(String),
    Path { path: String },
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

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DaytonaSnapshotSettings {
    pub name: String,
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
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ExeSettings {
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

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SshSettings {
    /// SSH destination (e.g. `user@host` or an SSH alias).
    pub destination: String,
    /// Remote working directory.
    pub working_directory: String,
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

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize, crate::Combine)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeMode {
    Always,
    #[default]
    Clean,
    Dirty,
    Never,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, crate::Combine)]
pub struct LocalSandboxConfig {
    pub worktree_mode: Option<WorktreeMode>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct LocalSandboxSettings {
    #[serde(default)]
    pub worktree_mode: WorktreeMode,
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

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct SandboxSettings {
    pub provider: Option<String>,
    pub preserve: Option<bool>,
    pub devcontainer: Option<bool>,
    pub local: Option<LocalSandboxSettings>,
    pub daytona: Option<DaytonaSettings>,
    #[cfg(feature = "exedev")]
    pub exe: Option<ExeSettings>,
    pub ssh: Option<SshSettings>,
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
