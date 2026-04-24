//! Workflow domain.
//!
//! `[workflow]` is descriptive: `name`, `description`, optional `graph` (a
//! path override for the default `workflow.fabro` file), and `metadata`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// A structurally resolved `[workflow]` view for consumers.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkflowNamespace {
    pub name:        Option<String>,
    pub description: Option<String>,
    pub graph:       String,
    pub metadata:    HashMap<String, String>,
}
