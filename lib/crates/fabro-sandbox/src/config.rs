//! Sandbox configuration runtime types.
//!
//! These types are the runtime shape that the sandbox providers consume.
//! The v2 parse tree lives in `fabro_types::settings::run::RunSandboxLayer`.
//! Conversion from the v2 shape lives in [`bridge_sandbox`].
//!
//! The `DaytonaSettings`/`DaytonaSnapshotSettings` names are kept for
//! backward compatibility with the old import path; [`crate::daytona`]
//! continues to re-export them under `DaytonaConfig`/`DaytonaSnapshotConfig`
//! aliases.

use std::collections::HashMap;

use fabro_types::settings::InterpString;
use fabro_types::settings::run::{
    DaytonaDockerfileLayer, DaytonaNetworkLayer, RunSandboxLayer, WorktreeMode as V2WorktreeMode,
};
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct DaytonaSettings {
    pub auto_stop_interval: Option<i32>,
    pub labels: Option<HashMap<String, String>>,
    pub snapshot: Option<DaytonaSnapshotSettings>,
    pub network: Option<DaytonaNetwork>,
    #[serde(default)]
    pub skip_clone: bool,
}

#[derive(Clone, Debug, PartialEq)]
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
            Self::Block => serializer.serialize_str("block"),
            Self::AllowAll => serializer.serialize_str("allow_all"),
            Self::AllowList(cidrs) => {
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DockerfileSource {
    Inline(String),
    Path { path: String },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DaytonaSnapshotSettings {
    pub name: String,
    pub cpu: Option<i32>,
    pub memory: Option<i32>,
    pub disk: Option<i32>,
    pub dockerfile: Option<DockerfileSource>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeMode {
    Always,
    #[default]
    Clean,
    Dirty,
    Never,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct LocalSandboxSettings {
    #[serde(default)]
    pub worktree_mode: WorktreeMode,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct SandboxSettings {
    pub provider: Option<String>,
    pub preserve: Option<bool>,
    pub devcontainer: Option<bool>,
    pub local: Option<LocalSandboxSettings>,
    pub daytona: Option<DaytonaSettings>,
    pub env: Option<HashMap<String, String>>,
}

/// Convert a v2 [`RunSandboxLayer`] into the runtime [`SandboxSettings`] shape.
#[must_use]
pub fn bridge_sandbox(sb: &RunSandboxLayer) -> SandboxSettings {
    SandboxSettings {
        provider: sb.provider.clone(),
        preserve: sb.preserve,
        devcontainer: sb.devcontainer,
        local: sb.local.as_ref().map(|local| LocalSandboxSettings {
            worktree_mode: local
                .worktree_mode
                .map(bridge_worktree_mode)
                .unwrap_or_default(),
        }),
        daytona: sb.daytona.as_ref().map(|d| DaytonaSettings {
            auto_stop_interval: d.auto_stop_interval,
            labels: if d.labels.is_empty() {
                None
            } else {
                Some(d.labels.clone())
            },
            snapshot: d.snapshot.as_ref().and_then(|s| {
                s.name.as_ref().map(|name| DaytonaSnapshotSettings {
                    name: name.clone(),
                    cpu: s.cpu,
                    memory: s.memory.map(|sz| size_to_gb_i32(sz.as_bytes())),
                    disk: s.disk.map(|sz| size_to_gb_i32(sz.as_bytes())),
                    dockerfile: s.dockerfile.as_ref().map(|d| match d {
                        DaytonaDockerfileLayer::Inline(text) => {
                            DockerfileSource::Inline(text.clone())
                        }
                        DaytonaDockerfileLayer::Path { path } => {
                            DockerfileSource::Path { path: path.clone() }
                        }
                    }),
                })
            }),
            network: d.network.as_ref().map(|n| match n {
                DaytonaNetworkLayer::Block => DaytonaNetwork::Block,
                DaytonaNetworkLayer::AllowAll => DaytonaNetwork::AllowAll,
                DaytonaNetworkLayer::AllowList { allow_list } => {
                    DaytonaNetwork::AllowList(allow_list.clone())
                }
            }),
            skip_clone: d.skip_clone.unwrap_or(false),
        }),
        env: if sb.env.is_empty() {
            None
        } else {
            Some(
                sb.env
                    .iter()
                    .map(|(k, v)| (k.clone(), interp_to_string(v)))
                    .collect(),
            )
        },
    }
}

/// Convert a v2 [`V2WorktreeMode`] into the runtime [`WorktreeMode`].
#[must_use]
pub fn bridge_worktree_mode(m: V2WorktreeMode) -> WorktreeMode {
    match m {
        V2WorktreeMode::Always => WorktreeMode::Always,
        V2WorktreeMode::Clean => WorktreeMode::Clean,
        V2WorktreeMode::Dirty => WorktreeMode::Dirty,
        V2WorktreeMode::Never => WorktreeMode::Never,
    }
}

fn interp_to_string(value: &InterpString) -> String {
    value.as_source()
}

fn size_to_gb_i32(bytes: u64) -> i32 {
    let gb = bytes / 1_000_000_000;
    i32::try_from(gb).unwrap_or(i32::MAX)
}
