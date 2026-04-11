//! Workflow domain.
//!
//! `[workflow]` is descriptive: `name`, `description`, optional `graph` (a
//! path override for the default `workflow.fabro` file), and `metadata`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// A structurally resolved `[workflow]` view for consumers.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WorkflowSettings {
    pub name:        Option<String>,
    pub description: Option<String>,
    pub graph:       String,
    pub metadata:    HashMap<String, String>,
}

/// A sparse `[workflow]` layer as it appears in a single settings file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name:        Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Optional override for the default `workflow.fabro` graph path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph:       Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata:    HashMap<String, String>,
}
