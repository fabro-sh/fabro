//! Sandbox configuration runtime types.
//!
//! These types are the runtime shape that the sandbox providers consume.
//!
//! The `DaytonaSettings`/`DaytonaSnapshotSettings` names are kept for
//! backward compatibility with the old import path; [`crate::daytona`]
//! continues to re-export them under `DaytonaConfig`/`DaytonaSnapshotConfig`
//! aliases.

use std::collections::HashMap;

use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Serialize};

#[cfg(feature = "forkd")]
use crate::forkd::DEFAULT_SNAPSHOT_TAG;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct DaytonaSettings {
    pub auto_stop_interval: Option<i32>,
    pub labels:             Option<HashMap<String, String>>,
    pub snapshot:           Option<DaytonaSnapshotSettings>,
    pub network:            Option<DaytonaNetwork>,
    #[serde(default)]
    pub skip_clone:         bool,
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
    pub cpu:        Option<i32>,
    pub memory:     Option<i32>,
    pub disk:       Option<i32>,
    pub dockerfile: Option<DockerfileSource>,
}

// ---------------------------------------------------------------------------
// Forkd microVM sandbox configuration types
// ---------------------------------------------------------------------------

/// Per-VM image and resource settings for a Forkd sandbox.
#[cfg(feature = "forkd")]
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ForkdSnapshotSettings {
    /// OCI image name for the microVM rootfs, e.g. "python:3.12-slim".
    pub image:          Option<String>,
    /// Path to the vmlinux kernel blob on the forkd host; resolved from
    /// `FORKD_KERNEL` env var when absent.
    pub kernel:         Option<String>,
    /// Guest memory in MiB; the forkd controller default is 1536.
    pub mem_mib:        Option<u32>,
    /// Comma-separated list of extra apt packages to install at VM boot.
    pub extra_packages: Option<String>,
}

/// Network isolation mode for a Forkd microVM.
#[cfg(feature = "forkd")]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ForkdNetwork {
    /// Block all outbound traffic (default).
    Block,
    /// Allow all outbound traffic.
    AllowAll,
    /// Allow only the specified CIDR ranges.
    AllowList(Vec<String>),
}

/// Per-sandbox runtime configuration passed in `SandboxCreateSpec::Forkd`.
///
/// Server-level connectivity (`forkd_url`, `forkd_token`) lives on
/// [`crate::forkd::ForkdConfig`] and is resolved from environment variables
/// at provider construction time — it is not part of this per-run slice.
#[cfg(feature = "forkd")]
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ForkdSettings {
    /// forkd snapshot tag to boot from (e.g. `"zen-gate-base"`).
    /// Resolved from `FORKD_SNAPSHOT_TAG` env var; default `"zen-gate-base"`.
    #[serde(default = "ForkdSettings::default_snapshot_tag")]
    pub snapshot_tag:      String,
    /// Legacy VM image/kernel/memory settings — retained for deserialization
    /// backward compatibility.  Not sent to the forkd 0.5.2 API.
    pub snapshot:          Option<ForkdSnapshotSettings>,
    /// Legacy network isolation policy — retained for deserialization backward
    /// compatibility.  Not sent to the forkd 0.5.2 API.
    pub network:           Option<ForkdNetwork>,
    /// Skip the repository clone step during `initialize()`.
    #[serde(default)]
    pub skip_clone:        bool,
    /// Auto-stop interval in minutes (`None` means no auto-stop).
    pub auto_stop_minutes: Option<i32>,
}

#[cfg(feature = "forkd")]
impl ForkdSettings {
    fn default_snapshot_tag() -> String {
        DEFAULT_SNAPSHOT_TAG.to_string()
    }
}

#[cfg(feature = "forkd")]
impl Default for ForkdSettings {
    fn default() -> Self {
        Self {
            snapshot_tag:      Self::default_snapshot_tag(),
            snapshot:          None,
            network:           None,
            skip_clone:        false,
            auto_stop_minutes: None,
        }
    }
}
