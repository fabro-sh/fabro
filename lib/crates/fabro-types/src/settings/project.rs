//! Project domain: first-class project object.
//!
//! `[project]` replaces the old flat `[fabro]` shape. `directory` means the
//! Fabro-managed project directory inside the repo, defaulting to `.`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// A structurally resolved `[project]` view for consumers.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ProjectSettings {
    pub name: Option<String>,
    pub description: Option<String>,
    pub directory: String,
    pub metadata: HashMap<String, String>,
}

/// A sparse `[project]` layer as it appears in a single settings file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The Fabro-managed project directory inside the repo. Defaults to
    /// `.` after layering when unspecified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, String>,
}
