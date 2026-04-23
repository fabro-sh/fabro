//! Project domain: first-class project object.
//!
//! `[project]` replaces the old flat `[fabro]` shape. `directory` means the
//! Fabro-managed project directory inside the repo, defaulting to `.`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// A structurally resolved `[project]` view for consumers.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProjectNamespace {
    pub name:        Option<String>,
    pub description: Option<String>,
    pub directory:   String,
    pub metadata:    HashMap<String, String>,
}
