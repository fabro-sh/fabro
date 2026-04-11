//! The top-level sparse settings layer.
//!
//! This struct models a single settings file (`~/.fabro/settings.toml`,
//! `.fabro/project.toml`, or `workflow.toml`) after deserialization. Fields
//! unset in the source stay `None`/empty and are layered later by
//! `fabro-config`.

use serde::{Deserialize, Serialize};

use super::cli::CliLayer;
use super::features::FeaturesLayer;
use super::project::ProjectLayer;
use super::run::RunLayer;
use super::server::ServerLayer;
use super::workflow::WorkflowLayer;

/// A sparse settings layer before merge/resolve.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SettingsLayer {
    #[serde(default, rename = "_version", skip_serializing_if = "Option::is_none")]
    pub version:  Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project:  Option<ProjectLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<WorkflowLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run:      Option<RunLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli:      Option<CliLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server:   Option<ServerLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub features: Option<FeaturesLayer>,
}
