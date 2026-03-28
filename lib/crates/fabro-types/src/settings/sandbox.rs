use std::collections::HashMap;

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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, crate::Combine)]
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

#[cfg(feature = "exedev")]
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ExeSettings {
    pub image: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SshSettings {
    pub destination: String,
    pub working_directory: String,
    pub config_file: Option<String>,
    pub preview_url_base: Option<String>,
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
    #[cfg(feature = "exedev")]
    pub exe: Option<ExeSettings>,
    pub ssh: Option<SshSettings>,
    pub env: Option<HashMap<String, String>>,
}
