//! Sparse top-level `[environments.<slug>]` settings layer definitions.

use fabro_types::settings::{Duration, InterpString, Size};
use serde::{Deserialize, Serialize};

use super::combine::Combine;
use super::maps::StickyMap;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, fabro_macros::Combine)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider:  Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image:     Option<EnvironmentImageLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<EnvironmentResourcesLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network:   Option<EnvironmentNetworkLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<EnvironmentLifecycleLayer>,
    #[serde(default, skip_serializing_if = "StickyMap::is_empty")]
    pub labels:    StickyMap<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volumes:   Option<Vec<EnvironmentVolumeLayer>>,
    #[serde(default, skip_serializing_if = "StickyMap::is_empty")]
    pub env:       StickyMap<InterpString>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, fabro_macros::Combine)]
#[serde(deny_unknown_fields)]
pub struct RunEnvironmentLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id:        Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image:     Option<EnvironmentImageLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<EnvironmentResourcesLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network:   Option<EnvironmentNetworkLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<EnvironmentLifecycleLayer>,
    #[serde(default, skip_serializing_if = "StickyMap::is_empty")]
    pub labels:    StickyMap<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volumes:   Option<Vec<EnvironmentVolumeLayer>>,
    #[serde(default, skip_serializing_if = "StickyMap::is_empty")]
    pub env:       StickyMap<InterpString>,
}

impl RunEnvironmentLayer {
    #[must_use]
    pub fn into_environment_override(self) -> EnvironmentLayer {
        EnvironmentLayer {
            provider:  None,
            image:     self.image,
            resources: self.resources,
            network:   self.network,
            lifecycle: self.lifecycle,
            labels:    self.labels,
            volumes:   self.volumes,
            env:       self.env,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, fabro_macros::Combine)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentImageLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docker:     Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dockerfile: Option<EnvironmentDockerfileLayer>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, fabro_macros::Combine)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentResourcesLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu:    Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<Size>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk:   Option<Size>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentNetworkLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode:  Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
}

impl Combine for EnvironmentNetworkLayer {
    fn combine(self, other: Self) -> Self {
        Self {
            mode:  self.mode.or(other.mode),
            allow: if self.allow.is_empty() {
                other.allow
            } else {
                self.allow
            },
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, fabro_macros::Combine)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentLifecycleLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preserve:         Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_on_terminal: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_stop:        Option<Duration>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentVolumeLayer {
    pub id:         String,
    pub mount_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subpath:    Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum EnvironmentDockerfileLayer {
    Inline(String),
    Path { path: String },
}
